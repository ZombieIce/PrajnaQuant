use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
};

use arrow_array::{
    Array, ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use chrono::{SecondsFormat, Utc};
use parquet::{
    arrow::{ArrowWriter, arrow_reader::ParquetRecordBatchReaderBuilder},
    file::properties::WriterProperties,
};
use prajna_data::logical_hash;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ExecutionIdentity, ExecutionReport, Experiment, ExperimentError, RunExecution, RunStatus,
    RunSummary, StrategyParameters, summarize,
};
use prajna_research::vector::VectorResult;

static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
const NOT_PRODUCED_BY_ENGINE: [&str; 3] = ["orders", "fills", "cash_ledger"];
const FAST_STANDARD: [&str; 3] = [
    "account_valuations",
    "portfolio_valuations",
    "corporate_actions",
];
const FAST_FULL: [&str; 6] = [
    "account_valuations",
    "portfolio_valuations",
    "orders",
    "fills",
    "fee_settlements",
    "corporate_actions",
];
const FAST_TERMINAL_STANDARD: [&str; 4] = [
    "account_valuations",
    "portfolio_valuations",
    "corporate_actions",
    "terminal_settlements",
];
const FAST_TERMINAL_FULL: [&str; 7] = [
    "account_valuations",
    "portfolio_valuations",
    "orders",
    "fills",
    "fee_settlements",
    "corporate_actions",
    "terminal_settlements",
];
const ACCURATE_STANDARD: [&str; 2] = ["account_valuations", "portfolio_valuations"];
const ACCURATE_FULL: [&str; 8] = [
    "account_valuations",
    "portfolio_valuations",
    "orders",
    "fills",
    "fee_settlements",
    "adapter_events",
    "backend_events",
    "backend_comparisons",
];
const STANDARD_TABLES: [&str; 3] = ["sessions", "executions", "corporate_actions"];
const TERMINAL_STANDARD_TABLES: [&str; 4] = [
    "sessions",
    "executions",
    "corporate_actions",
    "terminal_settlements",
];
const FULL_TABLES: [&str; 7] = [
    "sessions",
    "executions",
    "corporate_actions",
    "target_weights",
    "weights_after_execution",
    "decisions",
    "pending_at_end",
];
const TERMINAL_FULL_TABLES: [&str; 8] = [
    "sessions",
    "executions",
    "corporate_actions",
    "target_weights",
    "weights_after_execution",
    "decisions",
    "pending_at_end",
    "terminal_settlements",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultLevel {
    Summary,
    Standard,
    Full,
}

impl ResultLevel {
    fn tables_for(self, run: &RunExecution) -> &'static [&'static str] {
        let terminal_settlement = run.run_spec.get("terminal_settlement").is_some();
        if run.is_event() {
            match self {
                Self::Summary => &[],
                Self::Standard if run.run_spec["engine"]["kind"] == "accurate_event" => {
                    &ACCURATE_STANDARD
                }
                Self::Standard if terminal_settlement => &FAST_TERMINAL_STANDARD,
                Self::Standard => &FAST_STANDARD,
                Self::Full if run.run_spec["engine"]["kind"] == "accurate_event" => &ACCURATE_FULL,
                Self::Full if terminal_settlement => &FAST_TERMINAL_FULL,
                Self::Full => &FAST_FULL,
            }
        } else if terminal_settlement {
            match self {
                Self::Summary => &[],
                Self::Standard => &TERMINAL_STANDARD_TABLES,
                Self::Full => &TERMINAL_FULL_TABLES,
            }
        } else {
            self.tables()
        }
    }
}

#[derive(Debug)]
pub enum ResultReadError {
    NotSaved(String),
    NotProducedByEngine(String),
    Storage(ExperimentError),
}

impl fmt::Display for ResultReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotSaved(message) => write!(formatter, "result not saved: {message}"),
            Self::NotProducedByEngine(message) => {
                write!(formatter, "result not produced by engine: {message}")
            }
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ResultReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ExperimentError> for ResultReadError {
    fn from(error: ExperimentError) -> Self {
        Self::Storage(error)
    }
}

/// Read a persisted Run without execution or promotion. `level` is the minimum
/// required retention level; a saved table is returned with its original schema.
pub fn read_run_table(
    lake_root: impl AsRef<Path>,
    experiment_id: &str,
    execution_id: &str,
    run_id: &str,
    level: ResultLevel,
    table: &str,
) -> Result<(Arc<Schema>, Vec<RecordBatch>), ResultReadError> {
    for (identity, prefix) in [
        (experiment_id, "exp:sha256:"),
        (execution_id, "exe:sha256:"),
        (run_id, "run:sha256:"),
    ] {
        if !identity.starts_with(prefix) {
            return Err(err(format!("expected {prefix} identity, got {identity}")).into());
        }
        identity_hex(identity)?;
    }
    if table != "summary"
        && !FULL_TABLES.contains(&table)
        && table != "terminal_settlements"
        && !ACCURATE_FULL.contains(&table)
        && !NOT_PRODUCED_BY_ENGINE.contains(&table)
    {
        return Err(err(format!("unknown result table {table}")).into());
    }
    let directory = execution_path(lake_root.as_ref(), experiment_id, execution_id)?;
    let manifest = read_manifest_file(&directory.join("execution.json"))?;
    if manifest.experiment_id != experiment_id || manifest.execution.id != execution_id {
        return Err(err("stored Execution identity does not match the request").into());
    }
    let run = manifest
        .runs
        .iter()
        .find(|run| run.run_id == run_id)
        .ok_or_else(|| err(format!("Run {run_id} is not in Execution {execution_id}")))?;
    if table != "summary"
        && (run.not_produced_by_engine.iter().any(|name| name == table)
            || (!run.table_hashes.contains_key(table)
                && !run.not_saved.iter().any(|name| name == table)))
    {
        return Err(ResultReadError::NotProducedByEngine(format!(
            "{run_id}/{table}"
        )));
    }
    if level > run.result_level {
        return Err(ResultReadError::NotSaved(format!(
            "Run {run_id} retained {}, requested {level}",
            run.result_level
        )));
    }
    if table == "summary" {
        let (schema, batches) = read_summary_batches(&directory.join("summary.parquet"))?;
        let hash = logical_hash("summary", &schema, &batches, &["run_id"])
            .map_err(|error| err(error.to_string()))?;
        if hash != manifest.summary_logical_hash {
            return Err(err("stored summary logical hash mismatch").into());
        }
        let mut selected = Vec::with_capacity(batches.len());
        for batch in batches {
            let ids = batch
                .column_by_name("run_id")
                .and_then(|array| array.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| err("summary run_id is not Utf8"))?;
            let mask = BooleanArray::from_iter(ids.iter().map(|id| Some(id == Some(run_id))));
            selected.push(
                arrow_select::filter::filter_record_batch(&batch, &mask)
                    .map_err(|error| err(error.to_string()))?,
            );
        }
        if selected.iter().map(RecordBatch::num_rows).sum::<usize>() != 1 {
            return Err(err("stored summary must contain exactly one row for the Run").into());
        }
        return Ok((schema, selected));
    }
    let expected = run
        .table_hashes
        .get(table)
        .ok_or_else(|| ResultReadError::NotSaved(format!("{run_id}/{table}")))?;
    let path = directory
        .join("runs")
        .join(run_hex(run_id)?)
        .join(format!("{table}.parquet"));
    let (schema, batches) = read_table_batches(&path, table)?;
    let actual = logical_hash(table, &schema, &batches, &["row_id"])
        .map_err(|error| err(error.to_string()))?;
    if &actual != expected {
        return Err(err(format!("stored {table} logical hash mismatch")).into());
    }
    Ok((schema, batches))
}

