//! Local read-only HTTP adapter to the independent Dataset preview CLI.
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::{Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command, sync::Arc};

#[derive(Clone)]
struct DatasetState {
    root: Option<PathBuf>,
    cli: PathBuf,
}

pub fn router(root: Option<PathBuf>, authority: String) -> Router {
    configured_router(DatasetState {
        root,
        cli: std::env::var_os("PRAJNA_DATASET_PREVIEW_CLI")
            .map(PathBuf::from)
            .unwrap_or_else(|| "prajna-preview-dataset".into()),
    })
    .layer(middleware::from_fn_with_state(authority, local_page))
}

async fn local_page(
    State(authority): State<String>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let headers = request.headers();
    let default_authority = authority.strip_suffix(":80").unwrap_or(&authority);
    let origin = format!("http://{default_authority}");
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if !matches!(host, Some(value) if value == authority || value == default_authority)
        || headers
            .get(header::ORIGIN)
            .is_some_and(|value| value != origin.as_str())
    {
        return failure(
            StatusCode::FORBIDDEN,
            "dataset_origin_forbidden",
            "Dataset preview is restricted to the configured local workbench origin.",
        );
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        "cross-origin-resource-policy",
        "same-origin".parse().expect("fixed header"),
    );
    response
}

fn configured_router(state: DatasetState) -> Router {
    Router::new()
        .route("/api/v1/datasets", get(catalog))
        .route("/api/v1/datasets/{dsv}", get(detail))
        .route("/api/v1/datasets/{dsv}/bars", get(bars))
        .with_state(Arc::new(state))
}

async fn catalog(State(state): State<Arc<DatasetState>>) -> Response {
    read(state, vec!["catalog".into()]).await
}

async fn detail(State(state): State<Arc<DatasetState>>, Path(dsv): Path<String>) -> Response {
    read(state, vec!["show".into(), "--dsv".into(), dsv]).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BarsQuery {
    instrument: String,
    start: Option<String>,
    end: Option<String>,
    limit: Option<String>,
    offset: Option<String>,
}

async fn bars(
    State(state): State<Arc<DatasetState>>,
    Path(dsv): Path<String>,
    query: Result<Query<BarsQuery>, QueryRejection>,
) -> Response {
    let query = match query {
        Ok(Query(query)) => query,
        Err(_) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Invalid or unknown Dataset bars query fields.",
            );
        }
    };
    let mut args = vec![
        "bars".into(),
        "--dsv".into(),
        dsv,
        "--instrument".into(),
        query.instrument,
    ];
    for (name, value) in [
        ("start", query.start),
        ("end", query.end),
        ("limit", query.limit),
        ("offset", query.offset),
    ] {
        if let Some(value) = value {
            args.extend([format!("--{name}"), value]);
        }
    }
    read(state, args).await
}

async fn read(state: Arc<DatasetState>, args: Vec<String>) -> Response {
    let Some(root) = state.root.clone() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "dataset_root_unconfigured",
            "Configure serve --dataset-root to preview published research Datasets.",
        );
    };
    // Keep blocking disk/CLI work off the service's async executor. No shell,
    // collector or publisher is involved; the reader owns validation/semantics.
    let output = tokio::task::spawn_blocking(move || {
        Command::new(&state.cli)
            .args(args)
            .arg("--root")
            .arg(root)
            .output()
    })
    .await;
    let Ok(Ok(output)) = output else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "dataset_reader_unavailable",
            "Dataset preview CLI could not be executed.",
        );
    };
    let parsed = serde_json::from_slice::<Value>(&output.stdout);
    let Ok(value) = parsed else {
        return failure(
            StatusCode::BAD_GATEWAY,
            "invalid_reader_response",
            "Dataset preview CLI returned invalid JSON.",
        );
    };
    if value["schema_version"] != 1 {
        return failure(
            StatusCode::BAD_GATEWAY,
            "invalid_reader_response",
            "Unsupported Dataset preview schema.",
        );
    }
    let status =
        if output.status.success() && value["data"].is_object() && value.get("error").is_none() {
            StatusCode::OK
        } else if !output.status.success() && value["error"]["message"].is_string() {
            match value["error"]["code"].as_str() {
                Some("invalid_request") => StatusCode::BAD_REQUEST,
                Some("dataset_not_found") => StatusCode::NOT_FOUND,
                Some("invalid_dataset_root") => StatusCode::SERVICE_UNAVAILABLE,
                Some("invalid_manifest" | "invalid_dataset_table" | "dataset_read_failed") => {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
                _ => {
                    return failure(
                        StatusCode::BAD_GATEWAY,
                        "invalid_reader_response",
                        "Unrecognized Dataset preview error.",
                    );
                }
            }
        } else {
            return failure(
                StatusCode::BAD_GATEWAY,
                "invalid_reader_response",
                "Inconsistent Dataset preview result.",
            );
        };
    // Preserve the CLI envelope and decimal strings byte-for-byte. No adapter
    // calculation, missing-value filling or identity reserialization occurs.
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(output.stdout))
        .expect("fixed response headers")
}

