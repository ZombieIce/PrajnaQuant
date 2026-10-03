use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use polars::prelude::DataFrame;
use polars_io::{
    parquet::{
        read::ParquetReader,
        write::{ParquetCompression, ParquetWriter},
    },
    prelude::SerReader,
};
use prajna_domain::InstrumentId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    Panel,
    cache::key::FactorKey,
    compute_base, compute_composite,
    factor::{AvailabilityAssumption, Factor, FactorError, FactorGraph, FactorKind, values_schema},
};

static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum CacheError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Factor(FactorError),
    ParquetWrite {
        path: PathBuf,
        source: polars::error::PolarsError,
    },
    MetadataWrite {
        path: PathBuf,
        source: serde_json::Error,
    },
    InvalidValuesSchema {
        actual: String,
    },
    Integrity {
        path: PathBuf,
        reason: String,
    },
    Conflict {
        key: String,
    },
    StageCleanup {
        path: PathBuf,
        operation: String,
        source: Box<CacheError>,
    },
}

impl fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "factor cache I/O failed at {}: {source}",
                    path.display()
                )
            }
            Self::Factor(error) => write!(formatter, "factor computation failed: {error}"),
            Self::ParquetWrite { path, source } => write!(
                formatter,
                "could not write factor cache Parquet at {}: {source}",
                path.display()
            ),
            Self::MetadataWrite { path, source } => write!(
                formatter,
                "could not encode factor cache metadata at {}: {source}",
                path.display()
            ),
            Self::InvalidValuesSchema { actual } => {
                write!(
                    formatter,
                    "factor computation returned invalid values schema: {actual}"
                )
            }
            Self::Integrity { path, reason } => write!(
                formatter,
                "factor cache integrity error at {}: {reason}",
                path.display()
            ),
            Self::Conflict { key } => {
                write!(
                    formatter,
                    "concurrent factor cache write conflicts for {key}"
                )
            }
            Self::StageCleanup {
                path,
                operation,
                source,
            } => write!(
                formatter,
                "factor cache staging cleanup failed at {} after {operation}: {source}",
                path.display()
            ),
        }
    }
}

impl Error for CacheError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::StageCleanup { source, .. } => Some(source.as_ref()),
            Self::Factor(error) => Some(error),
            Self::ParquetWrite { source, .. } => Some(source),
            Self::MetadataWrite { source, .. } => Some(source),
            Self::InvalidValuesSchema { .. } | Self::Integrity { .. } | Self::Conflict { .. } => {
                None
            }
        }
    }
}

