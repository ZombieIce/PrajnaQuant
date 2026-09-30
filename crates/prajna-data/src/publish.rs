use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    Manifest, ManifestCore, ManifestError, ManifestFile, ManifestInput, ManifestProvenance,
    ManifestTable, NormalizationIssue, NormalizerRegistry, RawInput, RawStore, RawStoreError,
    SourceKind, logical_hash, read_manifest, schema_fingerprint, write_manifest, write_parquet,
};

static NEXT_STAGE_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum PublishError {
    Raw(RawStoreError),
    Manifest(ManifestError),
    Validation(Vec<NormalizationIssue>),
    Io(io::Error),
    NormalizerNotFound { id: String, version: String },
    ExistingOutputConflict(String),
    Hash(String),
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Raw(error) => write!(formatter, "read Raw input: {error}"),
            Self::Manifest(error) => write!(formatter, "write manifest: {error}"),
            Self::Validation(issues) => write!(
                formatter,
                "normalization rejected {} issue(s)",
                issues.len()
            ),
            Self::Io(error) => write!(formatter, "publish normalized dataset: {error}"),
            Self::NormalizerNotFound { id, version } => {
                write!(formatter, "Normalizer not registered: {id} v{version}")
            }
            Self::ExistingOutputConflict(dsv) => {
                write!(formatter, "normalized output conflicts with DSV {dsv}")
            }
            Self::Hash(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PublishError {}

impl From<RawStoreError> for PublishError {
    fn from(error: RawStoreError) -> Self {
        Self::Raw(error)
    }
}

impl From<ManifestError> for PublishError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

impl From<io::Error> for PublishError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reads immutable Raw inputs, normalizes and validates them, and publishes the
/// three Parquet tables and manifest under their content-derived DSV.
pub fn publish_dataset(
    lake_root: impl AsRef<Path>,
    raw_store: &RawStore,
    registry: &NormalizerRegistry,
    normalizer_id: &str,
    normalizer_version: &str,
    raw_sha256s: &[String],
    created_at: impl Into<String>,
) -> Result<Manifest, PublishError> {
    let normalizer = registry
        .get(normalizer_id, normalizer_version)
        .ok_or_else(|| PublishError::NormalizerNotFound {
            id: normalizer_id.to_owned(),
            version: normalizer_version.to_owned(),
        })?;
    let raw_inputs = raw_sha256s
        .iter()
        .map(|hash| {
            Ok(RawInput {
                raw_sha256: hash.clone(),
                bytes: raw_store.get(hash)?,
            })
        })
        .collect::<Result<Vec<_>, RawStoreError>>()?;
    for raw_input in &raw_inputs {
        if !raw_store
            .sources(&raw_input.raw_sha256)?
            .iter()
            .any(|record| record.source_kind == SourceKind::Fixture)
        {
            return Err(PublishError::Validation(vec![NormalizationIssue {
                code: "invalid_raw_source".into(),
                path: raw_input.raw_sha256.clone(),
                message:
                    "synthetic-etf-daily requires a Raw source record with source_kind=fixture"
                        .into(),
            }]));
        }
    }
    let output = normalizer
        .normalize(&raw_inputs)
        .map_err(PublishError::Validation)?;

    let tables = [
        TableToPublish {
            name: "instruments",
            schema_version: 1,
            schema: output.tables.instruments.schema(),
            batch: &output.tables.instruments,
            primary_key: &["instrument_id"],
        },
        TableToPublish {
            name: "sessions",
            schema_version: 1,
            schema: output.tables.sessions.schema(),
            batch: &output.tables.sessions,
            primary_key: &["venue_id", "session_date"],
        },
        TableToPublish {
            name: "bars",
            schema_version: 1,
            schema: output.tables.bars.schema(),
            batch: &output.tables.bars,
            primary_key: &["instrument_id", "bar_spec", "ts_open"],
        },
    ];
    let manifest_tables = tables
        .iter()
        .map(|table| {
            Ok(ManifestTable {
                table: table.name.into(),
                schema_version: table.schema_version,
                schema_fingerprint: schema_fingerprint(&table.schema)
                    .map_err(|error| PublishError::Hash(error.to_string()))?,
                logical_hash: logical_hash(
                    table.name,
                    &table.schema,
                    std::slice::from_ref(table.batch),
                    table.primary_key,
                )
                .map_err(|error| PublishError::Hash(error.to_string()))?,
                row_count: table.batch.num_rows() as u64,
            })
        })
        .collect::<Result<Vec<_>, PublishError>>()?;
    let core = ManifestCore {
        manifest_version: 1,
        normalizer: crate::NormalizerIdentity {
            id: normalizer.id().to_owned(),
            version: normalizer.version().to_owned(),
            config_sha256: normalizer.config_sha256(),
        },
        inputs: raw_sha256s
            .iter()
            .map(|raw_sha256| ManifestInput {
                raw_sha256: raw_sha256.clone(),
            })
            .collect(),
        tables: manifest_tables,
    };
    let initial_manifest = Manifest::new(
        core.clone(),
        ManifestProvenance {
            files: raw_inputs
                .iter()
                .map(|raw_input| {
                    let hex = raw_input
                        .raw_sha256
                        .strip_prefix("sha256:")
                        .unwrap_or_default();
                    ManifestFile {
                        path: format!("raw/sha256/{}/{hex}", &hex[..2]),
                        sha256: raw_input.raw_sha256.clone(),
                        byte_len: raw_input.bytes.len() as u64,
                    }
                })
                .collect(),
            writer: json!({}),
            created_at: created_at.into(),
            git_rev: None,
            git_dirty: None,
            host: None,
            synthetic_assumptions: output.synthetic_assumptions.clone(),
            coverage: output.coverage.clone(),
            ignored_input_fields: output.ignored_input_fields.clone(),
        },
    )?;
    let lake_root = lake_root.as_ref();
    if let Some(existing) = try_read_manifest(lake_root, &initial_manifest.dsv)? {
        if existing.core != initial_manifest.core || !published_files_match(lake_root, &existing)? {
            return Err(PublishError::ExistingOutputConflict(initial_manifest.dsv));
        }
        return Ok(existing);
    }

    let dsv_hex = initial_manifest
        .dsv
        .strip_prefix("dsv:sha256:")
        .expect("Manifest::new returns a validated DSV");
    let normalized_root = lake_root.join("normalized");
    fs::create_dir_all(&normalized_root)?;
    let stage_root = create_stage_dir(&normalized_root, dsv_hex)?;
    let mut new_destinations = Vec::new();
    let result: Result<Manifest, PublishError> = (|| {
        let mut files = initial_manifest.provenance.files.clone();
        for table in &tables {
            let stage_table = stage_root.join(table.name);
            fs::create_dir_all(&stage_table)?;
            let stage_file = stage_table.join("part-00000.parquet");
            write_parquet(&stage_file, table.batch, &initial_manifest.dsv)
                .map_err(|error| PublishError::Io(io::Error::other(error.to_string())))?;
            File::open(&stage_file)?.sync_all()?;
            let bytes = fs::read(&stage_file)?;
            files.push(ManifestFile {
                path: format!("normalized/{}/{dsv_hex}/part-00000.parquet", table.name),
                sha256: format!("sha256:{:x}", Sha256::digest(&bytes)),
                byte_len: bytes.len() as u64,
            });
        }

        for table in &tables {
            let table_root = normalized_root.join(table.name);
            fs::create_dir_all(&table_root)?;
            let destination = table_root.join(dsv_hex);
            if destination.exists() {
                return Err(PublishError::Io(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("output already exists at {}", destination.display()),
                )));
            }
            fs::rename(stage_root.join(table.name), &destination)?;
            new_destinations.push(destination);
            File::open(&table_root)?.sync_all()?;
        }

        let manifest = Manifest::new(
            core,
            ManifestProvenance {
                files,
                writer: json!({
                    "compression": "ZSTD",
                    "compression_level": 3,
                    "row_group_size": 65_536,
                }),
                created_at: initial_manifest.provenance.created_at.clone(),
                git_rev: initial_manifest.provenance.git_rev.clone(),
                git_dirty: initial_manifest.provenance.git_dirty,
                host: initial_manifest.provenance.host.clone(),
                synthetic_assumptions: output.synthetic_assumptions,
                coverage: output.coverage,
                ignored_input_fields: output.ignored_input_fields,
            },
        )?;
        write_manifest(lake_root, &manifest)?;
        Ok::<Manifest, PublishError>(manifest)
    })();

    if result.is_err() {
        for destination in new_destinations.iter().rev() {
            let _ = fs::remove_dir_all(destination);
        }
    }
    let _ = fs::remove_dir_all(&stage_root);
    result
}

struct TableToPublish<'a> {
    name: &'static str,
    schema_version: u32,
    schema: SchemaRef,
    batch: &'a RecordBatch,
    primary_key: &'static [&'static str],
}