impl ResultLevel {
    fn tables(self) -> &'static [&'static str] {
        match self {
            Self::Summary => &[],
            Self::Standard => &STANDARD_TABLES,
            Self::Full => &FULL_TABLES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionManifest {
    pub execution: ExecutionIdentity,
    pub experiment_id: String,
    pub default_result_level: ResultLevel,
    pub summary_logical_hash: String,
    pub runs: Vec<StoredRun>,
    pub threads: usize,
    #[serde(default)]
    pub machine: Option<MachineInfo>,
    pub started_at: String,
    pub finished_at: String,
    pub factor_time_ms: u128,
    pub run_time_ms: u128,
}

/// Informational provenance; excluded from every content identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineInfo {
    pub os: String,
    pub architecture: String,
    pub available_parallelism: Option<usize>,
}

impl MachineInfo {
    fn current() -> Self {
        Self {
            os: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            available_parallelism: std::thread::available_parallelism().ok().map(usize::from),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRun {
    pub run_id: String,
    pub status: RunStatus,
    pub result_level: ResultLevel,
    pub table_hashes: BTreeMap<String, String>,
    pub not_saved: Vec<String>,
    pub not_produced_by_engine: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounting_verified: Option<bool>,
    /// PIT session membership/eligibility and evidence identities survive Summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_spec: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct StoredExecution {
    pub manifest: ExecutionManifest,
    pub path: PathBuf,
    /// Statistics for this invocation, including on replay; never part of identity.
    pub statistics: InvocationStatistics,
    pub action: StoreAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreAction {
    Created,
    Replayed,
    Promoted,
}

/// Published experiment/result bytes and files only. Excludes Factor Cache,
/// lock files, and temporary replay/staging writes. Replaced manifests count once.
#[derive(Debug, Clone, Default, Serialize)]
pub struct InvocationStatistics {
    pub factor_time_ms: u128,
    pub run_time_ms: u128,
    pub compute_count: u64,
    pub cache_hit_count: u64,
    pub written_bytes: u64,
    pub written_files: u64,
}

impl InvocationStatistics {
    fn for_report(report: &ExecutionReport) -> Self {
        Self {
            factor_time_ms: report.factor_time_ms,
            run_time_ms: report.run_time_ms,
            compute_count: report.compute_count,
            cache_hit_count: report.cache_hit_count,
            ..Self::default()
        }
    }

    fn record_file(&mut self, path: &Path) -> Result<(), ExperimentError> {
        self.written_bytes += fs::metadata(path).map_err(io_error(path))?.len();
        self.written_files += 1;
        Ok(())
    }

    fn record_tree(&mut self, path: &Path) -> Result<(), ExperimentError> {
        for entry in fs::read_dir(path).map_err(io_error(path))? {
            let entry = entry.map_err(io_error(path))?;
            if entry.file_type().map_err(io_error(&entry.path()))?.is_dir() {
                self.record_tree(&entry.path())?;
            } else {
                self.record_file(&entry.path())?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunDifference {
    pub run_id: String,
    pub left_status: Option<RunStatus>,
    pub right_status: Option<RunStatus>,
    pub status_changed: bool,
    pub pending_at_end_changed: bool,
    pub metric_deltas: BTreeMap<String, Option<f64>>,
    pub table_differences: Vec<TableDifference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableDifference {
    pub table: String,
    pub left_hash: Option<String>,
    pub right_hash: Option<String>,
    pub first_different_key: Option<String>,
    pub first_different_field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionDiff {
    pub left_execution_id: String,
    pub right_execution_id: String,
    pub summary_changed: bool,
    pub only_left: Vec<String>,
    pub only_right: Vec<String>,
    pub runs: Vec<RunDifference>,
}

#[derive(Debug)]
struct StoredRow {
    run_id: String,
    summary: Option<RunSummary>,
}

impl fmt::Display for ResultLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Summary => "summary",
            Self::Standard => "standard",
            Self::Full => "full",
        })
    }
}

fn err(message: impl Into<String>) -> ExperimentError {
    ExperimentError::Execution(message.into())
}

pub(super) fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true)
}

pub(super) fn store_execution(
    experiment: &Experiment,
    identity: &ExecutionIdentity,
    report: &ExecutionReport,
    lake_root: &Path,
    level: ResultLevel,
    threads: usize,
    started_at: String,
) -> Result<StoredExecution, ExperimentError> {
    let mut statistics = InvocationStatistics::for_report(report);
    let experiment_id = experiment.id()?;
    let experiment_root = experiment_root(lake_root, &experiment_id);
    fs::create_dir_all(&experiment_root).map_err(io_error(&experiment_root))?;
    let definition_path = experiment_root.join("experiment.json");
    let new_definition = !definition_path.exists();
    ensure_experiment_file(experiment, &experiment_root)?;
    if new_definition {
        statistics.record_file(&definition_path)?;
    }
    let executions_root = experiment_root.join("executions");
    fs::create_dir_all(&executions_root).map_err(io_error(&executions_root))?;
    let destination = executions_root.join(identity_hex(&identity.id)?);

    if destination.exists() {
        verify_existing(&destination, identity, experiment, report, level)
            .map_err(|error| ExperimentError::ReplayMismatch(error.to_string()))?;
        let manifest = read_manifest_file(&destination.join("execution.json"))?;
        return Ok(StoredExecution {
            manifest,
            path: destination,
            statistics,
            action: StoreAction::Replayed,
        });
    }

    let staging = create_staging_dir(&executions_root)?;
    let result: Result<ExecutionManifest, ExperimentError> = (|| {
        let (rows, summary_hash) =
            write_summary(&staging.join("summary.parquet"), experiment, &report.runs)?;
        let mut stored_runs = Vec::with_capacity(rows.len());
        for execution in &report.runs {
            let mut table_hashes = BTreeMap::new();
            if execution.status == RunStatus::Completed {
                for table in level.tables_for(execution) {
                    let table_rows = execution_rows(execution, table)?;
                    let path = staging
                        .join("runs")
                        .join(run_hex(&execution.run_id)?)
                        .join(format!("{table}.parquet"));
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent).map_err(io_error(parent))?;
                    }
                    let hash = write_rows(&path, table, &table_rows)?;
                    table_hashes.insert((*table).to_owned(), hash);
                }
            }
            stored_runs.push(stored_run(execution, level, table_hashes));
        }
        let manifest = ExecutionManifest {
            execution: identity.clone(),
            experiment_id,
            default_result_level: level,
            summary_logical_hash: summary_hash,
            runs: stored_runs,
            threads,
            machine: Some(MachineInfo::current()),
            started_at,
            finished_at: now_rfc3339(),
            factor_time_ms: report.factor_time_ms,
            run_time_ms: report.run_time_ms,
        };
        write_json_synced(&staging.join("execution.json"), &manifest)?;
        sync_tree(&staging)?;
        statistics.record_tree(&staging)?;
        fs::rename(&staging, &destination).map_err(io_error(&destination))?;
        sync_dir(&executions_root)?;
        Ok(manifest)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    let manifest = result?;
    Ok(StoredExecution {
        manifest,
        path: destination,
        statistics,
        action: StoreAction::Created,
    })
}

pub(super) fn promote(
    experiment: &Experiment,
    identity: &ExecutionIdentity,
    lake_root: &Path,
    execution_id: &str,
    run_ids: &[String],
    level: ResultLevel,
    threads: usize,
) -> Result<StoredExecution, ExperimentError> {
    if identity.id != execution_id {
        return Err(err(format!(
            "promotion requires current Execution identity {}; requested {execution_id}",
            identity.id
        )));
    }
    if run_ids.is_empty() {
        return Err(ExperimentError::InvalidDefinition(
            "promotion requires at least one Run identity".into(),
        ));
    }
    let experiment_id = experiment.id()?;
    let destination = execution_path(lake_root, &experiment_id, execution_id)?;
    let _promotion_lock = lock_promotion(&destination)?;
    let manifest_path = destination.join("execution.json");
    let mut manifest = read_manifest_file(&manifest_path).map_err(|error| {
        if manifest_path.is_file() {
            ExperimentError::ReplayMismatch(error.to_string())
        } else {
            error
        }
    })?;
    if manifest.experiment_id != experiment_id || manifest.execution.id != execution_id {
        return Err(ExperimentError::ReplayMismatch(
            "stored Execution identity does not match requested Experiment".into(),
        ));
    }
    let summary_path = destination.join("summary.parquet");
    if read_summary_hash(&summary_path)
        .map_err(|error| ExperimentError::ReplayMismatch(error.to_string()))?
        != manifest.summary_logical_hash
    {
        return Err(ExperimentError::ReplayMismatch(
            "promotion found a modified summary.parquet; stored files were not changed".into(),
        ));
    }
    let unique_ids = run_ids.iter().collect::<BTreeSet<_>>();
    if unique_ids.len() != run_ids.len() {
        return Err(ExperimentError::InvalidDefinition(
            "promotion contains a duplicate Run identity".into(),
        ));
    }
    let report = experiment.execute(lake_root, threads)?;
    let mut statistics = InvocationStatistics::for_report(&report);
    let executions = report
        .runs
        .iter()
        .map(|run| (run.run_id.as_str(), run))
        .collect::<BTreeMap<_, _>>();
    // Validate and stage every requested table before touching final paths.
    let staging = create_staging_dir(&destination)?;
    let result = (|| {
        let (_, summary_hash) =
            write_summary(&staging.join("summary.parquet"), experiment, &report.runs)?;
        if summary_hash != manifest.summary_logical_hash {
            return Err(ExperimentError::ReplayMismatch(
                "promotion replay mismatch for summary.parquet; stored files were not changed"
                    .into(),
            ));
        }
        let mut pending = Vec::new();
        for run_id in run_ids {
            let stored_index = manifest
                .runs
                .iter()
                .position(|run| run.run_id == *run_id)
                .ok_or_else(|| {
                    ExperimentError::InvalidDefinition(format!(
                        "Run {run_id} is not part of this Execution"
                    ))
                })?;
            let old_level = manifest.runs[stored_index].result_level;
            if level <= old_level {
                return Err(ExperimentError::InvalidDefinition(format!(
                    "Run {run_id} cannot be promoted from {old_level} to {level}"
                )));
            }
            let run = executions
                .get(run_id.as_str())
                .ok_or_else(|| err(format!("current Experiment did not produce Run {run_id}")))?;
            if run.status != RunStatus::Completed {
                return Err(err(format!(
                    "Run {run_id} failed during promotion: {}",
                    run.error.as_deref().unwrap_or("unknown error")
                )));
            }
            let existing_hashes = manifest.runs[stored_index].table_hashes.clone();
            let tables = level.tables_for(run);
            for table in tables {
                let rows = execution_rows(run, table)?;
                let path = destination
                    .join("runs")
                    .join(run_hex(run_id)?)
                    .join(format!("{table}.parquet"));
                let computed_hash = row_hash(table, &rows)?;
                if let Some(existing_hash) = existing_hashes.get(*table) {
                    let actual_hash = read_rows_hash(&path, table)
                        .map_err(|error| ExperimentError::ReplayMismatch(error.to_string()))?;
                    if &actual_hash != existing_hash || actual_hash != computed_hash {
                        return Err(ExperimentError::ReplayMismatch(format!(
                            "promotion replay mismatch for run {run_id}, table {table}"
                        )));
                    }
                    continue;
                }
                if path.exists() {
                    return Err(err(format!(
                        "untracked table already exists at {}; refusing to overwrite it",
                        path.display()
                    )));
                }
                let stage = staging.join(format!("{}.parquet", pending.len()));
                let hash = write_rows(&stage, table, &rows)?;
                if hash != computed_hash {
                    return Err(err(format!(
                        "logical hash changed while writing run {run_id}, table {table}"
                    )));
                }
                pending.push((stage, path));
                manifest.runs[stored_index]
                    .table_hashes
                    .insert((*table).into(), hash);
            }
            manifest.runs[stored_index].result_level = level;
            manifest.runs[stored_index].not_saved = ResultLevel::Full
                .tables_for(run)
                .iter()
                .filter(|table| {
                    !manifest.runs[stored_index]
                        .table_hashes
                        .contains_key(**table)
                })
                .map(|table| (*table).to_owned())
                .collect();
        }
        let staged_manifest = staging.join("execution.json");
        write_json_synced(&staged_manifest, &manifest)?;
        sync_dir(&staging)?;
        statistics.record_file(&staged_manifest)?;
        for (stage, _) in &pending {
            statistics.record_file(stage)?;
        }
        publish_promotion(&pending, || {
            fs::rename(&staged_manifest, destination.join("execution.json"))
                .map_err(io_error(&destination))
        })?;
        // Rename is the commit point. A later sync error must not roll back
        // tables referenced by the newly committed manifest.
        sync_dir(&destination)
    })();
    let cleanup = fs::remove_dir_all(&staging).map_err(io_error(&staging));
    if let Err(error) = result {
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(err(format!(
                "{error}; staging cleanup failed: {cleanup_error}"
            ))),
        };
    }
    cleanup?;
    Ok(StoredExecution {
        manifest,
        path: destination,
        statistics,
        action: StoreAction::Promoted,
    })
}

pub fn diff_executions(
    lake_root: impl AsRef<Path>,
    left_execution_id: &str,
    right_execution_id: &str,
) -> Result<ExecutionDiff, ExperimentError> {
    let lake_root = lake_root.as_ref();
    let (left_dir, left) = find_execution(lake_root, left_execution_id)?;
    let (right_dir, right) = find_execution(lake_root, right_execution_id)?;
    if left.experiment_id != right.experiment_id {
        return Err(ExperimentError::DifferentExperiments(format!(
            "cannot diff Executions from different Experiments ({} vs {})",
            left.experiment_id, right.experiment_id
        )));
    }
    let left_rows = read_summary(&left_dir.join("summary.parquet"))?;
    let right_rows = read_summary(&right_dir.join("summary.parquet"))?;
    let left_runs = left
        .runs
        .iter()
        .map(|run| (run.run_id.as_str(), run))
        .collect::<BTreeMap<_, _>>();
    let right_runs = right
        .runs
        .iter()
        .map(|run| (run.run_id.as_str(), run))
        .collect::<BTreeMap<_, _>>();
    let left_summary = left_rows
        .into_iter()
        .map(|row| (row.run_id.clone(), row))
        .collect::<BTreeMap<_, _>>();
    let right_summary = right_rows
        .into_iter()
        .map(|row| (row.run_id.clone(), row))
        .collect::<BTreeMap<_, _>>();
    let only_left = left_runs
        .keys()
        .filter(|run_id| !right_runs.contains_key(**run_id))
        .map(|run_id| (*run_id).to_owned())
        .collect::<Vec<_>>();
    let only_right = right_runs
        .keys()
        .filter(|run_id| !left_runs.contains_key(**run_id))
        .map(|run_id| (*run_id).to_owned())
        .collect::<Vec<_>>();
    let mut runs = Vec::new();
    for run_id in left_runs
        .keys()
        .filter(|run_id| right_runs.contains_key(**run_id))
    {
        let left_run = left_runs[run_id];
        let right_run = right_runs[run_id];
        let left_summary = left_summary
            .get(*run_id)
            .and_then(|row| row.summary.as_ref());
        let right_summary = right_summary
            .get(*run_id)
            .and_then(|row| row.summary.as_ref());
        let mut table_differences = Vec::new();
        let table_names = left_run
            .table_hashes
            .keys()
            .chain(right_run.table_hashes.keys())
            .collect::<BTreeSet<_>>();
        for table in table_names {
            let left_hash = left_run.table_hashes.get(table);
            let right_hash = right_run.table_hashes.get(table);
            if left_hash == right_hash {
                continue;
            }
            let (key, field) = match (left_hash, right_hash) {
                (Some(_), Some(_)) => first_table_difference(
                    &left_dir
                        .join("runs")
                        .join(run_hex(run_id)?)
                        .join(format!("{table}.parquet")),
                    &right_dir
                        .join("runs")
                        .join(run_hex(run_id)?)
                        .join(format!("{table}.parquet")),
                    table,
                )?,
                _ => (None, None),
            };
            table_differences.push(TableDifference {
                table: (*table).clone(),
                left_hash: left_hash.cloned(),
                right_hash: right_hash.cloned(),
                first_different_key: key,
                first_different_field: field,
            });
        }
        runs.push(RunDifference {
            run_id: (*run_id).to_owned(),
            left_status: Some(left_run.status),
            right_status: Some(right_run.status),
            status_changed: left_run.status != right_run.status,
            pending_at_end_changed: left_summary
                .zip(right_summary)
                .is_some_and(|(left, right)| left.pending_at_end != right.pending_at_end),
            metric_deltas: metric_deltas(left_summary, right_summary),
            table_differences,
        });
    }
    Ok(ExecutionDiff {
        left_execution_id: left_execution_id.to_owned(),
        right_execution_id: right_execution_id.to_owned(),
        summary_changed: left.summary_logical_hash != right.summary_logical_hash,
        only_left,
        only_right,
        runs,
    })
}

fn experiment_root(lake_root: &Path, experiment_id: &str) -> PathBuf {
    lake_root
        .join("experiments")
        .join(identity_hex(experiment_id).unwrap_or_default())
}

fn execution_path(
    lake_root: &Path,
    experiment_id: &str,
    execution_id: &str,
) -> Result<PathBuf, ExperimentError> {
    Ok(experiment_root(lake_root, experiment_id)
        .join("executions")
        .join(identity_hex(execution_id)?))
}

fn identity_hex(identity: &str) -> Result<String, ExperimentError> {
    let (_, hex) = identity
        .split_once(":sha256:")
        .ok_or_else(|| err(format!("invalid content identity: {identity}")))?;
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(err(format!("invalid content identity: {identity}")));
    }
    Ok(hex.to_owned())
}

pub(super) fn run_hex(run_id: &str) -> Result<String, ExperimentError> {
    identity_hex(run_id)
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> ExperimentError + '_ {
    move |error| err(format!("storage I/O failed at {}: {error}", path.display()))
}

fn sync_dir(path: &Path) -> Result<(), ExperimentError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(io_error(path))
}

fn sync_tree(root: &Path) -> Result<(), ExperimentError> {
    let entries = fs::read_dir(root).map_err(io_error(root))?;
    for entry in entries {
        let entry = entry.map_err(io_error(root))?;
        let path = entry.path();
        if entry.file_type().map_err(io_error(&path))?.is_dir() {
            sync_tree(&path)?;
        }
    }
    sync_dir(root)
}

/// Hold this file until publication and staging cleanup complete. Keep the lock
/// inode in the executions directory permanently: unlinking it could let a new
/// writer lock a different inode while another writer still holds the old one.
/// Closing the file releases the OS lock, including after process termination.
pub(super) fn lock_promotion(destination: &Path) -> Result<File, ExperimentError> {
    let parent = destination
        .parent()
        .ok_or_else(|| err("invalid Execution path"))?;
    let name = destination
        .file_name()
        .ok_or_else(|| err("invalid Execution path"))?;
    let path = parent.join(format!(".promotion-{}.lock", name.to_string_lossy()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(io_error(&path))?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|error| {
        err(format!(
            "cannot acquire promotion lock at {} (another promotion may be active): {error}",
            path.display()
        ))
    })?;
    Ok(file)
}

fn create_staging_dir(parent: &Path) -> Result<PathBuf, ExperimentError> {
    fs::create_dir_all(parent).map_err(io_error(parent))?;
    loop {
        let path = parent.join(format!(".staging-{}", next_stage_id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error(&path)(error)),
        }
    }
}

fn next_stage_id() -> u64 {
    NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
}

fn ensure_experiment_file(
    experiment: &Experiment,
    experiment_root: &Path,
) -> Result<(), ExperimentError> {
    let path = experiment_root.join("experiment.json");
    let expected = experiment.canonical_json()?;
    if path.exists() {
        let actual = fs::read(&path).map_err(io_error(&path))?;
        if actual != expected {
            return Err(ExperimentError::ReplayMismatch(format!(
                "stored experiment.json is inconsistent at {}",
                path.display()
            )));
        }
        return Ok(());
    }
    write_new_atomic(&path, &expected)
}

fn write_new_atomic(path: &Path, bytes: &[u8]) -> Result<(), ExperimentError> {
    let parent = path
        .parent()
        .ok_or_else(|| err("invalid storage file path"))?;
    fs::create_dir_all(parent).map_err(io_error(parent))?;
    let staging = parent.join(format!(
        ".{}.staging-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file"),
        next_stage_id()
    ));
    let mut file = File::create(&staging).map_err(io_error(&staging))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(io_error(&staging))?;
    match fs::hard_link(&staging, path) {
        Ok(()) => {
            fs::remove_file(&staging).map_err(io_error(&staging))?;
            sync_dir(parent)
        }
        Err(error) if path.exists() => {
            let _ = fs::remove_file(&staging);
            let existing = fs::read(path).map_err(io_error(path))?;
            if existing == bytes {
                Ok(())
            } else {
                Err(io_error(path)(error))
            }
        }
        Err(error) => Err(io_error(path)(error)),
    }
}

fn write_json_synced(path: &Path, value: &impl Serialize) -> Result<(), ExperimentError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| err(format!("could not encode {}: {error}", path.display())))?;
    let mut file = File::create(path).map_err(io_error(path))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(io_error(path))
}

