use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use chrono::NaiveDate;
use fs2::FileExt;
use prajna_domain::{InstrumentId, InstrumentKind, Price, Quantity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
static SECURITY_EVIDENCE_LOCK: Mutex<()> = Mutex::new(());

/// One independently sourced claim about a security field.
/// `observed_at` is the local observation time, not a publication timestamp.
/// `effective_from` and `effective_to_exclusive` describe the claimed valid
/// interval `[start, end)` when the source provides one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSource {
    pub source_id: String,
    /// A citation, URL, or Raw object id such as `sha256:<lowercase hex>`.
    pub evidence_ref: String,
    pub source_version: String,
    pub observed_at: String,
    pub effective_from: Option<NaiveDate>,
    pub effective_to_exclusive: Option<NaiveDate>,
}

/// A candidate value retained when sources disagree about one field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceClaim<T> {
    pub value: T,
    pub source: EvidenceSource,
}

/// Field-level security evidence, keeping known, unknown, not-applicable, and
/// conflicting assertions distinct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum FieldEvidence<T> {
    Known {
        value: T,
        source: EvidenceSource,
    },
    Unknown {
        reason: String,
        source: EvidenceSource,
    },
    NotApplicable {
        reason: String,
        source: EvidenceSource,
    },
    Conflict {
        candidates: Vec<EvidenceClaim<T>>,
    },
}

/// Evidence for a Shanghai/Shenzhen stock or ETF.
/// Establishment and listing are independent evidence fields. This record does
/// not contain Raw market coverage, derive tradability windows, or combine
/// pre-listing NAV/predecessor observations into trading history. Callers
/// provide a local lake root; no source is fetched by this contract. It does
/// not prove strict point-in-time availability or complete lifecycle history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityEvidenceRecord {
    pub instrument_id: InstrumentId,
    pub kind: InstrumentKind,
    pub established_date: FieldEvidence<NaiveDate>,
    pub listing_date: FieldEvidence<NaiveDate>,
    pub lot_size: FieldEvidence<Quantity>,
    pub price_increment: FieldEvidence<Price>,
    pub size_increment: FieldEvidence<Quantity>,
}

impl SecurityEvidenceRecord {
    /// Validates identity, evidence provenance, effective ranges, and positive
    /// increments before the record can be persisted.
    pub fn validate(&self) -> Result<(), SecurityEvidenceError> {
        let venue = self.instrument_id.venue().as_str();
        if !matches!(venue, "XSHG" | "XSHE") {
            return Err(SecurityEvidenceError::InvalidRecord(format!(
                "unsupported venue `{venue}`; expected XSHG or XSHE"
            )));
        }
        if !matches!(self.kind, InstrumentKind::Equity | InstrumentKind::Etf) {
            return Err(SecurityEvidenceError::InvalidRecord(
                "security evidence supports only equity and ETF instruments".to_owned(),
            ));
        }

        self.established_date
            .validate("established_date", |_| true)?;
        self.listing_date.validate("listing_date", |_| true)?;
        self.lot_size
            .validate("lot_size", |value| *value > Quantity::ZERO)?;
        self.price_increment
            .validate("price_increment", |value| *value > Price::ZERO)?;
        self.size_increment
            .validate("size_increment", |value| *value > Quantity::ZERO)?;
        Ok(())
    }
}

impl EvidenceSource {
    fn validate(&self, field: &str) -> Result<(), SecurityEvidenceError> {
        for (name, value) in [
            ("source_id", &self.source_id),
            ("evidence_ref", &self.evidence_ref),
            ("source_version", &self.source_version),
            ("observed_at", &self.observed_at),
        ] {
            if value.trim().is_empty() {
                return Err(SecurityEvidenceError::InvalidRecord(format!(
                    "{field}.{name} must not be empty"
                )));
            }
        }
        chrono::DateTime::parse_from_rfc3339(&self.observed_at).map_err(|_| {
            SecurityEvidenceError::InvalidRecord(format!(
                "{field}.observed_at must be RFC 3339 with an explicit offset"
            ))
        })?;
        if let (Some(start), Some(end)) = (self.effective_from, self.effective_to_exclusive) {
            if start >= end {
                return Err(SecurityEvidenceError::InvalidRecord(format!(
                    "{field} effective range must be non-empty"
                )));
            }
        }
        Ok(())
    }
}

