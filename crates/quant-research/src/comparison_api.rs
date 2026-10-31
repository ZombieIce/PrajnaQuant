//! Frozen strategy-benchmark comparison task API. Definition derivation stays
//! behind the production comparison CLI; this adapter owns the persistent role
//! relation and the read-side verification envelope.
use super::*;
use axum::extract::rejection::JsonRejection;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitComparison {
    pub idempotency_key: String,
    pub selection: Value,
    pub reinvestment_calendar: String,
}

fn fail(status: StatusCode, code: &str, message: &str, details: Value) -> ApiFailure {
    ApiFailure::new(status, code, message, details)
}
fn bad(code: &str, message: &str) -> ApiFailure {
    fail(StatusCode::BAD_REQUEST, code, message, Value::Null)
}
fn conflict(code: &str, message: &str) -> ApiFailure {
    fail(StatusCode::CONFLICT, code, message, Value::Null)
}

fn validate_comparison(request: &SubmitComparison) -> Result<(), ApiFailure> {
    if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 128 {
        return Err(bad(
            "invalid_comparison",
            "idempotency_key must contain 1 to 128 characters",
        ));
    }
    if !request.selection.is_object() {
        return Err(bad("invalid_comparison", "selection must be a JSON object"));
    }
    if !matches!(
        request.reinvestment_calendar.as_str(),
        "strategy_rebalance" | "none"
    ) {
        return Err(bad(
            "invalid_comparison",
            "reinvestment_calendar must be strategy_rebalance or none",
        ));
    }
    Ok(())
}

fn comparison_hash(comparison: &Value) -> Result<String, ApiFailure> {
    let value = json!({"task_type": COMPARISON_TASK_TYPE, "task_version": EXPERIMENT_TASK_VERSION, "definition": comparison});
    serde_json::to_vec(&value)
        .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
        .map_err(ApiFailure::from_error)
}

fn read_comparison_request(directory: &FsPath) -> Option<Value> {
    serde_json::from_slice(&fs::read(directory.join("comparison.json")).ok()?).ok()
}

fn persist_comparison_request(
    root: &FsPath,
    task_id: Uuid,
    comparison: &Value,
) -> Result<(), ApiFailure> {
    let directory = experiment_task_dir(root, task_id);
    fs::create_dir_all(&directory).map_err(ApiFailure::from_error)?;
    atomic_write(
        &directory.join("comparison.json"),
        &serde_json::to_vec_pretty(comparison).map_err(ApiFailure::from_error)?,
    )
    .map_err(ApiFailure::from_error)
}

fn freeze_status(code: &str) -> StatusCode {
    match code {
        "invalid_request"
        | "invalid_selection"
        | "unsupported_venue"
        | "invalid_definition"
        | "adjusted_raw_dsv_mismatch"
        | "adjusted_instrument_mismatch" => StatusCode::BAD_REQUEST,
        "dataset_not_found" | "instrument_not_in_dataset" | "pool_not_found" => {
            StatusCode::NOT_FOUND
        }
        "selection_evidence_missing"
        | "adjusted_version_missing"
        | "adjusted_series_empty"
        | "evaluation_window_not_runnable"
        | "invalid_venue_selection"
        | "inconsistent_venue_calendar"
        | "comparison_requires_corporate_actions"
        | "comparison_requires_research_window"
        | "comparison_unsupported_strategy"
        | "comparison_requires_single_run"
        | "comparison_requires_single_venue"
        | "comparison_requires_fast_event"
        | "comparison_price_rules_mismatch"
        | "comparison_benchmark_invalid"
        | "comparison_definition_mismatch"
        | "comparison_window_invalid"
        | "comparison_definition_invalid" => StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_dataset_root" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_GATEWAY,
    }
}