/// Publish immutable additions, rolling back only files/directories we created
/// if publication or the manifest commit fails. Existing paths are never removed.
fn publish_promotion(
    pending: &[(PathBuf, PathBuf)],
    commit: impl FnOnce() -> Result<(), ExperimentError>,
) -> Result<(), ExperimentError> {
    let mut published = Vec::new();
    let mut created_dirs = Vec::new();
    let result = (|| {
        for (stage, path) in pending {
            let parent = path.parent().ok_or_else(|| err("invalid Run table path"))?;
            let mut missing = Vec::new();
            let mut ancestor = parent;
            while !ancestor.exists() {
                missing.push(ancestor.to_path_buf());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| err("invalid Run directory"))?;
            }
            for directory in missing.into_iter().rev() {
                fs::create_dir(&directory).map_err(io_error(&directory))?;
                created_dirs.push(directory);
            }
            fs::hard_link(stage, path).map_err(io_error(path))?;
            published.push(path.clone());
            sync_dir(parent)?;
        }
        for directory in created_dirs.iter().rev() {
            if let Some(parent) = directory.parent() {
                sync_dir(parent)?;
            }
        }
        commit()
    })();
    if let Err(error) = result {
        let mut cleanup_errors = Vec::new();
        for path in published.iter().rev() {
            if let Err(cleanup) = fs::remove_file(path) {
                cleanup_errors.push(format!("{}: {cleanup}", path.display()));
            }
        }
        for directory in created_dirs.iter().rev() {
            if let Err(cleanup) = fs::remove_dir(directory) {
                cleanup_errors.push(format!("{}: {cleanup}", directory.display()));
            }
        }
        if !cleanup_errors.is_empty() {
            return Err(err(format!(
                "{error}; rollback failed: {}",
                cleanup_errors.join("; ")
            )));
        }
        return Err(error);
    }
    Ok(())
}