impl<T> FieldEvidence<T> {
    fn validate(
        &self,
        field: &str,
        value_is_valid: impl Fn(&T) -> bool + Copy,
    ) -> Result<(), SecurityEvidenceError>
    where
        T: PartialEq,
    {
        match self {
            Self::Known { value, source } => {
                if !value_is_valid(value) {
                    return Err(SecurityEvidenceError::InvalidRecord(format!(
                        "{field} must be positive when known"
                    )));
                }
                source.validate(field)
            }
            Self::Unknown { reason, source } | Self::NotApplicable { reason, source } => {
                validate_reason(field, reason)?;
                source.validate(field)
            }
            Self::Conflict { candidates } => {
                if candidates.len() < 2
                    || !candidates.iter().enumerate().any(|(index, candidate)| {
                        candidates[index + 1..]
                            .iter()
                            .any(|other| candidate.value != other.value)
                    })
                {
                    return Err(SecurityEvidenceError::InvalidRecord(format!(
                        "{field} conflict requires at least two distinct claims"
                    )));
                }
                for candidate in candidates {
                    if !value_is_valid(&candidate.value) {
                        return Err(SecurityEvidenceError::InvalidRecord(format!(
                            "{field} conflict values must be positive"
                        )));
                    }
                    candidate.source.validate(field)?;
                }
                Ok(())
            }
        }
    }
}

fn validate_reason(field: &str, reason: &str) -> Result<(), SecurityEvidenceError> {
    if reason.trim().is_empty() {
        return Err(SecurityEvidenceError::InvalidRecord(format!(
            "{field} reason must not be empty"
        )));
    }
    Ok(())
}

/// The schema version of [`SecurityEvidenceSnapshot`].
pub const SECURITY_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// One explicitly selected evidence version and its reproducible gate outcome.
/// The full record is copied into the snapshot so an old selection remains
/// readable even after later evidence revisions are imported. The snapshot
/// contains no market coverage or availability window: `observed_at` remains
/// the local observation time and must not be read as historical availability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityInstrumentSnapshot {
    pub evidence_version: SecurityEvidenceVersionId,
    pub record: SecurityEvidenceRecord,
    pub publication: PublicationStatus,
}

/// A fixed, content-addressed set of explicitly selected evidence versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityEvidenceSnapshot {
    pub schema_version: u32,
    pub instruments: Vec<SecurityInstrumentSnapshot>,
}

/// The execution constraints required to publish a tradable daily instrument.
/// A snapshot exposes these values only when every required term is known and
/// positive; no default lot or increment is substituted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionTerms {
    pub lot_size: Quantity,
    pub price_increment: Price,
    pub size_increment: Quantity,
}

/// Whether one selected instrument has complete execution-required terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PublicationStatus {
    Publishable { terms: ExecutionTerms },
    NotPublishable { issues: Vec<PublicationIssue> },
}

/// One explicit reason an instrument cannot be published as executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationIssue {
    pub field: String,
    pub code: PublicationIssueCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationIssueCode {
    Unknown,
    Conflict,
    NotApplicable,
    NonPositive,
}

impl PublicationIssueCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Conflict => "conflict",
            Self::NotApplicable => "not_applicable",
            Self::NonPositive => "non_positive",
        }
    }
}

impl PublicationIssue {
    fn new(field: &str, code: PublicationIssueCode, message: impl Into<String>) -> Self {
        Self {
            field: field.to_owned(),
            code,
            message: message.into(),
        }
    }
}