impl RunJobApi {
    pub fn comparison_router(self, authority: String) -> Router {
        Router::new()
            .route("/api/v1/tasks/comparisons", get(list_comparisons).post(submit_comparison))
            .route("/api/v1/tasks/comparisons/{id}", get(get_comparison))
            .route("/api/v1/tasks/comparisons/{id}/result", get(get_comparison_result))
            .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
            .with_state(self)
            .layer(axum::middleware::from_fn_with_state(
                crate::server::dataset_api::LocalPage::new(
                    authority,
                    "comparison_origin_forbidden",
                    "Comparison submission and results are restricted to the configured local workbench origin.",
                ),
                crate::server::dataset_api::local_page,
            ))
            .layer(axum::middleware::map_response(|mut response: Response| async move {
                response.headers_mut().insert(header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-store"));
                response
            }))
    }

    fn submit_frozen_comparison(
        &self,
        idempotency_key: &str,
        comparison: Value,
    ) -> Result<(ExperimentTask, Value, bool), ApiFailure> {
        let request_hash = comparison_hash(&comparison)?;
        let mut store = self
            .0
            .gate
            .lock()
            .map_err(|_| ApiFailure::internal("task store lock poisoned"))?;
        if let Some((old_hash, task_id)) = store.task_idempotency.get(idempotency_key) {
            if old_hash != &request_hash {
                return Err(conflict(
                    "idempotency_conflict",
                    "idempotency_key was already used for a different comparison",
                ));
            }
            let task = store.tasks.get(task_id).cloned().ok_or_else(|| {
                ApiFailure::internal("comparison idempotency index is inconsistent")
            })?;
            let existing = read_comparison_request(&experiment_task_dir(&self.0.output, *task_id))
                .ok_or_else(|| {
                    fail(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "comparison_definition_missing",
                        "persisted comparison definition is missing or invalid",
                        Value::Null,
                    )
                })?;
            return Ok((task, existing, false));
        }
        if store.queue.len() + usize::from(store.active) >= MAX_QUEUED {
            return Err(fail(
                StatusCode::TOO_MANY_REQUESTS,
                "queue_full",
                "run queue is full; retry after an existing job finishes",
                Value::Null,
            ));
        }
        let now = chrono::Utc::now().to_rfc3339();
        let task = ExperimentTask {
            task_id: Uuid::new_v4(),
            task_type: COMPARISON_TASK_TYPE.into(),
            task_version: EXPERIMENT_TASK_VERSION,
            status: ExperimentTaskStatus::Queued,
            stage: "queued".into(),
            created_at: now.clone(),
            updated_at: now,
            request_sha256: request_hash.clone(),
            idempotency_key: idempotency_key.into(),
            result_level: "standard".into(),
            process_group_id: None,
            process_group_generation: None,
            result: None,
            error: None,
        };
        persist_comparison_request(&self.0.output, task.task_id, &comparison)?;
        persist_experiment_task(&self.0.output, &task)?;
        store
            .task_idempotency
            .insert(idempotency_key.into(), (request_hash, task.task_id));
        store
            .queue
            .push_back(QueueItem::ExperimentTask(task.task_id));
        store.tasks.insert(task.task_id, task.clone());
        self.0.wake.notify_one();
        Ok((task, comparison, true))
    }

    async fn freeze_comparison(&self, request: &SubmitComparison) -> Result<Value, ApiFailure> {
        let cli = std::env::var_os("PRAJNA_COMPARISON_CLI")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                self.0
                    .experiment_cli
                    .with_file_name("prajna-freeze-comparison")
            });
        let lake = self.0.output.clone();
        let input = json!({"selection": request.selection, "reinvestment_calendar": request.reinvestment_calendar}).to_string();
        let output = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
            let mut child = Command::new(cli)
                .arg("--lake")
                .arg(lake)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            child
                .stdin
                .take()
                .expect("piped stdin")
                .write_all(input.as_bytes())?;
            child.wait_with_output()
        })
        .await;
        let output = match output {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                return Err(fail(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "comparison_cli_unavailable",
                    "comparison CLI could not be executed",
                    json!({"cause": error.to_string()}),
                ));
            }
            Err(error) => {
                return Err(fail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "comparison_cli_join_failed",
                    "comparison CLI stopped unexpectedly",
                    json!({"cause": error.to_string()}),
                ));
            }
        };
        let envelope = serde_json::from_slice::<Value>(&output.stdout).map_err(|_| fail(
            StatusCode::BAD_GATEWAY, "invalid_comparison_response", "comparison CLI returned invalid JSON",
            json!({"stderr": String::from_utf8_lossy(&output.stderr).chars().take(4096).collect::<String>()}),
        ))?;
        if !output.status.success() {
            let code = envelope["error"]["code"]
                .as_str()
                .unwrap_or("invalid_comparison");
            return Err(fail(
                freeze_status(code),
                code,
                envelope["error"]["message"]
                    .as_str()
                    .unwrap_or("comparison freeze was rejected"),
                envelope["error"].clone(),
            ));
        }
        if envelope["schema_version"] != 1
            || envelope["data"]["semantic_version"] != "strategy-benchmark-comparison@1"
            || !envelope["data"]["strategy"]["definition"].is_object()
            || !envelope["data"]["benchmark"]["definition"].is_object()
            || !envelope["data"]["comparability"].is_object()
        {
            return Err(fail(
                StatusCode::BAD_GATEWAY,
                "invalid_comparison_response",
                "comparison CLI returned an unsupported envelope",
                Value::Null,
            ));
        }
        Ok(envelope["data"].clone())
    }

    pub(super) fn execute_comparison_task(
        &self,
        task: &ExperimentTask,
        directory: &FsPath,
    ) -> Result<Value, RunError> {
        let comparison = read_comparison_request(directory).ok_or_else(|| {
            run_error(
                "comparison_definition_missing",
                "persisted comparison definition is missing or invalid",
            )
        })?;
        if comparison_hash(&comparison).map_err(|error| error.0)? != task.request_sha256 {
            return Err(run_error(
                "comparison_definition_changed",
                "persisted comparison definition does not match its frozen identity",
            ));
        }
        let write_definition = |role: &str| -> Result<PathBuf, RunError> {
            let value = comparison[role]["definition"].as_object().ok_or_else(|| {
                run_error(
                    "comparison_definition_invalid",
                    &format!("comparison {role} definition is missing"),
                )
            })?;
            let path = directory.join(format!("{role}.json"));
            let bytes = serde_json::to_vec_pretty(&Value::Object(value.clone()))
                .map_err(|error| run_error("comparison_definition_invalid", &error.to_string()))?;
            if path.exists() {
                let existing = fs::read(&path).map_err(|error| {
                    run_error("task_definition_read_failed", &error.to_string())
                })?;
                if existing != bytes {
                    return Err(run_error(
                        "comparison_definition_changed",
                        &format!("persisted {role} definition changed"),
                    ));
                }
            } else {
                atomic_write(&path, &bytes).map_err(|error| {
                    run_error("task_definition_write_failed", &error.to_string())
                })?;
            }
            Ok(path)
        };
        let strategy_path = write_definition("strategy")?;
        let benchmark_path = write_definition("benchmark")?;
        let level = |role: &str| {
            comparison[role]["result_level"]
                .as_str()
                .unwrap_or("standard")
                .to_owned()
        };
        let mut outputs = Vec::new();
        for (role, path) in [("strategy", strategy_path), ("benchmark", benchmark_path)] {
            let result = self.execute_experiment_cli(
                task,
                directory,
                vec![
                    OsString::from("run"),
                    path.into_os_string(),
                    OsString::from("--lake"),
                    self.0.output.as_os_str().to_owned(),
                    OsString::from("--level"),
                    OsString::from(level(role)),
                ],
                &format!("{role}.stdout"),
            )?;
            if result["failed_count"].as_u64().unwrap_or(1) != 0
                || result["run_count"].as_u64().unwrap_or(0) == 0
            {
                return Err(run_error(
                    "comparison_run_failed",
                    &format!("{role} execution contains failed Runs"),
                ));
            }
            outputs.push((role, result));
        }
        let (_, strategy) = outputs.remove(0);
        let (_, benchmark) = outputs.remove(0);
        let result = json!({
            "semantic_version": comparison["semantic_version"],
            "strategy": {"exp": strategy["exp"], "exe": strategy["exe"], "failed_count": strategy["failed_count"], "run_count": strategy["run_count"], "result_level": level("strategy")},
            "benchmark": {"exp": benchmark["exp"], "exe": benchmark["exe"], "failed_count": benchmark["failed_count"], "run_count": benchmark["run_count"], "result_level": level("benchmark")},
        });
        atomic_write(
            &experiment_task_dir(&self.0.output, task.task_id).join("result.json"),
            &serde_json::to_vec_pretty(&result)
                .map_err(|error| run_error("task_result_persist_failed", &error.to_string()))?,
        )
        .map_err(|error| run_error("task_result_persist_failed", &error.to_string()))?;
        Ok(result)
    }
}