fn try_read_manifest(lake_root: &Path, dsv: &str) -> Result<Option<Manifest>, PublishError> {
    match read_manifest(lake_root, dsv) {
        Ok(manifest) => Ok(Some(manifest)),
        Err(ManifestError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn published_files_match(lake_root: &Path, manifest: &Manifest) -> Result<bool, PublishError> {
    for file in &manifest.provenance.files {
        let path = lake_root.join(&file.path);
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let hash = format!("sha256:{:x}", Sha256::digest(&bytes));
        if bytes.len() as u64 != file.byte_len || hash != file.sha256 {
            return Ok(false);
        }
    }
    Ok(manifest.provenance.files.len() == 3 + manifest.core.inputs.len())
}

fn create_stage_dir(normalized_root: &Path, dsv_hex: &str) -> io::Result<PathBuf> {
    loop {
        let id = NEXT_STAGE_ID.fetch_add(1, Ordering::Relaxed);
        let path = normalized_root.join(format!(".stage-{dsv_hex}-{}-{id}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use serde_json::json;
    use sha2::{Digest, Sha256};

    use crate::{
        NormalizerRegistry, PublishError, RawStore, SourceKind, SourceRecordInput, publish_dataset,
        read_manifest, read_parquet,
    };

    const FIXTURE: &[u8] = include_bytes!("../../../poc/poc0-benchmark/fixtures/dataset-v1.json");

    fn store_fixture(root: &Path, bytes: &[u8]) -> (RawStore, String) {
        let store = RawStore::open(root).unwrap();
        let hash = store
            .put(
                bytes,
                SourceRecordInput {
                    content_type: "application/json".into(),
                    source_kind: SourceKind::Fixture,
                    source_id: "dataset-v1.json".into(),
                    request: json!({}),
                    observed_at: "synthetic".into(),
                    ingested_by: "prajna-data-tests".into(),
                },
            )
            .unwrap();
        (store, hash)
    }

    fn replace_once(input: &[u8], before: &[u8], after: &[u8]) -> Vec<u8> {
        let index = input
            .windows(before.len())
            .position(|window| window == before)
            .expect("fixture mutation source exists");
        let mut output = input.to_vec();
        output.splice(index..index + before.len(), after.iter().copied());
        output
    }

    #[test]
    fn publishing_is_idempotent_and_records_hashes_coverage_and_assumptions() {
        let root = tempfile::tempdir().unwrap();
        let (store, raw_hash) = store_fixture(root.path(), FIXTURE);
        let registry = NormalizerRegistry::with_builtins();
        let first = publish_dataset(
            root.path(),
            &store,
            &registry,
            "synthetic-etf-daily",
            "1",
            std::slice::from_ref(&raw_hash),
            "2026-09-30T00:00:00Z",
        )
        .unwrap();
        assert_eq!(first.provenance.files.len(), 4);
        for file in &first.provenance.files {
            let bytes = fs::read(root.path().join(&file.path)).unwrap();
            assert_eq!(bytes.len() as u64, file.byte_len, "{}", file.path);
            assert_eq!(
                format!("sha256:{:x}", Sha256::digest(&bytes)),
                file.sha256,
                "{}",
                file.path
            );
        }
        let stored = read_manifest(root.path(), &first.dsv).unwrap();
        assert_eq!(stored.core, first.core);
        assert!(super::published_files_match(root.path(), &stored).unwrap());
        let second = publish_dataset(
            root.path(),
            &store,
            &registry,
            "synthetic-etf-daily",
            "1",
            std::slice::from_ref(&raw_hash),
            "2026-09-30T00:00:00Z",
        )
        .unwrap();

        assert_eq!(first.dsv, second.dsv);
        assert_eq!(first.core, second.core);
        assert_eq!(
            first
                .core
                .tables
                .iter()
                .map(|table| (table.table.as_str(), table.row_count))
                .collect::<Vec<_>>(),
            vec![("bars", 29), ("instruments", 3), ("sessions", 10)]
        );
        assert_eq!(first.provenance.files.len(), 4);
        assert!(
            first
                .provenance
                .synthetic_assumptions
                .contains(&"available_at = ts_close".into())
        );
        assert_eq!(
            first.provenance.coverage["B"]["missing_dates"],
            json!(["2026-01-16"])
        );
        assert_eq!(
            read_manifest(root.path(), &first.dsv).unwrap().dsv,
            first.dsv
        );
        for file in &first.provenance.files {
            let path = root.path().join(&file.path);
            assert!(path.is_file());
            if file.path.starts_with("normalized/") {
                let table = file.path.split('/').nth(1).unwrap();
                let batches = read_parquet(&path, table, "1").unwrap();
                assert_eq!(
                    batches[0].schema().metadata().get("prajna.dsv"),
                    Some(&first.dsv)
                );
            }
        }
        assert!(
            first
                .provenance
                .ignored_input_fields
                .contains(&"execution_status_default".into())
        );
        assert!(
            fs::read_dir(root.path().join("normalized"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".stage-"))
        );
        let version2 = publish_dataset(
            root.path(),
            &store,
            &registry,
            "synthetic-etf-daily",
            "2",
            &[raw_hash],
            "2026-09-30T00:00:00Z",
        )
        .unwrap();
        assert_ne!(first.dsv, version2.dsv);
        for table in ["instruments", "sessions"] {
            let first_hash = first
                .core
                .tables
                .iter()
                .find(|row| row.table == table)
                .unwrap()
                .logical_hash
                .as_str();
            let second_hash = version2
                .core
                .tables
                .iter()
                .find(|row| row.table == table)
                .unwrap()
                .logical_hash
                .as_str();
            assert_eq!(first_hash, second_hash);
        }
        assert_ne!(
            first
                .core
                .tables
                .iter()
                .find(|row| row.table == "bars")
                .unwrap()
                .logical_hash,
            version2
                .core
                .tables
                .iter()
                .find(|row| row.table == "bars")
                .unwrap()
                .logical_hash,
        );
    }

    #[test]
    fn each_invalid_fixture_is_rejected_before_any_normalized_files_or_manifest_exist() {
        let mutations: [(&str, &[u8], &[u8], &str); 9] = [
            (
                "OHLC high below close",
                b"\"high_rule\": \"max(open, close)\"",
                b"\"high_rule\": \"min(open, close)\"",
                "invalid_ohlc_or_bar",
            ),
            (
                "OHLC low above close",
                b"\"low_rule\": \"min(open, close)\"",
                b"\"low_rule\": \"max(open, close)\"",
                "invalid_ohlc_or_bar",
            ),
            (
                "OHLC high below low",
                b"\"high_rule\": \"max(open, close)\",\n    \"low_rule\": \"min(open, close)\"",
                b"\"high_rule\": \"min(open, close)\",\n    \"low_rule\": \"max(open, close)\"",
                "invalid_ohlc_or_bar",
            ),
            (
                "negative price",
                b"\"open\": 100.0",
                b"\"open\": -100.0",
                "negative_price",
            ),
            (
                "negative quantity",
                b"\"volume\": 1000000.0",
                b"\"volume\": -1000000.0",
                "negative_quantity",
            ),
            (
                "price increment mismatch",
                b"\"open\": 100.0",
                b"\"open\": 100.001",
                "misaligned_increment",
            ),
            (
                "duplicate session and bar primary key",
                b"\"2026-01-05\", \"2026-01-06\"",
                b"\"2026-01-05\", \"2026-01-05\"",
                "duplicate_primary_key",
            ),
            (
                "instrument ID reused with a changed currency",
                b"\"symbol\": \"B\", \"currency\": \"CNY\"",
                b"\"symbol\": \"A\", \"currency\": \"USD\"",
                "instrument_id_reused",
            ),
            (
                "required instrument field missing",
                b"\"symbol\": \"A\", \"currency\": \"CNY\"",
                b"\"symbol\": \"A\"",
                "missing_or_invalid_field",
            ),
        ];

        for (case, before, after, expected_code) in mutations {
            let root = tempfile::tempdir().unwrap();
            let bytes = replace_once(FIXTURE, before, after);
            let (store, raw_hash) = store_fixture(root.path(), &bytes);
            let result = publish_dataset(
                root.path(),
                &store,
                &NormalizerRegistry::with_builtins(),
                "synthetic-etf-daily",
                "1",
                &[raw_hash],
                "2026-09-30T00:00:00Z",
            );
            let Err(PublishError::Validation(issues)) = result else {
                panic!("{case}: expected structured validation rejection, got {result:?}");
            };
            assert!(
                issues.iter().any(|issue| issue.code == expected_code),
                "{case}: {issues:?}"
            );
            assert!(
                !root.path().join("normalized").exists(),
                "{case}: normalized output left behind"
            );
            assert!(
                !root.path().join("manifests").exists(),
                "{case}: manifest left behind"
            );
        }
    }
}
