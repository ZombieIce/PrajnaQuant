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
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use duckdb::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::Write,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread,
};
use uuid::Uuid;

const MAX_QUEUED: usize = 8;
const MAX_DATE_SPAN_DAYS: i64 = 3661;
const MAX_SNAPSHOT_ROWS: i64 = 2_000_000;
const MAX_UNIVERSE_INSTRUMENTS: usize = 100;

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
    gate: Mutex<Store>,
    wake: Condvar,
    universes: Arc<UniverseStore>,
}
#[derive(Default)]
struct Store {
    jobs: HashMap<Uuid, RunJob>,
    idempotency: HashMap<String, (String, Uuid)>,
    queue: VecDeque<Uuid>,
    active: bool,
}

impl RunJobApi {
    pub fn start(output: PathBuf, universes: Arc<UniverseStore>) -> Self {
        let api = Self(Arc::new(Inner {
            output,
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
    pub fn router(self) -> Router {
        Router::new()
            .route("/api/v1/research-snapshots", get(list_snapshots))
            .route("/api/v1/runs", get(list_runs).post(submit))
            .route("/api/v1/runs/preflight", post(preflight))
            .route("/api/v1/runs/{id}", get(get_run))
            .with_state(self)
    }
    fn recover(&self) {
        let jobs_dir = self.0.output.join("runs");
        let Ok(entries) = fs::read_dir(&jobs_dir) else {
            return;
        };
        let mut store = self.0.gate.lock().expect("run store lock");
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
                        store.queue.push_back(job.run_id)
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
        store.queue.push_back(job.run_id);
        store.jobs.insert(job.run_id, job.clone());
        self.0.wake.notify_one();
        Ok((job, true))
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
    fn worker_loop(&self) {
        loop {
            let job = {
                let mut store = self.0.gate.lock().expect("run store lock");
                while store.queue.is_empty() {
                    store = self.0.wake.wait(store).expect("run store lock")
                }
                let id = store.queue.pop_front().expect("queued id");
                store.active = true;
                let job = store.jobs.get_mut(&id).expect("queued job");
                job.status = RunStatus::Running;
                job.stage = "validating_fixed_inputs".into();
                job.updated_at = chrono::Utc::now().to_rfc3339();
                let _ = persist_job(&self.0.output, job);
                job.clone()
            };
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
            store.active = false;
            self.0.wake.notify_all();
        }
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
        (self.1, Json(json!({"error":self.0}))).into_response()
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
fn read_request(directory: &FsPath) -> Option<SubmitRun> {
    serde_json::from_slice(&fs::read(directory.join("request.json")).ok()?).ok()
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
    use std::sync::Arc;

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
    fn fixture() -> (RunJobApi, SubmitRun, tempfile::TempDir) {
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
        let api = RunJobApi(Arc::new(Inner {
            output: root.into(),
            gate: Mutex::new(Store::default()),
            wake: Condvar::new(),
            universes: universe_store,
        }));
        (api, request, dir)
    }
}
