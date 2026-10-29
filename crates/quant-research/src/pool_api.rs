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
struct PoolState {
    root: Option<PathBuf>,
    cli: PathBuf,
}
pub fn router(root: Option<PathBuf>, authority: String) -> Router {
    configured(
        PoolState {
            root,
            cli: std::env::var_os("PRAJNA_POOLS_CLI")
                .map(PathBuf::from)
                .unwrap_or_else(|| "prajna-pools".into()),
        },
        authority,
    )
}
fn configured(state: PoolState, authority: String) -> Router {
    Router::new()
        .route("/api/v1/research-pools", get(list).post(change))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(
            super::dataset_api::LocalPage::new(
                authority,
                "pool_origin_forbidden",
                "Pool access requires the configured local workbench origin.",
            ),
            super::dataset_api::local_page,
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
async fn list(State(state): State<PoolState>) -> Response {
    run(state, json!({"op":"list"})).await
}
async fn change(
    State(state): State<PoolState>,
    input: Result<Json<Value>, JsonRejection>,
) -> Response {
    match input {
        Ok(Json(value)) => run(state, value).await,
        Err(_) => failure(StatusCode::BAD_REQUEST, "invalid_pool"),
    }
}
async fn run(state: PoolState, input: Value) -> Response {
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
        return failure(StatusCode::SERVICE_UNAVAILABLE, "pool_cli_unavailable");
    };
    let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) else {
        return failure(StatusCode::BAD_GATEWAY, "invalid_pool_response");
    };
    if value["schema_version"] != 1 {
        return failure(StatusCode::BAD_GATEWAY, "invalid_pool_response");
    }
    let code = value["error"]["code"].as_str();
    let status = match (output.status.success(), code) {
        (true, None) if value["data"]["pools"].is_array() => StatusCode::OK,
        (false, Some("invalid_pool")) => StatusCode::BAD_REQUEST,
        (false, Some("pool_conflict")) => StatusCode::CONFLICT,
        (false, Some("pool_not_found")) => StatusCode::NOT_FOUND,
        (false, Some("invalid_dataset_root")) => StatusCode::SERVICE_UNAVAILABLE,
        (false, Some("pool_store_failed")) => StatusCode::INTERNAL_SERVER_ERROR,
        _ => return failure(StatusCode::BAD_GATEWAY, "invalid_pool_response"),
    };
    (status, [(header::CACHE_CONTROL, "no-store")], Json(value)).into_response()
}
