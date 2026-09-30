use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{CanonicalError, restricted_jcs};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
static MANIFEST_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Content-addressed dataset version manifest. Provenance is descriptive and
/// deliberately excluded from the dataset identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub dsv: String,
    pub core: ManifestCore,
    pub provenance: ManifestProvenance,
}

/// The canonical, content-addressed portion of a dataset manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestCore {
    pub manifest_version: u32,
    pub normalizer: NormalizerIdentity,
    pub inputs: Vec<ManifestInput>,
    pub tables: Vec<ManifestTable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizerIdentity {
    pub id: String,
    pub version: String,
    pub config_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestInput {
    pub raw_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTable {
    pub table: String,
    pub schema_version: u32,
    pub schema_fingerprint: String,
    pub logical_hash: String,
    pub row_count: u64,
}

/// Caller-owned, non-identity audit details from D8.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestProvenance {
    #[serde(default)]
    pub files: Vec<ManifestFile>,
    #[serde(default)]
    pub writer: Value,
    pub created_at: String,
    #[serde(default)]
    pub git_rev: Option<String>,
    #[serde(default)]
    pub git_dirty: Option<bool>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub synthetic_assumptions: Vec<String>,
    #[serde(default)]
    pub coverage: Value,
    #[serde(default)]
    pub ignored_input_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    pub path: String,
    pub sha256: String,
    pub byte_len: u64,
}

#[derive(Debug)]
pub enum ManifestError {
    Io(io::Error),
    Json(serde_json::Error),
    Canonical(CanonicalError),
    InvalidDsv(String),
    UnsortedCore,
    IntegrityMismatch { stored: String, calculated: String },
    Conflict(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "manifest I/O error: {error}"),
            Self::Json(error) => write!(f, "invalid manifest JSON: {error}"),
            Self::Canonical(error) => write!(f, "invalid manifest core: {error}"),
            Self::InvalidDsv(dsv) => write!(f, "invalid dataset version: {dsv}"),
            Self::UnsortedCore => f.write_str("manifest core inputs or tables are not sorted"),
            Self::IntegrityMismatch { stored, calculated } => write!(
                f,
                "manifest DSV integrity mismatch: stored {stored}, calculated {calculated}"
            ),
            Self::Conflict(dsv) => write!(f, "manifest core conflicts with existing DSV {dsv}"),
        }
    }
}

impl std::error::Error for ManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Canonical(error) => Some(error),
            Self::InvalidDsv(_)
            | Self::UnsortedCore
            | Self::IntegrityMismatch { .. }
            | Self::Conflict(_) => None,
        }
    }
}

impl From<io::Error> for ManifestError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ManifestError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<CanonicalError> for ManifestError {
    fn from(error: CanonicalError) -> Self {
        Self::Canonical(error)
    }
}

impl Manifest {
    /// Builds a manifest and canonicalizes unordered input and table collections.
    pub fn new(
        mut core: ManifestCore,
        provenance: ManifestProvenance,
    ) -> Result<Self, ManifestError> {
        core.inputs
            .sort_by(|left, right| left.raw_sha256.cmp(&right.raw_sha256));
        core.tables
            .sort_by(|left, right| left.table.cmp(&right.table));
        let dsv = calculate_dsv(&core)?;
        Ok(Self {
            dsv,
            core,
            provenance,
        })
    }

    /// Recomputes identity and verifies the in-memory canonical-order invariant.
    pub fn validate(&self) -> Result<(), ManifestError> {
        validate_order(&self.core)?;
        let calculated = calculate_dsv(&self.core)?;
        if calculated != self.dsv {
            return Err(ManifestError::IntegrityMismatch {
                stored: self.dsv.clone(),
                calculated,
            });
        }
        Ok(())
    }
}

/// Writes `<lake>/manifests/<dsv_hex>.json` via a synced staging file and rename.
/// An existing manifest with the same core is an idempotent success; a different
/// core at that content address is a conflict.
pub fn write_manifest(
    root: impl AsRef<Path>,
    manifest: &Manifest,
) -> Result<PathBuf, ManifestError> {
    manifest.validate()?;
    let hex = parse_dsv(&manifest.dsv)?;
    let directory = root.as_ref().join("manifests");
    fs::create_dir_all(&directory)?;
    let destination = directory.join(format!("{hex}.json"));
    let _guard = MANIFEST_WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if destination.exists() {
        let existing: Manifest = serde_json::from_slice(&fs::read(&destination)?)?;
        if existing.core != manifest.core {
            return Err(ManifestError::Conflict(manifest.dsv.clone()));
        }
        existing.validate()?;
        return Ok(destination);
    }

    let bytes = serde_json::to_vec_pretty(manifest)?;
    let (staging_path, mut staging_file) = create_staging_file(&directory)?;
    staging_file.write_all(&bytes)?;
    staging_file.sync_all()?;
    drop(staging_file);
    match fs::rename(&staging_path.0, &destination) {
        Ok(()) => {}
        Err(_error) if destination.exists() => {
            let existing: Manifest = serde_json::from_slice(&fs::read(&destination)?)?;
            if existing.core != manifest.core {
                return Err(ManifestError::Conflict(manifest.dsv.clone()));
            }
            existing.validate()?;
            return Ok(destination);
        }
        Err(error) => return Err(error.into()),
    }
    File::open(&directory)?.sync_all()?;
    Ok(destination)
}