fn read_manifest_file(path: &Path) -> Result<ExecutionManifest, ExperimentError> {
    let bytes = fs::read(path).map_err(io_error(path))?;
    let manifest: ExecutionManifest = serde_json::from_slice(&bytes).map_err(|error| {
        err(format!(
            "invalid execution manifest {}: {error}",
            path.display()
        ))
    })?;
    for run in &manifest.runs {
        if let Some(spec) = &run.run_spec {
            let calculated = crate::identity(
                "run",
                &prajna_data::restricted_jcs(spec).map_err(|e| err(e.to_string()))?,
            );
            if calculated != run.run_id {
                return Err(err("stored PIT Run Spec content identity mismatch"));
            }
        }
    }
    Ok(manifest)
}

fn stored_run(
    execution: &RunExecution,
    level: ResultLevel,
    table_hashes: BTreeMap<String, String>,
) -> StoredRun {
    let not_saved = ResultLevel::Full
        .tables_for(execution)
        .iter()
        .filter(|table| !table_hashes.contains_key(**table))
        .map(|table| (*table).to_owned())
        .collect();
    StoredRun {
        run_id: execution.run_id.clone(),
        status: execution.status,
        result_level: level,
        table_hashes,
        not_saved,
        not_produced_by_engine: if execution.is_event() {
            vec![]
        } else {
            NOT_PRODUCED_BY_ENGINE
                .iter()
                .map(|name| (*name).into())
                .collect()
        },
        accounting_verified: if execution.is_event() {
            Some(execution.status == RunStatus::Completed)
        } else {
            None
        },
        run_spec: execution
            .run_spec
            .get("pit")
            .map(|_| execution.run_spec.clone()),
    }
}

fn write_summary(
    path: &Path,
    experiment: &Experiment,
    runs: &[RunExecution],
) -> Result<(Vec<StoredRow>, String), ExperimentError> {
    let mut ids = Vec::with_capacity(runs.len());
    let mut short = Vec::with_capacity(runs.len());
    let mut long = Vec::with_capacity(runs.len());
    let mut vol = Vec::with_capacity(runs.len());
    let mut w_s = Vec::with_capacity(runs.len());
    let mut w_l = Vec::with_capacity(runs.len());
    let mut w_v = Vec::with_capacity(runs.len());
    let mut trend = Vec::with_capacity(runs.len());
    let mut top_k = Vec::with_capacity(runs.len());
    let mut rebalance = Vec::with_capacity(runs.len());
    let mut statuses = Vec::with_capacity(runs.len());
    let mut errors = Vec::with_capacity(runs.len());
    let mut summary_json = Vec::with_capacity(runs.len());
    let mut stored = Vec::with_capacity(runs.len());
    for run in runs {
        let parameters = if run.run_spec["strategy"]["kind"] == "s2_rotation" {
            Some(parameters_from_spec(&run.run_spec)?)
        } else {
            None
        };
        let summary = run.summary.clone().or_else(|| {
            run.result
                .as_ref()
                .map(|result| summarize(result, experiment.sessions_per_year()))
        });
        ids.push(Some(run.run_id.clone()));
        short.push(parameters.as_ref().map(|p| to_i64(p.short)).transpose()?);
        long.push(parameters.as_ref().map(|p| to_i64(p.long)).transpose()?);
        vol.push(parameters.as_ref().map(|p| to_i64(p.vol)).transpose()?);
        w_s.push(parameters.as_ref().map(|p| p.w_s));
        w_l.push(parameters.as_ref().map(|p| p.w_l));
        w_v.push(parameters.as_ref().map(|p| p.w_v));
        trend.push(
            parameters
                .as_ref()
                .and_then(|p| p.trend)
                .map(to_i64)
                .transpose()?,
        );
        top_k.push(parameters.as_ref().map(|p| to_i64(p.top_k)).transpose()?);
        rebalance.push(
            parameters
                .as_ref()
                .map(|p| to_i64(p.rebalance_every))
                .transpose()?,
        );
        statuses.push(Some(
            match run.status {
                RunStatus::Completed => "completed",
                RunStatus::Failed => "failed",
            }
            .to_owned(),
        ));
        errors.push(run.error.clone());
        summary_json.push(
            summary
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| err(error.to_string()))?,
        );
        stored.push(StoredRow {
            run_id: run.run_id.clone(),
            summary,
        });
    }

    let schema = summary_schema_for(experiment);
    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(ids)),
        Arc::new(Int64Array::from(short)),
        Arc::new(Int64Array::from(long)),
        Arc::new(Int64Array::from(vol)),
        Arc::new(Float64Array::from(w_s)),
        Arc::new(Float64Array::from(w_l)),
        Arc::new(Float64Array::from(w_v)),
        Arc::new(Int64Array::from(trend)),
        Arc::new(Int64Array::from(top_k)),
        Arc::new(Int64Array::from(rebalance)),
        Arc::new(StringArray::from(statuses)),
        Arc::new(StringArray::from(errors)),
        summary_float(&stored, |summary| summary.total_return)?,
        summary_float(&stored, |summary| summary.annualized_return)?,
        summary_integer(&stored, |summary| summary.n_returns)?,
        summary_float(&stored, |summary| summary.mean_return)?,
        summary_float(&stored, |summary| summary.std_return)?,
        summary_float(&stored, |summary| summary.annualized_volatility)?,
        summary_float(&stored, |summary| summary.sharpe)?,
        summary_float(&stored, |summary| summary.max_drawdown)?,
        summary_string(&stored, |summary| summary.drawdown_peak_session.clone())?,
        summary_string(&stored, |summary| summary.drawdown_trough_session.clone())?,
        summary_float(&stored, |summary| Some(summary.total_turnover))?,
        summary_float(&stored, |summary| Some(summary.total_cost))?,
        summary_integer(&stored, |summary| summary.executed_count)?,
        summary_integer(&stored, |summary| summary.deferred_count)?,
        summary_integer(&stored, |summary| summary.skipped_buy_count)?,
        summary_integer(&stored, |summary| summary.session_count)?,
        summary_bool(&stored, |summary| summary.pending_at_end)?,
        summary_bool(&stored, |summary| summary.insufficient_sessions)?,
        summary_bool(&stored, |summary| summary.zero_volatility)?,
        summary_string(&stored, |summary| {
            Some(serde_json::to_string(&summary.assumptions).unwrap_or_default())
        })?,
        summary_string(&stored, |summary| {
            Some(summary.availability_assumption.clone())
        })?,
        Arc::new(StringArray::from(summary_json)),
    ];
    let batch = RecordBatch::try_new(schema.clone(), columns)
        .map_err(|error| err(format!("invalid summary table: {error}")))?;
    let hash = write_batch(path, &schema, &batch, "summary", &["run_id"])?;
    Ok((stored, hash))
}

fn summary_schema_for(experiment: &Experiment) -> Arc<Schema> {
    let schema = summary_schema();
    if experiment.strategy_kind == "s2_rotation" {
        return schema;
    }
    Arc::new(Schema::new(
        schema
            .fields()
            .iter()
            .enumerate()
            .map(|(i, f)| {
                if (1..=9).contains(&i) {
                    f.as_ref().clone().with_nullable(true)
                } else {
                    f.as_ref().clone()
                }
            })
            .collect::<Vec<_>>(),
    ))
}

fn summary_schema() -> Arc<Schema> {
    use DataType::{Boolean, Float64, Int64, Utf8};
    Arc::new(Schema::new(vec![
        Field::new("run_id", Utf8, false),
        Field::new("short", Int64, false),
        Field::new("long", Int64, false),
        Field::new("vol", Int64, false),
        Field::new("w_s", Float64, false),
        Field::new("w_l", Float64, false),
        Field::new("w_v", Float64, false),
        Field::new("trend", Int64, true),
        Field::new("top_k", Int64, false),
        Field::new("rebalance_every", Int64, false),
        Field::new("status", Utf8, false),
        Field::new("error", Utf8, true),
        Field::new("total_return", Float64, true),
        Field::new("annualized_return", Float64, true),
        Field::new("n_returns", Int64, true),
        Field::new("mean_return", Float64, true),
        Field::new("std_return", Float64, true),
        Field::new("annualized_volatility", Float64, true),
        Field::new("sharpe", Float64, true),
        Field::new("max_drawdown", Float64, true),
        Field::new("drawdown_peak_session", Utf8, true),
        Field::new("drawdown_trough_session", Utf8, true),
        Field::new("total_turnover", Float64, true),
        Field::new("total_cost", Float64, true),
        Field::new("executed_count", Int64, true),
        Field::new("deferred_count", Int64, true),
        Field::new("skipped_buy_count", Int64, true),
        Field::new("session_count", Int64, true),
        Field::new("pending_at_end", Boolean, true),
        Field::new("insufficient_sessions", Boolean, true),
        Field::new("zero_volatility", Boolean, true),
        Field::new("assumptions", Utf8, true),
        Field::new("availability_assumption", Utf8, true),
        Field::new("summary_json", Utf8, true),
    ]))
}