fn comparison_task(api: &RunJobApi, id: &str) -> Result<ExperimentTask, ApiFailure> {
    let id =
        Uuid::parse_str(id).map_err(|_| bad("invalid_request", "invalid comparison task id"))?;
    api.get_experiment_task(id)
        .filter(|task| task.task_type == COMPARISON_TASK_TYPE)
        .ok_or_else(|| {
            fail(
                StatusCode::NOT_FOUND,
                "not_found",
                "comparison task not found",
                Value::Null,
            )
        })
}

async fn list_comparisons(State(api): State<RunJobApi>) -> Response {
    Json(json!({"items": api.list_experiment_tasks().into_iter().filter(|task| task.task_type == COMPARISON_TASK_TYPE).collect::<Vec<_>>()})).into_response()
}
async fn get_comparison(State(api): State<RunJobApi>, Path(id): Path<String>) -> Response {
    match comparison_task(&api, &id) {
        Ok(task) => Json(task).into_response(),
        Err(error) => error.into_response(),
    }
}
async fn submit_comparison(
    State(api): State<RunJobApi>,
    request: Result<Json<SubmitComparison>, JsonRejection>,
) -> Response {
    let Ok(Json(request)) = request else {
        return bad("invalid_comparison", "Invalid comparison request fields.").into_response();
    };
    if let Err(error) = validate_comparison(&request) {
        return error.into_response();
    }
    let comparison = match api.freeze_comparison(&request).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    match api.submit_frozen_comparison(&request.idempotency_key, comparison) {
        Ok((task, comparison, created)) => (
            (if created {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            }),
            Json(json!({"task": task, "comparison": comparison, "created": created})),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}

async fn read_result_cli(api: &RunJobApi, exp: String, exe: String) -> Result<Value, ApiFailure> {
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
            return Err(fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "experiment_result_unavailable",
                "prajna-experiment result reader could not be executed",
                json!({"cause": error.to_string()}),
            ));
        }
        Err(error) => {
            return Err(fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "experiment_result_join_failed",
                "prajna-experiment result reader stopped unexpectedly",
                json!({"cause": error.to_string()}),
            ));
        }
    };
    if !output.status.success() {
        return Err(fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "experiment_result_failed",
            "prajna-experiment could not read the persisted result",
            json!({"exit_code": output.status.code(), "stderr": String::from_utf8_lossy(&output.stderr).chars().take(4096).collect::<String>()}),
        ));
    }
    let value = serde_json::from_slice::<Value>(&output.stdout).map_err(|_| {
        fail(
            StatusCode::BAD_GATEWAY,
            "invalid_result_response",
            "prajna-experiment result returned invalid JSON",
            Value::Null,
        )
    })?;
    if value["schema_version"] != 1 || !value["data"].is_object() {
        return Err(fail(
            StatusCode::BAD_GATEWAY,
            "invalid_result_response",
            "prajna-experiment result returned an unsupported envelope",
            Value::Null,
        ));
    }
    Ok(value)
}