/// Reads a manifest by its DSV and verifies both its path identity and core hash.
pub fn read_manifest(root: impl AsRef<Path>, dsv: &str) -> Result<Manifest, ManifestError> {
    let hex = parse_dsv(dsv)?;
    let path = root.as_ref().join("manifests").join(format!("{hex}.json"));
    let manifest: Manifest = serde_json::from_slice(&fs::read(path)?)?;
    if manifest.dsv != dsv {
        return Err(ManifestError::IntegrityMismatch {
            stored: manifest.dsv,
            calculated: dsv.to_owned(),
        });
    }
    manifest.validate()?;
    Ok(manifest)
}

fn calculate_dsv(core: &ManifestCore) -> Result<String, ManifestError> {
    let value = serde_json::to_value(core)?;
    let bytes = restricted_jcs(&value)?;
    Ok(format!("dsv:sha256:{:x}", Sha256::digest(bytes)))
}

fn validate_order(core: &ManifestCore) -> Result<(), ManifestError> {
    if !core
        .inputs
        .windows(2)
        .all(|pair| pair[0].raw_sha256 <= pair[1].raw_sha256)
        || !core
            .tables
            .windows(2)
            .all(|pair| pair[0].table <= pair[1].table)
    {
        return Err(ManifestError::UnsortedCore);
    }
    Ok(())
}

fn parse_dsv(dsv: &str) -> Result<&str, ManifestError> {
    let Some(hex) = dsv.strip_prefix("dsv:sha256:") else {
        return Err(ManifestError::InvalidDsv(dsv.to_owned()));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ManifestError::InvalidDsv(dsv.to_owned()));
    }
    Ok(hex)
}

struct StagingPath(PathBuf);

