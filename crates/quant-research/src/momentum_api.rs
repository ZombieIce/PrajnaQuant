//! Thin local command adapter; publication/identity remain owned by the Rust CLI.
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
struct MomentumState {
    root: Option<PathBuf>,
    cli: PathBuf,
    jobs: super::RunJobApi,
}
impl super::RunJobApi {
    pub fn momentum_router(self, root: Option<PathBuf>, authority: String) -> Router {
        configured(
            MomentumState {
                root,
                jobs: self,
                cli: std::env::var_os("PRAJNA_MOMENTUM_CLI")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| "prajna-momentum".into()),
            },
            authority,
        )
    }
}
fn configured(state: MomentumState, authority: String) -> Router {
    Router::new()
        .route("/api/v1/momentum-research", get(list).post(change))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(
            super::super::dataset_api::LocalPage::new(
                authority,
                "momentum_origin_forbidden",
                "Momentum access requires the configured local workbench origin.",
            ),
            super::super::dataset_api::local_page,
        ))
}
fn failure(status: StatusCode, code: &str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"schema_version":1,"error":{"code":code,"message":code}})),
    )
        .into_response()
}
async fn list(State(state): State<MomentumState>) -> Response {
    run(state, json!({"op":"list"})).await
}
async fn change(
    State(state): State<MomentumState>,
    input: Result<Json<Value>, JsonRejection>,
) -> Response {
    match input {
        Ok(Json(mut value)) if value["op"] == "submit" => {
            let dataset = state.root.as_ref().and_then(|p| p.canonicalize().ok());
            if dataset.is_none() || dataset != state.jobs.0.output.canonicalize().ok() {
                return failure(StatusCode::SERVICE_UNAVAILABLE, "momentum_root_mismatch");
            }
            let key = value
                .as_object_mut()
                .and_then(|o| o.remove("idempotency_key"))
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let request = super::SubmitExperimentTask {
                idempotency_key: key,
                definition: value,
                result_level: None,
            };
            match state.jobs.submit_experiment_task(request) {
                Ok((task, created)) => (
                    if created {
                        StatusCode::ACCEPTED
                    } else {
                        StatusCode::OK
                    },
                    Json(json!({"task":task,"created":created})),
                )
                    .into_response(),
                Err(e) => e.into_response(),
            }
        }
        Ok(Json(value)) => run(state, value).await,
        Err(_) => failure(StatusCode::BAD_REQUEST, "invalid_momentum"),
    }
}
async fn run(state: MomentumState, input: Value) -> Response {
    let Some(root) = state.root else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "dataset_root_unconfigured");
    };
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
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
            .expect("piped stdin")
            .write_all(input.to_string().as_bytes())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        child.wait_with_output()
    })
    .await;
    let Ok(Ok(output)) = result else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "momentum_cli_unavailable");
    };
    let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) else {
        return failure(StatusCode::BAD_GATEWAY, "invalid_momentum_response");
    };
    if value["schema_version"] != 1 {
        return failure(StatusCode::BAD_GATEWAY, "invalid_momentum_response");
    }
    let code = value["error"]["code"].as_str();
    let status = match (output.status.success(), code) {
        (true, None) if value["data"].is_object() => StatusCode::OK,
        (false, Some("invalid_momentum")) => StatusCode::BAD_REQUEST,
        (false, Some("momentum_conflict")) => StatusCode::CONFLICT,
        (false, Some("momentum_not_found")) => StatusCode::NOT_FOUND,
        (false, Some("invalid_dataset_root")) => StatusCode::SERVICE_UNAVAILABLE,
        (false, Some("momentum_store_failed")) => StatusCode::INTERNAL_SERVER_ERROR,
        _ => return failure(StatusCode::BAD_GATEWAY, "invalid_momentum_response"),
    };
    (
        status,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_TYPE, "application/json"),
        ],
        output.stdout,
    )
        .into_response()
}