fn summary_float(
    runs: &[StoredRow],
    select: impl Fn(&RunSummary) -> Option<f64>,
) -> Result<ArrayRef, ExperimentError> {
    let values = runs
        .iter()
        .map(|run| run.summary.as_ref().and_then(&select))
        .collect::<Vec<_>>();
    Ok(Arc::new(Float64Array::from(values)))
}
fn summary_integer(
    runs: &[StoredRow],
    select: impl Fn(&RunSummary) -> usize,
) -> Result<ArrayRef, ExperimentError> {
    let values = runs
        .iter()
        .map(|run| {
            run.summary
                .as_ref()
                .map(|result| to_i64(select(result)))
                .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Arc::new(Int64Array::from(values)))
}
fn summary_string(
    runs: &[StoredRow],
    select: impl Fn(&RunSummary) -> Option<String>,
) -> Result<ArrayRef, ExperimentError> {
    let values = runs
        .iter()
        .map(|run| run.summary.as_ref().and_then(&select))
        .collect::<Vec<_>>();
    Ok(Arc::new(StringArray::from(values)))
}
fn summary_bool(
    runs: &[StoredRow],
    select: impl Fn(&RunSummary) -> bool,
) -> Result<ArrayRef, ExperimentError> {
    let values = runs
        .iter()
        .map(|run| run.summary.as_ref().map(&select))
        .collect::<Vec<_>>();
    Ok(Arc::new(BooleanArray::from(values)))
}

fn parameters_from_spec(spec: &Value) -> Result<StrategyParameters, ExperimentError> {
    let params = spec
        .pointer("/strategy/parameters")
        .and_then(Value::as_object)
        .ok_or_else(|| err("Run Spec is missing strategy parameters"))?;
    let number = |name: &str| {
        params
            .get(name)
            .and_then(Value::as_u64)
            .ok_or_else(|| err(format!("Run Spec parameter {name} is invalid")))
    };
    let float = |name: &str| -> Result<f64, ExperimentError> {
        let encoded = params
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| err(format!("Run Spec parameter {name} is invalid")))?;
        let bits = u64::from_str_radix(encoded, 16)
            .map_err(|error| err(format!("invalid float bits for {name}: {error}")))?;
        Ok(f64::from_bits(bits))
    };
    let trend = params
        .get("trend")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| err("Run Spec parameter trend is invalid"))
        })
        .transpose()?;
    Ok(StrategyParameters {
        short: number("short")?,
        long: number("long")?,
        vol: number("vol")?,
        w_s: float("w_s")?,
        w_l: float("w_l")?,
        w_v: float("w_v")?,
        trend,
        top_k: number("top_k")?,
        rebalance_every: number("rebalance_every")?,
    })
}

fn to_i64(value: impl TryInto<i64>) -> Result<i64, ExperimentError> {
    value
        .try_into()
        .map_err(|_| err("summary value exceeds Int64"))
}

fn result_rows(
    result: &VectorResult,
    table: &str,
) -> Result<Vec<(String, String)>, ExperimentError> {
    let mut rows = Vec::new();
    match table {
        "corporate_actions" => {
            for (index, entry) in result.corporate_action_ledger.iter().enumerate() {
                let mut payload =
                    serde_json::to_value(entry).map_err(|error| err(error.to_string()))?;
                payload["record_type"] = "corporate_action".into();
                rows.push((format!("event|{index:020}"), json_payload(&payload)?));
            }
        }
        "terminal_settlements" => {
            for (index, entry) in result.terminal_settlements.iter().enumerate() {
                let mut payload =
                    serde_json::to_value(entry).map_err(|error| err(error.to_string()))?;
                payload["record_type"] = "terminal_settlement".into();
                rows.push((format!("event|{index:020}"), json_payload(&payload)?));
            }
        }
        "sessions" => {
            for session in &result.sessions {
                let mut payload = serde_json::json!({
                    "session_date": session.session_date,
                    "nav": session.nav,
                    "gross_return": session.gross_return,
                    "net_return": session.net_return,
                    "turnover": session.turnover,
                    "cost": session.cost,
                    "valuation_carried": session.valuation_carried,
                });
                if let Some(components) = &session.portfolio_components {
                    payload["portfolio_components"] = serde_json::json!(components);
                }
                if let Some(components) = &session.portfolio_components_close {
                    payload["portfolio_components_close"] = serde_json::json!(components);
                }
                rows.push((
                    session.session_date.clone(),
                    serde_json::to_string(&payload).map_err(|error| err(error.to_string()))?,
                ));
            }
        }
        "executions" => {
            for execution in &result.executions {
                rows.push((execution.session_date.clone(), json_payload(execution)?));
            }
            for (index, attempt) in result.risk_exits.iter().enumerate() {
                rows.push((
                    format!("risk|{index:020}"),
                    json_payload(&serde_json::json!({
                        "record_type": "risk_exit", "attempt": attempt,
                    }))?,
                ));
            }
            for instrument in &result.pending_risk_exits {
                rows.push((
                    format!("risk_pending|{instrument}"),
                    json_payload(&serde_json::json!({
                        "record_type": "risk_exit_pending_at_end", "instrument_id": instrument,
                    }))?,
                ));
            }
        }
        "target_weights" | "weights_after_execution" => {
            for session in &result.sessions {
                let weights = if table == "target_weights" {
                    &session.target_weights
                } else {
                    &session.weights_after_execution
                };
                for (instrument, weight) in weights {
                    let key = format!("{}\0{}", session.session_date, instrument);
                    let payload = serde_json::json!({
                        "session_date": session.session_date,
                        "instrument_id": instrument,
                        "weight": weight,
                    });
                    rows.push((
                        key,
                        serde_json::to_string(&payload).map_err(|error| err(error.to_string()))?,
                    ));
                }
            }
        }
        "decisions" => {
            for decision in &result.decisions {
                for ranked in &decision.ranked {
                    let key = format!(
                        "{}\0{}\0{}",
                        decision.decision_session, ranked.rank, ranked.instrument_id
                    );
                    let target_weight = decision.targets.get(&ranked.instrument_id);
                    let payload = serde_json::json!({
                        "decision_session": decision.decision_session,
                        "instrument_id": ranked.instrument_id,
                        "rank": ranked.rank,
                        "score": ranked.score,
                        "target_weight": target_weight,
                    });
                    rows.push((
                        key,
                        serde_json::to_string(&payload).map_err(|error| err(error.to_string()))?,
                    ));
                }
            }
        }
        "pending_at_end" => {
            if let Some(pending) = &result.pending_at_end {
                if pending.targets.is_empty() {
                    let payload = serde_json::json!({
                        "decision_session": pending.decision_session,
                        "instrument_id": Value::Null,
                        "weight": Value::Null,
                    });
                    rows.push((
                        "__pending__".into(),
                        serde_json::to_string(&payload).map_err(|error| err(error.to_string()))?,
                    ));
                }
                for (instrument, weight) in &pending.targets {
                    let key = instrument.to_string();
                    let payload = serde_json::json!({
                        "decision_session": pending.decision_session,
                        "instrument_id": instrument,
                        "weight": weight,
                    });
                    rows.push((
                        key,
                        serde_json::to_string(&payload).map_err(|error| err(error.to_string()))?,
                    ));
                }
            }
        }
        _ => return Err(err(format!("unknown Vector result table {table}"))),
    }
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(rows)
}

fn json_payload(value: &impl Serialize) -> Result<String, ExperimentError> {
    serde_json::to_string(value)
        .map_err(|error| err(format!("could not encode Vector row: {error}")))
}

fn rows_batch(
    table: &str,
    rows: &[(String, String)],
) -> Result<(Arc<Schema>, RecordBatch), ExperimentError> {
    if ACCURATE_FULL.contains(&table) {
        let schema = fast_table_schema();
        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(
                rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(_, payload)| payload.as_str())
                    .collect::<Vec<_>>(),
            )),
        ];
        let batch =
            RecordBatch::try_new(schema.clone(), columns).map_err(|e| err(e.to_string()))?;
        return Ok((schema, batch));
    }
    let schema = vector_table_schema();
    let mut row_ids = Vec::with_capacity(rows.len());
    let mut session_dates = Vec::with_capacity(rows.len());
    let mut decision_sessions = Vec::with_capacity(rows.len());
    let mut instruments = Vec::with_capacity(rows.len());
    let mut kinds = Vec::with_capacity(rows.len());
    let mut ranks = Vec::with_capacity(rows.len());
    let mut navs = Vec::with_capacity(rows.len());
    let mut gross_returns = Vec::with_capacity(rows.len());
    let mut net_returns = Vec::with_capacity(rows.len());
    let mut turnovers = Vec::with_capacity(rows.len());
    let mut costs = Vec::with_capacity(rows.len());
    let mut weights = Vec::with_capacity(rows.len());
    let mut scores = Vec::with_capacity(rows.len());
    let mut target_weights = Vec::with_capacity(rows.len());
    let mut valuation_carried = Vec::with_capacity(rows.len());
    let mut blocked = Vec::with_capacity(rows.len());
    let mut skipped_buys = Vec::with_capacity(rows.len());
    let mut payloads = Vec::with_capacity(rows.len());
    for (row_id, payload) in rows {
        let value: Value = serde_json::from_str(payload)
            .map_err(|error| err(format!("invalid {table} row payload: {error}")))?;
        row_ids.push(Some(row_id.clone()));
        session_dates.push(string_value(&value, "session_date"));
        decision_sessions.push(string_value(&value, "decision_session"));
        instruments.push(string_value(&value, "instrument_id"));
        kinds.push(string_value(&value, "kind"));
        ranks.push(integer_value(&value, "rank"));
        navs.push(number_value(&value, "nav"));
        gross_returns.push(number_value(&value, "gross_return"));
        net_returns.push(number_value(&value, "net_return"));
        turnovers.push(number_value(&value, "turnover"));
        costs.push(number_value(&value, "cost"));
        weights.push(number_value(&value, "weight"));
        scores.push(number_value(&value, "score"));
        target_weights.push(number_value(&value, "target_weight"));
        valuation_carried.push(json_field(&value, "valuation_carried")?);
        blocked.push(json_field(&value, "blocked")?);
        skipped_buys.push(json_field(&value, "skipped_buys")?);
        payloads.push(Some(payload.clone()));
    }
    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(row_ids)),
        Arc::new(StringArray::from(session_dates)),
        Arc::new(StringArray::from(decision_sessions)),
        Arc::new(StringArray::from(instruments)),
        Arc::new(StringArray::from(kinds)),
        Arc::new(Int64Array::from(ranks)),
        Arc::new(Float64Array::from(navs)),
        Arc::new(Float64Array::from(gross_returns)),
        Arc::new(Float64Array::from(net_returns)),
        Arc::new(Float64Array::from(turnovers)),
        Arc::new(Float64Array::from(costs)),
        Arc::new(Float64Array::from(weights)),
        Arc::new(Float64Array::from(scores)),
        Arc::new(Float64Array::from(target_weights)),
        Arc::new(StringArray::from(valuation_carried)),
        Arc::new(StringArray::from(blocked)),
        Arc::new(StringArray::from(skipped_buys)),
        Arc::new(StringArray::from(payloads)),
    ];
    let batch = RecordBatch::try_new(schema.clone(), columns)
        .map_err(|error| err(format!("invalid {table} Vector table: {error}")))?;
    Ok((schema, batch))
}