impl Drop for StagingPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn create_staging_file(directory: &Path) -> Result<(StagingPath, File), ManifestError> {
    loop {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(".manifest-{}-{id}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((StagingPath(path), file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
fn restricted_core(core: &ManifestCore) -> Result<Vec<u8>, ManifestError> {
    Ok(restricted_jcs(&serde_json::to_value(core)?)?)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        Manifest, ManifestCore, ManifestError, ManifestFile, ManifestInput, ManifestProvenance,
        ManifestTable, NormalizerIdentity, read_manifest, restricted_core, write_manifest,
    };

    fn provenance(created_at: &str) -> ManifestProvenance {
        ManifestProvenance {
            files: vec![ManifestFile {
                path: "fixture.json".into(),
                sha256: format!("sha256:{}", "a".repeat(64)),
                byte_len: 7,
            }],
            writer: json!({"compression": "zstd"}),
            created_at: created_at.into(),
            git_rev: Some("abc123".into()),
            git_dirty: Some(false),
            host: Some("test-host".into()),
            synthetic_assumptions: vec!["synthetic available_at = ts_close".into()],
            coverage: json!({"sessions": 2}),
            ignored_input_fields: vec!["strategy".into()],
        }
    }

    fn core(inputs: Vec<&str>, tables: Vec<&str>) -> ManifestCore {
        ManifestCore {
            manifest_version: 1,
            normalizer: NormalizerIdentity {
                id: "synthetic-etf-daily".into(),
                version: "v1".into(),
                config_sha256: format!("sha256:{}", "1".repeat(64)),
            },
            inputs: inputs
                .into_iter()
                .map(|raw_sha256| ManifestInput {
                    raw_sha256: raw_sha256.into(),
                })
                .collect(),
            tables: tables
                .into_iter()
                .map(|table| ManifestTable {
                    table: table.into(),
                    schema_version: 1,
                    schema_fingerprint: "3".repeat(64),
                    logical_hash: "2".repeat(64),
                    row_count: 2,
                })
                .collect(),
        }
    }

    #[test]
    fn manually_fixed_jcs_and_dsv_vector() {
        let manifest = Manifest::new(
            core(
                vec![&format!("sha256:{}", "0".repeat(64))],
                vec!["sessions"],
            ),
            provenance("2026-09-30T00:00:00Z"),
        )
        .unwrap();
        let expected_jcs = br#"{"inputs":[{"raw_sha256":"sha256:0000000000000000000000000000000000000000000000000000000000000000"}],"manifest_version":1,"normalizer":{"config_sha256":"sha256:1111111111111111111111111111111111111111111111111111111111111111","id":"synthetic-etf-daily","version":"v1"},"tables":[{"logical_hash":"2222222222222222222222222222222222222222222222222222222222222222","row_count":2,"schema_fingerprint":"3333333333333333333333333333333333333333333333333333333333333333","schema_version":1,"table":"sessions"}]}"#;
        assert_eq!(restricted_core(&manifest.core).unwrap(), &expected_jcs[..]);
        assert_eq!(
            manifest.dsv,
            "dsv:sha256:57c21db71d2d18a5c6fd5b849cef4b748a2c12af3479384c05497ab59dbc80ec"
        );
    }

    #[test]
    fn provenance_does_not_affect_dsv_but_each_core_field_does() {
        let baseline_core = core(vec!["raw-a"], vec!["bars"]);
        let baseline = Manifest::new(baseline_core.clone(), provenance("first")).unwrap();
        assert_eq!(
            baseline.dsv,
            Manifest::new(baseline_core.clone(), provenance("second"))
                .unwrap()
                .dsv
        );

        let mut changed = baseline_core.clone();
        changed.manifest_version += 1;
        assert_ne!(
            baseline.dsv,
            Manifest::new(changed, provenance("first")).unwrap().dsv
        );
        let mut changed = baseline_core.clone();
        changed.normalizer.id = "other-normalizer".into();
        assert_ne!(
            baseline.dsv,
            Manifest::new(changed, provenance("first")).unwrap().dsv
        );
        let mut changed = baseline_core.clone();
        changed.normalizer.version = "v2".into();
        assert_ne!(
            baseline.dsv,
            Manifest::new(changed, provenance("first")).unwrap().dsv
        );
        let mut changed = baseline_core.clone();
        changed.normalizer.config_sha256.push('x');
        assert_ne!(
            baseline.dsv,
            Manifest::new(changed, provenance("first")).unwrap().dsv
        );
        let mut changed = baseline_core.clone();
        changed.inputs[0].raw_sha256.push('x');
        assert_ne!(
            baseline.dsv,
            Manifest::new(changed, provenance("first")).unwrap().dsv
        );
        let mut changed = baseline_core.clone();
        changed.tables[0].table = "sessions".into();
        assert_ne!(
            baseline.dsv,
            Manifest::new(changed, provenance("first")).unwrap().dsv
        );
        let mut changed = baseline_core.clone();
        changed.tables[0].schema_version += 1;
        assert_ne!(
            baseline.dsv,
            Manifest::new(changed, provenance("first")).unwrap().dsv
        );
        let mut changed = baseline_core.clone();
        changed.tables[0].schema_fingerprint.push('x');
        assert_ne!(
            baseline.dsv,
            Manifest::new(changed, provenance("first")).unwrap().dsv
        );
        let mut changed = baseline_core.clone();
        changed.tables[0].logical_hash.push('x');
        assert_ne!(
            baseline.dsv,
            Manifest::new(changed, provenance("first")).unwrap().dsv
        );
        let mut changed = baseline_core;
        changed.tables[0].row_count += 1;
        assert_ne!(
            baseline.dsv,
            Manifest::new(changed, provenance("first")).unwrap().dsv
        );
    }

    #[test]
    fn constructor_sorts_inputs_and_tables_before_hashing() {
        let a = format!("sha256:{}", "a".repeat(64));
        let b = format!("sha256:{}", "b".repeat(64));
        let ordered = Manifest::new(
            core(vec![&a, &b], vec!["bars", "sessions"]),
            provenance("t"),
        )
        .unwrap();
        let reversed = Manifest::new(
            core(vec![&b, &a], vec!["sessions", "bars"]),
            provenance("t"),
        )
        .unwrap();
        assert_eq!(ordered.dsv, reversed.dsv);
        assert_eq!(ordered.core.inputs[0].raw_sha256, a);
        assert_eq!(ordered.core.tables[0].table, "bars");
    }

    #[test]
    fn write_is_idempotent_conflicts_on_core_mismatch_and_read_checks_tampering() {
        let directory = tempfile::tempdir().unwrap();
        let manifest =
            Manifest::new(core(vec!["raw-a"], vec!["bars"]), provenance("first")).unwrap();
        let path = write_manifest(directory.path(), &manifest).unwrap();
        assert_eq!(write_manifest(directory.path(), &manifest).unwrap(), path);
        let same_core_different_provenance =
            Manifest::new(manifest.core.clone(), provenance("later")).unwrap();
        assert_eq!(
            write_manifest(directory.path(), &same_core_different_provenance).unwrap(),
            path
        );
        assert_eq!(
            read_manifest(directory.path(), &manifest.dsv).unwrap(),
            manifest
        );

        let mut tampered: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        tampered["core"]["tables"][0]["row_count"] = json!(99);
        std::fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        assert!(matches!(
            read_manifest(directory.path(), &manifest.dsv),
            Err(ManifestError::IntegrityMismatch { .. })
        ));
        assert!(matches!(
            write_manifest(directory.path(), &manifest),
            Err(ManifestError::Conflict(_))
        ));
    }
}
