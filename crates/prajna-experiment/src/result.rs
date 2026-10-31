//! Read-only projection of a persisted Execution and its saved result tables.

use std::path::Path;

use arrow_array::{Array, RecordBatch, StringArray};
use arrow_schema::Schema;
use prajna_domain::{Notional, TimestampNs};
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
/// path; Summary, NAV/equity and saved detail payloads are returned. Unavailable tables
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
    // Older manifests omit static Run Specs. Read the stored canonical
    // Experiment for units without requiring source data or re-execution.
    let definition_path = lake
        .join("experiments")
        .join(experiment_id.rsplit(':').next().unwrap_or_default())
        .join("experiment.json");
    let definition = match std::fs::read(&definition_path) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|e| ResultReadError::Storage(ExperimentError::Json(e)))?;
            let canonical =
                prajna_data::restricted_jcs(&value).map_err(|e| invalid(e.to_string()))?;
            if crate::identity("exp", &canonical) != experiment_id {
                return Err(invalid("stored Experiment content identity mismatch"));
            }
            Some(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(invalid(format!(
                "cannot read stored Experiment units: {error}"
            )));
        }
    };
    let currency = definition
        .as_ref()
        .and_then(|value| value.pointer("/fast_event/currency"));
    let mut runs = Vec::new();
    for run in &manifest.runs {
        // `--run` filters the returned envelope, never the integrity scope:
        // every Run's saved tables are read and hash-verified even when only
        // one Run is returned.
        let view = run_view(lake, experiment_id, execution_id, run, currency)?;
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
    currency: Option<&Value>,
) -> Result<Value, ResultReadError> {
    let (summary, error) = summary_view(lake, experiment_id, execution_id, &run.run_id)?;
    let mut series = Map::new();
    let mut details = Map::new();
    let mut allocations = None;
    // Verify every saved table before returning any result.
    for table in run.table_hashes.keys() {
        let (schema, batches) = read_run_table(
            lake,
            experiment_id,
            execution_id,
            &run.run_id,
            run.result_level,
            table,
        )?;
        if table == "summary" {
            continue;
        }
        let rows = rows_json(&schema, &batches)?;
        if table == "account_valuations" {
            allocations = Some(rows.iter().map(allocation).collect::<Vec<_>>());
        }
        if NAV_TABLES.iter().any(|name| *name == table) {
            series.insert(table.clone(), Value::Array(rows));
        } else {
            let mut payload = Value::Array(rows);
            normalize_timestamps(&mut payload);
            details.insert(table.clone(), payload);
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
        "details": details,
        "allocations": allocations,
        "units": {
            "currency": currency,
            "weight": "fraction_of_total_equity",
            "vector_cost": "fraction_of_previous_nav",
            "timestamp": "unix_nanoseconds",
        },
        "tables": {
            "saved": run.table_hashes.keys().collect::<Vec<_>>(),
            "not_saved": run.not_saved,
            "not_produced_by_engine": run.not_produced_by_engine,
        },
    }))
}

// JSON numbers cannot retain nanosecond timestamps in browser JavaScript.
fn normalize_timestamps(value: &mut Value) {
    match value {
        Value::Array(rows) => rows.iter_mut().for_each(normalize_timestamps),
        Value::Object(fields) => {
            for (key, value) in fields {
                if matches!(
                    key.as_str(),
                    "executed_at" | "submitted_at" | "attempted_at" | "ts_open"
                ) {
                    if value.is_number() {
                        *value = Value::String(value.to_string());
                    } else if let Some(timestamp) =
                        value.as_str().and_then(|s| TimestampNs::parse(s).ok())
                    {
                        *value = json!(timestamp.as_unix_nanos().to_string());
                    }
                } else {
                    normalize_timestamps(value);
                }
            }
        }
        _ => {}
    }
}