fn fast_table_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("row_id", DataType::Utf8, false),
        Field::new("payload", DataType::Utf8, false),
    ]))
}

fn vector_table_schema() -> Arc<Schema> {
    use DataType::{Float64, Int64, Utf8};
    Arc::new(Schema::new(vec![
        Field::new("row_id", Utf8, false),
        Field::new("session_date", Utf8, true),
        Field::new("decision_session", Utf8, true),
        Field::new("instrument_id", Utf8, true),
        Field::new("kind", Utf8, true),
        Field::new("rank", Int64, true),
        Field::new("nav", Float64, true),
        Field::new("gross_return", Float64, true),
        Field::new("net_return", Float64, true),
        Field::new("turnover", Float64, true),
        Field::new("cost", Float64, true),
        Field::new("weight", Float64, true),
        Field::new("score", Float64, true),
        Field::new("target_weight", Float64, true),
        Field::new("valuation_carried", Utf8, true),
        Field::new("blocked", Utf8, true),
        Field::new("skipped_buys", Utf8, true),
        Field::new("payload", Utf8, false),
    ]))
}

fn string_value(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn number_value(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn integer_value(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn json_field(value: &Value, key: &str) -> Result<Option<String>, ExperimentError> {
    value
        .get(key)
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| err(format!("could not encode {key}: {error}")))
}

fn row_hash(table: &str, rows: &[(String, String)]) -> Result<String, ExperimentError> {
    let (schema, batch) = rows_batch(table, rows)?;
    logical_hash(table, &schema, &[batch], &["row_id"])
        .map_err(|error| err(format!("could not hash {table}: {error}")))
}

fn write_rows(
    path: &Path,
    table: &str,
    rows: &[(String, String)],
) -> Result<String, ExperimentError> {
    let (schema, batch) = rows_batch(table, rows)?;
    write_batch(path, &schema, &batch, table, &["row_id"])
}

fn write_batch(
    path: &Path,
    schema: &Arc<Schema>,
    batch: &RecordBatch,
    table: &str,
    primary_key: &[&str],
) -> Result<String, ExperimentError> {
    let parent = path.parent().ok_or_else(|| err("invalid Parquet path"))?;
    fs::create_dir_all(parent).map_err(io_error(parent))?;
    let file = File::create(path).map_err(io_error(path))?;
    let props = WriterProperties::builder().build();
    let mut writer = ArrowWriter::try_new(file, schema.clone(), Some(props)).map_err(|error| {
        err(format!(
            "could not create Parquet writer at {}: {error}",
            path.display()
        ))
    })?;
    writer.write(batch).map_err(|error| {
        err(format!(
            "could not write Parquet at {}: {error}",
            path.display()
        ))
    })?;
    writer.close().map_err(|error| {
        err(format!(
            "could not finish Parquet at {}: {error}",
            path.display()
        ))
    })?;
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(io_error(path))?;
    let hash = logical_hash(table, schema, std::slice::from_ref(batch), primary_key)
        .map_err(|error| err(format!("could not hash {table}: {error}")))?;
    Ok(hash)
}

fn read_rows(path: &Path, table: &str) -> Result<Vec<(String, String)>, ExperimentError> {
    let (schema, batches) = read_table_batches(path, table)?;
    let id_index = schema
        .index_of("row_id")
        .map_err(|error| err(error.to_string()))?;
    let payload_index = schema
        .index_of("payload")
        .map_err(|error| err(error.to_string()))?;
    let mut rows = Vec::new();
    for batch in batches {
        let ids = batch
            .column(id_index)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| err(format!("invalid row_id type in {table}")))?;
        let payloads = batch
            .column(payload_index)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| err(format!("invalid payload type in {table}")))?;
        for row in 0..batch.num_rows() {
            if ids.is_null(row) || payloads.is_null(row) {
                return Err(err(format!("null row in {table} at {}", path.display())));
            }
            let mut payload: Value = serde_json::from_str(payloads.value(row))
                .map_err(|error| err(format!("invalid stored {table} payload: {error}")))?;
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
            rows.push((
                ids.value(row).to_owned(),
                serde_json::to_string(&payload).map_err(|error| err(error.to_string()))?,
            ));
        }
    }
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(rows)
}

fn cell_json(array: &dyn Array, row: usize, field: &Field) -> Option<Value> {
    if array.is_null(row) {
        return None;
    }
    match field.data_type() {
        DataType::Utf8 => {
            let value = array.as_any().downcast_ref::<StringArray>()?.value(row);
            if matches!(
                field.name().as_str(),
                "valuation_carried" | "blocked" | "skipped_buys"
            ) {
                serde_json::from_str(value).ok()
            } else {
                Some(Value::String(value.to_owned()))
            }
        }
        DataType::Int64 => Some(Value::from(
            array.as_any().downcast_ref::<Int64Array>()?.value(row),
        )),
        DataType::Float64 => {
            let value = array.as_any().downcast_ref::<Float64Array>()?.value(row);
            serde_json::Number::from_f64(value).map(Value::Number)
        }
        _ => None,
    }
}

fn read_table_batches(
    path: &Path,
    table: &str,
) -> Result<(Arc<Schema>, Vec<RecordBatch>), ExperimentError> {
    let file = File::open(path).map_err(io_error(path))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|error| {
        err(format!(
            "could not read Parquet {}: {error}",
            path.display()
        ))
    })?;
    let schema = builder.schema().clone();
    if schema.as_ref()
        != if ACCURATE_FULL.contains(&table) {
            fast_table_schema()
        } else {
            vector_table_schema()
        }
        .as_ref()
    {
        return Err(err(format!(
            "invalid {table} Parquet schema at {}",
            path.display()
        )));
    }
    let reader = builder.build().map_err(|error| {
        err(format!(
            "could not open Parquet {}: {error}",
            path.display()
        ))
    })?;
    let mut batches = Vec::new();
    for batch in reader {
        batches.push(batch.map_err(|error| {
            err(format!(
                "could not decode Parquet {}: {error}",
                path.display()
            ))
        })?);
    }
    Ok((schema, batches))
}

fn read_rows_hash(path: &Path, table: &str) -> Result<String, ExperimentError> {
    let (schema, batches) = read_table_batches(path, table)?;
    logical_hash(table, &schema, &batches, &["row_id"])
        .map_err(|error| err(format!("could not hash {table}: {error}")))
}

