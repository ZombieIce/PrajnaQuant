use std::{error::Error, fmt, path::Path};

use arrow_array::RecordBatch;
use prajna_data::{DataError, Manifest, ManifestTable, read_parquet};

#[derive(Debug)]
pub(crate) enum DsvTableError {
    Data(DataError),
    InvalidManifest(String),
}

impl fmt::Display for DsvTableError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Data(error) => write!(formatter, "invalid D7 Parquet data: {error}"),
            Self::InvalidManifest(message) => {
                write!(formatter, "invalid DSV table manifest: {message}")
            }
        }
    }
}

impl Error for DsvTableError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Data(error) => Some(error),
            Self::InvalidManifest(_) => None,
        }
    }
}

impl From<DataError> for DsvTableError {
    fn from(error: DataError) -> Self {
        Self::Data(error)
    }
}

pub(crate) fn read_table<'a>(
    lake_root: &Path,
    manifest: &'a Manifest,
    table_name: &str,
) -> Result<(&'a ManifestTable, Vec<RecordBatch>), DsvTableError> {
    let table = manifest
        .core
        .tables
        .iter()
        .find(|table| table.table == table_name)
        .ok_or_else(|| {
            DsvTableError::InvalidManifest(format!("missing {table_name} table descriptor"))
        })?;
    if table.schema_version != 1 {
        return Err(DsvTableError::InvalidManifest(format!(
            "unsupported {table_name} schema version {}",
            table.schema_version
        )));
    }
    let dsv_hex = manifest.dsv.strip_prefix("dsv:sha256:").ok_or_else(|| {
        DsvTableError::InvalidManifest("manifest DSV has an invalid format".into())
    })?;
    let path = lake_root
        .join("normalized")
        .join(table_name)
        .join(dsv_hex)
        .join("part-00000.parquet");
    let batches = read_parquet(path, table_name, &table.schema_version.to_string())?;
    for batch in &batches {
        if batch
            .schema()
            .metadata()
            .get("prajna.dsv")
            .map(String::as_str)
            != Some(manifest.dsv.as_str())
        {
            return Err(DsvTableError::InvalidManifest(format!(
                "{table_name} Parquet DSV does not match the manifest"
            )));
        }
    }
    Ok((table, batches))
}
