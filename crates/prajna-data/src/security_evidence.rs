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

/// An immutable, content-addressed evidence version id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecurityEvidenceVersionId(String);

impl SecurityEvidenceVersionId {
    /// Parses a `sha256:<64 lowercase hex>` version id.
    pub fn parse(value: impl Into<String>) -> Result<Self, SecurityEvidenceError> {
        let value = value.into();
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(SecurityEvidenceError::InvalidVersionId(value));
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(SecurityEvidenceError::InvalidVersionId(value));
        }
        Ok(Self(value))
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
    CorruptVersion(String),
    IndexConflict(String),
}

impl std::fmt::Display for SecurityEvidenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "security evidence I/O error: {error}"),
            Self::Json(error) => write!(formatter, "invalid security evidence JSON: {error}"),
            Self::InvalidRecord(reason) => write!(formatter, "invalid security evidence: {reason}"),
            Self::InvalidVersionId(id) => write!(formatter, "invalid evidence version id: {id}"),
            Self::CorruptVersion(id) => {
                write!(formatter, "security evidence version is corrupt: {id}")
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
            | Self::CorruptVersion(_)
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
            install_version(&destination, parent, &bytes, &version_id)?;
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
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("writing into String cannot fail");
    }
    SecurityEvidenceVersionId(format!("sha256:{hex}"))
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

fn install_version(
    destination: &Path,
    parent: &Path,
    bytes: &[u8],
    version_id: &SecurityEvidenceVersionId,
) -> Result<(), SecurityEvidenceError> {
    let (staging_path, mut file) = create_staging_file(parent)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    match fs::rename(&staging_path.0, destination) {
        Ok(()) => Ok(()),
        Err(_error) if destination.exists() => verify_version(destination, bytes, version_id),
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
