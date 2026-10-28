//! Thin selection adapter; the independent Rust CLI owns all quant semantics.
use super::*;
use axum::extract::rejection::JsonRejection;

#[derive(Clone)]
struct SelectionState {
    api: RunJobApi,
    root: Option<PathBuf>,
    cli: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    idempotency_key: String,
    dsv: String,
    instrument: String,
    strategy: String,
    #[serde(default, deserialize_with = "present_settings")]
    settings: Option<Value>,
    result_level: Option<String>,
}

fn present_settings<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

impl RunJobApi {
    pub fn selection_router(self, root: Option<PathBuf>, authority: String) -> Router {
        configured(
            SelectionState {
                api: self,
                root,
                cli: std::env::var_os("PRAJNA_SELECTION_CLI")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| "prajna-freeze-selection".into()),
            },
            authority,
        )
    }
}

fn configured(state: SelectionState, authority: String) -> Router {
    Router::new()
        .route("/api/v1/tasks/selections", post(submit_selection))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(
            crate::server::dataset_api::LocalPage::new(
                authority,
                "selection_origin_forbidden",
                "Selection submission is restricted to the configured local workbench origin.",
            ),
            crate::server::dataset_api::local_page,
        ))
}

fn fail(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"error":{"code":code,"message":message}})),
    )
        .into_response()
}