fn verify_existing(
    directory: &Path,
    identity: &ExecutionIdentity,
    experiment: &Experiment,
    report: &ExecutionReport,
    requested_level: ResultLevel,
) -> Result<(), ExperimentError> {
    let manifest_path = directory.join("execution.json");
    let manifest = read_manifest_file(&manifest_path)?;
    if manifest.execution.id != identity.id || manifest.experiment_id != experiment.id()? {
        return Err(err(
            "stored Execution metadata does not match requested identity",
        ));
    }
    if requested_level > manifest.default_result_level {
        return Err(err(format!(
            "Execution already exists at {}; use promote to add {requested_level} results",
            manifest.default_result_level
        )));
    }
    let parent = directory
        .parent()
        .ok_or_else(|| err("invalid Execution path"))?;
    let verify_dir = create_staging_dir(parent)?;
    let expected_summary = verify_dir.join("summary.parquet");
    let result = (|| {
        let (_, summary_hash) = write_summary(&expected_summary, experiment, &report.runs)?;
        let saved_summary_hash = read_summary_hash(&directory.join("summary.parquet"))?;
        if saved_summary_hash != manifest.summary_logical_hash {
            return Err(err(
                "replay found a modified summary.parquet; saved file was not changed",
            ));
        }
        if summary_hash != manifest.summary_logical_hash {
            return Err(err(
                "replay mismatch for summary.parquet; saved file was not changed",
            ));
        }
        let expected_runs = report
            .runs
            .iter()
            .map(|run| (run.run_id.as_str(), run))
            .collect::<BTreeMap<_, _>>();
        let saved_runs = manifest
            .runs
            .iter()
            .map(|run| (run.run_id.as_str(), run))
            .collect::<BTreeMap<_, _>>();
        if expected_runs.len() != saved_runs.len() {
            return Err(err(
                "replay mismatch for Run set; saved files were not changed",
            ));
        }
        for (run_id, expected) in expected_runs {
            let saved = saved_runs
                .get(run_id)
                .ok_or_else(|| err(format!("replay is missing Run {run_id}")))?;
            if saved.accounting_verified
                != stored_run(expected, saved.result_level, saved.table_hashes.clone())
                    .accounting_verified
            {
                return Err(err(format!(
                    "replay mismatch for run {run_id}, accounting verification marker"
                )));
            }
            if expected.status != saved.status {
                return Err(err(format!("replay mismatch for run {run_id}, status")));
            }
            if expected.status == RunStatus::Completed {
                for (table, expected_hash) in &saved.table_hashes {
                    let expected_rows = execution_rows(expected, table)?;
                    let hash = row_hash(table, &expected_rows)?;
                    let actual_hash = read_rows_hash(
                        &directory
                            .join("runs")
                            .join(run_hex(run_id)?)
                            .join(format!("{table}.parquet")),
                        table,
                    )?;
                    if &actual_hash != expected_hash {
                        return Err(err(format!(
                            "replay found modified run {run_id}/{table}.parquet; saved file was not changed"
                        )));
                    }
                    if &hash != expected_hash {
                        return Err(err(format!(
                            "replay mismatch for run {run_id}, table {table}; saved file was not changed"
                        )));
                    }
                }
            }
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&verify_dir);
    result
}

fn read_summary_hash(path: &Path) -> Result<String, ExperimentError> {
    let (schema, batches) = read_summary_batches(path)?;
    logical_hash("summary", &schema, &batches, &["run_id"])
        .map_err(|error| err(format!("could not hash saved summary: {error}")))
}

fn read_summary_batches(path: &Path) -> Result<(Arc<Schema>, Vec<RecordBatch>), ExperimentError> {
    let file = File::open(path).map_err(io_error(path))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|error| {
        err(format!(
            "could not read summary Parquet {}: {error}",
            path.display()
        ))
    })?;
    let schema = builder.schema().clone();
    let reader = builder.build().map_err(|error| {
        err(format!(
            "could not open summary Parquet {}: {error}",
            path.display()
        ))
    })?;
    let mut batches = Vec::new();
    for batch in reader {
        batches.push(batch.map_err(|error| {
            err(format!(
                "could not decode summary Parquet {}: {error}",
                path.display()
            ))
        })?);
    }
    Ok((schema, batches))
}

fn read_summary(path: &Path) -> Result<Vec<StoredRow>, ExperimentError> {
    let file = File::open(path).map_err(io_error(path))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|error| {
        err(format!(
            "could not read summary Parquet {}: {error}",
            path.display()
        ))
    })?;
    let reader = builder.build().map_err(|error| {
        err(format!(
            "could not open summary Parquet {}: {error}",
            path.display()
        ))
    })?;
    let mut rows = Vec::new();
    for batch in reader {
        let batch = batch.map_err(|error| {
            err(format!(
                "could not decode summary Parquet {}: {error}",
                path.display()
            ))
        })?;
        let run_idx = batch
            .schema()
            .index_of("run_id")
            .map_err(|error| err(error.to_string()))?;
        let status_idx = batch
            .schema()
            .index_of("status")
            .map_err(|error| err(error.to_string()))?;
        let summary_idx = batch
            .schema()
            .index_of("summary_json")
            .map_err(|error| err(error.to_string()))?;
        let ids = batch
            .column(run_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| err("summary run_id is not Utf8"))?;
        let statuses = batch
            .column(status_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| err("summary status is not Utf8"))?;
        let summaries = batch
            .column(summary_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| err("summary_json is not Utf8"))?;
        for index in 0..batch.num_rows() {
            let run_id = ids.value(index).to_owned();
            let _status = match statuses.value(index) {
                "completed" => RunStatus::Completed,
                "failed" => RunStatus::Failed,
                other => return Err(err(format!("unknown stored Run status {other}"))),
            };
            let summary = if summaries.is_null(index) {
                None
            } else {
                Some(
                    serde_json::from_str(summaries.value(index)).map_err(|error| {
                        err(format!("invalid summary for Run {run_id}: {error}"))
                    })?,
                )
            };
            rows.push(StoredRow { run_id, summary });
        }
    }
    Ok(rows)
}

fn find_execution(
    lake_root: &Path,
    execution_id: &str,
) -> Result<(PathBuf, ExecutionManifest), ExperimentError> {
    let hex = identity_hex(execution_id)?;
    let experiments = lake_root.join("experiments");
    let entries = fs::read_dir(&experiments).map_err(io_error(&experiments))?;
    for entry in entries {
        let entry = entry.map_err(io_error(&experiments))?;
        if !entry.file_type().map_err(io_error(&entry.path()))?.is_dir() {
            continue;
        }
        let directory = entry.path().join("executions").join(&hex);
        let metadata = directory.join("execution.json");
        if metadata.is_file() {
            let manifest = read_manifest_file(&metadata)?;
            if manifest.execution.id != execution_id {
                return Err(err(
                    "Execution directory content identity does not match its name",
                ));
            }
            return Ok((directory, manifest));
        }
    }
    Err(err(format!(
        "Execution {execution_id} was not found in the lake"
    )))
}

fn first_table_difference(
    left_path: &Path,
    right_path: &Path,
    table: &str,
) -> Result<(Option<String>, Option<String>), ExperimentError> {
    let left = read_rows(left_path, table)?;
    let right = read_rows(right_path, table)?;
    let left_map = left.into_iter().collect::<BTreeMap<_, _>>();
    let right_map = right.into_iter().collect::<BTreeMap<_, _>>();
    let keys = left_map
        .keys()
        .chain(right_map.keys())
        .collect::<BTreeSet<_>>();
    for key in keys {
        match (left_map.get(key), right_map.get(key)) {
            (Some(a), Some(b)) if a == b => {}
            (Some(a), Some(b)) => {
                let field = first_json_difference(
                    &serde_json::from_str(a).unwrap_or(Value::String(a.clone())),
                    &serde_json::from_str(b).unwrap_or(Value::String(b.clone())),
                    "",
                )
                .unwrap_or_else(|| "payload".into());
                return Ok((Some(key.clone()), Some(field)));
            }
            _ => return Ok((Some(key.clone()), Some("row".into()))),
        }
    }
    Ok((None, None))
}

fn first_json_difference(left: &Value, right: &Value, prefix: &str) -> Option<String> {
    if left == right {
        return None;
    }
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            let keys = left.keys().chain(right.keys()).collect::<BTreeSet<_>>();
            for key in keys {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                match (left.get(key), right.get(key)) {
                    (Some(a), Some(b)) => {
                        if let Some(field) = first_json_difference(a, b, &path) {
                            return Some(field);
                        }
                    }
                    _ => return Some(path),
                }
            }
            Some(prefix.into())
        }
        (Value::Array(left), Value::Array(right)) => {
            for (index, (a, b)) in left.iter().zip(right).enumerate() {
                if let Some(field) = first_json_difference(a, b, &format!("{prefix}[{index}]")) {
                    return Some(field);
                }
            }
            if left.len() != right.len() {
                Some(format!("{prefix}.length"))
            } else {
                Some(prefix.into())
            }
        }
        _ => Some(if prefix.is_empty() {
            "payload".into()
        } else {
            prefix.into()
        }),
    }
}

fn metric_deltas(
    left: Option<&RunSummary>,
    right: Option<&RunSummary>,
) -> BTreeMap<String, Option<f64>> {
    let names = [
        "total_return",
        "annualized_return",
        "mean_return",
        "std_return",
        "annualized_volatility",
        "sharpe",
        "max_drawdown",
        "total_turnover",
        "total_cost",
        "n_returns",
        "executed_count",
        "deferred_count",
        "skipped_buy_count",
        "session_count",
    ];
    let mut deltas = BTreeMap::new();
    for name in names {
        let (left_value, right_value) = match (left, right) {
            (Some(left), Some(right)) => (metric_value(left, name), metric_value(right, name)),
            _ => (None, None),
        };
        deltas.insert(name.into(), left_value.zip(right_value).map(|(a, b)| b - a));
    }
    deltas
}

fn metric_value(summary: &RunSummary, name: &str) -> Option<f64> {
    match name {
        "total_return" => summary.total_return,
        "annualized_return" => summary.annualized_return,
        "mean_return" => summary.mean_return,
        "std_return" => summary.std_return,
        "annualized_volatility" => summary.annualized_volatility,
        "sharpe" => summary.sharpe,
        "max_drawdown" => summary.max_drawdown,
        "total_turnover" => Some(summary.total_turnover),
        "total_cost" => Some(summary.total_cost),
        "n_returns" => Some(summary.n_returns as f64),
        "executed_count" => Some(summary.executed_count as f64),
        "deferred_count" => Some(summary.deferred_count as f64),
        "skipped_buy_count" => Some(summary.skipped_buy_count as f64),
        "session_count" => Some(summary.session_count as f64),
        _ => None,
    }
}

pub(super) fn replay_level(
    lake: &Path,
    experiment_id: &str,
    execution_id: &str,
    requested: ResultLevel,
) -> Result<ResultLevel, ExperimentError> {
    let path = execution_path(lake, experiment_id, execution_id)?.join("execution.json");
    if !path.exists() {
        return Ok(requested);
    }
    let manifest: ExecutionManifest =
        serde_json::from_slice(&fs::read(&path).map_err(io_error(&path))?)
            .map_err(|e| err(e.to_string()))?;
    Ok(manifest
        .runs
        .iter()
        .map(|r| r.result_level)
        .max()
        .unwrap_or(requested)
        .max(requested))
}