impl From<FactorError> for CacheError {
    fn from(error: FactorError) -> Self {
        Self::Factor(error)
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct CacheMetadata {
    key_object: Value,
    fv: String,
    values_sha256: String,
    row_count: u64,
}

pub struct FactorCache {
    lake_root: PathBuf,
    compute_count: AtomicU64,
}

impl FactorCache {
    pub fn open(lake_root: impl AsRef<Path>) -> Result<Self, CacheError> {
        let lake_root = lake_root.as_ref().to_path_buf();
        let staging = lake_root.join("features").join(".staging");
        fs::create_dir_all(&staging).map_err(|source| CacheError::Io {
            path: staging,
            source,
        })?;
        Ok(Self {
            lake_root,
            compute_count: AtomicU64::new(0),
        })
    }

    pub fn compute_count(&self) -> u64 {
        self.compute_count.load(Ordering::Relaxed)
    }

    pub fn get_or_compute(
        &self,
        panel: &Panel,
        members: &[InstrumentId],
        universe_id: &str,
        dsv: &str,
        factor: &Factor,
        assumption: AvailabilityAssumption,
    ) -> Result<DataFrame, CacheError> {
        let mut values_by_factor = BTreeMap::<String, DataFrame>::new();
        let mut keys_by_factor = BTreeMap::<String, FactorKey>::new();

        for node in FactorGraph::resolve(factor.clone())? {
            let identity = node.canonical_json().to_string();
            let dependencies = node.dependencies();
            let dependency_keys = dependencies
                .iter()
                .map(|(role, dependency)| {
                    keys_by_factor
                        .get(&dependency.canonical_json().to_string())
                        .cloned()
                        .map(|key| (*role, key))
                        .ok_or_else(|| {
                            CacheError::Factor(FactorError::Evaluation(format!(
                                "uncomputed factor-cache dependency {role}"
                            )))
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let key = FactorKey::new(&node, &dependency_keys, dsv, universe_id, assumption);

            let values = match self.read_if_present(&key)? {
                Some(values) => values,
                None => {
                    self.compute_count.fetch_add(1, Ordering::Relaxed);
                    let values = if node.kind() == FactorKind::RotationScore {
                        let dependency_values = dependencies
                            .iter()
                            .map(|(role, dependency)| {
                                values_by_factor
                                    .get(&dependency.canonical_json().to_string())
                                    .cloned()
                                    .map(|values| (*role, values))
                                    .ok_or_else(|| {
                                        CacheError::Factor(FactorError::Evaluation(format!(
                                            "uncomputed factor-cache dependency {role}"
                                        )))
                                    })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        compute_composite(&node, &dependency_values)?
                    } else {
                        compute_base(panel, members, &node, assumption)?
                    };
                    self.publish(&key, &values)?;
                    values
                }
            };

            keys_by_factor.insert(identity.clone(), key);
            values_by_factor.insert(identity, values);
        }

        values_by_factor
            .remove(&factor.canonical_json().to_string())
            .ok_or_else(|| {
                CacheError::Factor(FactorError::Evaluation(
                    "factor graph did not produce its root".into(),
                ))
            })
    }

    fn read_if_present(&self, key: &FactorKey) -> Result<Option<DataFrame>, CacheError> {
        let path = self.cache_path(key)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                self.read_existing(key, &path).map(Some)
            }
            Ok(_) => Err(integrity(&path, "cache entry is not a directory")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(CacheError::Io { path, source }),
        }
    }

    fn read_existing(&self, key: &FactorKey, directory: &Path) -> Result<DataFrame, CacheError> {
        let metadata_path = directory.join("meta.json");
        let metadata_bytes = read_cache_file(&metadata_path)?;
        let metadata: CacheMetadata = serde_json::from_slice(&metadata_bytes)
            .map_err(|error| integrity(&metadata_path, error.to_string()))?;

        if metadata.fv != key.as_str() {
            return Err(integrity(
                &metadata_path,
                format!(
                    "metadata key {} does not match requested {}",
                    metadata.fv,
                    key.as_str()
                ),
            ));
        }
        if metadata.key_object != *key.key_object() {
            return Err(integrity(
                &metadata_path,
                "metadata key object does not match request",
            ));
        }

        let values_path = directory.join("values.parquet");
        let digest = sha256_file(&values_path)?;
        if digest != metadata.values_sha256 {
            return Err(integrity(&values_path, "SHA-256 does not match metadata"));
        }

        let file = File::open(&values_path).map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                integrity(&values_path, "values.parquet is missing")
            } else {
                CacheError::Io {
                    path: values_path.clone(),
                    source,
                }
            }
        })?;
        let values = ParquetReader::new(file)
            .finish()
            .map_err(|error| integrity(&values_path, error.to_string()))?;
        if values.schema().as_ref() != &values_schema().polars {
            return Err(integrity(
                &values_path,
                "Factor Values schema does not match v1",
            ));
        }
        if u64::try_from(values.height()).ok() != Some(metadata.row_count) {
            return Err(integrity(&values_path, "row count does not match metadata"));
        }
        Ok(values)
    }

    fn publish(&self, key: &FactorKey, values: &DataFrame) -> Result<(), CacheError> {
        ensure_values_schema(values)?;
        let destination = self.cache_path(key)?;
        if let Some(existing) = self.read_if_present(key)? {
            return compare_existing(key, &existing, values);
        }

        let staging_root = self.lake_root.join("features").join(".staging");
        let stage = create_staging_dir(&staging_root)?;
        let result = self.write_and_rename(key, values, &stage, &destination);
        let cleanup = cleanup_staging_dir(&stage);
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Err(operation), Err(source)) => Err(CacheError::StageCleanup {
                path: stage,
                operation: operation.to_string(),
                source: Box::new(source),
            }),
        }
    }

    fn write_and_rename(
        &self,
        key: &FactorKey,
        values: &DataFrame,
        stage: &Path,
        destination: &Path,
    ) -> Result<(), CacheError> {
        let values_path = stage.join("values.parquet");
        let mut frame = values.clone();
        ParquetWriter::new(File::create(&values_path).map_err(|source| CacheError::Io {
            path: values_path.clone(),
            source,
        })?)
        .with_compression(ParquetCompression::Uncompressed)
        .finish(&mut frame)
        .map_err(|source| CacheError::ParquetWrite {
            path: values_path.clone(),
            source,
        })?;
        File::open(&values_path)
            .and_then(|file| file.sync_all())
            .map_err(|source| CacheError::Io {
                path: values_path.clone(),
                source,
            })?;

        let metadata = CacheMetadata {
            key_object: key.key_object().clone(),
            fv: key.as_str().to_owned(),
            values_sha256: sha256_file(&values_path)?,
            row_count: u64::try_from(values.height()).map_err(|_| {
                CacheError::InvalidValuesSchema {
                    actual: "row count does not fit u64".into(),
                }
            })?,
        };
        let metadata_path = stage.join("meta.json");
        let metadata_bytes =
            serde_json::to_vec(&metadata).map_err(|source| CacheError::MetadataWrite {
                path: metadata_path.clone(),
                source,
            })?;
        let mut metadata_file = File::create(&metadata_path).map_err(|source| CacheError::Io {
            path: metadata_path.clone(),
            source,
        })?;
        metadata_file
            .write_all(&metadata_bytes)
            .and_then(|()| metadata_file.sync_all())
            .map_err(|source| CacheError::Io {
                path: metadata_path.clone(),
                source,
            })?;
        File::open(stage)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| CacheError::Io {
                path: stage.to_path_buf(),
                source,
            })?;

        match fs::rename(stage, destination) {
            Ok(()) => {}
            Err(rename_error) => match self.read_if_present(key)? {
                Some(existing) => compare_existing(key, &existing, values)?,
                None => {
                    return Err(CacheError::Io {
                        path: destination.to_path_buf(),
                        source: rename_error,
                    });
                }
            },
        }
        sync_directory(&self.lake_root.join("features"))?;
        sync_directory(&self.lake_root.join("features/.staging"))?;
        Ok(())
    }

    fn cache_path(&self, key: &FactorKey) -> Result<PathBuf, CacheError> {
        let hex = key.as_str().strip_prefix("fv:sha256:").ok_or_else(|| {
            CacheError::Factor(FactorError::Evaluation(
                "FactorKey has an invalid versioned hash".into(),
            ))
        })?;
        Ok(self.lake_root.join("features").join(hex))
    }
}

fn ensure_values_schema(values: &DataFrame) -> Result<(), CacheError> {
    if values.schema().as_ref() == &values_schema().polars {
        Ok(())
    } else {
        Err(CacheError::InvalidValuesSchema {
            actual: format!("{:?}", values.schema()),
        })
    }
}

fn compare_existing(
    key: &FactorKey,
    existing: &DataFrame,
    computed: &DataFrame,
) -> Result<(), CacheError> {
    if existing.equals_missing(computed) {
        Ok(())
    } else {
        Err(CacheError::Conflict {
            key: key.as_str().to_owned(),
        })
    }
}

fn read_cache_file(path: &Path) -> Result<Vec<u8>, CacheError> {
    fs::read(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            integrity(path, "required cache file is missing")
        } else {
            CacheError::Io {
                path: path.to_path_buf(),
                source,
            }
        }
    })
}

fn sha256_file(path: &Path) -> Result<String, CacheError> {
    let mut file = File::open(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            integrity(path, "required cache file is missing")
        } else {
            CacheError::Io {
                path: path.to_path_buf(),
                source,
            }
        }
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|source| CacheError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn create_staging_dir(parent: &Path) -> Result<PathBuf, CacheError> {
    loop {
        let id = NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed);
        let stage = parent.join(format!("{}-{id}", std::process::id()));
        match fs::create_dir(&stage) {
            Ok(()) => return Ok(stage),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(CacheError::Io {
                    path: stage,
                    source,
                });
            }
        }
    }
}

fn cleanup_staging_dir(stage: &Path) -> Result<(), CacheError> {
    match fs::remove_dir_all(stage) {
        Ok(()) => sync_directory(stage.parent().unwrap_or(stage)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(CacheError::Io {
            path: stage.to_path_buf(),
            source,
        }),
    }
}

fn sync_directory(path: &Path) -> Result<(), CacheError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| CacheError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn integrity(path: &Path, reason: impl Into<String>) -> CacheError {
    CacheError::Integrity {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}
