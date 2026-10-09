use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::{self, File},
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
const STANDARD_TABLES: [&str; 2] = ["sessions", "executions"];
const FULL_TABLES: [&str; 6] = [
    "sessions",
    "executions",
    "target_weights",
    "weights_after_execution",
    "decisions",
    "pending_at_end",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultLevel {
    Summary,
    Standard,
    Full,
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
    pub started_at: String,
    pub finished_at: String,
    pub factor_time_ms: u128,
    pub run_time_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRun {
    pub run_id: String,
    pub status: RunStatus,
    pub result_level: ResultLevel,
    pub table_hashes: BTreeMap<String, String>,
    pub not_saved: Vec<String>,
    pub not_produced_by_engine: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct StoredExecution {
    pub manifest: ExecutionManifest,
    pub path: PathBuf,
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
    let experiment_id = experiment.id()?;
    let experiment_root = experiment_root(lake_root, &experiment_id);
    fs::create_dir_all(&experiment_root).map_err(io_error(&experiment_root))?;
    ensure_experiment_file(experiment, &experiment_root)?;
    let executions_root = experiment_root.join("executions");
    fs::create_dir_all(&executions_root).map_err(io_error(&executions_root))?;
    let destination = executions_root.join(identity_hex(&identity.id)?);

    if destination.exists() {
        verify_existing(&destination, identity, experiment, report, level)?;
        let manifest = read_manifest_file(&destination.join("execution.json"))?;
        return Ok(StoredExecution {
            manifest,
            path: destination,
        });
    }

    let staging = create_staging_dir(&executions_root)?;
    let result: Result<ExecutionManifest, ExperimentError> = (|| {
        let (rows, summary_hash) =
            write_summary(&staging.join("summary.parquet"), experiment, &report.runs)?;
        let mut stored_runs = Vec::with_capacity(rows.len());
        for execution in &report.runs {
            let mut table_hashes = BTreeMap::new();
            if let Some(result) = &execution.result {
                for table in level.tables() {
                    let table_rows = result_rows(result, table)?;
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
            started_at,
            finished_at: now_rfc3339(),
            factor_time_ms: report.factor_time_ms,
            run_time_ms: report.run_time_ms,
        };
        write_json_synced(&staging.join("execution.json"), &manifest)?;
        sync_tree(&staging)?;
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
        return Err(err("promotion requires at least one Run identity"));
    }
    let experiment_id = experiment.id()?;
    let destination = execution_path(lake_root, &experiment_id, execution_id)?;
    let mut manifest = read_manifest_file(&destination.join("execution.json"))?;
    if manifest.experiment_id != experiment_id || manifest.execution.id != execution_id {
        return Err(err(
            "stored Execution identity does not match requested Experiment",
        ));
    }
    let summary_path = destination.join("summary.parquet");
    if read_summary_hash(&summary_path)? != manifest.summary_logical_hash {
        return Err(err(
            "promotion found a modified summary.parquet; stored files were not changed",
        ));
    }
    let unique_ids = run_ids.iter().collect::<BTreeSet<_>>();
    if unique_ids.len() != run_ids.len() {
        return Err(err("promotion contains a duplicate Run identity"));
    }
    let report = experiment.execute(lake_root, threads)?;
    let executions = report
        .runs
        .iter()
        .map(|run| (run.run_id.as_str(), run))
        .collect::<BTreeMap<_, _>>();
    for run_id in run_ids {
        let stored_index = manifest
            .runs
            .iter()
            .position(|run| run.run_id == *run_id)
            .ok_or_else(|| err(format!("Run {run_id} is not part of this Execution")))?;
        let old_level = manifest.runs[stored_index].result_level;
        if level <= old_level {
            return Err(err(format!(
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
        let vector = run
            .result
            .as_ref()
            .ok_or_else(|| err(format!("Run {run_id} has no Vector result")))?;
        let existing_hashes = manifest.runs[stored_index].table_hashes.clone();
        let tables = level.tables();
        for table in tables {
            let rows = result_rows(vector, table)?;
            let path = destination
                .join("runs")
                .join(run_hex(run_id)?)
                .join(format!("{table}.parquet"));
            let computed_hash = row_hash(table, &rows)?;
            if let Some(existing_hash) = existing_hashes.get(*table) {
                let actual_hash = read_rows_hash(&path, table)?;
                if &actual_hash != existing_hash || actual_hash != computed_hash {
                    return Err(err(format!(
                        "promotion replay mismatch for run {run_id}, table {table}"
                    )));
                }
                continue;
            }
            let parent = path.parent().ok_or_else(|| err("invalid Run table path"))?;
            fs::create_dir_all(parent).map_err(io_error(parent))?;
            if path.exists() {
                return Err(err(format!(
                    "untracked table already exists at {}; refusing to overwrite it",
                    path.display()
                )));
            }
            let stage = parent.join(format!(".{table}.staging-{}", next_stage_id()));
            let write_result = write_rows(&stage, table, &rows);
            match write_result {
                Ok(hash) if hash == computed_hash => {
                    if let Err(error) = fs::hard_link(&stage, &path) {
                        let _ = fs::remove_file(&stage);
                        return Err(io_error(&path)(error));
                    }
                    fs::remove_file(&stage).map_err(io_error(&stage))?;
                    sync_dir(parent)?;
                    if let Some(runs_root) = parent.parent() {
                        sync_dir(runs_root)?;
                    }
                    manifest.runs[stored_index]
                        .table_hashes
                        .insert((*table).into(), hash);
                }
                Ok(_) => {
                    let _ = fs::remove_file(&stage);
                    return Err(err(format!(
                        "logical hash changed while writing run {run_id}, table {table}"
                    )));
                }
                Err(error) => {
                    let _ = fs::remove_file(&stage);
                    return Err(error);
                }
            }
        }
        manifest.runs[stored_index].result_level = level;
        manifest.runs[stored_index].not_saved = FULL_TABLES
            .iter()
            .filter(|table| {
                !manifest.runs[stored_index]
                    .table_hashes
                    .contains_key(**table)
            })
            .map(|table| (*table).to_owned())
            .collect();
    }
    manifest.threads = threads;
    manifest.finished_at = now_rfc3339();
    replace_json_synced(&destination.join("execution.json"), &manifest)?;
    Ok(StoredExecution {
        manifest,
        path: destination,
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
        return Err(err(format!(
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
            return Err(err(format!(
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

fn replace_json_synced(path: &Path, value: &impl Serialize) -> Result<(), ExperimentError> {
    let parent = path.parent().ok_or_else(|| err("invalid manifest path"))?;
    let staging = parent.join(format!(".execution.json.staging-{}", next_stage_id()));
    write_json_synced(&staging, value)?;
    fs::rename(&staging, path).map_err(io_error(path))?;
    sync_dir(parent)
}

fn read_manifest_file(path: &Path) -> Result<ExecutionManifest, ExperimentError> {
    let bytes = fs::read(path).map_err(io_error(path))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        err(format!(
            "invalid execution manifest {}: {error}",
            path.display()
        ))
    })
}

fn stored_run(
    execution: &RunExecution,
    level: ResultLevel,
    table_hashes: BTreeMap<String, String>,
) -> StoredRun {
    let not_saved = FULL_TABLES
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
        not_produced_by_engine: NOT_PRODUCED_BY_ENGINE
            .iter()
            .map(|name| (*name).into())
            .collect(),
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
    let mut summaries = Vec::with_capacity(runs.len());
    let mut summary_json = Vec::with_capacity(runs.len());
    let mut stored = Vec::with_capacity(runs.len());
    for run in runs {
        let parameters = parameters_from_spec(&run.run_spec)?;
        let summary = run
            .result
            .as_ref()
            .map(|result| summarize(result, experiment.sessions_per_year()));
        ids.push(Some(run.run_id.clone()));
        short.push(Some(to_i64(parameters.short)?));
        long.push(Some(to_i64(parameters.long)?));
        vol.push(Some(to_i64(parameters.vol)?));
        w_s.push(Some(parameters.w_s));
        w_l.push(Some(parameters.w_l));
        w_v.push(Some(parameters.w_v));
        trend.push(parameters.trend.map(to_i64).transpose()?);
        top_k.push(Some(to_i64(parameters.top_k)?));
        rebalance.push(Some(to_i64(parameters.rebalance_every)?));
        statuses.push(Some(
            match run.status {
                RunStatus::Completed => "completed",
                RunStatus::Failed => "failed",
            }
            .to_owned(),
        ));
        errors.push(run.error.clone());
        summaries.push(
            summary
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| err(error.to_string()))?,
        );
        summary_json.push(summaries.last().cloned().flatten());
        stored.push(StoredRow {
            run_id: run.run_id.clone(),
            summary,
        });
    }

    let schema = summary_schema();
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
        summary_float(runs, experiment, |summary| summary.total_return)?,
        summary_float(runs, experiment, |summary| summary.annualized_return)?,
        summary_integer(runs, experiment, |summary| summary.n_returns)?,
        summary_float(runs, experiment, |summary| summary.mean_return)?,
        summary_float(runs, experiment, |summary| summary.std_return)?,
        summary_float(runs, experiment, |summary| summary.annualized_volatility)?,
        summary_float(runs, experiment, |summary| summary.sharpe)?,
        summary_float(runs, experiment, |summary| summary.max_drawdown)?,
        summary_string(runs, experiment, |summary| {
            summary.drawdown_peak_session.clone()
        })?,
        summary_string(runs, experiment, |summary| {
            summary.drawdown_trough_session.clone()
        })?,
        summary_float(runs, experiment, |summary| Some(summary.total_turnover))?,
        summary_float(runs, experiment, |summary| Some(summary.total_cost))?,
        summary_integer(runs, experiment, |summary| summary.executed_count)?,
        summary_integer(runs, experiment, |summary| summary.deferred_count)?,
        summary_integer(runs, experiment, |summary| summary.skipped_buy_count)?,
        summary_integer(runs, experiment, |summary| summary.session_count)?,
        summary_bool(runs, experiment, |summary| summary.pending_at_end)?,
        summary_bool(runs, experiment, |summary| summary.insufficient_sessions)?,
        summary_bool(runs, experiment, |summary| summary.zero_volatility)?,
        summary_string(runs, experiment, |summary| {
            Some(serde_json::to_string(&summary.assumptions).unwrap_or_default())
        })?,
        summary_string(runs, experiment, |summary| {
            Some(summary.availability_assumption.clone())
        })?,
        Arc::new(StringArray::from(summary_json)),
    ];
    let batch = RecordBatch::try_new(schema.clone(), columns)
        .map_err(|error| err(format!("invalid summary table: {error}")))?;
    let hash = write_batch(path, &schema, &batch, "summary", &["run_id"])?;
    Ok((stored, hash))
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
    runs: &[RunExecution],
    experiment: &Experiment,
    select: impl Fn(&RunSummary) -> Option<f64>,
) -> Result<ArrayRef, ExperimentError> {
    let values = runs
        .iter()
        .map(|run| {
            run.result
                .as_ref()
                .and_then(|result| select(&summarize(result, experiment.sessions_per_year())))
        })
        .collect::<Vec<_>>();
    Ok(Arc::new(Float64Array::from(values)))
}
fn summary_integer(
    runs: &[RunExecution],
    experiment: &Experiment,
    select: impl Fn(&RunSummary) -> usize,
) -> Result<ArrayRef, ExperimentError> {
    let values = runs
        .iter()
        .map(|run| {
            run.result
                .as_ref()
                .map(|result| to_i64(select(&summarize(result, experiment.sessions_per_year()))))
                .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Arc::new(Int64Array::from(values)))
}
fn summary_string(
    runs: &[RunExecution],
    experiment: &Experiment,
    select: impl Fn(&RunSummary) -> Option<String>,
) -> Result<ArrayRef, ExperimentError> {
    let values = runs
        .iter()
        .map(|run| {
            run.result
                .as_ref()
                .and_then(|result| select(&summarize(result, experiment.sessions_per_year())))
        })
        .collect::<Vec<_>>();
    Ok(Arc::new(StringArray::from(values)))
}
fn summary_bool(
    runs: &[RunExecution],
    experiment: &Experiment,
    select: impl Fn(&RunSummary) -> bool,
) -> Result<ArrayRef, ExperimentError> {
    let values = runs
        .iter()
        .map(|run| {
            run.result
                .as_ref()
                .map(|result| select(&summarize(result, experiment.sessions_per_year())))
        })
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
        "sessions" => {
            for session in &result.sessions {
                let payload = serde_json::json!({
                    "session_date": session.session_date,
                    "nav": session.nav,
                    "gross_return": session.gross_return,
                    "net_return": session.net_return,
                    "turnover": session.turnover,
                    "cost": session.cost,
                    "valuation_carried": session.valuation_carried,
                });
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
    if schema.as_ref() != vector_table_schema().as_ref() {
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
            if expected.status != saved.status {
                return Err(err(format!("replay mismatch for run {run_id}, status")));
            }
            if let Some(vector) = &expected.result {
                for (table, expected_hash) in &saved.table_hashes {
                    let expected_rows = result_rows(vector, table)?;
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
    logical_hash("summary", &schema, &batches, &["run_id"])
        .map_err(|error| err(format!("could not hash saved summary: {error}")))
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