fn close_dates(result: &Value) -> Result<Vec<String>, String> {
    let rows = result["data"]["runs"]
        .as_array()
        .and_then(|runs| runs.first())
        .and_then(|run| run["series"]["account_valuations"].as_array())
        .ok_or_else(|| "missing account_valuations".to_owned())?;
    let dates = rows
        .iter()
        .filter(|row| row["valuation_point"] == "close")
        .map(|row| {
            row["session_date"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "close row without session_date".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if dates.is_empty() {
        return Err("no close valuations".into());
    }
    if dates
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != dates.len()
    {
        return Err("duplicate close valuation dates".into());
    }
    Ok(dates)
}

fn same_number(left: &Value, right: &Value) -> bool {
    match (left.as_f64(), right.as_f64()) {
        (Some(left), Some(right)) => {
            (left - right).abs() <= 1e-12 * left.abs().max(right.abs()).max(1.0)
        }
        _ => left == right,
    }
}

/// Verify the opt-in complete Summary and calendar report on a frozen role.
/// Legacy comparisons without the binding stay readable; new frozen roles must
/// have a persisted report, and incomplete reports are surfaced as reasons
/// instead of becoming zero performance.
fn verify_complete_report(
    comparison: &Value,
    role: &str,
    summary: &Value,
    reasons: &mut Vec<String>,
    mismatches: &mut Vec<String>,
) {
    let Some(binding) = comparison[role]["definition"].get("summary") else {
        return;
    };
    if !summary["complete"].is_object() {
        mismatches.push(format!("{role} complete Summary is missing"));
        return;
    }
    let complete = &summary["complete"];
    if complete["semantic_version"] != binding["semantic_version"] {
        mismatches.push(format!("{role} complete Summary version changed"));
    }
    let Some(calendar_version) = binding["calendar"].as_str() else {
        return;
    };
    if !complete["calendar"].is_object() {
        mismatches.push(format!("{role} calendar report is missing"));
        return;
    }
    let calendar = &complete["calendar"];
    if calendar["semantic_version"].as_str() != Some(calendar_version) {
        mismatches.push(format!("{role} calendar report version changed"));
    }
    if calendar["valuation_point"] != "close" {
        mismatches.push(format!("{role} calendar valuation point changed"));
    }
    let evaluation = &comparison["comparability"]["evaluation"];
    if calendar["evaluation_start"] != evaluation["start"]
        || calendar["evaluation_end"] != evaluation["end"]
    {
        mismatches.push(format!("{role} calendar evaluation window changed"));
    }
    if calendar["initial_capital"] != comparison["comparability"]["initial_cash"] {
        mismatches.push(format!("{role} calendar initial capital changed"));
    }
    if calendar["available"] != true {
        reasons.push(format!(
            "{role}:calendar_unavailable:{}",
            calendar["reason"].as_str().unwrap_or("unknown")
        ));
        return;
    }
    if calendar["points"].as_array().map(Vec::len)
        != calendar["sessions"].as_u64().map(|n| n as usize)
    {
        mismatches.push(format!("{role} calendar session count changed"));
    }
    if calendar["monthly"].as_array().is_none_or(Vec::is_empty)
        || calendar["annual"].as_array().is_none_or(Vec::is_empty)
    {
        mismatches.push(format!("{role} calendar periods are missing"));
    }
    if !same_number(&calendar["max_drawdown"], &summary["max_drawdown"])
        || calendar["drawdown_peak_session"] != summary["drawdown_peak_session"]
        || calendar["drawdown_trough_session"] != summary["drawdown_trough_session"]
    {
        mismatches.push(format!(
            "{role} calendar drawdown disagrees with its Summary"
        ));
    }
    if let Some(nav) = calendar["points"]
        .as_array()
        .and_then(|points| points.last())
        .and_then(|point| point["nav"].as_f64())
    {
        if let Some(total) = complete["metrics"]["total_return"]["value"].as_f64() {
            if (nav - 1.0 - total).abs() > 1e-12 {
                mismatches.push(format!(
                    "{role} calendar endpoints disagree with its total return"
                ));
            }
        }
    }
}

fn verify_comparison(
    comparison: &Value,
    strategy: &Value,
    benchmark: &Value,
) -> Result<Value, ApiFailure> {
    let start = comparison["comparability"]["evaluation"]["start"]
        .as_str()
        .ok_or_else(|| ApiFailure::internal("comparison evaluation start is missing"))?;
    let end = comparison["comparability"]["evaluation"]["end"]
        .as_str()
        .ok_or_else(|| ApiFailure::internal("comparison evaluation end is missing"))?;
    let caf = comparison["comparability"]["corporate_actions"]["version"].as_str();
    let mut mismatches = Vec::new();
    let mut reasons = Vec::new();
    if comparison["comparability"]["validation_version"] != "effective-rules-calendar@2" {
        reasons.push("comparison:effective_rules_calendar_unverified".into());
    }
    let mut date_sets = Vec::new();
    for (role, result) in [("strategy", strategy), ("benchmark", benchmark)] {
        let expected_exp = comparison[role]["experiment"].as_str().unwrap_or_default();
        let expected_run = comparison[role]["run"].as_str().unwrap_or_default();
        if result["data"]["exp"].as_str() != Some(expected_exp) {
            mismatches.push(format!("{role} experiment identity changed"));
        }
        let runs = result["data"]["runs"].as_array();
        let Some(run) = runs.and_then(|runs| (runs.len() == 1).then(|| &runs[0])) else {
            mismatches.push(format!("{role} must have exactly one Run"));
            continue;
        };
        if run["run_id"].as_str() != Some(expected_run) {
            mismatches.push(format!("{role} Run identity changed"));
        }
        if run["status"] != "completed" {
            mismatches.push(format!("{role} Run is not completed"));
        }
        if run["accounting_verified"] != true {
            reasons.push(format!("{role}:accounting_unverified"));
        }
        let summary = &run["summary"];
        if !summary.is_object() {
            mismatches.push(format!("{role} summary is missing"));
        } else {
            if summary["return_basis"] != "total_return" {
                reasons.push(format!("{role}:return_basis_missing"));
            }
            if summary["corporate_action_schedule_version"].as_str() != caf {
                reasons.push(format!("{role}:corporate_action_version_mismatch"));
            }
            if summary["corporate_action_completeness"]["is_complete"] != true {
                reasons.push(format!("{role}:corporate_actions_incomplete"));
            }
        }
        verify_complete_report(comparison, role, summary, &mut reasons, &mut mismatches);
        match close_dates(result) {
            Ok(dates) => {
                if dates.first().map(String::as_str) != Some(start)
                    || dates.last().map(String::as_str) != Some(end)
                {
                    mismatches.push(format!("{role} evaluation window changed"));
                }
                if let Some(count) = summary["session_count"].as_u64() {
                    if count != dates.len() as u64 {
                        mismatches.push(format!("{role} Session count changed"));
                    }
                }
                date_sets.push(dates);
            }
            Err(error) => mismatches.push(format!("{role}: {error}")),
        }
    }
    if date_sets.len() == 2 && date_sets[0] != date_sets[1] {
        mismatches.push("strategy and benchmark close dates differ".into());
    }
    if !mismatches.is_empty() {
        return Err(fail(
            StatusCode::CONFLICT,
            "comparison_result_mismatch",
            "strategy and benchmark results are not a valid frozen comparison",
            json!({"reasons": mismatches}),
        ));
    }
    Ok(json!({
        "comparable": reasons.is_empty(), "reasons": reasons,
        "window": {"start": start, "end": end, "session_count": date_sets.first().map(Vec::len).unwrap_or(0)},
        "role_identities_verified": true,
    }))
}

fn role_view(comparison: &Value, role: &str, result: &Value) -> Value {
    let mut view = result["data"].clone();
    view["role"] = json!(role);
    view["definition"] = comparison[role]["definition"].clone();
    view["result_level"] = comparison[role]["result_level"].clone();
    view["kind"] = if role == "benchmark" {
        comparison["benchmark"]["kind"].clone()
    } else {
        comparison["comparability"]["strategy_kind"].clone()
    };
    view
}

async fn get_comparison_result(State(api): State<RunJobApi>, Path(id): Path<String>) -> Response {
    let task = match comparison_task(&api, &id) {
        Ok(task) => task,
        Err(error) => return error.into_response(),
    };
    if !matches!(task.status, ExperimentTaskStatus::Succeeded) {
        return fail(
            StatusCode::CONFLICT,
            "task_not_completed",
            "comparison task has not succeeded",
            json!({"status": task.status}),
        )
        .into_response();
    }
    let Some(result) = task.result.as_ref() else {
        return fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "task_result_missing",
            "succeeded comparison task has no persisted result identity",
            Value::Null,
        )
        .into_response();
    };
    let Some(benchmark) = result.get("benchmark") else {
        return conflict(
            "comparison_benchmark_missing",
            "comparison result has no saved benchmark role",
        )
        .into_response();
    };
    let Some(strategy) = result.get("strategy") else {
        return conflict(
            "comparison_strategy_missing",
            "comparison result has no saved strategy role",
        )
        .into_response();
    };
    let identities = |role: &Value| -> Option<(String, String)> {
        Some((
            role["exp"].as_str()?.to_owned(),
            role["exe"].as_str()?.to_owned(),
        ))
    };
    let Some((strategy_exp, strategy_exe)) = identities(strategy) else {
        return ApiFailure::internal("comparison strategy identities are invalid").into_response();
    };
    let Some((benchmark_exp, benchmark_exe)) = identities(benchmark) else {
        return ApiFailure::internal("comparison benchmark identities are invalid").into_response();
    };
    let Some(comparison) =
        read_comparison_request(&experiment_task_dir(&api.0.output, task.task_id))
    else {
        return fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "comparison_definition_missing",
            "persisted comparison definition is missing or invalid",
            Value::Null,
        )
        .into_response();
    };
    if comparison["benchmark"]["definition"].is_null()
        || comparison["benchmark"]["experiment"].as_str().is_none()
        || comparison["benchmark"]["run"].as_str().is_none()
    {
        return conflict(
            "comparison_benchmark_missing",
            "frozen comparison has no benchmark role",
        )
        .into_response();
    }
    if comparison["strategy"]["definition"].is_null()
        || comparison["strategy"]["experiment"].as_str().is_none()
        || comparison["strategy"]["run"].as_str().is_none()
    {
        return conflict(
            "comparison_strategy_missing",
            "frozen comparison has no strategy role",
        )
        .into_response();
    }
    let Ok(comparison_digest) = comparison_hash(&comparison) else {
        return ApiFailure::internal("comparison identity could not be computed").into_response();
    };
    if comparison_digest != task.request_sha256 {
        return conflict(
            "comparison_definition_changed",
            "persisted comparison definition no longer matches its frozen identity",
        )
        .into_response();
    }
    let strategy_result = match read_result_cli(&api, strategy_exp, strategy_exe).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let benchmark_result = match read_result_cli(&api, benchmark_exp, benchmark_exe).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let verification = match verify_comparison(&comparison, &strategy_result, &benchmark_result) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let body = json!({"schema_version": 1, "data": {
        "semantic_version": comparison["semantic_version"], "comparability": comparison["comparability"],
        "verification": verification,
        "strategy": role_view(&comparison, "strategy", &strategy_result),
        "benchmark": role_view(&comparison, "benchmark", &benchmark_result),
    }});
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(body),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_comparison_cannot_claim_effective_rule_or_calendar_verification() {
        let mut comparison = json!({"strategy":{"experiment":"exp","run":"run"},
            "benchmark":{"experiment":"exp","run":"run"},
            "comparability":{"evaluation":{"start":"2026-01-05","end":"2026-01-05"},
                "corporate_actions":{"version":"caf"},"validation_version":"effective-rules-calendar@2"}});
        let result = json!({"data":{"exp":"exp","runs":[{"run_id":"run","status":"completed",
            "accounting_verified":true,"summary":{"return_basis":"total_return","session_count":1,
                "corporate_action_schedule_version":"caf","corporate_action_completeness":{"is_complete":true}},
            "series":{"account_valuations":[{"session_date":"2026-01-05","valuation_point":"close"}]}}]}});
        assert_eq!(
            verify_comparison(&comparison, &result, &result).unwrap()["comparable"],
            true
        );
        comparison["comparability"]
            .as_object_mut()
            .unwrap()
            .remove("validation_version");
        let verification = verify_comparison(&comparison, &result, &result).unwrap();
        assert_eq!(verification["comparable"], false);
        assert_eq!(
            verification["reasons"],
            json!(["comparison:effective_rules_calendar_unverified"])
        );
        assert_eq!(
            freeze_status("comparison_price_rules_mismatch"),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn complete_calendar_report_is_required_executed_and_checked_against_summary() {
        let calendar = json!({
            "semantic_version": "calendar-performance@1", "valuation_point": "close",
            "initial_capital": "1000", "evaluation_start": "2026-01-05",
            "evaluation_end": "2026-01-05", "sessions": 1, "available": true,
            "max_drawdown": 0.0, "drawdown_peak_session": null, "drawdown_trough_session": null,
            "points": [{"session_date": "2026-01-05", "equity": "1000", "nav": 1.0, "drawdown": 0.0}],
            "monthly": [{"label": "2026-01", "start_basis": "initial_capital", "start_session": null,
                "end_session": "2026-01-05", "return_value": 0.0}],
            "annual": [{"label": "2026", "start_basis": "initial_capital", "start_session": null,
                "end_session": "2026-01-05", "return_value": 0.0}],
            "missing_months": []
        });
        let summary = json!({"return_basis":"total_return","session_count":1,
            "corporate_action_schedule_version":"caf",
            "corporate_action_completeness":{"is_complete":true},
            "max_drawdown":0.0,"drawdown_peak_session":null,"drawdown_trough_session":null,
            "complete":{"semantic_version":"complete-performance@1","currency":"CNY",
                "metrics":{"total_return":{"value":0.0,"reason":null}},"calendar":calendar}});
        let definition = json!({"summary":{"semantic_version":"complete-performance@1",
            "calendar":"calendar-performance@1"}});
        let comparison = json!({
            "strategy":{"experiment":"exp-s","run":"run-s","definition":definition},
            "benchmark":{"experiment":"exp-b","run":"run-b","definition":definition},
            "comparability":{"evaluation":{"start":"2026-01-05","end":"2026-01-05"},
                "corporate_actions":{"version":"caf"},"initial_cash":"1000",
                "validation_version":"effective-rules-calendar@2"}});
        let result = |exp: &str, run: &str, summary: &Value| {
            json!({"data":{"exp":exp,"runs":[{
            "run_id":run,"status":"completed","accounting_verified":true,"summary":summary,
            "series":{"account_valuations":[{"session_date":"2026-01-05","valuation_point":"close"}]}}]}})
        };
        let strategy = result("exp-s", "run-s", &summary);
        let benchmark = result("exp-b", "run-b", &summary);
        let verified = verify_comparison(&comparison, &strategy, &benchmark).unwrap();
        assert_eq!(verified["comparable"], true);

        let mut missing = summary.clone();
        missing["complete"]
            .as_object_mut()
            .unwrap()
            .remove("calendar");
        let failure =
            verify_comparison(&comparison, &result("exp-s", "run-s", &missing), &benchmark)
                .unwrap_err();
        assert_eq!(failure.0.code, "comparison_result_mismatch");
        assert!(
            failure.0.details["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason.as_str().unwrap().contains("calendar"))
        );

        let mut incomplete = summary.clone();
        incomplete["complete"]["calendar"]["available"] = json!(false);
        incomplete["complete"]["calendar"]["reason"] = json!("corporate_actions_incomplete");
        incomplete["complete"]["calendar"]["points"] = json!([]);
        incomplete["complete"]["calendar"]["monthly"] = json!([]);
        incomplete["complete"]["calendar"]["annual"] = json!([]);
        incomplete["complete"]["calendar"]["max_drawdown"] = Value::Null;
        let verified = verify_comparison(
            &comparison,
            &result("exp-s", "run-s", &incomplete),
            &benchmark,
        )
        .unwrap();
        assert_eq!(verified["comparable"], false);
        assert!(
            verified["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason
                    .as_str()
                    .unwrap()
                    .contains("calendar_unavailable:corporate_actions_incomplete"))
        );

        let mut mismatched = summary;
        mismatched["complete"]["calendar"]["max_drawdown"] = json!(-0.5);
        let failure = verify_comparison(
            &comparison,
            &result("exp-s", "run-s", &mismatched),
            &benchmark,
        )
        .unwrap_err();
        assert_eq!(failure.0.code, "comparison_result_mismatch");
    }
}
