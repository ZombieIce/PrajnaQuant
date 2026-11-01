//! Update preparation freezes local inputs; execution uses the existing serial task worker.
use super::{ExperimentTaskStatus, RunJobApi, SubmitExperimentTask};
use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Clone)]
struct UpdateState {
    root: Option<PathBuf>,
    cli: PathBuf,
    jobs: RunJobApi,
}
impl RunJobApi {
    pub fn pool_update_router(self, root: Option<PathBuf>, authority: String) -> Router {
        let state = UpdateState {
            root,
            jobs: self,
            cli: std::env::var_os("PRAJNA_POOL_UPDATE_CLI")
                .map(PathBuf::from)
                .unwrap_or_else(|| "prajna-pool-update".into()),
        };
        Router::new()
            .route("/api/v1/pool-updates", get(list).post(change))
            .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
            .with_state(state)
            .layer(axum::middleware::from_fn_with_state(
                super::super::dataset_api::LocalPage::new(
                    authority,
                    "pool_update_origin_forbidden",
                    "Updates require the configured local workbench origin.",
                ),
                super::super::dataset_api::local_page,
            ))
    }
}
fn failure(status: StatusCode, code: &str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"schema_version":1,"error":{"code":code,"message":code}})),
    )
        .into_response()
}
async fn invoke(state: UpdateState, input: Value) -> Result<Value, Box<Response>> {
    let root = state
        .root
        .ok_or_else(|| failure(StatusCode::SERVICE_UNAVAILABLE, "dataset_root_unconfigured"))?;
    let output = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
        let mut child = Command::new(state.cli)
            .arg("--lake")
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        if let Err(error) = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("missing stdin"))?
            .write_all(input.to_string().as_bytes())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        child.wait_with_output()
    })
    .await;
    let Ok(Ok(output)) = output else {
        return Err(failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "pool_update_cli_unavailable",
        )
        .into());
    };
    let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) else {
        return Err(failure(StatusCode::BAD_GATEWAY, "invalid_pool_update_response").into());
    };
    if value["schema_version"] != 1 {
        return Err(failure(StatusCode::BAD_GATEWAY, "invalid_pool_update_response").into());
    }
    if !output.status.success() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            [(header::CACHE_CONTROL, "no-store")],
            Json(value),
        )
            .into_response()
            .into());
    }
    if !value["data"].is_object() {
        return Err(failure(StatusCode::BAD_GATEWAY, "invalid_pool_update_response").into());
    }
    Ok(value)
}
async fn list(State(state): State<UpdateState>) -> Response {
    match invoke(state.clone(), json!({"op":"list"})).await {
        Ok(mut value) => {
            let tasks = state
                .jobs
                .list_experiment_tasks()
                .into_iter()
                .filter(|t| t.task_type == "prajna_pool_update")
                .map(|task| {
                    let request = super::read_experiment_request(&super::experiment_task_dir(
                        &state.jobs.0.output,
                        task.task_id,
                    ));
                    json!({"attempt":request.map(|r| r.definition["attempt"].clone()),"task":task})
                })
                .collect::<Vec<_>>();
            value["data"]["tasks"] = json!(tasks);
            ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response()
        }
        Err(response) => *response,
    }
}
async fn change(
    State(state): State<UpdateState>,
    input: Result<Json<Value>, JsonRejection>,
) -> Response {
    let Ok(Json(mut input)) = input else {
        return failure(StatusCode::BAD_REQUEST, "invalid_pool_update");
    };
    if !matches!(input["op"].as_str(), Some("prepare" | "retry")) {
        return failure(StatusCode::BAD_REQUEST, "invalid_pool_update_operation");
    }
    let root = state.root.as_ref().and_then(|p| p.canonicalize().ok());
    if root.is_none() || root != state.jobs.0.output.canonicalize().ok() {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "pool_update_root_mismatch");
    }
    if input["op"] == "retry" {
        if input.get("interrupted").is_some() {
            return failure(StatusCode::BAD_REQUEST, "invalid_pool_update");
        }
        let mut interrupted = true;
        for task in state.jobs.list_experiment_tasks() {
            if task.task_type == "prajna_pool_update"
                && super::read_experiment_request(&super::experiment_task_dir(
                    &state.jobs.0.output,
                    task.task_id,
                ))
                .is_some_and(|r| r.definition["attempt"] == input["attempt"])
            {
                if matches!(
                    task.status,
                    ExperimentTaskStatus::Queued | ExperimentTaskStatus::Running
                ) {
                    return failure(StatusCode::CONFLICT, "pool_update_still_active");
                }
                interrupted = matches!(
                    task.status,
                    ExperimentTaskStatus::Interrupted | ExperimentTaskStatus::Failed
                );
            }
        }
        input["interrupted"] = json!(interrupted);
    }
    let value = match invoke(state.clone(), input).await {
        Ok(v) => v,
        Err(response) => return *response,
    };
    let Some(id) = value["data"]["attempt"]["id"].as_str() else {
        return failure(StatusCode::BAD_GATEWAY, "invalid_pool_update_response");
    };
    let request = SubmitExperimentTask {
        idempotency_key: format!("pool-update:{id}"),
        definition: json!({"op":"pool_update","attempt":id}),
        result_level: None,
    };
    match state.jobs.submit_experiment_task(request) {
        Ok((task, created)) => (
            StatusCode::ACCEPTED,
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({"task":task,"created":created,"attempt":value["data"]["attempt"]})),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}
