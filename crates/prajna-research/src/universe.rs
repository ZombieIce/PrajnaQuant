use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use prajna_data::{
    CanonicalError, DataError, ManifestError, instruments_from_record_batch, instruments_schema,
    logical_hash, read_manifest, restricted_jcs, schema_fingerprint,
};
use prajna_domain::InstrumentId;
use serde::{Deserialize, Serialize};
use serde_json::{self, json};
use sha2::{Digest, Sha256};

use crate::dsv::{DsvTableError, read_table};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticUniverse {
    label: String,
    members: Vec<InstrumentId>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UniverseDocument {
    universe_version: u32,
    label: String,
    members: Vec<InstrumentId>,
}

#[derive(Debug)]
pub enum UniverseError {
    EmptyMembers,
    Manifest(ManifestError),
    Data(DataError),
    Io(io::Error),
    Canonical(CanonicalError),
    Json(serde_json::Error),
    InvalidId(String),
    UnsupportedVersion(u32),
    IntegrityMismatch {
        requested: String,
        calculated: String,
    },
    NonCanonicalContent,
    Conflict(PathBuf),
    InvalidManifest(String),
    InvalidTableData(String),
    MissingMembers(Vec<String>),
}

impl fmt::Display for UniverseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyMembers => formatter.write_str("static universe must have members"),
            Self::Manifest(error) => write!(formatter, "invalid DSV manifest: {error}"),
            Self::Data(error) => write!(formatter, "invalid D7 Parquet data: {error}"),
            Self::Io(error) => write!(formatter, "universe storage I/O error: {error}"),
            Self::Canonical(error) => write!(formatter, "could not canonicalize universe: {error}"),
            Self::Json(error) => write!(formatter, "could not encode universe JSON: {error}"),
            Self::InvalidId(id) => write!(formatter, "invalid static universe ID: {id}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported static universe version {version}")
            }
            Self::IntegrityMismatch {
                requested,
                calculated,
            } => write!(
                formatter,
                "static universe ID integrity mismatch: requested {requested}, calculated {calculated}"
            ),
            Self::NonCanonicalContent => {
                formatter.write_str("stored static universe is not canonical JSON")
            }
            Self::Conflict(path) => {
                write!(
                    formatter,
                    "existing static universe conflicts at {}",
                    path.display()
                )
            }
            Self::InvalidManifest(message) => {
                write!(formatter, "invalid universe DSV manifest: {message}")
            }
            Self::InvalidTableData(message) => {
                write!(formatter, "invalid instruments table: {message}")
            }
            Self::MissingMembers(members) => write!(
                formatter,
                "universe members are absent from the DSV instruments table: {}",
                members.join(", ")
            ),
        }
    }
}

impl Error for UniverseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Manifest(error) => Some(error),
            Self::Data(error) => Some(error),
            Self::Canonical(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::EmptyMembers
            | Self::InvalidId(_)
            | Self::UnsupportedVersion(_)
            | Self::IntegrityMismatch { .. }
            | Self::NonCanonicalContent
            | Self::Conflict(_)
            | Self::InvalidManifest(_)
            | Self::InvalidTableData(_)
            | Self::MissingMembers(_) => None,
        }
    }
}

impl From<ManifestError> for UniverseError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

impl From<DataError> for UniverseError {
    fn from(error: DataError) -> Self {
        Self::Data(error)
    }
}

impl From<DsvTableError> for UniverseError {
    fn from(error: DsvTableError) -> Self {
        match error {
            DsvTableError::Data(error) => Self::Data(error),
            DsvTableError::InvalidManifest(message) => Self::InvalidManifest(message),
        }
    }
}

impl From<io::Error> for UniverseError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<CanonicalError> for UniverseError {
    fn from(error: CanonicalError) -> Self {
        Self::Canonical(error)
    }
}

