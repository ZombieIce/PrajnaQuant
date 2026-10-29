//! Persistent, bounded local ETF run jobs. Jobs pin an immutable research snapshot and
//! published Universe version; only explicitly requested diagnostic runs are admitted.
use super::universe_api::UniverseStore;
use crate::{
    core::ExperimentConfig,
    data::{self, SnapshotManifest},
    runner,
    universe::{AssetScope, UniverseDefinition},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use duckdb::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path as FsPath, PathBuf},
    process::{Child, ChildStderr, Command, ExitStatus, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[path = "momentum_api.rs"]
mod momentum_api;
#[path = "selection_api.rs"]
mod selection_api;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use rustix::process::{Pid, Signal, kill_process_group};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::os::unix::process::CommandExt;

const MAX_QUEUED: usize = 8;
const MAX_DATE_SPAN_DAYS: i64 = 3661;
const MAX_SNAPSHOT_ROWS: i64 = 2_000_000;
const MAX_UNIVERSE_INSTRUMENTS: usize = 100;
const EXPERIMENT_TASK_TYPE: &str = "prajna_experiment";
const EXPERIMENT_TASK_VERSION: u32 = 1;
const DEFAULT_RESULT_LEVEL: &str = "summary";

fn default_result_level() -> String {
    DEFAULT_RESULT_LEVEL.into()
}

fn is_summary_level(level: &str) -> bool {
    level == DEFAULT_RESULT_LEVEL
}

fn normalized_result_level(level: Option<&str>) -> Result<Option<String>, ApiFailure> {
    match level {
        None | Some(DEFAULT_RESULT_LEVEL) => Ok(None),
        Some("standard") => Ok(Some("standard".into())),
        Some(other) => Err(ApiFailure::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "result_level must be summary or standard",
            json!({"result_level": other}),
        )),
    }
}
const MAX_CLI_STDERR_BYTES: usize = 16 * 1024;
const STDERR_CAPTURE_STOP_TIMEOUT: Duration = Duration::from_millis(200);
const STDERR_CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub details: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunJob {
    pub run_id: Uuid,
    pub kind: String,
    pub status: RunStatus,
    pub stage: String,
    pub created_at: String,
    pub updated_at: String,
    pub request_sha256: String,
    pub idempotency_key: String,
    pub universe_id: Uuid,
    pub version_id: Uuid,
    pub universe_content_hash: String,
    pub snapshot_id: Uuid,
    pub snapshot_sha256: String,
    pub config_sha256: String,
    pub diagnostic: bool,
    pub experiment_id: Option<Uuid>,
    pub error: Option<RunError>,
    #[serde(skip)]
    config: Option<ExperimentConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentTaskStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentTask {
    pub task_id: Uuid,
    pub task_type: String,
    pub task_version: u32,
    pub status: ExperimentTaskStatus,
    pub stage: String,
    pub created_at: String,
    pub updated_at: String,
    pub request_sha256: String,
    pub idempotency_key: String,
    #[serde(
        default = "default_result_level",
        skip_serializing_if = "is_summary_level"
    )]
    pub result_level: String,
    pub process_group_id: Option<u32>,
    #[serde(default)]
    pub process_group_generation: Option<Uuid>,
    pub result: Option<Value>,
    pub error: Option<RunError>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubmitExperimentTask {
    pub idempotency_key: String,
    pub definition: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_level: Option<String>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubmitRun {
    pub kind: String,
    pub universe_id: Uuid,
    pub version_id: Uuid,
    pub snapshot_id: Uuid,
    pub config: ExperimentConfig,
    pub idempotency_key: String,
    #[serde(default)]
    pub diagnostic_mode: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct Preflight {
    pub ready: bool,
    pub diagnostic_allowed: bool,
    pub blockers: Vec<RunError>,
    pub universe_content_hash: Option<String>,
    pub snapshot_sha256: Option<String>,
}

#[derive(Clone)]
pub struct RunJobApi(Arc<Inner>);
struct Inner {
    output: PathBuf,
    experiment_cli: PathBuf,
    process_inspector: PathBuf,
    gate: Mutex<Store>,
    wake: Condvar,
    universes: Arc<UniverseStore>,
}
#[derive(Default)]
struct Store {
    jobs: HashMap<Uuid, RunJob>,
    tasks: HashMap<Uuid, ExperimentTask>,
    idempotency: HashMap<String, (String, Uuid)>,
    task_idempotency: HashMap<String, (String, Uuid)>,
    queue: VecDeque<QueueItem>,
    active: bool,
}

#[derive(Debug, Clone, Copy)]
enum QueueItem {
    LegacyRun(Uuid),
    ExperimentTask(Uuid),
}

enum QueuedWork {
    LegacyRun(Box<RunJob>),
    ExperimentTask(Box<ExperimentTask>),
}

impl RunJobApi {
    pub fn start(output: PathBuf, universes: Arc<UniverseStore>) -> Self {
        let experiment_cli = std::env::var_os("PRAJNA_EXPERIMENT_CLI")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("prajna-experiment"));
        Self::start_with_cli(output, universes, experiment_cli)
    }
    fn start_with_cli(
        output: PathBuf,
        universes: Arc<UniverseStore>,
        experiment_cli: PathBuf,
    ) -> Self {
        let api = Self(Arc::new(Inner {
            output,
            experiment_cli,
            process_inspector: PathBuf::from("ps"),
            gate: Mutex::new(Store::default()),
            wake: Condvar::new(),
            universes,
        }));
        api.recover();
        let worker = api.clone();
        thread::Builder::new()
            .name("prajna-run-worker".into())
            .spawn(move || worker.worker_loop())
            .expect("run worker thread starts");
        api
    }
    #[cfg(test)]
    fn without_worker(
        output: PathBuf,
        universes: Arc<UniverseStore>,
        experiment_cli: PathBuf,
    ) -> Self {
        Self::without_worker_with_process_inspector(
            output,
            universes,
            experiment_cli,
            PathBuf::from("ps"),
        )
    }
    #[cfg(test)]
    fn without_worker_with_process_inspector(
        output: PathBuf,
        universes: Arc<UniverseStore>,
        experiment_cli: PathBuf,
        process_inspector: PathBuf,
    ) -> Self {
        Self(Arc::new(Inner {
            output,
            experiment_cli,
            process_inspector,
            gate: Mutex::new(Store::default()),
            wake: Condvar::new(),
            universes,
        }))
    }
    pub fn router(self) -> Router {
        Router::new()
            .route("/api/v1/research-snapshots", get(list_snapshots))
            .route("/api/v1/runs", get(list_runs).post(submit))
            .route("/api/v1/runs/preflight", post(preflight))
            .route("/api/v1/runs/{id}", get(get_run))
            .route(
                "/api/v1/tasks/experiments",
                get(list_experiment_tasks).post(submit_experiment_task),
            )
            .route("/api/v1/tasks/experiments/{id}", get(get_experiment_task))
            .with_state(self)
    }
    pub fn result_router(self, authority: String) -> Router {
        Router::new()
            .route(
                "/api/v1/tasks/experiments/{id}/result",
                get(get_experiment_result),
            )
            .layer(axum::middleware::from_fn_with_state(
                super::dataset_api::LocalPage::new(
                    authority,
                    "result_origin_forbidden",
                    "Experiment result is restricted to the configured local workbench origin.",
                ),
                super::dataset_api::local_page,
            ))
            .with_state(self)
    }
    fn recover(&self) {
        let jobs_dir = self.0.output.join("runs");
        let mut store = self.0.gate.lock().expect("run store lock");
        if let Ok(entries) = fs::read_dir(&jobs_dir) {
            for entry in entries.flatten() {
                let directory = entry.path();
                let path = directory.join("run.json");
                let Ok(bytes) = fs::read(path) else { continue };
                let Ok(mut job) = serde_json::from_slice::<RunJob>(&bytes) else {
                    continue;
                };
                match job.status {
                    RunStatus::Queued => {
                        job.config = read_request(&directory).map(|r| r.config);
                        if job.config.is_some() {
                            store.queue.push_back(QueueItem::LegacyRun(job.run_id))
                        } else {
                            fail_job(
                                &mut job,
                                "worker_interrupted",
                                "queued request payload is missing",
                            )
                        }
                    }
                    RunStatus::Running => fail_job(
                        &mut job,
                        "worker_interrupted",
                        "process stopped while the run was executing; no experiment was committed",
                    ),
                    RunStatus::Succeeded | RunStatus::Failed => {}
                }
                store.idempotency.insert(
                    job.idempotency_key.clone(),
                    (job.request_sha256.clone(), job.run_id),
                );
                let _ = persist_job(&self.0.output, &job);
                store.jobs.insert(job.run_id, job);
            }
        }

        let tasks_dir = self.0.output.join("tasks");
        let Ok(entries) = fs::read_dir(&tasks_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let directory = entry.path();
            let path = directory.join("task.json");
            let Ok(bytes) = fs::read(path) else { continue };
            let Ok(mut task) = serde_json::from_slice::<ExperimentTask>(&bytes) else {
                continue;
            };
            if !matches!(
                task.task_type.as_str(),
                EXPERIMENT_TASK_TYPE | "prajna_momentum"
            ) || task.task_version != EXPERIMENT_TASK_VERSION
            {
                continue;
            }
            let process_group_generation = task.process_group_generation;
            let cleanup_error = task.process_group_id.and_then(|process_group_id| {
                terminate_process_group(
                    process_group_id,
                    task.task_id,
                    process_group_generation,
                    &self.0.process_inspector,
                )
                .err()
                .map(|error| {
                    (
                        process_group_id,
                        process_group_generation,
                        process_group_cleanup_error(error),
                    )
                })
            });
            match task.status {
                ExperimentTaskStatus::Queued | ExperimentTaskStatus::Running => interrupt_task(
                    &mut task,
                    "service restarted before the experiment task completed",
                ),
                ExperimentTaskStatus::Succeeded
                | ExperimentTaskStatus::Failed
                | ExperimentTaskStatus::Interrupted => {}
            }
            task.process_group_id = cleanup_error
                .as_ref()
                .map(|(process_group_id, _, _)| *process_group_id);
            task.process_group_generation = cleanup_error
                .as_ref()
                .and_then(|(_, generation, _)| *generation);
            if let Some((_, _, mut error)) = cleanup_error {
                preserve_cli_error_on_cleanup(&mut error, task.error.as_ref());
                task.error = Some(error);
                task.updated_at = chrono::Utc::now().to_rfc3339();
            }
            store.task_idempotency.insert(
                task.idempotency_key.clone(),
                (task.request_sha256.clone(), task.task_id),
            );
            let _ = persist_experiment_task(&self.0.output, &task);
            store.tasks.insert(task.task_id, task);
        }
    }
    fn submit(&self, request: SubmitRun) -> Result<(RunJob, bool), ApiFailure> {
        let request_hash = canonical_hash(&request)?;
        validate_shape(&request)?;
        let existing = {
            self.0
                .gate
                .lock()
                .map_err(|_| ApiFailure::internal("run store lock poisoned"))?
                .idempotency
                .get(&request.idempotency_key)
                .cloned()
        };
        if let Some((old_hash, run_id)) = existing {
            if old_hash != request_hash {
                return Err(ApiFailure::new(
                    StatusCode::CONFLICT,
                    "idempotency_conflict",
                    "idempotency_key was already used for a different request",
                    Value::Null,
                ));
            }
            let job = self
                .get(run_id)
                .ok_or_else(|| ApiFailure::internal("idempotency index is inconsistent"))?;
            return Ok((job, false));
        }
        let preflight = self.inspect(&request);
        if (!request.diagnostic_mode && !preflight.ready)
            || (request.diagnostic_mode && !preflight.diagnostic_allowed)
        {
            let diagnostic = request.diagnostic_mode;
            return Err(ApiFailure::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "capability_not_ready",
                if diagnostic {
                    "diagnostic ETF run prerequisites are not met"
                } else {
                    "trusted historical ETF run is blocked by current evidence gates"
                },
                json!({"blockers":preflight.blockers}),
            ));
        }
        let definition = self.load_definition(request.universe_id, request.version_id)?;
        let snapshot = self.load_snapshot(request.snapshot_id)?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut store = self
            .0
            .gate
            .lock()
            .map_err(|_| ApiFailure::internal("run store lock poisoned"))?;
        if let Some((old_hash, run_id)) = store.idempotency.get(&request.idempotency_key) {
            if old_hash != &request_hash {
                return Err(ApiFailure::new(
                    StatusCode::CONFLICT,
                    "idempotency_conflict",
                    "idempotency_key was already used for a different request",
                    Value::Null,
                ));
            }
            let job = store
                .jobs
                .get(run_id)
                .cloned()
                .ok_or_else(|| ApiFailure::internal("idempotency index is inconsistent"))?;
            return Ok((job, false));
        }
        if store.queue.len() + usize::from(store.active) >= MAX_QUEUED {
            return Err(ApiFailure::new(
                StatusCode::TOO_MANY_REQUESTS,
                "queue_full",
                "run queue is full; retry after an existing job finishes",
                Value::Null,
            ));
        }
        let config_bytes = serde_json::to_vec(&request.config).map_err(ApiFailure::from_error)?;
        let job = RunJob {
            run_id: Uuid::new_v4(),
            kind: request.kind.clone(),
            status: RunStatus::Queued,
            stage: "queued".into(),
            created_at: now.clone(),
            updated_at: now,
            request_sha256: request_hash.clone(),
            idempotency_key: request.idempotency_key.clone(),
            universe_id: definition.universe_id,
            version_id: definition.version_id,
            universe_content_hash: definition.content_hash.clone(),
            snapshot_id: snapshot.snapshot_id,
            snapshot_sha256: snapshot.sha256.clone(),
            config_sha256: format!("{:x}", Sha256::digest(config_bytes)),
            diagnostic: request.diagnostic_mode,
            experiment_id: None,
            error: None,
            config: Some(request.config.clone()),
        };
        persist_request(&self.0.output, job.run_id, &request)?;
        persist_job(&self.0.output, &job)?;
        store
            .idempotency
            .insert(request.idempotency_key, (request_hash, job.run_id));
        store.queue.push_back(QueueItem::LegacyRun(job.run_id));
        store.jobs.insert(job.run_id, job.clone());
        self.0.wake.notify_one();
        Ok((job, true))
    }
    fn submit_experiment_task(
        &self,
        mut request: SubmitExperimentTask,
    ) -> Result<(ExperimentTask, bool), ApiFailure> {
        let result_level = normalized_result_level(request.result_level.as_deref())?;
        request.result_level = result_level.clone();
        validate_experiment_task_request(&request)?;
        let request_hash = canonical_experiment_task_hash(&request)?;
        let task_result_level = result_level.unwrap_or_else(default_result_level);
        let mut store = self
            .0
            .gate
            .lock()
            .map_err(|_| ApiFailure::internal("run store lock poisoned"))?;
        if let Some((old_hash, task_id)) = store.task_idempotency.get(&request.idempotency_key) {
            if old_hash != &request_hash {
                return Err(ApiFailure::new(
                    StatusCode::CONFLICT,
                    "idempotency_conflict",
                    "idempotency_key was already used for a different experiment task",
                    Value::Null,
                ));
            }
            let task = store
                .tasks
                .get(task_id)
                .ok_or_else(|| ApiFailure::internal("task idempotency index is inconsistent"))?;
            return Ok((task.clone(), false));
        }
        if store.queue.len() + usize::from(store.active) >= MAX_QUEUED {
            return Err(ApiFailure::new(
                StatusCode::TOO_MANY_REQUESTS,
                "queue_full",
                "run queue is full; retry after an existing job finishes",
                Value::Null,
            ));
        }
        let now = chrono::Utc::now().to_rfc3339();
        let task = ExperimentTask {
            task_id: Uuid::new_v4(),
            task_type: task_type(&request.definition).into(),
            task_version: EXPERIMENT_TASK_VERSION,
            status: ExperimentTaskStatus::Queued,
            stage: "queued".into(),
            created_at: now.clone(),
            updated_at: now,
            request_sha256: request_hash.clone(),
            idempotency_key: request.idempotency_key.clone(),
            result_level: task_result_level,
            process_group_id: None,
            process_group_generation: None,
            result: None,
            error: None,
        };
        persist_experiment_request(&self.0.output, task.task_id, &request)?;
        persist_experiment_task(&self.0.output, &task)?;
        store
            .task_idempotency
            .insert(request.idempotency_key, (request_hash, task.task_id));
        store
            .queue
            .push_back(QueueItem::ExperimentTask(task.task_id));
        store.tasks.insert(task.task_id, task.clone());
        self.0.wake.notify_one();
        Ok((task, true))
    }
    fn inspect(&self, request: &SubmitRun) -> Preflight {
        let mut blockers = Vec::new();
        if request.kind != "etf_strategy" {
            blockers.push(blocker(
                "unsupported_kind",
                "only etf_strategy is currently supported",
            ));
        }
        let definition = match self.load_definition(request.universe_id, request.version_id) {
            Ok(value) => Some(value),
            Err(error) => {
                blockers.push(error.0);
                None
            }
        };
        let snapshot = match self.load_snapshot(request.snapshot_id) {
            Ok(value) => Some(value),
            Err(error) => {
                blockers.push(error.0);
                None
            }
        };
        if definition
            .as_ref()
            .is_some_and(|u| u.asset_scope != AssetScope::Etf)
        {
            blockers.push(blocker(
                "asset_scope_blocked",
                "stock and mixed Universe runs remain blocked",
            ));
        }
        if let (Some(definition), Some(snapshot)) = (&definition, &snapshot) {
            if definition.calculate_content_hash().ok().as_deref()
                != Some(definition.content_hash.as_str())
            {
                blockers.push(blocker(
                    "universe_hash_mismatch",
                    "published Universe content hash is invalid",
                ));
            }
            let (snapshot_symbols, legacy_etf_only) = match snapshot_symbols(snapshot) {
                Ok(value) => value,
                Err(message) => {
                    blockers.push(RunError {
                        code: "snapshot_invalid".into(),
                        message,
                        details: Value::Null,
                    });
                    (std::collections::BTreeSet::new(), false)
                }
            };
            if !legacy_etf_only && snapshot_symbols.is_empty() {
                blockers.push(blocker(
                    "no_universe_data",
                    "snapshot has no classified ETF securities",
                ));
            }
            if snapshot.rows > MAX_SNAPSHOT_ROWS {
                blockers.push(blocker(
                    "snapshot_too_large",
                    "research snapshot exceeds the 2,000,000 row job limit",
                ));
            }
            let member_types_ok = definition
                .manual_members
                .iter()
                .all(|member| member.instrument.asset_type == crate::universe::AssetType::Etf)
                && definition
                    .index_events
                    .iter()
                    .all(|event| event.instrument.asset_type == crate::universe::AssetType::Etf);
            if !member_types_ok {
                blockers.push(blocker(
                    "asset_scope_blocked",
                    "published Universe definition contains a non-ETF member",
                ));
            }
            let members = definition
                .manual_members
                .iter()
                .map(|m| &m.instrument)
                .chain(
                    definition
                        .index_events
                        .iter()
                        .map(|event| &event.instrument),
                )
                .filter_map(|instrument| {
                    crate::universe::market_bar_symbol(&instrument.exchange, &instrument.code).ok()
                })
                .collect::<std::collections::BTreeSet<_>>();
            if members.len() > MAX_UNIVERSE_INSTRUMENTS {
                blockers.push(blocker(
                    "universe_too_large",
                    "ETF run jobs are limited to 100 distinct Universe members",
                ));
            }
            if members.is_empty()
                || !members
                    .iter()
                    .any(|symbol| snapshot_symbols.contains(symbol))
            {
                blockers.push(blocker(
                    "no_universe_data",
                    "the selected ETF version has no matching securities in the fixed snapshot",
                ));
            }
            if let (Some(start), Some(end)) = (request.config.start, request.config.end) {
                let snapshot_start =
                    chrono::NaiveDate::parse_from_str(&snapshot.first_date, "%Y-%m-%d").ok();
                let snapshot_end =
                    chrono::NaiveDate::parse_from_str(&snapshot.last_date, "%Y-%m-%d").ok();
                if snapshot_start.is_none_or(|date| start < date)
                    || snapshot_end.is_none_or(|date| end > date)
                {
                    blockers.push(blocker(
                        "date_range_outside_snapshot",
                        "requested dates must be inside the fixed snapshot date range",
                    ));
                } else {
                    match data::load_bars(&snapshot.file, Some(start), Some(end)) {
                        Ok(bars) if bars.is_empty() => blockers.push(blocker(
                            "date_range_no_bars",
                            "fixed snapshot has no daily bars in the requested date range",
                        )),
                        Err(error) => blockers.push(RunError {
                            code: "snapshot_fields_missing".into(),
                            message: error.to_string(),
                            details: Value::Null,
                        }),
                        _ => {}
                    }
                }
            }
            if request.config.strategy.score_lookback() > 2500
                || request.config.strategy.top_n > 100
            {
                blockers.push(blocker(
                    "strategy_resource_limit",
                    "lookback is limited to 2500 bars and top_n to 100",
                ));
            }
            // `ready` always means trusted historical readiness. A diagnostic
            // request may still be accepted below, but must never make the
            // trusted-history gate look satisfied in preflight.
            blockers.push(RunError { code:"trusted_history_evidence_missing".into(), message:"trusted history requires independently verified run-eligibility evidence; no eligible sidecar is available".into(), details:json!({"pit_status":"unverified","execution_status_mode":"not_proven"}) });
        }
        let structural = blockers.iter().any(|e| {
            matches!(
                e.code.as_str(),
                "unsupported_kind"
                    | "asset_scope_blocked"
                    | "snapshot_asset_scope_blocked"
                    | "snapshot_invalid"
                    | "snapshot_hash_mismatch"
                    | "snapshot_identity_mismatch"
                    | "universe_hash_mismatch"
                    | "snapshot_too_large"
                    | "universe_too_large"
                    | "date_range_outside_snapshot"
                    | "date_range_no_bars"
                    | "snapshot_fields_missing"
                    | "strategy_resource_limit"
                    | "no_universe_data"
                    | "snapshot_missing"
                    | "universe_not_found"
                    | "version_not_found"
            )
        });
        let diagnostic_allowed = !structural
            && request.kind == "etf_strategy"
            && definition
                .as_ref()
                .is_some_and(|u| u.asset_scope == AssetScope::Etf)
            && snapshot.is_some();
        Preflight {
            ready: blockers.is_empty(),
            diagnostic_allowed,
            blockers,
            universe_content_hash: definition.map(|u| u.content_hash),
            snapshot_sha256: snapshot.map(|s| s.sha256),
        }
    }
    fn load_definition(
        &self,
        universe_id: Uuid,
        version_id: Uuid,
    ) -> Result<UniverseDefinition, ApiFailure> {
        let config = self.0.universes.get(universe_id).map_err(|_| {
            ApiFailure::new(
                StatusCode::NOT_FOUND,
                "universe_not_found",
                "Universe was not found",
                Value::Null,
            )
        })?;
        config
            .versions
            .into_iter()
            .find(|v| v.version_id == version_id)
            .ok_or_else(|| {
                ApiFailure::new(
                    StatusCode::NOT_FOUND,
                    "version_not_found",
                    "published Universe version was not found",
                    Value::Null,
                )
            })
    }
    fn load_snapshot(&self, id: Uuid) -> Result<SnapshotManifest, ApiFailure> {
        let path = self
            .0
            .output
            .join("snapshots")
            .join(id.to_string())
            .join("manifest.json");
        let bytes = fs::read(path).map_err(|_| {
            ApiFailure::new(
                StatusCode::NOT_FOUND,
                "snapshot_missing",
                "fixed research snapshot was not found",
                json!({"snapshot_id":id}),
            )
        })?;
        let snapshot: SnapshotManifest = serde_json::from_slice(&bytes).map_err(|e| {
            ApiFailure::new(
                StatusCode::CONFLICT,
                "snapshot_invalid",
                &e.to_string(),
                Value::Null,
            )
        })?;
        if snapshot.snapshot_id != id {
            return Err(ApiFailure::new(
                StatusCode::CONFLICT,
                "snapshot_identity_mismatch",
                "manifest snapshot_id does not match request",
                Value::Null,
            ));
        }
        data::verify_snapshot_manifest(&snapshot).map_err(|e| {
            ApiFailure::new(
                StatusCode::CONFLICT,
                "snapshot_hash_mismatch",
                &e.to_string(),
                Value::Null,
            )
        })?;
        Ok(snapshot)
    }
    fn get(&self, id: Uuid) -> Option<RunJob> {
        self.0.gate.lock().ok()?.jobs.get(&id).cloned()
    }
    fn get_experiment_task(&self, id: Uuid) -> Option<ExperimentTask> {
        self.0.gate.lock().ok()?.tasks.get(&id).cloned()
    }
    fn list(&self) -> Vec<RunJob> {
        let mut jobs = self
            .0
            .gate
            .lock()
            .map(|g| g.jobs.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        jobs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        jobs.truncate(100);
        jobs
    }
    fn list_experiment_tasks(&self) -> Vec<ExperimentTask> {
        let mut tasks = self
            .0
            .gate
            .lock()
            .map(|g| g.tasks.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        tasks.truncate(100);
        tasks
    }
    fn worker_loop(&self) {
        loop {
            {
                let mut store = self.0.gate.lock().expect("run store lock");
                while store.queue.is_empty() {
                    store = self.0.wake.wait(store).expect("run store lock")
                }
            }
            self.process_next_queued();
        }
    }
    fn process_next_queued(&self) -> bool {
        let work = {
            let mut store = self.0.gate.lock().expect("run store lock");
            let Some(item) = store.queue.pop_front() else {
                return false;
            };
            store.active = true;
            match item {
                QueueItem::LegacyRun(id) => {
                    let job = store.jobs.get_mut(&id).expect("queued job");
                    job.status = RunStatus::Running;
                    job.stage = "validating_fixed_inputs".into();
                    job.updated_at = chrono::Utc::now().to_rfc3339();
                    let _ = persist_job(&self.0.output, job);
                    QueuedWork::LegacyRun(Box::new(job.clone()))
                }
                QueueItem::ExperimentTask(id) => {
                    let task = store.tasks.get_mut(&id).expect("queued experiment task");
                    task.status = ExperimentTaskStatus::Running;
                    task.stage = if task.task_type == "prajna_momentum" {
                        "starting_momentum_cli"
                    } else {
                        "starting_experiment_cli"
                    }
                    .into();
                    task.updated_at = chrono::Utc::now().to_rfc3339();
                    let _ = persist_experiment_task(&self.0.output, task);
                    QueuedWork::ExperimentTask(Box::new(task.clone()))
                }
            }
        };

        match work {
            QueuedWork::LegacyRun(job) => {
                let result = self.execute(&job);
                let mut store = self.0.gate.lock().expect("run store lock");
                let target = store.jobs.get_mut(&job.run_id).expect("active job");
                match result {
                    Ok(id) => {
                        target.status = RunStatus::Succeeded;
                        target.stage = "complete".into();
                        target.experiment_id = Some(id);
                        target.error = None
                    }
                    Err(error) => fail_job(target, &error.code, &error.message),
                }
                target.updated_at = chrono::Utc::now().to_rfc3339();
                let _ = persist_job(&self.0.output, target);
            }
            QueuedWork::ExperimentTask(task) => {
                let result = self.execute_experiment_task(&task);
                let mut store = self.0.gate.lock().expect("run store lock");
                let target = store
                    .tasks
                    .get_mut(&task.task_id)
                    .expect("active experiment task");
                match result {
                    Ok(result) => {
                        target.status = ExperimentTaskStatus::Succeeded;
                        target.stage = "complete".into();
                        target.result = Some(result);
                        target.process_group_id = None;
                        target.process_group_generation = None;
                        target.error = None;
                    }
                    Err(error) => {
                        fail_experiment_task(target, error);
                    }
                }
                target.updated_at = chrono::Utc::now().to_rfc3339();
                let _ = persist_experiment_task(&self.0.output, target);
            }
        }
        let mut store = self.0.gate.lock().expect("run store lock");
        store.active = false;
        self.0.wake.notify_all();
        true
    }
    fn execute_experiment_task(&self, task: &ExperimentTask) -> Result<Value, RunError> {
        let directory = experiment_task_dir(&self.0.output, task.task_id);
        let request = read_experiment_request(&directory).ok_or_else(|| {
            run_error(
                "task_request_missing",
                "persisted experiment task request is missing or invalid",
            )
        })?;
        validate_experiment_task_request(&request).map_err(|error| error.0)?;
        if task.task_type != task_type(&request.definition) {
            return Err(run_error(
                "task_type_changed",
                "task type does not match its frozen definition",
            ));
        }
        if canonical_experiment_task_hash(&request).map_err(|error| error.0)? != task.request_sha256
        {
            return Err(run_error(
                "task_request_changed",
                "persisted experiment task definition does not match its frozen identity",
            ));
        }
        let definition_path = directory.join("experiment.json");
        let definition_bytes = fs::read(&definition_path)
            .map_err(|error| run_error("task_definition_missing", &error.to_string()))?;
        let definition: Value = serde_json::from_slice(&definition_bytes)
            .map_err(|error| run_error("task_definition_invalid", &error.to_string()))?;
        if definition != request.definition {
            return Err(run_error(
                "task_definition_changed",
                "persisted CLI definition does not match the frozen task request",
            ));
        }

        if task.task_type == "prajna_momentum" {
            return self.execute_experiment_cli(
                task,
                &directory,
                vec![
                    "--lake".into(),
                    self.0.output.as_os_str().to_owned(),
                    "--input".into(),
                    definition_path.as_os_str().to_owned(),
                ],
                "momentum.stdout",
            );
        }
        let result_level = request
            .result_level
            .as_deref()
            .unwrap_or(DEFAULT_RESULT_LEVEL);
        let run_args = vec![
            OsString::from("run"),
            definition_path.as_os_str().to_owned(),
            OsString::from("--lake"),
            self.0.output.as_os_str().to_owned(),
            OsString::from("--level"),
            OsString::from(result_level),
        ];
        if result_level == "standard" {
            // Prefer promoting the current Execution: this both upgrades an
            // existing Summary result and makes an already-Standard result a
            // no-op. Only fall back to executing when promote reports that no
            // current Execution exists (exit 6); an existing but unreadable or
            // partially replayable Execution must not be retried as `run`.
            let promoted = self.execute_experiment_cli(
                task,
                &directory,
                vec![
                    OsString::from("promote"),
                    definition_path.into_os_string(),
                    OsString::from("--lake"),
                    self.0.output.as_os_str().to_owned(),
                    OsString::from("--level"),
                    OsString::from("standard"),
                ],
                "cli-promote.stdout",
            );
            match promoted {
                Ok(result) => Ok(result),
                Err(error) => {
                    let _ = fs::remove_file(directory.join("cli-promote.stdout"));
                    if promote_missing_execution(&error) {
                        self.execute_experiment_cli(task, &directory, run_args, "cli.stdout")
                    } else {
                        Err(error)
                    }
                }
            }
        } else {
            self.execute_experiment_cli(task, &directory, run_args, "cli.stdout")
        }
    }
    fn execute_experiment_cli(
        &self,
        task: &ExperimentTask,
        directory: &FsPath,
        args: Vec<OsString>,
        stdout_name: &str,
    ) -> Result<Value, RunError> {
        let stdout_path = directory.join(stdout_name);
        let stdout = fs::File::create(&stdout_path)
            .map_err(|error| run_error("task_output_create_failed", &error.to_string()))?;
        let process_generation = Uuid::new_v4();
        let cli = if task.task_type == "prajna_momentum" {
            std::env::var_os("PRAJNA_MOMENTUM_CLI")
                .map(PathBuf::from)
                .unwrap_or_else(|| self.0.experiment_cli.with_file_name("prajna-momentum"))
        } else {
            self.0.experiment_cli.clone()
        };
        let mut command = Command::new(cli);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::piped());
        let mut process = ExperimentProcessGroup::spawn_with_process_inspector(
            &mut command,
            task.task_id,
            process_generation,
            self.0.process_inspector.clone(),
        )
        .map_err(|error| run_error("experiment_cli_spawn_failed", &error.to_string()))?;
        let group_id = process.process_group_id();
        let stderr = process.take_stderr().ok_or_else(|| {
            run_error(
                "experiment_cli_stderr_missing",
                "CLI stderr pipe is missing",
            )
        })?;
        let stop_stderr_capture = Arc::new(AtomicBool::new(false));
        let reader_stop_signal = Arc::clone(&stop_stderr_capture);
        let stderr_reader = thread::spawn(move || read_bounded_stderr(stderr, reader_stop_signal));
        self.set_experiment_process_group(task.task_id, Some(group_id), Some(process_generation))?;
        let wait = match process.wait() {
            Ok(wait) => wait,
            Err(error) => {
                stop_stderr_capture.store(true, Ordering::Release);
                drop(stderr_reader);
                return Err(process_group_cleanup_error(error));
            }
        };
        stop_stderr_capture.store(true, Ordering::Release);
        let stderr = match stderr_reader.join() {
            Ok(Ok(stderr)) => Ok(stderr),
            Ok(Err(error)) => Err(run_error(
                "experiment_cli_stderr_capture_failed",
                &error.to_string(),
            )),
            Err(_) => Err(run_error(
                "experiment_cli_stderr_capture_failed",
                "CLI stderr capture worker stopped unexpectedly",
            )),
        };
        if let Some(cleanup_error) = wait.cleanup_error {
            return Err(process_group_cleanup_error_with_cli(
                cleanup_error,
                &wait.status,
                &stderr,
            ));
        }
        self.set_experiment_process_group(task.task_id, None, None)?;
        let stderr = stderr?;
        if let Some(error) = experiment_cli_error(&wait.status, &stderr) {
            if task.task_type == "prajna_momentum" {
                if let Ok(bytes) = fs::read(&stdout_path) {
                    if let Ok(envelope) = serde_json::from_slice::<Value>(&bytes) {
                        if let (Some(code), Some(message)) = (
                            envelope["error"]["code"].as_str(),
                            envelope["error"]["message"].as_str(),
                        ) {
                            return Err(run_error(code, message));
                        }
                    }
                }
            }
            return Err(error);
        }
        let bytes = fs::read(&stdout_path)
            .map_err(|error| run_error("experiment_cli_output_missing", &error.to_string()))?;
        let mut result: Value = serde_json::from_slice(&bytes)
            .map_err(|error| run_error("experiment_cli_output_invalid", &error.to_string()))?;
        if task.task_type == "prajna_momentum" {
            let id = result["data"]["id"].as_str().ok_or_else(|| {
                run_error(
                    "momentum_result_missing",
                    "momentum CLI did not return a saved report",
                )
            })?;
            result = json!({"report_id":id});
        } else if result.get("exp").and_then(Value::as_str).is_none()
            || result.get("exe").and_then(Value::as_str).is_none()
        {
            return Err(run_error(
                "experiment_cli_identity_missing",
                "prajna-experiment output must contain string exp and exe identities",
            ));
        }
        let result_bytes = serde_json::to_vec_pretty(&result)
            .map_err(|error| run_error("experiment_cli_output_invalid", &error.to_string()))?;
        atomic_write(&directory.join("result.json"), &result_bytes)
            .map_err(|error| run_error("task_result_persist_failed", &error.to_string()))?;
        let _ = fs::remove_file(stdout_path);
        Ok(result)
    }
    fn set_experiment_process_group(
        &self,
        task_id: Uuid,
        process_group_id: Option<u32>,
        process_group_generation: Option<Uuid>,
    ) -> Result<(), RunError> {
        let mut store = self
            .0
            .gate
            .lock()
            .map_err(|_| run_error("task_store_lock_failed", "task store lock poisoned"))?;
        let task = store
            .tasks
            .get_mut(&task_id)
            .ok_or_else(|| run_error("task_missing", "experiment task is missing"))?;
        task.process_group_id = process_group_id;
        task.process_group_generation = process_group_generation;
        task.stage = if process_group_id.is_some() {
            if task.task_type == "prajna_momentum" {
                "running_momentum_cli"
            } else {
                "running_experiment_cli"
            }
            .into()
        } else {
            if task.task_type == "prajna_momentum" {
                "reading_momentum_report"
            } else {
                "reading_experiment_result"
            }
            .into()
        };
        task.updated_at = chrono::Utc::now().to_rfc3339();
        persist_experiment_task(&self.0.output, task).map_err(|error| error.0)
    }
    fn execute(&self, job: &RunJob) -> Result<Uuid, RunError> {
        let request = read_request(&self.0.output.join("runs").join(job.run_id.to_string()))
            .ok_or_else(|| {
                run_error(
                    "request_payload_missing",
                    "persisted request payload is missing",
                )
            })?;
        let preflight = self.inspect(&request);
        if !request.diagnostic_mode || !preflight.diagnostic_allowed {
            if let Some(blocker) = preflight.blockers.first().cloned() {
                return Err(blocker);
            }
            return Err(RunError {
                code: "capability_changed".into(),
                message: "run prerequisites changed or trusted history remains blocked".into(),
                details: json!({"blockers":preflight.blockers}),
            });
        }
        let definition = self
            .load_definition(job.universe_id, job.version_id)
            .map_err(|e| e.0)?;
        if definition.content_hash != job.universe_content_hash {
            return Err(run_error(
                "universe_version_changed",
                "pinned Universe version hash changed",
            ));
        }
        let snapshot = self.load_snapshot(job.snapshot_id).map_err(|e| e.0)?;
        if snapshot.sha256 != job.snapshot_sha256 {
            return Err(run_error(
                "snapshot_changed",
                "pinned research snapshot hash changed",
            ));
        }
        let bars = data::load_bars(&snapshot.file, None, None)
            .map_err(|e| run_error("snapshot_read_failed", &e.to_string()))?;
        let benchmark = snapshot
            .benchmark_file
            .as_ref()
            .map(|p| data::load_bars(p, request.config.start, request.config.end))
            .transpose()
            .map_err(|e| run_error("benchmark_read_failed", &e.to_string()))?
            .unwrap_or_default();
        let mut result = runner::run_experiment_with_universe(
            request.config,
            snapshot,
            &bars,
            &benchmark,
            &definition,
            false,
        )
        .map_err(|e| run_error("run_failed", &e.to_string()))?;
        let execution_mode = result
            .backtest
            .execution_status_mode
            .as_deref()
            .unwrap_or("unknown");
        result.run_mode = Some(format!("diagnostic_{execution_mode}"));
        result.assumptions.push(format!("DIAGNOSTIC RUN ONLY: execution_status_mode={execution_mode}; retrospective static membership; zero dividends; raw prices. Do not interpret metrics as trusted historical strategy performance."));
        result
            .save_atomic(&self.0.output)
            .map_err(|e| run_error("experiment_persist_failed", &e.to_string()))?;
        Ok(result.experiment_id)
    }
}

#[derive(Debug)]
struct ApiFailure(RunError, StatusCode);
impl ApiFailure {
    fn new(status: StatusCode, code: &str, message: &str, details: Value) -> Self {
        Self(
            RunError {
                code: code.into(),
                message: message.into(),
                details,
            },
            status,
        )
    }
    fn internal(message: &str) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_error",
            message,
            Value::Null,
        )
    }
    fn from_error(error: impl std::fmt::Display) -> Self {
        Self::internal(&error.to_string())
    }
}
impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        let mut response = (self.1, Json(json!({"error":self.0}))).into_response();
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            "no-store".parse().expect("fixed header"),
        );
        response
    }
}
async fn preflight(State(api): State<RunJobApi>, Json(request): Json<SubmitRun>) -> Response {
    match validate_shape(&request) {
        Ok(()) => Json(api.inspect(&request)).into_response(),
        Err(e) => e.into_response(),
    }
}
async fn list_runs(State(api): State<RunJobApi>) -> Response {
    Json(json!({"items":api.list()})).into_response()
}
async fn list_experiment_tasks(State(api): State<RunJobApi>) -> Response {
    Json(json!({"items":api.list_experiment_tasks()})).into_response()
}
async fn submit_experiment_task(
    State(api): State<RunJobApi>,
    Json(request): Json<SubmitExperimentTask>,
) -> Response {
    match api.submit_experiment_task(request) {
        Ok((task, created)) => (
            if created {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            },
            Json(json!({"task":task,"created":created})),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}
async fn get_experiment_task(State(api): State<RunJobApi>, Path(id): Path<String>) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return ApiFailure::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid experiment task id",
            Value::Null,
        )
        .into_response();
    };
    match api.get_experiment_task(id) {
        Some(task) => Json(task).into_response(),
        None => ApiFailure::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "experiment task not found",
            Value::Null,
        )
        .into_response(),
    }
}
async fn get_experiment_result(State(api): State<RunJobApi>, Path(id): Path<String>) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return ApiFailure::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid experiment task id",
            Value::Null,
        )
        .into_response();
    };
    let Some(task) = api.get_experiment_task(id) else {
        return ApiFailure::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "experiment task not found",
            Value::Null,
        )
        .into_response();
    };
    if !matches!(task.status, ExperimentTaskStatus::Succeeded) {
        return ApiFailure::new(
            StatusCode::CONFLICT,
            "task_not_completed",
            "experiment task has not succeeded",
            json!({"status": task.status}),
        )
        .into_response();
    }
    let Some(result) = task.result.as_ref() else {
        return ApiFailure::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "task_result_missing",
            "succeeded experiment task has no persisted result identity",
            Value::Null,
        )
        .into_response();
    };
    let (Some(exp), Some(exe)) = (
        result.get("exp").and_then(Value::as_str),
        result.get("exe").and_then(Value::as_str),
    ) else {
        return ApiFailure::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "task_result_invalid",
            "persisted experiment result has no string exp and exe identity",
            Value::Null,
        )
        .into_response();
    };
    let (exp, exe) = (exp.to_owned(), exe.to_owned());
    let cli = api.0.experiment_cli.clone();
    let lake = api.0.output.clone();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(cli)
            .arg("result")
            .arg(exp)
            .arg(exe)
            .arg("--lake")
            .arg(lake)
            .output()
    })
    .await;
    let output = match output {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return ApiFailure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "experiment_result_unavailable",
                "prajna-experiment result reader could not be executed",
                json!({"cause": error.to_string()}),
            )
            .into_response();
        }
        Err(error) => {
            return ApiFailure::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "experiment_result_join_failed",
                "prajna-experiment result reader stopped unexpectedly",
                json!({"cause": error.to_string()}),
            )
            .into_response();
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_truncated = stderr.chars().count() > 4096;
        let stderr = stderr.chars().take(4096).collect::<String>();
        return ApiFailure::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "experiment_result_failed",
            "prajna-experiment could not read the persisted result",
            json!({
                "exit_code": output.status.code(),
                "stderr": stderr,
                "stderr_truncated": stderr_truncated,
            }),
        )
        .into_response();
    }
    let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) else {
        return ApiFailure::new(
            StatusCode::BAD_GATEWAY,
            "invalid_result_response",
            "prajna-experiment result returned invalid JSON",
            Value::Null,
        )
        .into_response();
    };
    if value.get("schema_version").and_then(Value::as_u64) != Some(1)
        || !value.get("data").is_some_and(Value::is_object)
    {
        return ApiFailure::new(
            StatusCode::BAD_GATEWAY,
            "invalid_result_response",
            "prajna-experiment result returned an unsupported envelope",
            Value::Null,
        )
        .into_response();
    }
    // Preserve the CLI envelope and stored decimal strings byte-for-byte.
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(output.stdout))
        .expect("fixed response headers")
}
async fn list_snapshots(State(api): State<RunJobApi>) -> Response {
    let directory = api.0.output.join("snapshots");
    let mut items = Vec::new();
    let Ok(entries) = fs::read_dir(directory) else {
        return Json(json!({"items":items})).into_response();
    };
    for entry in entries.flatten() {
        let path = entry.path().join("manifest.json");
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(manifest) = serde_json::from_slice::<SnapshotManifest>(&bytes) else {
            continue;
        };
        items.push(json!({"snapshot_id":manifest.snapshot_id,"sha256":manifest.sha256,"created_at":manifest.created_at,"first_date":manifest.first_date,"last_date":manifest.last_date,"rows":manifest.rows,"symbols":manifest.symbols,"hash_verified_on_submit":false,"execution_status_mode":"checked_on_preflight"}));
    }
    items.sort_by(|a, b| b["created_at"].as_str().cmp(&a["created_at"].as_str()));
    items.truncate(100);
    Json(json!({"items":items})).into_response()
}
async fn submit(State(api): State<RunJobApi>, Json(request): Json<SubmitRun>) -> Response {
    match api.submit(request) {
        Ok((job, created)) => (
            if created {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            },
            Json(json!({"run":job,"created":created})),
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}
async fn get_run(State(api): State<RunJobApi>, Path(id): Path<String>) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return ApiFailure::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid run id",
            Value::Null,
        )
        .into_response();
    };
    match api.get(id) {
        Some(job) => Json(job).into_response(),
        None => ApiFailure::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "run job not found",
            Value::Null,
        )
        .into_response(),
    }
}
fn validate_shape(request: &SubmitRun) -> Result<(), ApiFailure> {
    if request.kind != "etf_strategy" {
        return Err(ApiFailure::new(
            StatusCode::BAD_REQUEST,
            "unsupported_kind",
            "kind must be etf_strategy",
            Value::Null,
        ));
    }
    if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 128 {
        return Err(ApiFailure::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "idempotency_key must contain 1 to 128 characters",
            Value::Null,
        ));
    }
    request.config.validate().map_err(|e| {
        ApiFailure::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &e.to_string(),
            Value::Null,
        )
    })?;
    let (Some(start), Some(end)) = (request.config.start, request.config.end) else {
        return Err(ApiFailure::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "both config.start and config.end are required",
            Value::Null,
        ));
    };
    if start > end || (end - start).num_days() > MAX_DATE_SPAN_DAYS {
        return Err(ApiFailure::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "date range must be ordered and no longer than 3662 inclusive dates",
            Value::Null,
        ));
    }
    if request
        .config
        .universe
        .as_ref()
        .is_none_or(|s| s.universe_id != request.universe_id || s.version_id != request.version_id)
    {
        return Err(ApiFailure::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "config.universe must match request universe_id and version_id",
            Value::Null,
        ));
    }
    Ok(())
}
fn canonical_hash(request: &SubmitRun) -> Result<String, ApiFailure> {
    let value = json!({"kind":request.kind,"universe_id":request.universe_id,"version_id":request.version_id,"snapshot_id":request.snapshot_id,"config":request.config,"diagnostic_mode":request.diagnostic_mode});
    serde_json::to_vec(&value)
        .map(|b| format!("{:x}", Sha256::digest(b)))
        .map_err(ApiFailure::from_error)
}
fn validate_experiment_task_request(request: &SubmitExperimentTask) -> Result<(), ApiFailure> {
    normalized_result_level(request.result_level.as_deref())?;
    if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 128 {
        return Err(ApiFailure::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "idempotency_key must contain 1 to 128 characters",
            Value::Null,
        ));
    }
    if !request.definition.is_object() {
        return Err(ApiFailure::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "experiment definition must be a JSON object",
            Value::Null,
        ));
    }
    Ok(())
}
fn task_type(definition: &Value) -> &'static str {
    if definition["op"] == "submit" {
        "prajna_momentum"
    } else {
        EXPERIMENT_TASK_TYPE
    }
}
fn canonical_experiment_task_hash(request: &SubmitExperimentTask) -> Result<String, ApiFailure> {
    let mut value = json!({
        "task_type": task_type(&request.definition),
        "task_version": EXPERIMENT_TASK_VERSION,
        "definition": &request.definition,
    });
    if let Some(level) = &request.result_level {
        if !is_summary_level(level) {
            value["result_level"] = json!(level);
        }
    }
    serde_json::to_vec(&value)
        .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
        .map_err(ApiFailure::from_error)
}
fn promote_missing_execution(error: &RunError) -> bool {
    error.code == "experiment_cli_failed"
        && error.details.get("exit_code").and_then(Value::as_i64) == Some(6)
}