/// Applies the execution-required publication gate to one evidence record.
/// Unknown, conflicting, non-applicable, or non-positive required terms fail
/// closed; they are never replaced with defaults.
pub fn publication_status(record: &SecurityEvidenceRecord) -> PublicationStatus {
    match (
        required_term("lot_size", &record.lot_size, |value| value > Quantity::ZERO),
        required_term("price_increment", &record.price_increment, |value| {
            value > Price::ZERO
        }),
        required_term("size_increment", &record.size_increment, |value| {
            value > Quantity::ZERO
        }),
    ) {
        (Ok(lot_size), Ok(price_increment), Ok(size_increment)) => PublicationStatus::Publishable {
            terms: ExecutionTerms {
                lot_size,
                price_increment,
                size_increment,
            },
        },
        (lot_size, price_increment, size_increment) => PublicationStatus::NotPublishable {
            issues: [lot_size.err(), price_increment.err(), size_increment.err()]
                .into_iter()
                .flatten()
                .collect(),
        },
    }
}

fn required_term<T: Copy>(
    field: &str,
    evidence: &FieldEvidence<T>,
    is_positive: impl Fn(T) -> bool,
) -> Result<T, PublicationIssue> {
    match evidence {
        FieldEvidence::Known { value, .. } if is_positive(*value) => Ok(*value),
        FieldEvidence::Known { .. } => Err(PublicationIssue::new(
            field,
            PublicationIssueCode::NonPositive,
            "known value must be positive",
        )),
        FieldEvidence::Unknown { .. } => Err(PublicationIssue::new(
            field,
            PublicationIssueCode::Unknown,
            "no known value",
        )),
        FieldEvidence::Conflict { candidates } => Err(PublicationIssue::new(
            field,
            PublicationIssueCode::Conflict,
            format!("{} conflicting candidates", candidates.len()),
        )),
        FieldEvidence::NotApplicable { .. } => Err(PublicationIssue::new(
            field,
            PublicationIssueCode::NotApplicable,
            "evidence marks the field not applicable",
        )),
    }
}