async fn submit_selection(
    State(state): State<SelectionState>,
    request: Result<Json<Request>, JsonRejection>,
) -> Response {
    let Ok(Json(request)) = request else {
        return fail(
            StatusCode::BAD_REQUEST,
            "invalid_selection",
            "Invalid selection request fields.",
        );
    };
    let Some(root) = state.root else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "dataset_root_unconfigured",
            "Configure --dataset-root equal to --output for task submission.",
        );
    };
    let (Ok(root), Ok(output)) = (root.canonicalize(), state.api.0.output.canonicalize()) else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid_dataset_root",
            "Dataset/output roots must exist and be readable.",
        );
    };
    if root != output {
        return fail(
            StatusCode::CONFLICT,
            "dataset_root_mismatch",
            "--dataset-root must resolve to --output; cross-root copying is unsupported.",
        );
    }
    let mut selection =
        json!({"dsv":request.dsv,"instrument":request.instrument,"strategy":request.strategy});
    if let Some(settings) = request.settings {
        selection["settings"] = settings;
    }
    let frozen = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
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
            .write_all(selection.to_string().as_bytes())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        child.wait_with_output()
    })
    .await;
    let Ok(Ok(frozen)) = frozen else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "selection_cli_unavailable",
            "Selection CLI could not be executed.",
        );
    };
    let Ok(envelope) = serde_json::from_slice::<Value>(&frozen.stdout) else {
        return fail(
            StatusCode::BAD_GATEWAY,
            "invalid_selection_response",
            "Selection CLI returned invalid JSON.",
        );
    };
    if envelope["schema_version"] != 1 {
        return fail(
            StatusCode::BAD_GATEWAY,
            "invalid_selection_response",
            "Unsupported selection CLI schema.",
        );
    }
    if !frozen.status.success() {
        let code = envelope["error"]["code"].as_str().unwrap_or("");
        let status = match code {
            "invalid_selection" | "invalid_request" | "unsupported_venue"
            | "invalid_definition" => StatusCode::BAD_REQUEST,
            "dataset_not_found" | "instrument_not_in_dataset" => StatusCode::NOT_FOUND,
            "selection_evidence_missing" => StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_dataset_root" => StatusCode::SERVICE_UNAVAILABLE,
            "invalid_manifest"
            | "invalid_dataset_table"
            | "dataset_read_failed"
            | "selection_store_failed" => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::BAD_GATEWAY,
        };
        if envelope["error"]["message"].is_string() && status != StatusCode::BAD_GATEWAY {
            return (
                status,
                [(header::CACHE_CONTROL, "no-store")],
                Json(envelope),
            )
                .into_response();
        }
    } else if envelope["data"]["definition"].is_object() && envelope.get("error").is_none() {
        let submitted = state.api.submit_experiment_task(SubmitExperimentTask {
            idempotency_key: request.idempotency_key,
            definition: envelope["data"]["definition"].clone(),
            result_level: request.result_level,
        });
        return match submitted {
            Ok((task, created)) => (
                if created {
                    StatusCode::ACCEPTED
                } else {
                    StatusCode::OK
                },
                [(header::CACHE_CONTROL, "no-store")],
                Json(json!({"task":task,"created":created,"selection":envelope["data"]})),
            )
                .into_response(),
            Err(error) => error.into_response(),
        };
    }
    fail(
        StatusCode::BAD_GATEWAY,
        "invalid_selection_response",
        "Inconsistent selection CLI result.",
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tower::ServiceExt;

    fn api(root: &FsPath) -> RunJobApi {
        RunJobApi::without_worker(
            root.into(),
            Arc::new(UniverseStore::new(root)),
            "unused".into(),
        )
    }

    fn script(root: &FsPath, payload: Value, exit: i32) -> PathBuf {
        let cli = root.join("freeze");
        let json = payload.to_string();
        fs::write(
            &cli,
            format!("#!/bin/sh\ncat >/dev/null\nprintf '%s' '{json}'\nexit {exit}\n"),
        )
        .unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        cli
    }

    async fn submit(app: Router, body: Value, host: &str) -> (StatusCode, Value) {
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/tasks/selections")
                    .header("host", host)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn request() -> Value {
        json!({"idempotency_key":"selection","dsv":"synthetic","instrument":"510300.XSHG","strategy":"buy_and_hold"})
    }

    #[tokio::test]
    async fn selection_maps_roots_origins_cli_errors_and_protocol_failures() {
        let temp = tempfile::tempdir().unwrap();
        let api = api(temp.path());
        let state = SelectionState {
            api,
            root: None,
            cli: "missing".into(),
        };
        let app = |state| configured(state, "localhost:7878".into());
        let mut oversized = request();
        oversized["dsv"] = json!("x".repeat(65536));
        assert_eq!(
            submit(app(state.clone()), oversized, "localhost:7878")
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            submit(app(state.clone()), request(), "evil.invalid")
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            submit(app(state.clone()), request(), "localhost:7878")
                .await
                .1["error"]["code"],
            "dataset_root_unconfigured"
        );
        let mut missing = state.clone();
        missing.root = Some(temp.path().join("absent"));
        assert_eq!(
            submit(app(missing), request(), "localhost:7878").await.1["error"]["code"],
            "invalid_dataset_root"
        );
        let other = tempfile::tempdir().unwrap();
        let mut mismatch = state.clone();
        mismatch.root = Some(other.path().into());
        assert_eq!(
            submit(app(mismatch), request(), "localhost:7878").await.1["error"]["code"],
            "dataset_root_mismatch"
        );
        let mut state = state;
        state.root = Some(temp.path().into());
        assert_eq!(
            submit(app(state.clone()), request(), "localhost:7878")
                .await
                .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        for (code, status) in [
            ("invalid_selection", 400),
            ("invalid_request", 400),
            ("unsupported_venue", 400),
            ("invalid_definition", 400),
            ("dataset_not_found", 404),
            ("instrument_not_in_dataset", 404),
            ("selection_evidence_missing", 422),
            ("invalid_dataset_root", 503),
            ("invalid_manifest", 500),
            ("invalid_dataset_table", 500),
            ("dataset_read_failed", 500),
            ("selection_store_failed", 500),
            ("unrecognized", 502),
        ] {
            state.cli = script(
                temp.path(),
                json!({"schema_version":1,"error":{"code":code,"message":"synthetic"}}),
                2,
            );
            assert_eq!(
                submit(app(state.clone()), request(), "localhost:7878")
                    .await
                    .0
                    .as_u16(),
                status,
                "{code}"
            );
        }
        for payload in [
            json!({"schema_version":2}),
            json!({"schema_version":1,"data":{}}),
        ] {
            state.cli = script(temp.path(), payload, 0);
            assert_eq!(
                submit(app(state.clone()), request(), "localhost:7878")
                    .await
                    .0,
                StatusCode::BAD_GATEWAY
            );
        }
        state.cli = script(
            temp.path(),
            json!({"schema_version":1,"data":{"definition":{}}}),
            0,
        );
        let mut invalid = request();
        invalid["extra"] = json!(1);
        assert_eq!(
            submit(app(state), invalid, "localhost:7878").await.0,
            StatusCode::BAD_REQUEST
        );
        assert!(!temp.path().join("tasks").exists());
    }

    #[tokio::test]
    async fn selection_reuses_t01_frozen_request_persistence_and_idempotency() {
        let temp = tempfile::tempdir().unwrap();
        let api = api(temp.path());
        let definition = json!({"synthetic":"definition"});
        let cli = script(
            temp.path(),
            json!({"schema_version":1,"data":{"definition":definition}}),
            0,
        );
        let state = SelectionState {
            api: api.clone(),
            root: Some(temp.path().into()),
            cli,
        };
        let app = || configured(state.clone(), "localhost:7878".into());
        let (status, first) = submit(app(), request(), "localhost:7878").await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let id = Uuid::parse_str(first["task"]["task_id"].as_str().unwrap()).unwrap();
        let path = temp.path().join("tasks").join(id.to_string());
        let stored: Value =
            serde_json::from_slice(&fs::read(path.join("experiment.json")).unwrap()).unwrap();
        assert_eq!(stored, definition);
        assert!(path.join("request.json").exists() && path.join("task.json").exists());
        let (status, reused) = submit(app(), request(), "localhost:7878").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first["task"], reused["task"]);
        assert_eq!(reused["created"], false);
        let mut conflicting = request();
        conflicting["result_level"] = json!("standard");
        assert_eq!(
            submit(app(), conflicting, "localhost:7878").await.0,
            StatusCode::CONFLICT
        );
        assert_eq!(api.list_experiment_tasks().len(), 1);
    }
}
