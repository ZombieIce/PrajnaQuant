use crate::{
    experiment::{list_experiments, load_experiment},
    signal,
};
use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, sync::Arc, time::SystemTime};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
};

#[derive(Clone)]
struct AppState {
    output: Arc<PathBuf>,
}

pub async fn serve(address: &str, output: PathBuf, web_dist: PathBuf) -> Result<()> {
    let state = AppState {
        output: Arc::new(output),
    };
    let api = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/signals", get(signals))
        .route("/api/v1/signals/{key}", get(signal_definition))
        .route("/api/research/signals/{key}/report", get(signal_report))
        .route("/api/experiments", get(experiments))
        .route("/api/experiments/{id}", get(experiment))
        .with_state(state)
        .layer(CorsLayer::permissive());
    let app = api.fallback_service(
        ServeDir::new(&web_dist)
            .append_index_html_on_directories(true)
            .not_found_service(ServeFile::new(web_dist.join("index.html"))),
    );
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("bind {address}"))?;
    eprintln!("Prajna Quant: http://{address}");
    axum::serve(listener, app).await.context("run Axum server")
}

async fn health() -> Json<Value> {
    Json(json!({"status":"ok", "service":"prajna-quant"}))
}
async fn signals() -> Json<Vec<signal::SignalDefinition>> {
    Json(signal::registry())
}
async fn signal_definition(Path(key): Path<String>) -> ApiResponse {
    signal::definition(&key)
        .map(|value| Json(value).into_response())
        .unwrap_or_else(|| error(StatusCode::NOT_FOUND, "unknown signal"))
}
async fn signal_report(State(state): State<AppState>, Path(key): Path<String>) -> ApiResponse {
    match load_latest_signal_report(&state.output, &key) {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(StatusCode::NOT_FOUND, &e.to_string()),
    }
}
async fn experiments(State(state): State<AppState>) -> ApiResponse {
    match list_experiments(&state.output) {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}
async fn experiment(State(state): State<AppState>, Path(id): Path<String>) -> ApiResponse {
    match load_experiment(&state.output, &id) {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(StatusCode::NOT_FOUND, &e.to_string()),
    }
}
type ApiResponse = Response;
fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

fn load_latest_signal_report(root: &std::path::Path, signal_key: &str) -> Result<Value> {
    let directory = root.join("signal-reports");
    let mut candidates = fs::read_dir(&directory)
        .with_context(|| format!("read signal reports at {}", directory.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path().join("report.json");
            Some((path.metadata().ok()?.modified().ok()?, path))
        })
        .collect::<Vec<(SystemTime, PathBuf)>>();
    candidates.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in candidates {
        let value: Value = serde_json::from_slice(
            &fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )?;
        if value
            .pointer("/report/definition/key")
            .and_then(Value::as_str)
            == Some(signal_key)
        {
            return Ok(value);
        }
    }
    anyhow::bail!("no saved report for signal {signal_key}")
}