impl From<serde_json::Error> for UniverseError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl StaticUniverse {
    pub fn new(
        label: impl Into<String>,
        mut members: Vec<InstrumentId>,
    ) -> Result<Self, UniverseError> {
        members.sort_by_key(ToString::to_string);
        members.dedup();
        if members.is_empty() {
            return Err(UniverseError::EmptyMembers);
        }
        Ok(Self {
            label: label.into(),
            members,
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn members(&self) -> &[InstrumentId] {
        &self.members
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, UniverseError> {
        let value = json!({
            "universe_version": 1,
            "label": self.label,
            "members": self.members.iter().map(ToString::to_string).collect::<Vec<_>>(),
        });
        Ok(restricted_jcs(&value)?)
    }

    pub fn id(&self) -> Result<String, UniverseError> {
        let digest = Sha256::digest(self.canonical_json()?);
        Ok(format!("uni:sha256:{digest:x}"))
    }

    pub fn store(&self, lake_root: impl AsRef<Path>) -> Result<PathBuf, UniverseError> {
        let id = self.id()?;
        let hex = parse_universe_id(&id)?;
        let bytes = self.canonical_json()?;
        let directory = lake_root.as_ref().join("universes");
        fs::create_dir_all(&directory)?;
        let destination = directory.join(format!("{hex}.json"));

        if destination.exists() {
            return verify_existing(&destination, &bytes);
        }

        let (staging_path, mut staging_file) = create_staging_file(&directory)?;
        staging_file.write_all(&bytes)?;
        staging_file.sync_all()?;
        drop(staging_file);
        match fs::rename(&staging_path.0, &destination) {
            Ok(()) => {}
            Err(_error) if destination.exists() => {
                verify_existing(&destination, &bytes)?;
                fs::remove_file(&staging_path.0)?;
            }
            Err(error) => return Err(error.into()),
        }
        File::open(&directory)?.sync_all()?;
        Ok(destination)
    }

    pub fn load(lake_root: impl AsRef<Path>, id: &str) -> Result<Self, UniverseError> {
        let hex = parse_universe_id(id)?;
        let path = lake_root
            .as_ref()
            .join("universes")
            .join(format!("{hex}.json"));
        let bytes = fs::read(path)?;
        let document: UniverseDocument = serde_json::from_slice(&bytes)?;
        if document.universe_version != 1 {
            return Err(UniverseError::UnsupportedVersion(document.universe_version));
        }
        let universe = Self::new(document.label, document.members)?;
        let calculated = universe.id()?;
        if calculated != id {
            return Err(UniverseError::IntegrityMismatch {
                requested: id.to_owned(),
                calculated,
            });
        }
        if universe.canonical_json()? != bytes {
            return Err(UniverseError::NonCanonicalContent);
        }
        Ok(universe)
    }

    pub fn validate_against(
        &self,
        lake_root: impl AsRef<Path>,
        dsv: &str,
    ) -> Result<(), UniverseError> {
        let available = read_dsv_instruments(lake_root.as_ref(), dsv)?;
        let missing = self
            .members
            .iter()
            .filter(|member| !available.contains(*member))
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(UniverseError::MissingMembers(missing))
        }
    }
}

fn read_dsv_instruments(
    lake_root: &Path,
    dsv: &str,
) -> Result<BTreeSet<InstrumentId>, UniverseError> {
    let manifest = read_manifest(lake_root, dsv)?;
    if manifest.core.manifest_version != 1 {
        return Err(UniverseError::InvalidManifest(format!(
            "unsupported manifest version {}",
            manifest.core.manifest_version
        )));
    }
    let table = manifest
        .core
        .tables
        .iter()
        .find(|table| table.table == "instruments")
        .ok_or_else(|| UniverseError::InvalidManifest("missing instruments table".into()))?;
    let batches = read_table(lake_root, &manifest, "instruments")?;
    let schema = batches
        .first()
        .map(|batch| batch.schema())
        .unwrap_or_else(instruments_schema);
    if schema_fingerprint(&schema)? != table.schema_fingerprint {
        return Err(UniverseError::InvalidManifest(
            "instruments schema fingerprint does not match the manifest".into(),
        ));
    }
    let row_count = batches.iter().try_fold(0_u64, |count, batch| {
        let batch_rows = u64::try_from(batch.num_rows()).map_err(|_| {
            UniverseError::InvalidTableData("instruments row count exceeds u64".into())
        })?;
        count.checked_add(batch_rows).ok_or_else(|| {
            UniverseError::InvalidTableData("instruments row count exceeds u64".into())
        })
    })?;
    if row_count != table.row_count {
        return Err(UniverseError::InvalidManifest(format!(
            "instruments row count does not match the manifest: expected {}, found {row_count}",
            table.row_count
        )));
    }
    let calculated_hash = logical_hash("instruments", &schema, &batches, &["instrument_id"])?;
    if calculated_hash != table.logical_hash {
        return Err(UniverseError::InvalidManifest(
            "instruments logical hash does not match the manifest".into(),
        ));
    }
    let mut instruments = BTreeSet::new();
    for batch in batches {
        for spec in instruments_from_record_batch(&batch)? {
            if !instruments.insert(spec.id().clone()) {
                return Err(UniverseError::InvalidTableData(format!(
                    "duplicate instrument_id {}",
                    spec.id()
                )));
            }
        }
    }
    Ok(instruments)
}

fn parse_universe_id(id: &str) -> Result<&str, UniverseError> {
    let Some(hex) = id.strip_prefix("uni:sha256:") else {
        return Err(UniverseError::InvalidId(id.to_owned()));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(UniverseError::InvalidId(id.to_owned()));
    }
    Ok(hex)
}

fn verify_existing(destination: &Path, expected: &[u8]) -> Result<PathBuf, UniverseError> {
    let existing = fs::read(destination)?;
    if existing != expected {
        return Err(UniverseError::Conflict(destination.to_owned()));
    }
    Ok(destination.to_owned())
}

struct StagingPath(PathBuf);

impl Drop for StagingPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn create_staging_file(directory: &Path) -> Result<(StagingPath, File), UniverseError> {
    loop {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(".universe-{}-{id}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((StagingPath(path), file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
}