pub(super) fn execution_rows(
    run: &RunExecution,
    table: &str,
) -> Result<Vec<(String, String)>, ExperimentError> {
    if matches!(
        table,
        "adapter_events" | "backend_events" | "backend_comparisons"
    ) {
        return accurate_rows(run, table);
    }
    if let Some(result) = &run.fast_event_result {
        return fast_rows(result, table);
    }

    fn accurate_rows(
        run: &RunExecution,
        table: &str,
    ) -> Result<Vec<(String, String)>, ExperimentError> {
        use prajna_adapter::AdapterEvent;
        let diagnostics = run
            .backend_diagnostics
            .as_ref()
            .ok_or_else(|| err("completed Accurate Run has no Backend diagnostics"))?;
        let mut rows = vec![];
        let mut push = |prefix: &str, index: usize, payload: Value| {
            rows.push((format!("{prefix}|{index:020}"), json_payload(&payload)?));
            Ok::<_, ExperimentError>(())
        };
        match table {
            "adapter_events" => {
                for (index, event) in run.adapter_events.iter().enumerate() {
                    let payload = match event {
                        AdapterEvent::SyntheticOpen {
                            session_date,
                            quotes,
                        } => {
                            serde_json::json!({"event":"synthetic_open","session_date":session_date,"quotes":quotes})
                        }
                        AdapterEvent::PlatformGate {
                            session_date,
                            instrument_id,
                        } => {
                            serde_json::json!({"event":"platform_gate","session_date":session_date,"instrument_id":instrument_id})
                        }
                    };
                    push("platform_adapter", index, payload)?;
                }
                for (index, event) in diagnostics.adapter_constructions.iter().enumerate() {
                    push("worker_adapter", index, event.clone())?;
                }
            }
            "backend_events" => {
                for (index, event) in run.backend_events.iter().enumerate() {
                    push(
                        "mapped_response",
                        index,
                        serde_json::json!({
                            "evidence": diagnostics.evidence, "response": event
                        }),
                    )?;
                }
                for (index, event) in diagnostics.native_events.iter().enumerate() {
                    push("backend_native", index, event.clone())?;
                }
            }
            "backend_comparisons" => {
                push(
                    "evidence",
                    0,
                    serde_json::json!({
                        "evidence":diagnostics.evidence,
                        "native_account_available":diagnostics.evidence != "fake_backend",
                        "authority":"prajna-account"
                    }),
                )?;
                for (index, comparison) in diagnostics.comparisons.iter().enumerate() {
                    push("comparison", index, comparison.clone())?;
                }
            }
            _ => return Err(err(format!("unknown Accurate table {table}"))),
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(rows)
    }
    result_rows(
        run.result
            .as_ref()
            .ok_or_else(|| err("completed Run has no detail result"))?,
        table,
    )
}

fn attempt_payload(attempt: &prajna_research::fast_event::FastEventAttempt) -> Value {
    use prajna_research::strategy::ExecutionEvent;
    let mut value =
        serde_json::json!({"record_type":"attempt", "portfolio_id":attempt.portfolio_id.0});
    let (decision, session) = match &attempt.event {
        ExecutionEvent::Deferred {
            decision_session,
            attempt_session,
            blocked,
        } => {
            value["kind"] = "deferred".into();
            value["blocked"] = serde_json::json!(blocked);
            value["reason"] = "held_instrument_not_executable".into();
            (decision_session, Some(attempt_session))
        }
        ExecutionEvent::Executed {
            decision_session,
            attempt_session,
            applied,
            skipped_buys,
        } => {
            value["kind"] = "executed".into();
            value["applied"] = serde_json::json!(applied);
            value["skipped_buys"] = serde_json::json!(skipped_buys);
            (decision_session, Some(attempt_session))
        }
        ExecutionEvent::RetryableExecution {
            decision_session,
            attempt_session,
            applied,
            skipped_buys,
            is_retry,
        } => {
            value["kind"] = "retryable_execution".into();
            value["is_retry"] = (*is_retry).into();
            value["applied"] = serde_json::json!(applied);
            value["skipped_buys"] = serde_json::json!(skipped_buys);
            (decision_session, Some(attempt_session))
        }
        ExecutionEvent::PendingAtEnd {
            decision_session,
            targets,
        } => {
            value["kind"] = "pending_at_end".into();
            value["targets"] = serde_json::json!(targets);
            (decision_session, None)
        }
    };
    value["decision_session"] = decision.session_date.to_string().into();
    if let Some(session) = session {
        value["session_date"] = session.session_date.to_string().into();
        value["ts_open"] = serde_json::json!(session.ts_open);
    }
    value
}
fn fast_rows(
    result: &prajna_research::fast_event::FastEventResult,
    table: &str,
) -> Result<Vec<(String, String)>, ExperimentError> {
    use prajna_research::fast_event::PriceField;
    let mut rows = vec![];
    match table {
        "account_valuations" | "portfolio_valuations" => {
            for session in &result.sessions {
                for (point, valuation, sources) in [
                    ("open_after", &session.open_after, &session.open_sources),
                    ("close", &session.close, &session.close_sources),
                ] {
                    let source=sources.iter().map(|(id,s)| (id.to_string(),serde_json::json!({"session_date":s.session_date.to_string(),"field":match s.field {PriceField::Open=>"open",PriceField::Close=>"close"}}))).collect::<serde_json::Map<_,_>>();
                    let mut push = |scope: &str,
                                    id: Option<u64>,
                                    value: &prajna_account::PortfolioValuation|
                     -> Result<(), ExperimentError> {
                        let payload = serde_json::json!({"session_date":session.session.session_date.to_string(),"valuation_point":point,"scope":scope,"portfolio_id":id,"cash":value.cash,"dividend_receivable":value.dividend_receivable,"liquidation_receivable":value.liquidation_receivable,"positions":value.positions,"equity":value.equity,"price_sources":source,"unallocated_capital":if scope=="account" {Some(valuation.unallocated_capital)} else {None},"accounting_verified":true});
                        rows.push((
                            format!(
                                "{}|{point}|{scope}|{}",
                                session.session.session_date,
                                id.map(|n| n.to_string()).unwrap_or_default()
                            ),
                            json_payload(&payload)?,
                        ));
                        Ok(())
                    };
                    if table == "account_valuations" {
                        push("account", None, &valuation.account)?;
                    } else {
                        for (id, value) in &valuation.portfolios {
                            push("vp", Some(id.0), value)?;
                        }
                    }
                }
            }
        }
        "corporate_actions" => {
            for (index, entry) in result.corporate_action_ledger.iter().enumerate() {
                let mut payload =
                    serde_json::to_value(entry).map_err(|error| err(error.to_string()))?;
                payload["record_type"] = "corporate_action".into();
                rows.push((format!("event|{index:020}"), json_payload(&payload)?));
            }
        }
        "terminal_settlements" => {
            for (index, entry) in result.terminal_settlements.iter().enumerate() {
                let mut payload =
                    serde_json::to_value(entry).map_err(|error| err(error.to_string()))?;
                payload["record_type"] = "terminal_settlement".into();
                rows.push((format!("event|{index:020}"), json_payload(&payload)?));
            }
        }
        "orders" => {
            for (id, order) in result.account.orders() {
                rows.push((
                    format!("order|{:020}", id.0),
                    json_payload(&serde_json::json!({"record_type":"order","order":order}))?,
                ));
            }
            for (index, adjustment) in result.sizing_adjustments.iter().enumerate() {
                let mut payload =
                    serde_json::to_value(adjustment).map_err(|e| err(e.to_string()))?;
                payload["record_type"] = "sizing_adjustment".into();
                rows.push((format!("sizing|{index:020}"), json_payload(&payload)?));
            }
            for (index, attempt) in result.attempts.iter().enumerate() {
                let mut payload = attempt_payload(attempt);
                if let Some(skipped) = payload.get("skipped_buys").cloned() {
                    payload["blocked_entry_reason"] = "instrument_not_executable".into();
                    payload["blocked_entries"] = skipped;
                }
                rows.push((format!("attempt|{index:020}"), json_payload(&payload)?));
            }
            for (index, (portfolio, attempt)) in result.risk_exits.iter().enumerate() {
                rows.push((
                    format!("risk|{index:020}"),
                    json_payload(&serde_json::json!({
                        "record_type": "risk_exit", "portfolio_id": portfolio,
                        "attempt": attempt,
                    }))?,
                ));
            }
            for (portfolio, instrument) in &result.pending_risk_exits {
                rows.push((
                    format!("risk_pending|{}|{instrument}", portfolio.0),
                    json_payload(
                        &serde_json::json!({"record_type":"risk_exit_pending_at_end",
                        "portfolio_id":portfolio,"instrument_id":instrument}),
                    )?,
                ));
            }
        }
        "fills" => {
            for (index, fill) in result.fills.iter().enumerate() {
                rows.push((format!("{index:020}"),json_payload(&serde_json::json!({"fill":fill.fill,"fee_basis":fill.fee_basis,"turnover_basis":fill.turnover_basis,"fee_side":fill.fee_side}))?));
            }
        }
        "fee_settlements" => {
            for (index, fee) in result.account.fee_settlements().iter().enumerate() {
                rows.push((format!("{index:020}"), json_payload(fee)?));
            }
        }
        _ => return Err(err(format!("unknown Fast Event table {table}"))),
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rows)
}

#[cfg(test)]
mod promotion_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn later_publication_failure_rolls_back_additions_and_allows_retry() {
        let temp = TempDir::new().unwrap();
        let first_stage = temp.path().join("first.stage");
        let second_stage = temp.path().join("second.stage");
        fs::write(&first_stage, b"first").unwrap();
        fs::write(&second_stage, b"second").unwrap();
        let first = temp.path().join("runs/new/first.parquet");
        let second = temp.path().join("runs/existing/second.parquet");
        fs::create_dir_all(second.parent().unwrap()).unwrap();
        fs::write(&second, b"preexisting").unwrap();
        let pending = vec![(first_stage, first.clone()), (second_stage, second.clone())];
        let error = publish_promotion(&pending, || panic!("must not commit")).unwrap_err();
        assert!(error.to_string().contains("second.parquet"));
        assert!(!first.exists());
        assert!(!first.parent().unwrap().exists());
        assert_eq!(fs::read(&second).unwrap(), b"preexisting");
        fs::remove_file(&second).unwrap();
        publish_promotion(&pending, || Ok(())).unwrap();
        assert_eq!(fs::read(&first).unwrap(), b"first");
        assert_eq!(fs::read(&second).unwrap(), b"second");
    }

    #[test]
    fn manifest_commit_failure_rolls_back_every_published_table() {
        let temp = TempDir::new().unwrap();
        let stage = temp.path().join("stage");
        fs::write(&stage, b"table").unwrap();
        let first = temp.path().join("runs/a/first.parquet");
        let second = temp.path().join("runs/b/second.parquet");
        let pending = vec![(stage.clone(), first.clone()), (stage, second.clone())];
        publish_promotion(&pending, || {
            assert!(first.exists() && second.exists());
            Err(err("injected manifest rename failure"))
        })
        .unwrap_err();
        assert!(!temp.path().join("runs").exists());
        publish_promotion(&pending, || Ok(())).unwrap();
    }
}