impl SecurityEvidenceSnapshot {
    fn new(instruments: Vec<SecurityInstrumentSnapshot>) -> Result<Self, SecurityEvidenceError> {
        let mut selected: std::collections::BTreeMap<InstrumentId, SecurityInstrumentSnapshot> =
            std::collections::BTreeMap::new();
        for instrument in instruments {
            let instrument_id = instrument.record.instrument_id.clone();
            if selected.insert(instrument_id.clone(), instrument).is_some() {
                return Err(SecurityEvidenceError::InvalidRecord(format!(
                    "duplicate evidence selection for {instrument_id}"
                )));
            }
        }
        if selected.is_empty() {
            return Err(SecurityEvidenceError::InvalidRecord(
                "snapshot must select at least one security evidence version".to_owned(),
            ));
        }
        let snapshot = Self {
            schema_version: SECURITY_SNAPSHOT_SCHEMA_VERSION,
            instruments: selected.into_values().collect(),
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Recomputes every embedded evidence identity, gate outcome, and ordering.
    pub fn validate(&self) -> Result<(), SecurityEvidenceError> {
        if self.schema_version != SECURITY_SNAPSHOT_SCHEMA_VERSION {
            return Err(SecurityEvidenceError::InvalidRecord(format!(
                "unsupported security snapshot schema version {}",
                self.schema_version
            )));
        }
        if self.instruments.is_empty() {
            return Err(SecurityEvidenceError::InvalidRecord(
                "snapshot must not be empty".to_owned(),
            ));
        }
        for pair in self.instruments.windows(2) {
            if pair[0].record.instrument_id >= pair[1].record.instrument_id {
                return Err(SecurityEvidenceError::InvalidRecord(
                    "snapshot instruments must be sorted and unique".to_owned(),
                ));
            }
        }
        for instrument in &self.instruments {
            instrument.record.validate()?;
            let bytes = serde_json::to_vec(&instrument.record)?;
            if calculate_version_id(&bytes) != instrument.evidence_version {
                return Err(SecurityEvidenceError::CorruptVersion(
                    instrument.evidence_version.to_string(),
                ));
            }
            if publication_status(&instrument.record) != instrument.publication {
                return Err(SecurityEvidenceError::InvalidRecord(format!(
                    "snapshot publication status does not match {}",
                    instrument.record.instrument_id
                )));
            }
        }
        Ok(())
    }

    /// Content-addressed identity of the fixed selection and its gate outcomes.
    pub fn id(&self) -> Result<SecuritySnapshotId, SecurityEvidenceError> {
        Ok(calculate_snapshot_id(&serde_json::to_vec(self)?))
    }
}

/// An immutable, content-addressed security evidence snapshot id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecuritySnapshotId(String);

impl SecuritySnapshotId {
    /// Parses a `sha256:<64 lowercase hex>` snapshot id.
    pub fn parse(value: impl Into<String>) -> Result<Self, SecurityEvidenceError> {
        parse_sha256_id(value.into())
            .map(Self)
            .map_err(SecurityEvidenceError::InvalidSnapshotId)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SecuritySnapshotId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// An immutable, content-addressed evidence version id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct SecurityEvidenceVersionId(String);

impl TryFrom<String> for SecurityEvidenceVersionId {
    type Error = SecurityEvidenceError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl SecurityEvidenceVersionId {
    /// Parses a `sha256:<64 lowercase hex>` version id.
    pub fn parse(value: impl Into<String>) -> Result<Self, SecurityEvidenceError> {
        parse_sha256_id(value.into())
            .map(Self)
            .map_err(SecurityEvidenceError::InvalidVersionId)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SecurityEvidenceVersionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn parse_sha256_id(value: String) -> Result<String, String> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(value);
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(value);
    }
    Ok(value)
}

/// Persistent, immutable versions of security lifecycle and trading-term evidence.
///
/// The caller chooses the local lake root. Re-importing identical input returns
/// the same version id; a changed record creates another id and never changes
/// the bytes or identity of an earlier version. `versions_for` returns versions
/// in import order; there is deliberately no mutable current-version pointer.
#[derive(Debug, Clone)]
pub struct SecurityEvidenceStore {
    root: PathBuf,
}

/// Errors returned by [`SecurityEvidenceStore`].
#[derive(Debug)]
pub enum SecurityEvidenceError {
    Io(io::Error),
    Json(serde_json::Error),
    InvalidRecord(String),
    InvalidVersionId(String),
    InvalidSnapshotId(String),
    CorruptVersion(String),
    CorruptSnapshot(String),
    IndexConflict(String),
}

impl std::fmt::Display for SecurityEvidenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "security evidence I/O error: {error}"),
            Self::Json(error) => write!(formatter, "invalid security evidence JSON: {error}"),
            Self::InvalidRecord(reason) => write!(formatter, "invalid security evidence: {reason}"),
            Self::InvalidVersionId(id) => write!(formatter, "invalid evidence version id: {id}"),
            Self::InvalidSnapshotId(id) => write!(formatter, "invalid evidence snapshot id: {id}"),
            Self::CorruptVersion(id) => {
                write!(formatter, "security evidence version is corrupt: {id}")
            }
            Self::CorruptSnapshot(id) => {
                write!(formatter, "security evidence snapshot is corrupt: {id}")
            }
            Self::IndexConflict(id) => {
                write!(formatter, "evidence version index conflicts for {id}")
            }
        }
    }
}

impl std::error::Error for SecurityEvidenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::InvalidRecord(_)
            | Self::InvalidVersionId(_)
            | Self::InvalidSnapshotId(_)
            | Self::CorruptVersion(_)
            | Self::CorruptSnapshot(_)
            | Self::IndexConflict(_) => None,
        }
    }
}