fn blocker(code: &str, message: &str) -> RunError {
    RunError {
        code: code.into(),
        message: message.into(),
        details: Value::Null,
    }
}
fn snapshot_symbols(
    snapshot: &SnapshotManifest,
) -> Result<(std::collections::BTreeSet<String>, bool), String> {
    match data::load_snapshot_security_directory(&snapshot.file) {
        Ok(items) => {
            if items.iter().any(|item| item.asset_type != "ETF") {
                return Err("research snapshot contains non-ETF assets".into());
            }
            Ok((items.into_iter().map(|item| item.symbol).collect(), false))
        }
        Err(directory_error)
            if snapshot
                .selection_rule
                .starts_with("one ETF row per symbol/date; Tencent preferred over TDX") =>
        {
            // Batch 1 ETF manifests predate identity/is_preheat columns. Their frozen
            // extraction rule proves ETF-only; this compatibility reader is diagnostic-only.
            let escaped = snapshot.file.to_string_lossy().replace('\'', "''");
            let connection = Connection::open_in_memory().map_err(|error| error.to_string())?;
            let mut statement = connection
                .prepare(&format!(
                    "SELECT DISTINCT symbol FROM read_parquet('{escaped}') ORDER BY symbol"
                ))
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?;
            let symbols = rows
                .collect::<std::result::Result<std::collections::BTreeSet<_>, _>>()
                .map_err(|error| error.to_string())?;
            if symbols.is_empty() {
                return Err(format!(
                    "legacy ETF snapshot has no symbols ({directory_error})"
                ));
            }
            Ok((symbols, true))
        }
        Err(error) => Err(error.to_string()),
    }
}
fn run_error(code: &str, message: &str) -> RunError {
    blocker(code, message)
}
fn fail_job(job: &mut RunJob, code: &str, message: &str) {
    job.status = RunStatus::Failed;
    job.stage = "failed".into();
    job.updated_at = chrono::Utc::now().to_rfc3339();
    job.error = Some(blocker(code, message))
}
fn fail_experiment_task(task: &mut ExperimentTask, error: RunError) {
    task.status = ExperimentTaskStatus::Failed;
    task.stage = "failed".into();
    if !error.code.starts_with("process_group_") {
        task.process_group_id = None;
        task.process_group_generation = None;
    }
    task.updated_at = chrono::Utc::now().to_rfc3339();
    task.error = Some(error);
}
fn interrupt_task(task: &mut ExperimentTask, message: &str) {
    task.status = ExperimentTaskStatus::Interrupted;
    task.stage = "interrupted".into();
    task.process_group_id = None;
    task.process_group_generation = None;
    task.updated_at = chrono::Utc::now().to_rfc3339();
    task.error = Some(blocker("worker_interrupted", message));
}
fn read_request(directory: &FsPath) -> Option<SubmitRun> {
    serde_json::from_slice(&fs::read(directory.join("request.json")).ok()?).ok()
}
fn experiment_task_dir(root: &FsPath, task_id: Uuid) -> PathBuf {
    root.join("tasks").join(task_id.to_string())
}
fn read_experiment_request(directory: &FsPath) -> Option<SubmitExperimentTask> {
    serde_json::from_slice(&fs::read(directory.join("request.json")).ok()?).ok()
}
fn persist_experiment_request(
    root: &FsPath,
    task_id: Uuid,
    request: &SubmitExperimentTask,
) -> Result<(), ApiFailure> {
    let dir = experiment_task_dir(root, task_id);
    fs::create_dir_all(&dir).map_err(ApiFailure::from_error)?;
    atomic_write(
        &dir.join("request.json"),
        &serde_json::to_vec_pretty(request).map_err(ApiFailure::from_error)?,
    )
    .map_err(ApiFailure::from_error)?;
    atomic_write(
        &dir.join("experiment.json"),
        &serde_json::to_vec_pretty(&request.definition).map_err(ApiFailure::from_error)?,
    )
    .map_err(ApiFailure::from_error)
}
fn persist_experiment_task(root: &FsPath, task: &ExperimentTask) -> Result<(), ApiFailure> {
    let dir = experiment_task_dir(root, task.task_id);
    fs::create_dir_all(&dir).map_err(ApiFailure::from_error)?;
    atomic_write(
        &dir.join("task.json"),
        &serde_json::to_vec_pretty(task).map_err(ApiFailure::from_error)?,
    )
    .map_err(ApiFailure::from_error)
}
fn persist_request(root: &FsPath, id: Uuid, request: &SubmitRun) -> Result<(), ApiFailure> {
    let dir = root.join("runs").join(id.to_string());
    fs::create_dir_all(&dir).map_err(ApiFailure::from_error)?;
    atomic_write(
        &dir.join("request.json"),
        &serde_json::to_vec_pretty(request).map_err(ApiFailure::from_error)?,
    )
    .map_err(ApiFailure::from_error)
}
fn persist_job(root: &FsPath, job: &RunJob) -> Result<(), ApiFailure> {
    let dir = root.join("runs").join(job.run_id.to_string());
    fs::create_dir_all(&dir).map_err(ApiFailure::from_error)?;
    atomic_write(
        &dir.join("run.json"),
        &serde_json::to_vec_pretty(job).map_err(ApiFailure::from_error)?,
    )
    .map_err(ApiFailure::from_error)
}
fn atomic_write(path: &FsPath, bytes: &[u8]) -> std::io::Result<()> {
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let mut file = fs::File::create(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temp, path)
}

