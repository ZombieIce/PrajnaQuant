//! Read-only projection of a persisted Execution and its saved result tables.

use std::path::Path;

use arrow_array::{Array, RecordBatch, StringArray};
use arrow_schema::Schema;
use serde_json::{Map, Value, json};

use crate::{
    ExperimentError,
    storage::{ResultLevel, ResultReadError, cell_json, read_execution_manifest, read_run_table},
};

/// Version of the persisted-result JSON envelope. Additive fields may be added
/// without changing this value; incompatible changes require a new version.
pub(crate) const RESULT_SCHEMA_VERSION: u32 = 1;

/// Tables that carry the saved NAV/equity series for Vector and Fast/Accurate Event.
const NAV_TABLES: [&str; 2] = ["sessions", "account_valuations"];

/// Read a persisted Execution without executing, promoting or rewriting it.
///
/// The returned envelope is `{schema_version, data}`. Every table in the Run's
/// saved `table_hashes` is opened and hash-verified through the storage read
/// path; only Summary and NAV/equity payloads are returned. Unavailable tables
/// stay explicit in `tables.not_saved` / `tables.not_produced_by_engine` instead
/// of becoming empty or zero-valued series.
pub fn read_persisted_result(
    lake_root: impl AsRef<Path>,
    experiment_id: &str,
    execution_id: &str,
    run_filter: Option<&str>,
) -> Result<Value, ResultReadError> {
    let lake = lake_root.as_ref();
    let manifest = read_execution_manifest(lake, experiment_id, execution_id)?;
    let mut runs = Vec::new();
    for run in &manifest.runs {
        // `--run` filters the returned envelope, never the integrity scope:
        // every Run's saved tables are read and hash-verified even when only
        // one Run is returned.
        let view = run_view(lake, experiment_id, execution_id, run)?;
        if run_filter.is_none_or(|requested| requested == run.run_id) {
            runs.push(view);
        }
    }
    if runs.is_empty() {
        return Err(invalid(format!(
            "Run {} is not in Execution {execution_id}",
            run_filter.unwrap_or_default()
        )));
    }
    Ok(json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "data": {
            "exp": manifest.experiment_id,
            "exe": manifest.execution.id,
            "default_result_level": manifest.default_result_level,
            "runs": runs,
        }
    }))
}

fn run_view(
    lake: &Path,
    experiment_id: &str,
    execution_id: &str,
    run: &crate::storage::StoredRun,
) -> Result<Value, ResultReadError> {
    let (summary, error) = summary_view(lake, experiment_id, execution_id, &run.run_id)?;
    let mut series = Map::new();
    // Verify every saved table, including tables this envelope does not return.
    for table in run.table_hashes.keys() {
        let (schema, batches) = read_run_table(
            lake,
            experiment_id,
            execution_id,
            &run.run_id,
            run.result_level,
            table,
        )?;
        if NAV_TABLES.iter().any(|name| *name == table) {
            series.insert(table.clone(), Value::Array(rows_json(&schema, &batches)?));
        }
    }
    Ok(json!({
        "run_id": run.run_id,
        "status": run.status,
        "result_level": run.result_level,
        "accounting_verified": run.accounting_verified,
        "error": error,
        "summary": summary,
        "series": series,
        "tables": {
            "saved": run.table_hashes.keys().collect::<Vec<_>>(),
            "not_saved": run.not_saved,
            "not_produced_by_engine": run.not_produced_by_engine,
        },
    }))
}

fn summary_view(
    lake: &Path,
    experiment_id: &str,
    execution_id: &str,
    run_id: &str,
) -> Result<(Option<Value>, Option<String>), ResultReadError> {
    let (schema, batches) = read_run_table(
        lake,
        experiment_id,
        execution_id,
        run_id,
        ResultLevel::Summary,
        "summary",
    )?;
    let batch = batches
        .iter()
        .find(|batch| batch.num_rows() > 0)
        .ok_or_else(|| invalid(format!("stored summary has no row for Run {run_id}")))?;
    let summary = column(batch, &schema, "summary_json")?;
    let summary = if summary.is_null(0) {
        None
    } else {
        Some(
            serde_json::from_str::<Value>(summary.value(0))
                .map_err(|error| ResultReadError::Storage(ExperimentError::Json(error)))?,
        )
    };
    let error = column(batch, &schema, "error")?;
    let error = (!error.is_null(0)).then(|| error.value(0).to_owned());
    Ok((summary, error))
}

fn column<'a>(
    batch: &'a RecordBatch,
    schema: &Schema,
    name: &str,
) -> Result<&'a StringArray, ResultReadError> {
    let index = schema
        .index_of(name)
        .map_err(|_| invalid(format!("stored summary has no {name} column")))?;
    batch
        .column(index)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| invalid(format!("stored summary {name} is not Utf8")))
}

fn rows_json(schema: &Schema, batches: &[RecordBatch]) -> Result<Vec<Value>, ResultReadError> {
    let id_index = schema
        .index_of("row_id")
        .map_err(|_| invalid("stored table has no row_id column"))?;
    let payload_index = schema
        .index_of("payload")
        .map_err(|_| invalid("stored table has no payload column"))?;
    let mut rows = Vec::new();
    for batch in batches {
        let ids = batch
            .column(id_index)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| invalid("stored row_id is not Utf8"))?;
        let payloads = batch
            .column(payload_index)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| invalid("stored payload is not Utf8"))?;
        for row in 0..batch.num_rows() {
            if ids.is_null(row) || payloads.is_null(row) {
                return Err(invalid("stored table contains a null row_id or payload"));
            }
            let mut payload = serde_json::from_str::<Value>(payloads.value(row))
                .map_err(|error| ResultReadError::Storage(ExperimentError::Json(error)))?;
            if let Value::Object(object) = &mut payload {
                for (column, field) in schema.fields().iter().enumerate() {
                    if column == id_index || column == payload_index {
                        continue;
                    }
                    if let Some(value) = cell_json(batch.column(column).as_ref(), row, field) {
                        object.insert(field.name().clone(), value);
                    }
                }
            }
            rows.push(payload);
        }
    }
    Ok(rows)
}

fn invalid(message: impl Into<String>) -> ResultReadError {
    ResultReadError::Storage(ExperimentError::InvalidDefinition(message.into()))
}