/// Display projection only: stored amounts stay unchanged; receivables remain
/// in the equity denominator and never become spendable cash. Missing fields
/// and non-positive equity are explicit nulls, including legacy payloads.
fn allocation(row: &Value) -> Value {
    let equity = row
        .get("equity")
        .and_then(decimal)
        .filter(|v| *v > Notional::ZERO);
    let ratio = |value: Option<&Value>| -> Value {
        value
            .and_then(decimal)
            .zip(equity)
            .and_then(|(amount, total)| amount.checked_div_decimal(total).ok())
            .map(|value| json!(value.to_string()))
            .unwrap_or(Value::Null)
    };
    let positions =
        row.get("positions")
            .and_then(Value::as_object)
            .map(|positions| {
                positions.iter().map(|(id, position)| (id.clone(), json!({
            "quantity": position.get("quantity"), "price": position.get("price"),
            "market_value": position.get("market_value"),
            "weight": ratio(position.get("market_value")),
        }))).collect::<Map<_,_>>()
            });
    json!({
        "session_date": row.get("session_date"), "valuation_point": row.get("valuation_point"),
        "cash": row.get("cash"), "equity": row.get("equity"),
        "cash_weight": ratio(row.get("cash")),
        "dividend_receivable": row.get("dividend_receivable"),
        "dividend_receivable_weight": ratio(row.get("dividend_receivable")),
        "liquidation_receivable": row.get("liquidation_receivable"),
        "liquidation_receivable_weight": ratio(row.get("liquidation_receivable")),
        "positions": positions,
        "weight_unavailable_reason": if equity.is_none() { Some("missing_or_nonpositive_equity") } else { None },
    })
}

fn decimal(value: &Value) -> Option<Notional> {
    value.as_str().and_then(|value| Notional::parse(value).ok())
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
                        object.entry(field.name().clone()).or_insert(value);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nanoseconds_remain_exact_for_browser_transport() {
        let mut row = json!({"fill":{"executed_at":1_767_600_000_000_000_123_i64},
            "submitted_at":"1970-01-01T00:00:01.000000123Z",
            "quantity":"900","session_date":"2026-01-05"});
        normalize_timestamps(&mut row);
        assert_eq!(row["fill"]["executed_at"], "1767600000000000123");
        assert_eq!(row["submitted_at"], "1000000123");
        assert_eq!(row["quantity"], "900");
    }

    #[test]
    fn allocation_golden_includes_receivables_without_spending_them() {
        // Independent hand calculation: 25 cash + 60 security + 10 dividend
        // + 5 liquidation = 100 equity. Investable cash is only 25, not 40.
        let saved = json!({"session_date":"2026-01-05","valuation_point":"close",
            "cash":"25","equity":"100","dividend_receivable":"10",
            "liquidation_receivable":"5","positions":{"SYNTH.XSHG":{
                "quantity":"20","price":"3","market_value":"60"}}});
        let projected = allocation(&saved);
        assert_eq!(projected["cash"], "25");
        assert_eq!(projected["cash_weight"], "0.25");
        assert_eq!(projected["dividend_receivable_weight"], "0.1");
        assert_eq!(projected["liquidation_receivable_weight"], "0.05");
        assert_eq!(projected["positions"]["SYNTH.XSHG"]["weight"], "0.6");
        assert!(projected["weight_unavailable_reason"].is_null());
    }

    #[test]
    fn allocation_missing_zero_negative_and_legacy_fields_are_not_zero_weights() {
        for equity in [Value::Null, json!("0"), json!("-1"), json!("bad")] {
            let projected = allocation(&json!({"cash":"25","equity":equity,
                "positions":{"SYNTH.XSHG":{"market_value":"60"}}}));
            assert!(projected["cash_weight"].is_null());
            assert!(projected["positions"]["SYNTH.XSHG"]["weight"].is_null());
            assert_eq!(
                projected["weight_unavailable_reason"],
                "missing_or_nonpositive_equity"
            );
        }
        let projected = allocation(&json!({"equity":"100"}));
        assert!(projected["cash_weight"].is_null());
        assert!(projected["dividend_receivable_weight"].is_null());
        assert!(projected["positions"].is_null());
        // Decimal division retains scale-18 precision; no float conversion.
        let projected = allocation(&json!({"cash":"1","equity":"3","positions":{}}));
        assert_eq!(projected["cash_weight"], "0.333333333333333333");
        assert_eq!(projected["positions"], json!({}));
    }
}