struct BoundedStderr {
    text: String,
    truncated: bool,
    capture_incomplete: bool,
}

#[derive(Debug)]
enum ProcessGroupCleanupError {
    OwnershipUnverified,
    Io(std::io::Error),
}

impl From<std::io::Error> for ProcessGroupCleanupError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl ProcessGroupCleanupError {
    #[cfg(test)]
    fn kind(&self) -> std::io::ErrorKind {
        match self {
            Self::OwnershipUnverified => std::io::ErrorKind::PermissionDenied,
            Self::Io(error) => error.kind(),
        }
    }
}

fn process_group_cleanup_error(error: ProcessGroupCleanupError) -> RunError {
    match error {
        ProcessGroupCleanupError::OwnershipUnverified => blocker(
            "process_group_ownership_unverified",
            "process group task ownership or generation could not be verified; cleanup was refused",
        ),
        ProcessGroupCleanupError::Io(error) => blocker(
            "process_group_cleanup_failed",
            &format!("could not clean up experiment process group: {error}"),
        ),
    }
}

fn cli_result_details(status: &ExitStatus, stderr: &BoundedStderr) -> Value {
    let mut details = json!({
        "success": status.success(),
        "exit_code": status.code(),
        "stderr": stderr.text,
        "stderr_truncated": stderr.truncated,
        "stderr_capture_incomplete": stderr.capture_incomplete,
    });
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(status) {
        details["signal"] = json!(signal);
    }
    details
}