impl From<io::Error> for SecurityEvidenceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for SecurityEvidenceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl SecurityEvidenceStore {
    /// Opens a store rooted under `<root>/security-evidence`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, SecurityEvidenceError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("security-evidence"))?;
        Ok(Self { root })
    }

    /// Validates and persists a record, returning its content-addressed version.
    pub fn import(
        &self,
        record: &SecurityEvidenceRecord,
    ) -> Result<SecurityEvidenceVersionId, SecurityEvidenceError> {
        record.validate()?;
        let bytes = serde_json::to_vec(record)?;
        let version_id = calculate_version_id(&bytes);
        let destination = self.object_path(&version_id);
        let parent = destination.parent().expect("version path has a parent");
        fs::create_dir_all(parent)?;
        let _guard = SECURITY_EVIDENCE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if destination.exists() {
            verify_version(&destination, &bytes, &version_id)?;
        } else {
            install_content(&destination, parent, &bytes)?;
        }
        self.index_version(&record.instrument_id, &version_id)?;
        Ok(version_id)
    }

    /// Reads and validates one exact immutable version.
    pub fn get(
        &self,
        version_id: &SecurityEvidenceVersionId,
    ) -> Result<SecurityEvidenceRecord, SecurityEvidenceError> {
        let path = self.object_path(version_id);
        let bytes = fs::read(path)?;
        if calculate_version_id(&bytes) != *version_id {
            return Err(SecurityEvidenceError::CorruptVersion(
                version_id.to_string(),
            ));
        }
        let record: SecurityEvidenceRecord = serde_json::from_slice(&bytes)?;
        record.validate()?;
        Ok(record)
    }

    /// Lists immutable version ids for one canonical instrument in import order.
    pub fn versions_for(
        &self,
        instrument_id: &InstrumentId,
    ) -> Result<Vec<SecurityEvidenceVersionId>, SecurityEvidenceError> {
        let path = self.index_path();
        let mut file = match OpenOptions::new().read(true).open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        FileExt::lock_shared(&file)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        let mut seen = std::collections::HashSet::new();
        let mut versions = Vec::new();
        for line in contents.lines().filter(|line| !line.is_empty()) {
            let entry: IndexEntry = serde_json::from_str(line)?;
            if &entry.instrument_id == instrument_id && seen.insert(entry.version_id.clone()) {
                versions.push(SecurityEvidenceVersionId::parse(entry.version_id)?);
            }
        }
        Ok(versions)
    }

    /// Selects explicit evidence versions and persists their fixed snapshot.
    ///
    /// The same selected set is idempotent regardless of input order. A changed
    /// evidence revision creates a new snapshot; an earlier snapshot is never
    /// rewritten and remains readable by its exact id.
    pub fn select_snapshot(
        &self,
        selections: impl IntoIterator<Item = (InstrumentId, SecurityEvidenceVersionId)>,
    ) -> Result<(SecuritySnapshotId, SecurityEvidenceSnapshot), SecurityEvidenceError> {
        let mut instruments = Vec::new();
        for (instrument_id, evidence_version) in selections {
            let record = self.get(&evidence_version).map_err(|error| match error {
                SecurityEvidenceError::Io(error) if error.kind() == io::ErrorKind::NotFound => {
                    SecurityEvidenceError::InvalidRecord(format!(
                        "evidence version {evidence_version} is not readable for {instrument_id}"
                    ))
                }
                other => other,
            })?;
            if record.instrument_id != instrument_id {
                return Err(SecurityEvidenceError::InvalidRecord(format!(
                    "evidence version {evidence_version} belongs to {}, not {instrument_id}",
                    record.instrument_id
                )));
            }
            let publication = publication_status(&record);
            instruments.push(SecurityInstrumentSnapshot {
                evidence_version,
                record,
                publication,
            });
        }
        let snapshot = SecurityEvidenceSnapshot::new(instruments)?;
        let bytes = serde_json::to_vec(&snapshot)?;
        let snapshot_id = calculate_snapshot_id(&bytes);
        let destination = self.snapshot_path(&snapshot_id);
        let parent = destination.parent().expect("snapshot path has a parent");
        fs::create_dir_all(parent)?;
        let _guard = SECURITY_EVIDENCE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if destination.exists() {
            let stored = fs::read(&destination)?;
            if stored != bytes || calculate_snapshot_id(&stored) != snapshot_id {
                return Err(SecurityEvidenceError::CorruptSnapshot(
                    snapshot_id.to_string(),
                ));
            }
        } else {
            install_content(&destination, parent, &bytes)?;
        }
        Ok((snapshot_id, snapshot))
    }

    /// Reads and validates one exact immutable snapshot.
    pub fn get_snapshot(
        &self,
        snapshot_id: &SecuritySnapshotId,
    ) -> Result<SecurityEvidenceSnapshot, SecurityEvidenceError> {
        let path = self.snapshot_path(snapshot_id);
        let bytes = fs::read(path)?;
        if calculate_snapshot_id(&bytes) != *snapshot_id {
            return Err(SecurityEvidenceError::CorruptSnapshot(
                snapshot_id.to_string(),
            ));
        }
        let snapshot: SecurityEvidenceSnapshot = serde_json::from_slice(&bytes)?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    fn object_path(&self, version_id: &SecurityEvidenceVersionId) -> PathBuf {
        let hex = &version_id.as_str()["sha256:".len()..];
        self.root
            .join("security-evidence/sha256")
            .join(&hex[..2])
            .join(format!("{hex}.json"))
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("security-evidence/versions.jsonl")
    }

    fn snapshot_path(&self, snapshot_id: &SecuritySnapshotId) -> PathBuf {
        let hex = &snapshot_id.as_str()["sha256:".len()..];
        self.root
            .join("security-evidence-snapshots/sha256")
            .join(&hex[..2])
            .join(format!("{hex}.json"))
    }

    fn index_version(
        &self,
        instrument_id: &InstrumentId,
        version_id: &SecurityEvidenceVersionId,
    ) -> Result<(), SecurityEvidenceError> {
        let path = self.index_path();
        let parent = path.parent().expect("index path has a parent");
        fs::create_dir_all(parent)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        FileExt::lock_exclusive(&file)?;
        let mut contents = String::new();
        file.seek(SeekFrom::Start(0))?;
        file.read_to_string(&mut contents)?;
        for line in contents.lines().filter(|line| !line.is_empty()) {
            let entry: IndexEntry = serde_json::from_str(line)?;
            if entry.version_id == version_id.as_str() {
                if &entry.instrument_id != instrument_id {
                    return Err(SecurityEvidenceError::IndexConflict(version_id.to_string()));
                }
                return Ok(());
            }
        }
        file.seek(SeekFrom::End(0))?;
        let mut line = serde_json::to_vec(&IndexEntry {
            instrument_id: instrument_id.clone(),
            version_id: version_id.to_string(),
        })?;
        line.push(b'\n');
        file.write_all(&line)?;
        file.sync_all()?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexEntry {
    instrument_id: InstrumentId,
    version_id: String,
}

fn calculate_version_id(bytes: &[u8]) -> SecurityEvidenceVersionId {
    SecurityEvidenceVersionId(format!("sha256:{}", sha256_hex(bytes)))
}

fn calculate_snapshot_id(bytes: &[u8]) -> SecuritySnapshotId {
    SecuritySnapshotId(format!("sha256:{}", sha256_hex(bytes)))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("writing into String cannot fail");
    }
    hex
}

fn verify_version(
    path: &Path,
    expected: &[u8],
    version_id: &SecurityEvidenceVersionId,
) -> Result<(), SecurityEvidenceError> {
    let stored = fs::read(path)?;
    if stored != expected || calculate_version_id(&stored) != *version_id {
        return Err(SecurityEvidenceError::CorruptVersion(
            version_id.to_string(),
        ));
    }
    Ok(())
}

struct TempPath(PathBuf);

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn install_content(
    destination: &Path,
    parent: &Path,
    bytes: &[u8],
) -> Result<(), SecurityEvidenceError> {
    let (staging_path, mut file) = create_staging_file(parent)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    match fs::rename(&staging_path.0, destination) {
        Ok(()) => Ok(()),
        Err(_error) if destination.exists() => {
            if fs::read(destination)? != bytes {
                return Err(SecurityEvidenceError::InvalidRecord(
                    "existing content conflicts with staged bytes".to_owned(),
                ));
            }
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn create_staging_file(parent: &Path) -> Result<(TempPath, File), SecurityEvidenceError> {
    loop {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".security-evidence-{}-{id}.tmp",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((TempPath(path), file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
}