fn failure(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "schema_version": 1, "error": {"code": code, "message": message}
        })),
    )
        .into_response()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use axum::{body::to_bytes, http::Request};
    use std::{fs, os::unix::fs::PermissionsExt};
    use tower::ServiceExt;

    async fn get(app: Router, uri: &str) -> (StatusCode, Vec<u8>) {
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        (
            status,
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
    }

    #[tokio::test]
    async fn origin_boundary_precedes_the_reader_and_has_no_permissive_cors() {
        for (host, origin, status) in [
            ("127.0.0.1:7878", None, 503),
            ("127.0.0.1:7878", Some("http://127.0.0.1:7878"), 503),
            (
                "127.0.0.1:7878",
                Some("https://synthetic-review.invalid"),
                403,
            ),
            ("127.0.0.1:7878", Some("null"), 403),
            ("synthetic-review.invalid", None, 403),
        ] {
            let mut request = Request::builder()
                .uri("/api/v1/datasets")
                .header("host", host);
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            let response = router(None, "127.0.0.1:7878".into())
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status);
            assert!(
                response
                    .headers()
                    .get("access-control-allow-origin")
                    .is_none()
            );
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
    }

    #[tokio::test]
    async fn missing_root_and_cli_are_explicitly_unavailable() {
        for root in [None, Some(PathBuf::from("/synthetic"))] {
            let app = configured_router(DatasetState {
                root,
                cli: "/no-such-preview-cli".into(),
            });
            let (status, bytes) = get(app, "/api/v1/datasets").await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(serde_json::from_slice::<Value>(&bytes).unwrap()["error"]["code"].is_string());
        }
    }

    #[tokio::test]
    async fn routes_preserve_payloads_and_forward_only_explicit_reader_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let cli = temp.path().join("reader");
        let root = temp.path().join("local lake");
        fs::create_dir(&root).unwrap();
        fs::write(&cli, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\nprintf '%s' '{\"schema_version\":1,\"data\":{\"listing_date\":null,\"state\":\"unknown\",\"session_gaps\":[\"2026-01-06\"],\"close\":\"11.123456789012345678\",\"amount\":null}}'\n").unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        let app = configured_router(DatasetState {
            root: Some(root.clone()),
            cli: cli.clone(),
        });
        for (uri, expected) in [
            ("/api/v1/datasets", vec!["catalog"]),
            (
                "/api/v1/datasets/dsv%3Asha256%3Asynthetic",
                vec!["show", "--dsv", "dsv:sha256:synthetic"],
            ),
            (
                "/api/v1/datasets/synthetic/bars?instrument=000001.XSHE&start=2026-01-05&end=2026-01-09&limit=1&offset=1",
                vec![
                    "bars",
                    "--dsv",
                    "synthetic",
                    "--instrument",
                    "000001.XSHE",
                    "--start",
                    "2026-01-05",
                    "--end",
                    "2026-01-09",
                    "--limit",
                    "1",
                    "--offset",
                    "1",
                ],
            ),
        ] {
            let (status, bytes) = get(app.clone(), uri).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(bytes, b"{\"schema_version\":1,\"data\":{\"listing_date\":null,\"state\":\"unknown\",\"session_gaps\":[\"2026-01-06\"],\"close\":\"11.123456789012345678\",\"amount\":null}}");
            let mut expected = expected.into_iter().map(String::from).collect::<Vec<_>>();
            expected.extend(["--root".into(), root.display().to_string()]);
            assert_eq!(
                fs::read_to_string(temp.path().join("args")).unwrap(),
                expected.join("\n") + "\n"
            );
        }
        let (status, _) = get(app.clone(), "/api/v1/datasets/synthetic/bars").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = get(
            app,
            "/api/v1/datasets/synthetic/bars?instrument=x&adjustment=qfq",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn reader_errors_and_protocol_failures_never_become_success() {
        let temp = tempfile::tempdir().unwrap();
        let cli = temp.path().join("reader");
        for (code, status) in [
            ("invalid_request", 400),
            ("dataset_not_found", 404),
            ("invalid_dataset_root", 503),
            ("invalid_manifest", 500),
            ("invalid_dataset_table", 500),
            ("dataset_read_failed", 500),
            ("unexpected", 502),
        ] {
            fs::write(&cli, format!("#!/bin/sh\necho '{{\"schema_version\":1,\"error\":{{\"code\":\"{code}\",\"message\":\"synthetic failure\"}}}}'\nexit 2\n")).unwrap();
            fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
            let app = configured_router(DatasetState {
                root: Some(temp.path().into()),
                cli: cli.clone(),
            });
            assert_eq!(get(app, "/api/v1/datasets").await.0.as_u16(), status);
        }
        for body in [
            "not-json",
            "{}",
            "{\"schema_version\":2,\"data\":{}}",
            "{\"schema_version\":1}",
            "{\"schema_version\":1,\"data\":null}",
        ] {
            fs::write(&cli, format!("#!/bin/sh\necho '{body}'\n")).unwrap();
            let app = configured_router(DatasetState {
                root: Some(temp.path().into()),
                cli: cli.clone(),
            });
            assert_eq!(
                get(app, "/api/v1/datasets").await.0,
                StatusCode::BAD_GATEWAY
            );
        }
    }
}