fn experiment_cli_error(status: &ExitStatus, stderr: &BoundedStderr) -> Option<RunError> {
    if status.success() {
        return None;
    }
    Some(RunError {
        code: "experiment_cli_failed".into(),
        message: "prajna-experiment exited unsuccessfully".into(),
        details: cli_result_details(status, stderr),
    })
}

fn process_group_cleanup_error_with_cli(
    error: ProcessGroupCleanupError,
    status: &ExitStatus,
    stderr: &Result<BoundedStderr, RunError>,
) -> RunError {
    let mut cleanup_error = process_group_cleanup_error(error);
    match stderr {
        Ok(stderr) => {
            if let Some(cli_error) = experiment_cli_error(status, stderr) {
                cleanup_error.details = json!({"cli_error": cli_error});
            } else {
                cleanup_error.details = json!({
                    "cli_result": cli_result_details(status, stderr),
                });
            }
        }
        Err(capture_error) => {
            let mut details = json!({
                "exit_code": status.code(),
                "stderr_capture_error": {
                    "code": capture_error.code,
                    "message": capture_error.message,
                },
            });
            #[cfg(unix)]
            if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(status) {
                details["signal"] = json!(signal);
            }
            cleanup_error.details = details;
        }
    }
    cleanup_error
}

fn preserve_cli_error_on_cleanup(cleanup_error: &mut RunError, previous: Option<&RunError>) {
    if let Some(cli_error) = previous.and_then(|error| error.details.get("cli_error")) {
        cleanup_error.details = json!({"cli_error": cli_error});
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn read_bounded_stderr(
    mut stderr: ChildStderr,
    stop_requested: Arc<AtomicBool>,
) -> std::io::Result<BoundedStderr> {
    let flags = fcntl_getfl(&stderr).map_err(std::io::Error::from)?;
    fcntl_setfl(&stderr, flags | OFlags::NONBLOCK).map_err(std::io::Error::from)?;
    let mut retained = Vec::with_capacity(MAX_CLI_STDERR_BYTES);
    let mut buffer = [0_u8; 4096];
    let mut truncated = false;
    let mut capture_incomplete = false;
    let mut stop_deadline = None;
    loop {
        let now = Instant::now();
        if stop_deadline.is_none() && stop_requested.load(Ordering::Acquire) {
            let flags = fcntl_getfl(&stderr).map_err(std::io::Error::from)?;
            fcntl_setfl(&stderr, flags | OFlags::NONBLOCK).map_err(std::io::Error::from)?;
            stop_deadline = Some(now + STDERR_CAPTURE_STOP_TIMEOUT);
        }
        if stop_deadline.is_some_and(|deadline| now >= deadline) {
            capture_incomplete = true;
            break;
        }
        match stderr.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let remaining = MAX_CLI_STDERR_BYTES.saturating_sub(retained.len());
                let keep = remaining.min(read);
                retained.extend_from_slice(&buffer[..keep]);
                truncated |= keep < read;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(STDERR_CAPTURE_POLL_INTERVAL);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(BoundedStderr {
        text: String::from_utf8_lossy(&retained).into_owned(),
        truncated,
        capture_incomplete,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn read_bounded_stderr(
    mut stderr: ChildStderr,
    _stop_requested: Arc<AtomicBool>,
) -> std::io::Result<BoundedStderr> {
    let mut retained = Vec::with_capacity(MAX_CLI_STDERR_BYTES);
    let mut buffer = [0_u8; 4096];
    let mut truncated = false;
    loop {
        let read = stderr.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = MAX_CLI_STDERR_BYTES.saturating_sub(retained.len());
        let keep = remaining.min(read);
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok(BoundedStderr {
        text: String::from_utf8_lossy(&retained).into_owned(),
        truncated,
        capture_incomplete: false,
    })
}

struct ExperimentProcessGroup {
    child: Child,
    process_group_id: u32,
    task_id: Uuid,
    process_generation: Uuid,
    process_inspector: PathBuf,
    finished: bool,
}

struct ProcessGroupWait {
    status: ExitStatus,
    cleanup_error: Option<ProcessGroupCleanupError>,
}

impl ExperimentProcessGroup {
    #[cfg(test)]
    fn spawn(
        command: &mut Command,
        task_id: Uuid,
        process_generation: Uuid,
    ) -> std::io::Result<Self> {
        Self::spawn_with_process_inspector(
            command,
            task_id,
            process_generation,
            PathBuf::from("ps"),
        )
    }

    fn spawn_with_process_inspector(
        command: &mut Command,
        task_id: Uuid,
        process_generation: Uuid,
        process_inspector: PathBuf,
    ) -> std::io::Result<Self> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        command
            .process_group(0)
            .env("PRAJNA_EXPERIMENT_TASK_ID", task_id.to_string())
            .env(
                "PRAJNA_EXPERIMENT_PROCESS_GENERATION",
                process_generation.to_string(),
            );
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "experiment task process groups require macOS or Linux",
        ));
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let child = command.spawn()?;
            let process_group_id = child.id();
            Ok(Self {
                child,
                process_group_id,
                task_id,
                process_generation,
                process_inspector,
                finished: false,
            })
        }
    }

    fn process_group_id(&self) -> u32 {
        self.process_group_id
    }

    fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    fn wait(&mut self) -> Result<ProcessGroupWait, ProcessGroupCleanupError> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let status = self.child.wait().map_err(ProcessGroupCleanupError::Io)?;
            let cleanup_error = terminate_process_group(
                self.process_group_id,
                self.task_id,
                Some(self.process_generation),
                &self.process_inspector,
            )
            .err();
            self.finished = cleanup_error.is_none();
            Ok(ProcessGroupWait {
                status,
                cleanup_error,
            })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(ProcessGroupCleanupError::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "experiment task process groups require macOS or Linux",
            )))
        }
    }

    #[cfg(test)]
    fn terminate(&mut self) -> Result<ExitStatus, ProcessGroupCleanupError> {
        terminate_process_group(
            self.process_group_id,
            self.task_id,
            Some(self.process_generation),
            &self.process_inspector,
        )?;
        let status = self.child.wait()?;
        self.finished = true;
        Ok(status)
    }
}

impl Drop for ExperimentProcessGroup {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let _ = terminate_process_group(
            self.process_group_id,
            self.task_id,
            Some(self.process_generation),
            &self.process_inspector,
        );
        let _ = self.child.wait();
        self.finished = true;
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn terminate_process_group(
    process_group_id: u32,
    task_id: Uuid,
    process_generation: Option<Uuid>,
    process_inspector: &FsPath,
) -> Result<(), ProcessGroupCleanupError> {
    if process_group_id <= 1 || process_group_id > i32::MAX as u32 {
        return Err(ProcessGroupCleanupError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid experiment process group id",
        )));
    }
    let ownership = verify_process_group_owner(
        process_group_id,
        task_id,
        process_generation,
        process_inspector,
    )
    .map_err(|_| ProcessGroupCleanupError::OwnershipUnverified)?;
    match ownership {
        ProcessGroupOwnership::Absent => return Ok(()),
        ProcessGroupOwnership::Owned => {}
        ProcessGroupOwnership::Unverified => {
            return Err(ProcessGroupCleanupError::OwnershipUnverified);
        }
    }
    let process_group = Pid::from_raw(process_group_id as i32).ok_or_else(|| {
        ProcessGroupCleanupError::Io(std::io::Error::from(std::io::ErrorKind::InvalidInput))
    })?;
    match kill_process_group(process_group, Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(ProcessGroupCleanupError::Io(error.into())),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessGroupOwnership {
    Absent,
    Owned,
    Unverified,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn verify_process_group_owner(
    process_group_id: u32,
    task_id: Uuid,
    process_generation: Option<Uuid>,
    process_inspector: &FsPath,
) -> std::io::Result<ProcessGroupOwnership> {
    let members = process_group_members(process_group_id, process_inspector)?;
    if members.is_empty() {
        return Ok(ProcessGroupOwnership::Absent);
    }
    let Some(process_generation) = process_generation else {
        return Ok(ProcessGroupOwnership::Unverified);
    };
    for pid in members {
        let mut command = Command::new(process_inspector);
        #[cfg(target_os = "macos")]
        command.args(["-Eww", "-p"]);
        #[cfg(target_os = "linux")]
        command.args(["eww", "-p"]);
        let output = match command.arg(pid.to_string()).output() {
            Ok(output) if output.status.success() => output,
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let process = String::from_utf8_lossy(&output.stdout);
        if process_listing_matches_owner(&process, task_id, process_generation) {
            return Ok(ProcessGroupOwnership::Owned);
        }
    }
    if process_group_members(process_group_id, process_inspector)?.is_empty() {
        Ok(ProcessGroupOwnership::Absent)
    } else {
        Ok(ProcessGroupOwnership::Unverified)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn process_listing_matches_owner(listing: &str, task_id: Uuid, process_generation: Uuid) -> bool {
    let task_marker = format!("PRAJNA_EXPERIMENT_TASK_ID={task_id}");
    let generation_marker = format!("PRAJNA_EXPERIMENT_PROCESS_GENERATION={process_generation}");
    let mut has_task = false;
    let mut has_generation = false;
    for field in listing.split_whitespace() {
        has_task |= field == task_marker;
        has_generation |= field == generation_marker;
    }
    has_task && has_generation
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn process_group_members(
    process_group_id: u32,
    process_inspector: &FsPath,
) -> std::io::Result<Vec<u32>> {
    let output = Command::new(process_inspector)
        .args(["-axo", "pid=,pgid="])
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "could not inspect process groups; refusing cleanup",
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            let pid = columns.next()?.parse::<u32>().ok()?;
            let pgid = columns.next()?.parse::<u32>().ok()?;
            (pgid == process_group_id).then_some(pid)
        })
        .collect())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn terminate_process_group(
    _process_group_id: u32,
    _task_id: Uuid,
    _process_generation: Option<Uuid>,
    _process_inspector: &FsPath,
) -> Result<(), ProcessGroupCleanupError> {
    Err(ProcessGroupCleanupError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "experiment task process groups require macOS or Linux",
    )))
}

#[cfg(test)]
mod tests {
    use super::super::universe_api::UniverseDraft;
    use super::*;
    use crate::{
        core::{CostConfig, StrategyConfig},
        universe::{AssetType, Instrument, ManualMember, SourceKind},
    };
    use chrono::{Duration, NaiveDate};
    use duckdb::Connection;
    use sha2::{Digest, Sha256};
    use std::{
        sync::Arc,
        time::{Duration as StdDuration, Instant},
    };
    use tower::ServiceExt;

    #[test]
    fn request_requires_bounded_dates_and_matching_version() {
        let mut req = sample_request();
        assert!(validate_shape(&req).is_ok());
        req.config.end = None;
        assert_eq!(validate_shape(&req).unwrap_err().0.code, "invalid_request")
    }

    #[test]
    fn synthetic_three_etf_job_is_idempotent_atomic_and_uses_frozen_identity() {
        let (api, request, _dir) = fixture();
        let (first, created) = api.submit(request.clone()).unwrap();
        assert!(created);
        let first_json = serde_json::to_value(&first).unwrap();
        let legacy_fields = first_json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            legacy_fields,
            [
                "run_id",
                "kind",
                "status",
                "stage",
                "created_at",
                "updated_at",
                "request_sha256",
                "idempotency_key",
                "universe_id",
                "version_id",
                "universe_content_hash",
                "snapshot_id",
                "snapshot_sha256",
                "config_sha256",
                "diagnostic",
                "experiment_id",
                "error",
            ]
            .into_iter()
            .collect()
        );
        let (duplicate, created) = api.submit(request.clone()).unwrap();
        assert!(!created);
        assert_eq!(first.run_id, duplicate.run_id);
        let mut changed = request.clone();
        changed.config.initial_cash += 1.0;
        assert_eq!(
            api.submit(changed).unwrap_err().0.code,
            "idempotency_conflict"
        );

        let experiment_id = api.execute(&first).unwrap();
        let experiment =
            crate::experiment::load_experiment(&api.0.output, &experiment_id.to_string()).unwrap();
        assert_eq!(experiment.experiment_id, experiment_id);
        assert!(
            api.0
                .output
                .join("experiments")
                .join(experiment_id.to_string())
                .join("experiment.json")
                .is_file()
        );
        assert_eq!(experiment.snapshot.snapshot_id, request.snapshot_id);
        assert_eq!(experiment.snapshot.sha256, first.snapshot_sha256);
        assert_eq!(
            experiment.universe.as_ref().unwrap().version_id,
            request.version_id
        );
        assert_eq!(
            experiment.backtest.execution_status_mode.as_deref(),
            Some("legacy_bar_only")
        );
        assert_eq!(
            experiment.run_mode.as_deref(),
            Some("diagnostic_legacy_bar_only")
        );
        assert!(
            experiment
                .assumptions
                .iter()
                .any(|text| text.contains("retrospective static membership")
                    && text.contains("raw prices"))
        );
        assert!(!experiment.backtest.equity_curve.is_empty());
        assert!(experiment.backtest.metrics.trade_count > 0);
        assert_eq!(experiment.backtest.metrics.total_cost, 0.0);
        let equity_by_date = experiment
            .backtest
            .equity_curve
            .iter()
            .map(|point| (point.date, point.equity))
            .collect::<HashMap<_, _>>();
        for point in &experiment.backtest.position_curve {
            let holdings: f64 = point
                .holdings
                .iter()
                .map(|holding| holding.market_value)
                .sum();
            let equity = equity_by_date[&point.date];
            assert!((point.cash + holdings - equity).abs() < 1e-7);
            assert!(point.holdings.iter().all(|holding| holding.quantity >= 0));
        }
    }

    #[test]
    fn missing_snapshot_and_stock_capability_fail_closed() {
        let (api, mut request, _dir) = fixture();
        request.snapshot_id = Uuid::new_v4();
        assert!(
            api.inspect(&request)
                .blockers
                .iter()
                .any(|error| error.code == "snapshot_missing")
        );
        assert!(!api.inspect(&request).diagnostic_allowed);
        request.snapshot_id = Uuid::from_u128(77);
        let stock = UniverseDraft {
            name: "stock blocker".into(),
            description: String::new(),
            asset_scope: AssetScope::Stock,
            source_kind: SourceKind::Manual,
            source_ref: "user:test".into(),
            members: vec![{
                let mut stock_member = member("600000", "SH");
                stock_member.instrument.asset_type = AssetType::Stock;
                stock_member
            }],
        };
        let created = api.0.universes.create(stock).unwrap();
        let version = api
            .0
            .universes
            .publish(created.universe_id, None)
            .unwrap()
            .version;
        request.universe_id = version.universe_id;
        request.version_id = version.version_id;
        request.config.universe = Some(crate::universe::UniverseSelection {
            universe_id: version.universe_id,
            version_id: version.version_id,
            asset_scope: AssetScope::Stock,
        });
        assert!(
            api.inspect(&request)
                .blockers
                .iter()
                .any(|error| error.code == "asset_scope_blocked")
        );
        assert!(!api.inspect(&request).diagnostic_allowed);
    }

    #[test]
    fn simultaneous_same_key_requests_create_one_queued_job() {
        let (api, request, _dir) = fixture();
        let api = Arc::new(api);
        let threads = (0..8)
            .map(|_| {
                let api = api.clone();
                let request = request.clone();
                std::thread::spawn(move || api.submit(request).unwrap())
            })
            .collect::<Vec<_>>();
        let replies = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(replies.iter().filter(|(_, created)| *created).count(), 1);
        assert_eq!(
            replies
                .iter()
                .map(|(job, _)| job.run_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            1
        );
        assert_eq!(api.list().len(), 1);
    }

    #[test]
    fn trusted_history_submission_returns_structured_capability_block() {
        let (api, mut request, _dir) = fixture();
        request.diagnostic_mode = false;
        let error = api.submit(request).unwrap_err();
        assert_eq!(error.1, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.0.code, "capability_not_ready");
        assert!(
            error.0.details["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["code"] == "trusted_history_evidence_missing")
        );
    }

    #[test]
    fn corrupt_fixed_snapshot_fails_a_queued_job_without_publishing_experiment() {
        let (api, request, _dir) = fixture();
        let (job, created) = api.submit(request.clone()).unwrap();
        assert!(created);
        // Publishing a new snapshot elsewhere must not affect the job's selected ID.
        let pinned = api.load_snapshot(request.snapshot_id).unwrap();
        let next_id = Uuid::from_u128(78);
        let next_dir = api.0.output.join("snapshots").join(next_id.to_string());
        fs::create_dir_all(&next_dir).unwrap();
        let mut next = pinned.clone();
        next.snapshot_id = next_id;
        fs::write(
            next_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&next).unwrap(),
        )
        .unwrap();
        assert_eq!(
            api.load_snapshot(job.snapshot_id).unwrap().snapshot_id,
            request.snapshot_id
        );

        fs::write(&pinned.file, b"corrupt after submit").unwrap();
        let failure = api.execute(&job).unwrap_err();
        assert_eq!(failure.code, "snapshot_hash_mismatch");
        assert!(
            !api.0
                .output
                .join("experiments")
                .join("some-result")
                .exists()
        );
        assert!(
            !api.0
                .output
                .join("experiments")
                .join(job.run_id.to_string())
                .join("experiment.json")
                .exists()
        );
    }
    #[test]
    fn restart_marks_running_failed_and_keeps_idempotency() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let mut job = sample_job(id);
        job.status = RunStatus::Running;
        let request = sample_request();
        persist_request(dir.path(), id, &request).unwrap();
        persist_job(dir.path(), &job).unwrap();
        let api = RunJobApi(Arc::new(Inner {
            output: dir.path().into(),
            experiment_cli: PathBuf::from("prajna-experiment"),
            process_inspector: PathBuf::from("ps"),
            gate: Mutex::new(Store::default()),
            wake: Condvar::new(),
            universes: Arc::new(UniverseStore::new(dir.path())),
        }));
        api.recover();
        let recovered = api.get(id).unwrap();
        assert!(matches!(recovered.status, RunStatus::Failed));
        assert_eq!(recovered.error.unwrap().code, "worker_interrupted");
        assert!(
            api.0
                .gate
                .lock()
                .unwrap()
                .idempotency
                .contains_key("stable-key")
        )
    }

    #[test]
    fn restart_requeues_legacy_queued_job_with_its_original_request() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::from_u128(9);
        let job = sample_job(id);
        let request = sample_request();
        persist_request(dir.path(), id, &request).unwrap();
        persist_job(dir.path(), &job).unwrap();
        let api = RunJobApi::without_worker(
            dir.path().into(),
            Arc::new(UniverseStore::new(dir.path())),
            PathBuf::from("prajna-experiment"),
        );

        api.recover();

        let recovered = api.get(id).unwrap();
        assert!(matches!(recovered.status, RunStatus::Queued));
        assert_eq!(recovered.run_id, id);
        let store = api.0.gate.lock().unwrap();
        assert!(matches!(store.queue.front(), Some(QueueItem::LegacyRun(run_id)) if *run_id == id));
        assert!(store.jobs.get(&id).unwrap().config.is_some());
        assert!(store.idempotency.contains_key("stable-key"));
    }

    #[test]
    fn restart_marks_unfinished_experiment_tasks_interrupted_without_requeueing() {
        for (task_id, status) in [
            (Uuid::from_u128(10), ExperimentTaskStatus::Queued),
            (Uuid::from_u128(11), ExperimentTaskStatus::Running),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let request = SubmitExperimentTask {
                idempotency_key: format!("task-{}", task_id.as_u128()),
                definition: json!({"name":"synthetic restart fixture"}),
                result_level: None,
            };
            persist_experiment_request(dir.path(), task_id, &request).unwrap();
            let mut task = sample_experiment_task(task_id, status);
            task.request_sha256 = canonical_experiment_task_hash(&request).unwrap();
            task.idempotency_key = request.idempotency_key.clone();
            persist_experiment_task(dir.path(), &task).unwrap();

            let api = RunJobApi::without_worker(
                dir.path().into(),
                Arc::new(UniverseStore::new(dir.path())),
                PathBuf::from("prajna-experiment"),
            );
            api.recover();

            let recovered = api.get_experiment_task(task_id).unwrap();
            assert!(matches!(
                recovered.status,
                ExperimentTaskStatus::Interrupted
            ));
            assert_eq!(recovered.error.unwrap().code, "worker_interrupted");
            assert_eq!(recovered.process_group_id, None);
            let store = api.0.gate.lock().unwrap();
            assert!(store.queue.is_empty());
            assert!(
                store
                    .task_idempotency
                    .contains_key(&request.idempotency_key)
            );
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn process_listing_requires_exact_task_and_generation_markers() {
        let task_id = Uuid::from_u128(120);
        let process_generation = Uuid::from_u128(1200);
        let matching = format!(
            "123 42 PRAJNA_EXPERIMENT_TASK_ID={task_id} PRAJNA_EXPERIMENT_PROCESS_GENERATION={process_generation}"
        );
        assert!(process_listing_matches_owner(
            &matching,
            task_id,
            process_generation
        ));
        assert!(!process_listing_matches_owner(
            &matching,
            Uuid::from_u128(121),
            process_generation
        ));
        assert!(!process_listing_matches_owner(
            &matching,
            task_id,
            Uuid::from_u128(1201)
        ));
        assert!(!process_listing_matches_owner(
            &format!("123 42 PRAJNA_EXPERIMENT_TASK_ID={task_id}"),
            task_id,
            process_generation
        ));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn restart_terminates_persisted_experiment_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let ready_path = dir.path().join("restart-group-ready");
        let marker_path = dir.path().join("restart-group-survived");
        let task_id = Uuid::from_u128(12);
        let process_generation = Uuid::from_u128(1200);
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(format!(
            "(/bin/sleep 0.5; /usr/bin/touch {}) & /usr/bin/touch {}; wait",
            shell_quote(&marker_path),
            shell_quote(&ready_path),
        ));
        let mut process =
            ExperimentProcessGroup::spawn(&mut command, task_id, process_generation).unwrap();
        let deadline = Instant::now() + StdDuration::from_secs(5);
        while !ready_path.is_file() {
            assert!(
                Instant::now() < deadline,
                "process group helper did not start"
            );
            std::thread::sleep(StdDuration::from_millis(10));
        }

        let request = SubmitExperimentTask {
            idempotency_key: "running-after-restart".into(),
            definition: json!({"name":"synthetic restart fixture"}),
            result_level: None,
        };
        let mut task = sample_experiment_task(task_id, ExperimentTaskStatus::Running);
        task.process_group_id = Some(process.process_group_id());
        task.process_group_generation = Some(process_generation);
        task.request_sha256 = canonical_experiment_task_hash(&request).unwrap();
        task.idempotency_key = request.idempotency_key.clone();
        persist_experiment_request(dir.path(), task_id, &request).unwrap();
        persist_experiment_task(dir.path(), &task).unwrap();

        let api = RunJobApi::without_worker(
            dir.path().into(),
            Arc::new(UniverseStore::new(dir.path())),
            PathBuf::from("prajna-experiment"),
        );
        api.recover();
        let recovered = api.get_experiment_task(task_id).unwrap();
        assert!(matches!(
            recovered.status,
            ExperimentTaskStatus::Interrupted
        ));
        if recovered.process_group_id.is_some() {
            let cleanup_code = recovered.error.as_ref().unwrap().code.as_str();
            assert!(
                matches!(
                    cleanup_code,
                    "process_group_cleanup_failed" | "process_group_ownership_unverified"
                ),
                "unexpected cleanup error while retaining the process group: {cleanup_code}"
            );
            eprintln!("SKIP: sandbox denies process-group inspection/signaling ({cleanup_code})");
            let _ = process.child.kill();
            let _ = process.child.wait();
            process.finished = true;
            return;
        }
        let _ = process.child.wait();
        process.finished = true;
        std::thread::sleep(StdDuration::from_millis(600));
        assert!(
            !marker_path.exists(),
            "restart left a task child process running"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn restart_does_not_terminate_a_process_group_owned_by_a_different_task() {
        let dir = tempfile::tempdir().unwrap();
        let ready_path = dir.path().join("mismatched-group-ready");
        let marker_path = dir.path().join("mismatched-group-survived");
        let actual_task_id = Uuid::from_u128(120);
        let actual_generation = Uuid::from_u128(1200);
        let mut command = Command::new("/bin/sh");
        command
            .env("PRAJNA_EXPERIMENT_TASK_ID", actual_task_id.to_string())
            .env(
                "PRAJNA_EXPERIMENT_PROCESS_GENERATION",
                actual_generation.to_string(),
            )
            .arg("-c")
            .arg(format!(
                "(/bin/sleep 0.5; /usr/bin/touch {}) & /usr/bin/touch {}; wait",
                shell_quote(&marker_path),
                shell_quote(&ready_path),
            ));
        let mut process =
            ExperimentProcessGroup::spawn(&mut command, actual_task_id, actual_generation).unwrap();
        let deadline = Instant::now() + StdDuration::from_secs(5);
        while !ready_path.is_file() {
            assert!(
                Instant::now() < deadline,
                "mismatched process group helper did not start"
            );
            std::thread::sleep(StdDuration::from_millis(10));
        }

        for (task_id, generation) in [
            (Uuid::from_u128(121), Some(actual_generation)),
            (actual_task_id, Some(Uuid::from_u128(1201))),
            (actual_task_id, None),
        ] {
            let output = dir.path().join(task_id.to_string());
            let request = SubmitExperimentTask {
                idempotency_key: format!("stale-owner-{task_id}"),
                definition: json!({"name":"synthetic stale process group"}),
                result_level: None,
            };
            let mut task = sample_experiment_task(task_id, ExperimentTaskStatus::Running);
            task.process_group_id = Some(process.process_group_id());
            task.process_group_generation = generation;
            task.request_sha256 = canonical_experiment_task_hash(&request).unwrap();
            task.idempotency_key = request.idempotency_key.clone();
            persist_experiment_request(&output, task_id, &request).unwrap();
            persist_experiment_task(&output, &task).unwrap();

            let api = RunJobApi::without_worker(
                output.clone(),
                Arc::new(UniverseStore::new(&output)),
                PathBuf::from("prajna-experiment"),
            );
            api.recover();
            let recovered = api.get_experiment_task(task_id).unwrap();
            assert!(matches!(
                recovered.status,
                ExperimentTaskStatus::Interrupted
            ));
            assert_eq!(recovered.process_group_id, Some(process.process_group_id()));
            assert_eq!(
                recovered.error.unwrap().code,
                "process_group_ownership_unverified"
            );
        }

        std::thread::sleep(StdDuration::from_millis(600));
        assert!(
            marker_path.is_file(),
            "recovery terminated a process group owned by another task"
        );
        let _ = process.terminate();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn restart_reports_process_group_cleanup_failure_and_retains_group_id() {
        let dir = tempfile::tempdir().unwrap();
        let task_id = Uuid::from_u128(13);
        let mut task = sample_experiment_task(task_id, ExperimentTaskStatus::Running);
        task.process_group_id = Some(1);
        persist_experiment_task(dir.path(), &task).unwrap();
        let api = RunJobApi::without_worker(
            dir.path().into(),
            Arc::new(UniverseStore::new(dir.path())),
            PathBuf::from("prajna-experiment"),
        );

        api.recover();

        let recovered = api.get_experiment_task(task_id).unwrap();
        assert!(matches!(
            recovered.status,
            ExperimentTaskStatus::Interrupted
        ));
        assert_eq!(recovered.process_group_id, Some(1));
        assert_eq!(
            recovered.error.unwrap().code,
            "process_group_cleanup_failed"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn experiment_process_group_cleanup_stops_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let ready_path = dir.path().join("descendant-ready");
        let marker_path = dir.path().join("descendant-survived");
        let task_id = Uuid::new_v4();
        let process_generation = Uuid::new_v4();
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(format!(
            "(/bin/sleep 0.5; /usr/bin/touch {}) & /usr/bin/touch {}; wait",
            shell_quote(&marker_path),
            shell_quote(&ready_path),
        ));
        let mut process =
            ExperimentProcessGroup::spawn(&mut command, task_id, process_generation).unwrap();
        let deadline = Instant::now() + StdDuration::from_secs(5);
        while !ready_path.is_file() {
            assert!(
                Instant::now() < deadline,
                "process group helper did not start"
            );
            std::thread::sleep(StdDuration::from_millis(10));
        }

        match process.terminate() {
            Ok(status) => assert!(!status.success()),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("SKIP: sandbox denies process-group signaling");
                let _ = process.child.kill();
                let _ = process.child.wait();
                process.finished = true;
                std::thread::sleep(StdDuration::from_millis(600));
                return;
            }
            Err(error) => panic!("process-group cleanup failed: {error:?}"),
        }
        std::thread::sleep(StdDuration::from_millis(600));
        assert!(!marker_path.exists(), "a child process outlived its group");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn successful_cli_exit_cleans_up_process_group_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let marker_path = dir.path().join("successful-cli-child-survived");
        let task_id = Uuid::new_v4();
        let process_generation = Uuid::new_v4();
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(format!(
            "(/bin/sleep 0.5; /usr/bin/touch {}) & exit 0",
            shell_quote(&marker_path),
        ));
        let mut process =
            ExperimentProcessGroup::spawn(&mut command, task_id, process_generation).unwrap();

        match process.wait() {
            Ok(wait) => {
                assert!(wait.status.success());
                if let Some(error) = wait.cleanup_error {
                    if error.kind() == std::io::ErrorKind::PermissionDenied {
                        eprintln!("SKIP: sandbox denies process-group inspection/signaling");
                        let _ = process.child.wait();
                        process.finished = true;
                        return;
                    }
                    panic!("process-group cleanup failed: {error:?}");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("SKIP: sandbox denies process-group signaling");
                let _ = process.child.wait();
                process.finished = true;
                return;
            }
            Err(error) => panic!("process-group cleanup failed: {error:?}"),
        }
        std::thread::sleep(StdDuration::from_millis(600));
        assert!(
            !marker_path.exists(),
            "a child process outlived a successful CLI"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test(flavor = "current_thread")]
    async fn experiment_task_api_uses_versioned_shared_queue_and_persists_cli_identity() {
        let dir = tempfile::tempdir().unwrap();
        let args_path = dir.path().join("cli-args.txt");
        let pid_path = dir.path().join("cli-pid.txt");
        let release_path = dir.path().join("release-cli");
        let cli_path = dir.path().join("prajna-experiment");
        let inspector_path = dir.path().join("empty-ps");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\necho $$ > {}\n[ \"$1\" = run ] || exit 91\n[ -f \"$2\" ] || exit 92\nwhile [ ! -f {} ]; do /bin/sleep 0.02; done\nprintf '{{\"exp\":\"exp:sha256:synthetic\",\"exe\":\"exe:sha256:synthetic\",\"action\":\"created\"}}\\n'\n",
            shell_quote(&args_path),
            shell_quote(&pid_path),
            shell_quote(&release_path),
        );
        fs::write(&cli_path, script).unwrap();
        fs::write(&inspector_path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cli_path, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&inspector_path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let (api, legacy_request, _fixture_dir) =
            fixture_with_cli_and_process_inspector(cli_path, inspector_path);
        let submitted = json!({
            "idempotency_key": "experiment-task-key",
            "definition": {"name": "synthetic experiment", "parameters": {"lookback": 2}}
        });
        let (status, accepted) = api_json(
            &api,
            "POST",
            "/api/v1/tasks/experiments",
            Some(submitted.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let task_id = accepted["task"]["task_id"].as_str().unwrap();
        assert_eq!(accepted["created"], true);
        assert_eq!(accepted["task"]["task_type"], "prajna_experiment");
        assert_eq!(accepted["task"]["task_version"], 1);
        assert_eq!(accepted["task"]["status"], "queued");

        let (status, duplicate) = api_json(
            &api,
            "POST",
            "/api/v1/tasks/experiments",
            Some(submitted.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(duplicate["created"], false);
        assert_eq!(duplicate["task"]["task_id"], task_id);

        let mut conflict = submitted.clone();
        conflict["definition"]["name"] = json!("different definition");
        let (status, rejected) =
            api_json(&api, "POST", "/api/v1/tasks/experiments", Some(conflict)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(rejected["error"]["code"], "idempotency_conflict");

        let (status, list) = api_json(&api, "GET", "/api/v1/tasks/experiments", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["items"][0]["task_id"], task_id);

        let (status, legacy) = api_json(
            &api,
            "POST",
            "/api/v1/runs",
            Some(serde_json::to_value(&legacy_request).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(legacy["run"]["kind"], "etf_strategy");
        assert_eq!(legacy["run"]["status"], "queued");
        let legacy_run_id = Uuid::parse_str(legacy["run"]["run_id"].as_str().unwrap()).unwrap();
        assert_eq!(api.0.gate.lock().unwrap().queue.len(), 2);

        let worker_api = api.clone();
        let worker = std::thread::spawn(move || worker_api.process_next_queued());
        let deadline = Instant::now() + StdDuration::from_secs(5);
        let running_task = loop {
            let task = api
                .get_experiment_task(Uuid::parse_str(task_id).unwrap())
                .unwrap();
            if task.process_group_id.is_some() {
                break task;
            }
            assert!(
                Instant::now() < deadline,
                "task process group was not recorded"
            );
            std::thread::sleep(StdDuration::from_millis(10));
        };
        let process_group_id = running_task.process_group_id.unwrap();
        while !pid_path.is_file() {
            assert!(Instant::now() < deadline, "fake CLI did not start");
            std::thread::sleep(StdDuration::from_millis(10));
        }
        assert_eq!(
            fs::read_to_string(pid_path).unwrap().trim(),
            process_group_id.to_string()
        );
        fs::write(release_path, "continue").unwrap();
        assert!(worker.join().unwrap());
        assert_eq!(api.0.gate.lock().unwrap().queue.len(), 1);
        let (status, detail) = api_json(
            &api,
            "GET",
            &format!("/api/v1/tasks/experiments/{task_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["status"], "succeeded", "{detail}");
        assert_eq!(detail["task_type"], "prajna_experiment");
        assert_eq!(detail["task_version"], 1);
        assert_eq!(detail["result"]["exp"], "exp:sha256:synthetic");
        assert_eq!(detail["result"]["exe"], "exe:sha256:synthetic");
        assert_eq!(detail["process_group_id"], Value::Null);

        let task_dir = api.0.output.join("tasks").join(task_id);
        assert!(task_dir.join("task.json").is_file());
        assert!(task_dir.join("request.json").is_file());
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(task_dir.join("experiment.json")).unwrap())
                .unwrap(),
            submitted["definition"]
        );
        let args = fs::read_to_string(args_path).unwrap();
        let args = args.lines().collect::<Vec<_>>();
        assert_eq!(args[0], "run");
        assert_eq!(PathBuf::from(args[1]), task_dir.join("experiment.json"));
        assert_eq!(args[2], "--lake");
        assert_eq!(PathBuf::from(args[3]), api.0.output);
        assert_eq!(&args[4..], &["--level", "summary"]);

        assert!(api.process_next_queued());
        let legacy_detail = api.get(legacy_run_id).unwrap();
        assert!(matches!(legacy_detail.status, RunStatus::Succeeded));
        let experiment_id = legacy_detail.experiment_id.unwrap();
        let experiment =
            crate::experiment::load_experiment(&api.0.output, &experiment_id.to_string()).unwrap();
        assert_eq!(experiment.experiment_id, experiment_id);
        assert_eq!(experiment.snapshot.snapshot_id, legacy_request.snapshot_id);
        assert!(api.0.gate.lock().unwrap().queue.is_empty());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test(flavor = "current_thread")]
    async fn failed_cli_diagnostic_and_exit_code_survive_api_persistence_and_restart() {
        const EXPECTED_STDERR_LIMIT: usize = 16 * 1024;
        let dir = tempfile::tempdir().unwrap();
        let cli_path = dir.path().join("failing-prajna-experiment");
        let inspector_path = dir.path().join("empty-ps");
        fs::write(
            &cli_path,
            "#!/bin/sh\nprintf 'synthetic failure cause\\n' >&2\n/usr/bin/head -c 32768 /dev/zero | /usr/bin/tr '\\000' x >&2\nexit 23\n",
        )
        .unwrap();
        fs::write(&inspector_path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cli_path, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&inspector_path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let (api, _legacy_request, _fixture_dir) =
            fixture_with_cli_and_process_inspector(cli_path.clone(), inspector_path);
        let (status, accepted) = api_json(
            &api,
            "POST",
            "/api/v1/tasks/experiments",
            Some(json!({
                "idempotency_key": "failing-cli-key",
                "definition": {"name":"synthetic failing task"}
            })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let task_id = Uuid::parse_str(accepted["task"]["task_id"].as_str().unwrap()).unwrap();

        assert!(api.process_next_queued());
        let (status, failed) = api_json(
            &api,
            "GET",
            &format!("/api/v1/tasks/experiments/{task_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(failed["status"], "failed");
        assert_eq!(failed["error"]["code"], "experiment_cli_failed");
        assert_eq!(failed["error"]["details"]["exit_code"], 23);
        let stderr = failed["error"]["details"]["stderr"].as_str().unwrap();
        assert!(stderr.starts_with("synthetic failure cause\n"));
        assert_eq!(stderr.len(), EXPECTED_STDERR_LIMIT);
        assert_eq!(failed["error"]["details"]["stderr_truncated"], true);

        let task_dir = api.0.output.join("tasks").join(task_id.to_string());
        let persisted: Value =
            serde_json::from_slice(&fs::read(task_dir.join("task.json")).unwrap()).unwrap();
        assert_eq!(persisted["error"]["details"]["exit_code"], 23);
        assert_eq!(persisted["error"]["details"]["stderr"], stderr);

        let restarted = RunJobApi::without_worker(
            api.0.output.clone(),
            Arc::new(UniverseStore::new(&api.0.output)),
            cli_path,
        );
        restarted.recover();
        let (status, after_restart) = api_json(
            &restarted,
            "GET",
            &format!("/api/v1/tasks/experiments/{task_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(after_restart["error"]["code"], "experiment_cli_failed");
        assert_eq!(after_restart["error"]["details"]["exit_code"], 23);
        assert_eq!(after_restart["error"]["details"]["stderr"], stderr);
        assert_eq!(after_restart["error"]["details"]["stderr_truncated"], true);
        assert_eq!(
            after_restart["error"]["details"]["stderr_capture_incomplete"],
            false
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test(flavor = "current_thread")]
    async fn failed_cli_diagnostic_survives_process_group_cleanup_failure_and_restart() {
        const EXPECTED_STDERR_LIMIT: usize = 16 * 1024;
        let dir = tempfile::tempdir().unwrap();
        let cli_path = dir.path().join("failing-prajna-experiment");
        let inspector_path = dir.path().join("denied-ps");
        fs::write(
            &cli_path,
            "#!/bin/sh\nprintf 'synthetic cleanup-failure cause\\n' >&2\n/usr/bin/head -c 32768 /dev/zero | /usr/bin/tr '\\000' x >&2\nexit 23\n",
        )
        .unwrap();
        fs::write(&inspector_path, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cli_path, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&inspector_path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let (api, _legacy_request, _fixture_dir) =
            fixture_with_cli_and_process_inspector(cli_path.clone(), inspector_path.clone());
        let (status, accepted) = api_json(
            &api,
            "POST",
            "/api/v1/tasks/experiments",
            Some(json!({
                "idempotency_key": "failing-cli-cleanup-key",
                "definition": {"name":"synthetic cleanup-failing task"}
            })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let task_id = Uuid::parse_str(accepted["task"]["task_id"].as_str().unwrap()).unwrap();

        assert!(api.process_next_queued());
        let (status, failed) = api_json(
            &api,
            "GET",
            &format!("/api/v1/tasks/experiments/{task_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(failed["status"], "failed");
        assert_eq!(
            failed["error"]["code"],
            "process_group_ownership_unverified"
        );
        assert_eq!(
            failed["error"]["details"]["cli_error"]["code"],
            "experiment_cli_failed"
        );
        let failure_details = failed["error"]["details"].clone();
        let cli_details = &failed["error"]["details"]["cli_error"]["details"];
        assert_eq!(cli_details["exit_code"], 23);
        let stderr = cli_details["stderr"].as_str().unwrap();
        assert!(stderr.starts_with("synthetic cleanup-failure cause\n"));
        assert_eq!(stderr.len(), EXPECTED_STDERR_LIMIT);
        assert_eq!(cli_details["stderr_truncated"], true);

        let task_dir = api.0.output.join("tasks").join(task_id.to_string());
        let persisted: Value =
            serde_json::from_slice(&fs::read(task_dir.join("task.json")).unwrap()).unwrap();
        assert_eq!(
            persisted["error"]["details"]["cli_error"]["details"]["exit_code"],
            23
        );
        assert_eq!(
            persisted["error"]["details"]["cli_error"]["details"]["stderr"],
            stderr
        );

        let restarted = RunJobApi::without_worker_with_process_inspector(
            api.0.output.clone(),
            Arc::new(UniverseStore::new(&api.0.output)),
            cli_path,
            inspector_path,
        );
        restarted.recover();
        restarted.recover();
        let (status, after_restart) = api_json(
            &restarted,
            "GET",
            &format!("/api/v1/tasks/experiments/{task_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            after_restart["error"]["code"],
            "process_group_ownership_unverified"
        );
        assert_eq!(after_restart["error"]["details"], failure_details);
        assert_eq!(
            after_restart["error"]["details"]["cli_error"]["code"],
            "experiment_cli_failed"
        );
        assert_eq!(
            after_restart["error"]["details"]["cli_error"]["details"]["exit_code"],
            23
        );
        assert_eq!(
            after_restart["error"]["details"]["cli_error"]["details"]["stderr"],
            stderr
        );
        assert_eq!(
            after_restart["error"]["details"]["cli_error"]["details"]["stderr_truncated"],
            true
        );
        let persisted_after_restart: Value =
            serde_json::from_slice(&fs::read(task_dir.join("task.json")).unwrap()).unwrap();
        assert_eq!(persisted_after_restart["error"]["details"], failure_details);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test(flavor = "current_thread")]
    async fn cleanup_failure_bounds_inherited_stderr_and_advances_shared_queue() {
        let dir = tempfile::tempdir().unwrap();
        let cli_path = dir.path().join("inherited-stderr-prajna-experiment");
        let inspector_path = dir.path().join("denied-ps");
        let release_path = dir.path().join("release-stderr-descendant");
        let descendant_done_path = dir.path().join("stderr-descendant-done");
        let release_descendant_on_drop = ReleaseDescendantOnDrop(release_path.clone());
        let script = format!(
            "#!/bin/sh\nif /usr/bin/grep -q 'hold-stderr-open' \"$2\"; then\n  printf 'synthetic failure before descendant\n' >&2\n  (while [ ! -f {} ]; do /bin/sleep 0.02; done; printf 'released\n' > {}) &\n  exit 23\nfi\nprintf '{{\"exp\":\"exp:sha256:synthetic\",\"exe\":\"exe:sha256:synthetic\"}}\\n'\n",
            shell_quote(&release_path),
            shell_quote(&descendant_done_path),
        );
        fs::write(&cli_path, script).unwrap();
        fs::write(&inspector_path, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cli_path, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&inspector_path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let (api, legacy_request, _fixture_dir) =
            fixture_with_cli_and_process_inspector(cli_path, inspector_path);
        let (status, accepted) = api_json(
            &api,
            "POST",
            "/api/v1/tasks/experiments",
            Some(json!({
                "idempotency_key": "inherited-stderr-cleanup-key",
                "definition": {"name":"synthetic hold-stderr-open task"}
            })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let task_id = Uuid::parse_str(accepted["task"]["task_id"].as_str().unwrap()).unwrap();

        let (status, legacy) = api_json(
            &api,
            "POST",
            "/api/v1/runs",
            Some(serde_json::to_value(&legacy_request).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let legacy_run_id = Uuid::parse_str(legacy["run"]["run_id"].as_str().unwrap()).unwrap();
        assert_eq!(api.0.gate.lock().unwrap().queue.len(), 2);

        let worker_api = api.clone();
        let (worker_done, worker_result) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let _ = worker_done.send(worker_api.process_next_queued());
        });
        let first_work = worker_result.recv_timeout(StdDuration::from_secs(5));
        if !matches!(&first_work, Ok(true)) {
            let task_dir = api.0.output.join("tasks").join(task_id.to_string());
            let task_json = fs::read_to_string(task_dir.join("task.json")).unwrap_or_default();
            panic!(
                "cleanup failure left the shared queue worker blocked on descendant-held stderr: {first_work:?}; task={task_json}"
            );
        }
        assert!(
            !descendant_done_path.exists(),
            "descendant should still own stderr when the task worker returns"
        );

        let (status, failed) = api_json(
            &api,
            "GET",
            &format!("/api/v1/tasks/experiments/{task_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(failed["status"], "failed");
        assert_eq!(
            failed["error"]["code"],
            "process_group_ownership_unverified"
        );
        let cli_error = &failed["error"]["details"]["cli_error"];
        assert_eq!(cli_error["code"], "experiment_cli_failed");
        assert_eq!(cli_error["details"]["exit_code"], 23);
        assert_eq!(
            cli_error["details"]["stderr"],
            "synthetic failure before descendant\n"
        );
        assert_eq!(cli_error["details"]["stderr_capture_incomplete"], true);

        let task_dir = api.0.output.join("tasks").join(task_id.to_string());
        let persisted_task: Value =
            serde_json::from_slice(&fs::read(task_dir.join("task.json")).unwrap()).unwrap();
        assert_eq!(persisted_task["status"], "failed");
        assert_eq!(
            persisted_task["error"]["details"]["cli_error"]["details"]["exit_code"],
            23
        );
        assert_eq!(
            persisted_task["error"]["details"]["cli_error"]["details"]["stderr_capture_incomplete"],
            true
        );

        assert!(
            api.process_next_queued(),
            "following legacy run did not advance"
        );
        let legacy_job = api.get(legacy_run_id).unwrap();
        assert!(matches!(legacy_job.status, RunStatus::Succeeded));
        let persisted_run: Value = serde_json::from_slice(
            &fs::read(
                api.0
                    .output
                    .join("runs")
                    .join(legacy_run_id.to_string())
                    .join("run.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(persisted_run["status"], "succeeded");

        drop(release_descendant_on_drop);
        let cleanup_deadline = Instant::now() + StdDuration::from_secs(2);
        while !descendant_done_path.exists() && Instant::now() < cleanup_deadline {
            thread::sleep(StdDuration::from_millis(10));
        }
        assert!(
            descendant_done_path.exists(),
            "synthetic stderr writer did not exit"
        );
    }

    struct ReleaseDescendantOnDrop(PathBuf);

    impl Drop for ReleaseDescendantOnDrop {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, b"release\n");
        }
    }

    fn sample_request() -> SubmitRun {
        SubmitRun {
            kind: "etf_strategy".into(),
            universe_id: Uuid::from_u128(1),
            version_id: Uuid::from_u128(2),
            snapshot_id: Uuid::from_u128(3),
            idempotency_key: "stable-key".into(),
            diagnostic_mode: true,
            config: ExperimentConfig {
                start: Some("2026-01-01".parse().unwrap()),
                end: Some("2026-01-10".parse().unwrap()),
                universe: Some(crate::universe::UniverseSelection {
                    universe_id: Uuid::from_u128(1),
                    version_id: Uuid::from_u128(2),
                    asset_scope: AssetScope::Etf,
                }),
                ..ExperimentConfig::default()
            },
        }
    }
    fn sample_job(run_id: Uuid) -> RunJob {
        RunJob {
            run_id,
            kind: "etf_strategy".into(),
            status: RunStatus::Queued,
            stage: "queued".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            request_sha256: "hash".into(),
            idempotency_key: "stable-key".into(),
            universe_id: Uuid::from_u128(1),
            version_id: Uuid::from_u128(2),
            universe_content_hash: "universe-hash".into(),
            snapshot_id: Uuid::from_u128(3),
            snapshot_sha256: "snapshot-hash".into(),
            config_sha256: "config-hash".into(),
            diagnostic: true,
            experiment_id: None,
            error: None,
            config: None,
        }
    }

    fn sample_experiment_task(task_id: Uuid, status: ExperimentTaskStatus) -> ExperimentTask {
        ExperimentTask {
            task_id,
            task_type: EXPERIMENT_TASK_TYPE.into(),
            task_version: EXPERIMENT_TASK_VERSION,
            status,
            stage: "running_experiment_cli".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            request_sha256: "request-hash".into(),
            idempotency_key: "experiment-task-key".into(),
            result_level: "summary".into(),
            process_group_id: None,
            process_group_generation: None,
            result: None,
            error: None,
        }
    }

    fn shell_quote(path: &FsPath) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }

    async fn api_json(
        api: &RunJobApi,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let bytes = body
            .map(|value| serde_json::to_vec(&value).unwrap())
            .unwrap_or_default();
        let response = api
            .clone()
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(bytes))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    fn member(code: &str, exchange: &str) -> ManualMember {
        ManualMember {
            instrument: Instrument {
                instrument_id: format!("ETF:{code}"),
                exchange: if exchange == "SH" {
                    "XSHG".into()
                } else {
                    "XSHE".into()
                },
                code: code.into(),
                name: format!("ETF {code}"),
                asset_type: AssetType::Etf,
            },
            effective_from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            effective_to: None,
            source_ref: "synthetic fixture".into(),
        }
    }
    #[test]
    fn momentum_tasks_pin_definitions_deduplicate_recover_and_retain_results() {
        let (api, _, _dir) = fixture();
        let request = SubmitExperimentTask {
            idempotency_key: "momentum-fixed".into(),
            definition: json!({"op":"submit","request":{"dsv":"synthetic-fixed","windows":[20,60,120],"adjusted":{"510300.XSHG":"fixed"}}}),
            result_level: None,
        };
        let (task, created) = api.submit_experiment_task(request.clone()).unwrap();
        assert!(created);
        assert_eq!(task.task_type, "prajna_momentum");
        let (duplicate, created) = api.submit_experiment_task(request.clone()).unwrap();
        assert!(!created);
        assert_eq!(duplicate.task_id, task.task_id);
        let mut changed = request.clone();
        changed.definition["request"]["windows"] = json!([1]);
        assert_eq!(
            api.submit_experiment_task(changed).unwrap_err().0.code,
            "idempotency_conflict"
        );
        let directory = experiment_task_dir(&api.0.output, task.task_id);
        assert_eq!(
            read_experiment_request(&directory).unwrap().definition,
            request.definition
        );
        api.recover();
        assert!(matches!(
            api.get_experiment_task(task.task_id).unwrap().status,
            ExperimentTaskStatus::Interrupted
        ));
        let mut running = task.clone();
        running.status = ExperimentTaskStatus::Running;
        persist_experiment_task(&api.0.output, &running).unwrap();
        api.recover();
        assert!(matches!(
            api.get_experiment_task(task.task_id).unwrap().status,
            ExperimentTaskStatus::Interrupted
        ));
        let mut completed = task.clone();
        completed.status = ExperimentTaskStatus::Succeeded;
        completed.result = Some(json!({"report_id":"synthetic-saved-report"}));
        persist_experiment_task(&api.0.output, &completed).unwrap();
        api.recover();
        let restored = api.get_experiment_task(task.task_id).unwrap();
        assert!(matches!(restored.status, ExperimentTaskStatus::Succeeded));
        assert_eq!(restored.result, completed.result);
        let mut failed = task;
        failed.status = ExperimentTaskStatus::Failed;
        failed.error = Some(run_error("invalid_momentum", "invalid window"));
        persist_experiment_task(&api.0.output, &failed).unwrap();
        api.recover();
        let restored = api.get_experiment_task(failed.task_id).unwrap();
        assert!(matches!(restored.status, ExperimentTaskStatus::Failed));
        assert_eq!(restored.error.unwrap().code, "invalid_momentum");
    }

    fn fixture() -> (RunJobApi, SubmitRun, tempfile::TempDir) {
        fixture_with_cli(PathBuf::from("prajna-experiment"))
    }

    fn fixture_with_cli(experiment_cli: PathBuf) -> (RunJobApi, SubmitRun, tempfile::TempDir) {
        fixture_with_cli_and_process_inspector(experiment_cli, PathBuf::from("ps"))
    }

    fn fixture_with_cli_and_process_inspector(
        experiment_cli: PathBuf,
        process_inspector: PathBuf,
    ) -> (RunJobApi, SubmitRun, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let universe_store = Arc::new(UniverseStore::new(root));
        let draft = UniverseDraft {
            name: "synthetic three ETF".into(),
            description: "test only".into(),
            asset_scope: AssetScope::Etf,
            source_kind: SourceKind::Manual,
            source_ref: "synthetic:test".into(),
            members: vec![
                member("510300", "SH"),
                member("510500", "SH"),
                member("159915", "SZ"),
            ],
        };
        let created = universe_store.create(draft).unwrap();
        let version = universe_store
            .publish(created.universe_id, None)
            .unwrap()
            .version;
        let snapshot_id = Uuid::from_u128(77);
        let snapshot_dir = root.join("snapshots").join(snapshot_id.to_string());
        fs::create_dir_all(&snapshot_dir).unwrap();
        let file = snapshot_dir.join("etf.parquet");
        let conn = Connection::open_in_memory().unwrap();
        let start = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let values=(0..10).flat_map(|day|[("ETF:510300","sh510300","510300","SH","沪深300ETF",10.0),("ETF:510500","sh510500","510500","SH","中证500ETF",20.0),("ETF:159915","sz159915","159915","SZ","创业板ETF",30.0)].into_iter().map(move |(instrument_id,symbol,code,exchange,name,price)|(day,instrument_id,symbol,code,exchange,name,price+day as f64))).map(|(day,instrument_id,symbol,code,exchange,name,close)|format!("('{instrument_id}','{symbol}','{code}','{exchange}','{name}','ETF',DATE '{}',{}, {}, {}, {},1000000.0,10000000.0,FALSE)",start+Duration::days(day),close,close*1.01,close*0.99,close)).collect::<Vec<_>>().join(",");
        conn.execute_batch(&format!("COPY (SELECT * FROM (VALUES {values}) bars(instrument_id,symbol,code,exchange,name,asset_type,trade_date,open,high,low,close,volume,amount,is_preheat)) TO '{}' (FORMAT PARQUET)",file.to_string_lossy().replace('\'', "''"))).unwrap();
        let hash = format!("{:x}", Sha256::digest(fs::read(&file).unwrap()));
        let snapshot = SnapshotManifest {
            snapshot_id,
            created_at: "2026-01-15T00:00:00Z".into(),
            source_database: "fixture".into(),
            file: file.clone(),
            sha256: hash,
            rows: 30,
            symbols: 3,
            first_date: start.to_string(),
            last_date: (start + Duration::days(9)).to_string(),
            benchmark_file: None,
            benchmark_sha256: None,
            benchmark_rows: 0,
            benchmark_name: None,
            trading_calendar_file: None,
            trading_calendar_sha256: None,
            trading_calendar_rows: 0,
            trading_calendar_first_date: None,
            trading_calendar_last_date: None,
            trading_calendar_source: None,
            selection_rule: "synthetic 3 ETF fixture".into(),
            limitations: vec!["synthetic".into()],
        };
        fs::write(
            snapshot_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&snapshot).unwrap(),
        )
        .unwrap();
        let request = SubmitRun {
            kind: "etf_strategy".into(),
            universe_id: version.universe_id,
            version_id: version.version_id,
            snapshot_id,
            idempotency_key: "job-fixture-key".into(),
            diagnostic_mode: true,
            config: ExperimentConfig {
                name: "synthetic diagnostic".into(),
                universe: Some(crate::universe::UniverseSelection {
                    universe_id: version.universe_id,
                    version_id: version.version_id,
                    asset_scope: AssetScope::Etf,
                }),
                start: Some(start + Duration::days(2)),
                end: Some(start + Duration::days(9)),
                initial_cash: 100_000.0,
                lot_size: 100,
                strategy: StrategyConfig {
                    lookback_days: 1,
                    top_n: 1,
                    rebalance_every: 1,
                    ..StrategyConfig::default()
                },
                costs: CostConfig {
                    commission_rate: 0.0,
                    minimum_commission: 0.0,
                    buy_tax_rate: 0.0,
                    sell_tax_rate: 0.0,
                    buy_slippage_bps: 0.0,
                    sell_slippage_bps: 0.0,
                },
                ..ExperimentConfig::default()
            },
        };
        let api = RunJobApi::without_worker_with_process_inspector(
            root.into(),
            universe_store,
            experiment_cli,
            process_inspector,
        );
        (api, request, dir)
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    async fn get_result(
        api: &RunJobApi,
        authority: &str,
        id: Uuid,
        host: &str,
        origin: Option<&str>,
    ) -> (StatusCode, Value, axum::http::HeaderMap) {
        let mut request = axum::http::Request::builder()
            .method("GET")
            .uri(format!("/api/v1/tasks/experiments/{id}/result"))
            .header("host", host);
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        let response = api
            .clone()
            .result_router(authority.into())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            headers,
        )
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test(flavor = "current_thread")]
    async fn experiment_result_route_serves_saved_identity_and_enforces_local_origin() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("result-reader");
        fs::write(
            &cli,
            "#!/bin/sh\n[ \"$1\" = result ] || exit 91\nprintf '{\"schema_version\":1,\"data\":{\"exp\":\"%s\",\"exe\":\"%s\"}}\\n' \"$2\" \"$3\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let api = RunJobApi::without_worker(
            dir.path().into(),
            Arc::new(UniverseStore::new(dir.path())),
            cli,
        );
        let succeeded = Uuid::from_u128(41);
        let mut task = sample_experiment_task(succeeded, ExperimentTaskStatus::Succeeded);
        task.result = Some(json!({
            "exp": "exp:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "exe": "exe:sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        }));
        api.0.gate.lock().unwrap().tasks.insert(succeeded, task);

        let (status, body, headers) =
            get_result(&api, "127.0.0.1:7878", succeeded, "127.0.0.1:7878", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["schema_version"], 1);
        assert_eq!(
            body["data"]["exp"],
            "exp:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(headers["content-type"], "application/json");
        assert_eq!(headers["cache-control"], "no-store");

        let (status, body, _) = get_result(
            &api,
            "127.0.0.1:7878",
            succeeded,
            "synthetic-review.invalid",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], "result_origin_forbidden");
        let (status, body, _) = get_result(
            &api,
            "127.0.0.1:7878",
            succeeded,
            "127.0.0.1:7878",
            Some("https://synthetic-review.invalid"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], "result_origin_forbidden");

        let queued = Uuid::from_u128(42);
        api.0.gate.lock().unwrap().tasks.insert(
            queued,
            sample_experiment_task(queued, ExperimentTaskStatus::Queued),
        );
        let (status, body, headers) =
            get_result(&api, "127.0.0.1:7878", queued, "127.0.0.1:7878", None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "task_not_completed");
        assert_eq!(headers["cache-control"], "no-store");

        let (status, _, headers) = get_result(
            &api,
            "127.0.0.1:7878",
            Uuid::from_u128(43),
            "127.0.0.1:7878",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(headers["cache-control"], "no-store");

        let invalid = Uuid::from_u128(44);
        let mut invalid_task = sample_experiment_task(invalid, ExperimentTaskStatus::Succeeded);
        invalid_task.result = Some(json!({"exp": 1}));
        api.0
            .gate
            .lock()
            .unwrap()
            .tasks
            .insert(invalid, invalid_task);
        let (status, body, _) =
            get_result(&api, "127.0.0.1:7878", invalid, "127.0.0.1:7878", None).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "task_result_invalid");

        let missing = dir.path().join("missing-result-reader");
        let unavailable = RunJobApi::without_worker(
            dir.path().into(),
            Arc::new(UniverseStore::new(dir.path())),
            missing,
        );
        let unavailable_id = Uuid::from_u128(45);
        let mut unavailable_task =
            sample_experiment_task(unavailable_id, ExperimentTaskStatus::Succeeded);
        unavailable_task.result = Some(json!({
            "exp": "exp:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "exe": "exe:sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        }));
        unavailable
            .0
            .gate
            .lock()
            .unwrap()
            .tasks
            .insert(unavailable_id, unavailable_task);
        let (status, body, _) = get_result(
            &unavailable,
            "127.0.0.1:7878",
            unavailable_id,
            "127.0.0.1:7878",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "experiment_result_unavailable");

        let failing_reader = dir.path().join("failing-result-reader");
        fs::write(
            &failing_reader,
            "#!/bin/sh\nprintf 'synthetic read failure\\n' >&2\nexit 2\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&failing_reader, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let failing = RunJobApi::without_worker(
            dir.path().into(),
            Arc::new(UniverseStore::new(dir.path())),
            failing_reader,
        );
        let failing_id = Uuid::from_u128(47);
        let mut failing_task = sample_experiment_task(failing_id, ExperimentTaskStatus::Succeeded);
        failing_task.result = Some(json!({
            "exp": "exp:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "exe": "exe:sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        }));
        failing
            .0
            .gate
            .lock()
            .unwrap()
            .tasks
            .insert(failing_id, failing_task);
        let (status, body, headers) = get_result(
            &failing,
            "127.0.0.1:7878",
            failing_id,
            "127.0.0.1:7878",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "experiment_result_failed");
        assert_eq!(body["error"]["details"]["exit_code"], 2);
        assert!(body["error"]["details"]["stderr"].is_string());
        assert_eq!(body["error"]["details"]["stderr_truncated"], false);
        assert_eq!(headers["cache-control"], "no-store");

        let malformed_reader = dir.path().join("malformed-result-reader");
        fs::write(&malformed_reader, "#!/bin/sh\nprintf 'not-json\\n'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&malformed_reader, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let malformed = RunJobApi::without_worker(
            dir.path().into(),
            Arc::new(UniverseStore::new(dir.path())),
            malformed_reader,
        );
        let malformed_id = Uuid::from_u128(48);
        let mut malformed_task =
            sample_experiment_task(malformed_id, ExperimentTaskStatus::Succeeded);
        malformed_task.result = Some(json!({
            "exp": "exp:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "exe": "exe:sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        }));
        malformed
            .0
            .gate
            .lock()
            .unwrap()
            .tasks
            .insert(malformed_id, malformed_task);
        let (status, body, headers) = get_result(
            &malformed,
            "127.0.0.1:7878",
            malformed_id,
            "127.0.0.1:7878",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"]["code"], "invalid_result_response");
        assert_eq!(headers["cache-control"], "no-store");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test(flavor = "current_thread")]
    async fn experiment_task_standard_level_is_frozen_and_drives_cli() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("prajna-experiment");
        fs::write(
            &cli,
            "#!/bin/sh\n[ \"$1\" = run ] || exit 6\nprintf '%s\\n' \"$@\" > \"$(dirname \"$2\")/cli-args.txt\"\nprintf '{\"exp\":\"exp:sha256:synthetic\",\"exe\":\"exe:sha256:synthetic\"}\\n'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let inspector = dir.path().join("empty-ps");
        fs::write(&inspector, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&inspector, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (api, _legacy, _fixture_dir) = fixture_with_cli_and_process_inspector(cli, inspector);
        let submitted = json!({
            "idempotency_key": "standard-level-key",
            "definition": {"name": "synthetic standard experiment"},
            "result_level": "standard"
        });
        let (status, accepted) = api_json(
            &api,
            "POST",
            "/api/v1/tasks/experiments",
            Some(submitted.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(accepted["task"]["result_level"], "standard");
        let task_id = Uuid::parse_str(accepted["task"]["task_id"].as_str().unwrap()).unwrap();
        assert!(api.process_next_queued());
        let (status, detail) = api_json(
            &api,
            "GET",
            &format!("/api/v1/tasks/experiments/{task_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["status"], "succeeded", "{detail}");
        assert_eq!(detail["result_level"], "standard");
        let args = fs::read_to_string(
            api.0
                .output
                .join("tasks")
                .join(task_id.to_string())
                .join("cli-args.txt"),
        )
        .unwrap();
        let args = args.lines().collect::<Vec<_>>();
        assert_eq!(&args[4..], &["--level", "standard"]);

        let (status, conflict) = api_json(
            &api,
            "POST",
            "/api/v1/tasks/experiments",
            Some(json!({
                "idempotency_key": "standard-level-key",
                "definition": {"name": "synthetic standard experiment"}
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(conflict["error"]["code"], "idempotency_conflict");

        let (status, invalid) = api_json(
            &api,
            "POST",
            "/api/v1/tasks/experiments",
            Some(json!({
                "idempotency_key": "invalid-level-key",
                "definition": {"name": "synthetic invalid level"},
                "result_level": "full"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(invalid["error"]["code"], "invalid_request");
    }
    #[test]
    fn legacy_task_and_request_json_default_to_summary() {
        let task_id = Uuid::from_u128(70);
        let task: ExperimentTask = serde_json::from_value(json!({
            "task_id": task_id,
            "task_type": "prajna_experiment",
            "task_version": 1,
            "status": "succeeded",
            "stage": "reading_experiment_result",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "request_sha256": "request-hash",
            "idempotency_key": "legacy-key",
            "process_group_id": null,
            "process_group_generation": null,
            "result": null,
            "error": null
        }))
        .unwrap();
        assert_eq!(task.result_level, "summary");
        assert!(
            serde_json::to_value(&task)
                .unwrap()
                .get("result_level")
                .is_none()
        );

        let request: SubmitExperimentTask = serde_json::from_value(json!({
            "idempotency_key": "legacy-key",
            "definition": {"name": "legacy"}
        }))
        .unwrap();
        assert_eq!(request.result_level, None);
        let legacy_hash = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&json!({
                    "task_type": "prajna_experiment",
                    "task_version": 1,
                    "definition": {"name": "legacy"}
                }))
                .unwrap()
            )
        );
        assert_eq!(
            canonical_experiment_task_hash(&request).unwrap(),
            legacy_hash
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test(flavor = "current_thread")]
    async fn standard_task_promotes_an_existing_execution_first() {
        let dir = tempfile::tempdir().unwrap();
        let args_path = dir.path().join("cli-args.txt");
        let cli = dir.path().join("prajna-experiment");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> {}\nif [ \"$1\" = run ]; then\n  if [ \"$6\" = standard ]; then\n    printf 'Execution already exists at summary; use promote\\n' >&2\n    exit 3\n  fi\n  printf '{{\"exp\":\"exp:sha256:synthetic\",\"exe\":\"exe:sha256:synthetic\"}}\\n'\n  exit 0\nfi\nif [ \"$1\" = promote ]; then\n  printf '{{\"exp\":\"exp:sha256:synthetic\",\"exe\":\"exe:sha256:synthetic\",\"action\":\"promoted\"}}\\n'\n  exit 0\nfi\nexit 91\n",
            shell_quote(&args_path)
        );
        fs::write(&cli, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let inspector = dir.path().join("empty-ps");
        fs::write(&inspector, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&inspector, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (api, _legacy, _fixture_dir) = fixture_with_cli_and_process_inspector(cli, inspector);

        let (status, accepted) = api_json(
            &api,
            "POST",
            "/api/v1/tasks/experiments",
            Some(json!({
                "idempotency_key": "standard-fallback",
                "definition": {"name": "synthetic fallback"},
                "result_level": "standard"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let standard_id = Uuid::parse_str(accepted["task"]["task_id"].as_str().unwrap()).unwrap();
        assert!(api.process_next_queued());
        let (status, standard) = api_json(
            &api,
            "GET",
            &format!("/api/v1/tasks/experiments/{standard_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(standard["status"], "succeeded", "{standard}");
        assert_eq!(standard["result_level"], "standard");
        assert_eq!(standard["result"]["action"], "promoted");

        let args = fs::read_to_string(args_path).unwrap();
        let args = args.lines().collect::<Vec<_>>();
        assert!(args.contains(&"promote"));
        assert!(!args.contains(&"run"));
        assert_eq!(
            args.iter()
                .filter(|argument| **argument == "standard")
                .count(),
            1
        );
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test(flavor = "current_thread")]
    async fn standard_task_does_not_fallback_when_promotion_replay_fails() {
        let dir = tempfile::tempdir().unwrap();
        let args_path = dir.path().join("cli-args.txt");
        let cli = dir.path().join("prajna-experiment");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> {}\nif [ \"$1\" = promote ]; then\n  printf 'Run run:sha256:synthetic failed during promotion: synthetic replay failure\\n' >&2\n  exit 1\nfi\nif [ \"$1\" = run ]; then\n  printf '{{\"exp\":\"exp:sha256:synthetic\",\"exe\":\"exe:sha256:synthetic\"}}\\n'\n  exit 0\nfi\nexit 91\n",
            shell_quote(&args_path)
        );
        fs::write(&cli, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let inspector = dir.path().join("empty-ps");
        fs::write(&inspector, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&inspector, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (api, _legacy, _fixture_dir) = fixture_with_cli_and_process_inspector(cli, inspector);
        let (status, accepted) = api_json(
            &api,
            "POST",
            "/api/v1/tasks/experiments",
            Some(json!({
                "idempotency_key": "promote-failure",
                "definition": {"name": "synthetic promote failure"},
                "result_level": "standard"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let task_id = Uuid::parse_str(accepted["task"]["task_id"].as_str().unwrap()).unwrap();
        assert!(api.process_next_queued());
        let (status, detail) = api_json(
            &api,
            "GET",
            &format!("/api/v1/tasks/experiments/{task_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["status"], "failed", "{detail}");
        assert_eq!(detail["error"]["code"], "experiment_cli_failed");
        assert_eq!(detail["error"]["details"]["exit_code"], 1);
        assert!(
            detail["error"]["details"]["stderr"]
                .as_str()
                .is_some_and(|stderr| stderr.contains("failed during promotion"))
        );
        let args = fs::read_to_string(args_path).unwrap();
        let args = args.lines().collect::<Vec<_>>();
        assert!(args.contains(&"promote"));
        assert!(!args.contains(&"run"));
        assert!(
            !api.0
                .output
                .join("tasks")
                .join(task_id.to_string())
                .join("cli-promote.stdout")
                .exists()
        );
    }
    #[test]
    fn only_a_missing_execution_promote_error_falls_back_to_run() {
        for (exit_code, expected) in [
            (Some(1), false),
            (Some(2), false),
            (Some(3), false),
            (Some(6), true),
            (None, false),
        ] {
            let error = RunError {
                code: "experiment_cli_failed".into(),
                message: "synthetic".into(),
                details: json!({"exit_code": exit_code}),
            };
            assert_eq!(promote_missing_execution(&error), expected, "{exit_code:?}");
        }
        let other = RunError {
            code: "process_group_cleanup_failed".into(),
            message: "synthetic".into(),
            details: json!({"exit_code": 6}),
        };
        assert!(!promote_missing_execution(&other));
    }
}
