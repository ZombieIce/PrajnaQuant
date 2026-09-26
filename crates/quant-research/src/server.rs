use crate::{
    data::{MarketCoverageAudit, SnapshotManifest, audit_etf_market_coverage},
    experiment::{list_experiments, load_experiment},
    signal,
    universe::{
        CoverageStatus, PitStatus, SourceKind, UniverseDefinition, resolve_members,
        strategy_capability,
    },
};
use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::SystemTime,
};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
};
use uuid::Uuid;

#[path = "market_api.rs"]
mod market_api;
#[path = "run_jobs.rs"]
mod run_jobs;
#[path = "universe_api.rs"]
mod universe_api;
use universe_api::{UniverseDraft, UniverseStore, UniverseSummary};

#[derive(Clone)]
struct AppState {
    output: Arc<PathBuf>,
    universes: Arc<UniverseStore>,
}

pub fn load_universe_version(
    output: &FsPath,
    universe_id: Uuid,
    version_id: Uuid,
) -> Result<UniverseDefinition> {
    let config = UniverseStore::new(output).get(universe_id)?;
    config
        .versions
        .into_iter()
        .find(|version| version.version_id == version_id)
        .ok_or_else(|| anyhow::anyhow!("universe version not found"))
}

pub async fn serve(
    address: &str,
    output: PathBuf,
    market_data_dir: PathBuf,
    web_dist: PathBuf,
) -> Result<()> {
    let socket = validate_loopback_address(address)?;
    let output = Arc::new(output);
    let state = AppState {
        universes: Arc::new(UniverseStore::new(&output)),
        output: output.clone(),
    };
    let run_jobs = run_jobs::RunJobApi::start(output.as_ref().clone(), state.universes.clone());
    let api = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/signals", get(signals))
        .route("/api/v1/signals/{key}", get(signal_definition))
        .route("/api/research/signals/{key}/report", get(signal_report))
        .route("/api/experiments", get(experiments))
        .route("/api/experiments/{id}", get(experiment))
        .route("/api/v1/factor-reports", get(factor_reports))
        .route("/api/v1/universes", get(universes).post(create_universe))
        .route(
            "/api/v1/universes/{id}",
            get(universe).delete(delete_universe),
        )
        .route("/api/v1/universes/{id}/draft", patch(update_draft))
        .route(
            "/api/v1/universes/{id}/versions/preview",
            get(preview_publish_version),
        )
        .route("/api/v1/universes/{id}/versions", post(publish_version))
        .route("/api/v1/universes/{id}/members", get(universe_members))
        .route("/api/v1/universes/{id}/coverage", get(universe_coverage))
        .with_state(state)
        .merge(run_jobs.router())
        .merge(market_api::router(Arc::new(market_data_dir)))
        .layer(CorsLayer::permissive());
    let app = api.fallback_service(
        ServeDir::new(&web_dist)
            .append_index_html_on_directories(true)
            .not_found_service(ServeFile::new(web_dist.join("index.html"))),
    );
    let listener = tokio::net::TcpListener::bind(socket)
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
        .unwrap_or_else(|| {
            error(
                StatusCode::NOT_FOUND,
                "not_found",
                "unknown signal",
                Value::Null,
            )
        })
}
async fn signal_report(State(state): State<AppState>, Path(key): Path<String>) -> ApiResponse {
    match load_latest_signal_report(&state.output, &key) {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(
            StatusCode::NOT_FOUND,
            "not_found",
            &e.to_string(),
            Value::Null,
        ),
    }
}

#[derive(Deserialize, Default)]
struct ExperimentFilter {
    universe_id: Option<Uuid>,
    version_id: Option<Uuid>,
}
async fn experiments(
    State(state): State<AppState>,
    Query(filter): Query<ExperimentFilter>,
) -> ApiResponse {
    if filter.universe_id.is_some() != filter.version_id.is_some() {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "universe_id and version_id must be provided together",
            Value::Null,
        );
    }
    match list_experiments(&state.output) {
        Ok(items) => {
            let Some((uid, vid)) = filter.universe_id.zip(filter.version_id) else {
                return Json(items).into_response();
            };
            let filtered = items
                .into_iter()
                .filter(|summary| {
                    load_experiment(&state.output, &summary.experiment_id.to_string())
                        .ok()
                        .and_then(|e| e.universe)
                        .is_some_and(|u| u.universe_id == uid && u.version_id == vid)
                })
                .collect::<Vec<_>>();
            Json(filtered).into_response()
        }
        Err(e) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_error",
            &e.to_string(),
            Value::Null,
        ),
    }
}
async fn experiment(State(state): State<AppState>, Path(id): Path<String>) -> ApiResponse {
    match load_experiment(&state.output, &id) {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(
            StatusCode::NOT_FOUND,
            "not_found",
            &e.to_string(),
            Value::Null,
        ),
    }
}

#[derive(Deserialize, Default)]
struct FactorReportFilter {
    signal_key: Option<String>,
    universe_id: Option<Uuid>,
    version_id: Option<Uuid>,
}
async fn factor_reports(
    State(state): State<AppState>,
    Query(filter): Query<FactorReportFilter>,
) -> ApiResponse {
    if filter.universe_id.is_some() != filter.version_id.is_some() {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "universe_id and version_id must be provided together",
            Value::Null,
        );
    }
    let dir = state.output.join("signal-reports");
    let mut reports = Vec::<Value>::new();
    if dir.exists() {
        let entries = match fs::read_dir(&dir) {
            Ok(v) => v,
            Err(e) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage_error",
                    &e.to_string(),
                    Value::Null,
                );
            }
        };
        for entry in entries.flatten() {
            let path = entry.path().join("report.json");
            let Ok(bytes) = fs::read(path) else {
                continue;
            };
            let Ok(report) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if let Some(signal_key) = &filter.signal_key {
                if report
                    .pointer("/report/definition/key")
                    .and_then(Value::as_str)
                    != Some(signal_key.as_str())
                {
                    continue;
                }
            }
            if let Some((uid, vid)) = filter.universe_id.zip(filter.version_id) {
                let identity = report
                    .pointer("/universe")
                    .or_else(|| report.pointer("/report/universe"));
                if identity
                    .and_then(|v| v.get("universe_id"))
                    .and_then(Value::as_str)
                    != Some(uid.to_string().as_str())
                    || identity
                        .and_then(|v| v.get("version_id"))
                        .and_then(Value::as_str)
                        != Some(vid.to_string().as_str())
                {
                    continue;
                }
            }
            reports.push(report);
        }
    }
    Json(json!({"items":reports})).into_response()
}

#[derive(Deserialize, Default)]
struct UniverseListFilter {
    asset_scope: Option<String>,
    status: Option<String>,
    q: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}
async fn universes(
    State(state): State<AppState>,
    Query(query): Query<UniverseListFilter>,
) -> ApiResponse {
    if query.cursor.is_some() {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "cursor is not supported by the small local registry",
            Value::Null,
        );
    }
    let limit = query.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "limit must be between 1 and 200",
            Value::Null,
        );
    }
    match state.universes.list() {
        Ok(values) => {
            let mut items = values
                .iter()
                .filter(|v| v.status != "deleted")
                .filter(|v| {
                    query
                        .asset_scope
                        .as_ref()
                        .is_none_or(|x| format!("{:?}", v.asset_scope).eq_ignore_ascii_case(x))
                })
                .filter(|v| query.status.as_ref().is_none_or(|x| &v.status == x))
                .filter(|v| {
                    query.q.as_ref().is_none_or(|x| {
                        v.name.to_lowercase().contains(&x.to_lowercase())
                            || v.description.to_lowercase().contains(&x.to_lowercase())
                    })
                })
                .map(UniverseSummary::from)
                .collect::<Vec<_>>();
            items.truncate(limit);
            Json(json!({"items":items,"next_cursor":null})).into_response()
        }
        Err(e) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_error",
            &e.to_string(),
            Value::Null,
        ),
    }
}
#[derive(Deserialize)]
struct CreateUniverseRequest {
    name: String,
    #[serde(default)]
    description: String,
    asset_scope: crate::universe::AssetScope,
    source_kind: SourceKind,
    source_ref: Option<String>,
    #[serde(default)]
    members: Vec<crate::universe::ManualMember>,
}
async fn create_universe(
    State(state): State<AppState>,
    Json(request): Json<CreateUniverseRequest>,
) -> ApiResponse {
    let default_ref = match request.source_kind {
        SourceKind::Manual => "user:local-user",
        SourceKind::IndexHistory => "index:provider-unconfigured",
    };
    let draft = UniverseDraft {
        name: request.name,
        description: request.description,
        asset_scope: request.asset_scope,
        source_kind: request.source_kind,
        source_ref: request.source_ref.unwrap_or_else(|| default_ref.into()),
        members: request.members,
    };
    match state.universes.create(draft) {
        Ok(config) => (
            StatusCode::CREATED,
            Json(json!({"universe":UniverseSummary::from(&config),"draft":config.draft})),
        )
            .into_response(),
        Err(e) => error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &e.to_string(),
            Value::Null,
        ),
    }
}
async fn universe(State(state): State<AppState>, Path(id): Path<String>) -> ApiResponse {
    let Ok(id) = Uuid::parse_str(&id) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid universe id",
            Value::Null,
        );
    };
    match state.universes.get(id) {
        Ok(config) => Json(json!({"universe":UniverseSummary::from(&config),"versions":config.versions.iter().map(version_summary).collect::<Vec<_>>(),"draft":config.draft})).into_response(),
        Err(e) => error(StatusCode::NOT_FOUND, "not_found", &e.to_string(), Value::Null),
    }
}
#[derive(Deserialize)]
struct DraftPatch {
    name: Option<String>,
    description: Option<String>,
    asset_scope: Option<crate::universe::AssetScope>,
    source_ref: Option<String>,
    members: Option<Vec<crate::universe::ManualMember>>,
}
async fn update_draft(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(patch): Json<DraftPatch>,
) -> ApiResponse {
    let Ok(id) = Uuid::parse_str(&id) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid universe id",
            Value::Null,
        );
    };
    match state.universes.patch_draft(
        id,
        patch.name,
        patch.description,
        patch.asset_scope,
        patch.source_ref,
        patch.members,
    ) {
        Ok(config) => Json(json!({"universe":UniverseSummary::from(&config),"draft":config.draft}))
            .into_response(),
        Err(e) => {
            let text = e.to_string();
            let (status, code) = if text.contains("no editable draft") || text.contains("archived")
            {
                (StatusCode::CONFLICT, "conflict")
            } else if text.contains("No such file") || text.contains("read ") {
                (StatusCode::NOT_FOUND, "not_found")
            } else {
                (StatusCode::BAD_REQUEST, "invalid_request")
            };
            error(status, code, &text, Value::Null)
        }
    }
}
async fn preview_publish_version(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResponse {
    let Ok(id) = Uuid::parse_str(&id) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid universe id",
            Value::Null,
        );
    };
    match state.universes.preview_publish(id) {
        Ok(preview) => Json(preview).into_response(),
        Err(e) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "conflict",
            &e.to_string(),
            Value::Null,
        ),
    }
}

#[derive(Deserialize, Default)]
struct PublishVersionRequest {
    expected_draft_hash: Option<String>,
}

async fn publish_version(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<PublishVersionRequest>,
) -> ApiResponse {
    let Ok(id) = Uuid::parse_str(&id) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid universe id",
            Value::Null,
        );
    };
    match state
        .universes
        .publish(id, request.expected_draft_hash.as_deref())
    {
        Ok(outcome) => (
            if outcome.created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            Json(json!({"version":version_summary(&outcome.version),"created":outcome.created})),
        )
            .into_response(),
        Err(e) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "conflict",
            &e.to_string(),
            Value::Null,
        ),
    }
}
async fn delete_universe(State(state): State<AppState>, Path(id): Path<String>) -> ApiResponse {
    let Ok(id) = Uuid::parse_str(&id) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid universe id",
            Value::Null,
        );
    };
    match state.universes.delete_or_archive(id) {
        Ok(config) => Json(json!({"universe":UniverseSummary::from(&config)})).into_response(),
        Err(e) => error(
            StatusCode::NOT_FOUND,
            "not_found",
            &e.to_string(),
            Value::Null,
        ),
    }
}

#[derive(Deserialize)]
struct MembersQuery {
    version_id: Uuid,
    as_of: NaiveDate,
}
async fn universe_members(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<MembersQuery>,
) -> ApiResponse {
    let definition = match find_version(&state.universes, &id, query.version_id) {
        Ok(v) => v,
        Err((status, code, message)) => return error(status, code, &message, Value::Null),
    };
    let cutoff = shanghai_close(query.as_of);
    match resolve_members(&definition, query.as_of, cutoff, false) {
        Ok(mut snapshot) => {
            if definition.source_kind == SourceKind::IndexHistory
                && (definition.index_events.is_empty() || definition.coverage.is_empty())
            {
                snapshot.warnings.push("historical membership provider is not configured; facts and complete coverage are unavailable".into());
            }
            Json(json!({"universe_id":snapshot.universe_id,"version_id":snapshot.version_id,"as_of":snapshot.as_of,"members":snapshot.members,"pit_status":snapshot.pit_status,"coverage":snapshot.coverage,"warnings":snapshot.warnings,"membership_hash":snapshot.membership_hash})).into_response()
        }
        Err(e) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "coverage_gap",
            &e.to_string(),
            Value::Null,
        ),
    }
}
#[derive(Deserialize)]
struct CoverageQuery {
    version_id: Uuid,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
}
async fn universe_coverage(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<CoverageQuery>,
) -> ApiResponse {
    let definition = match find_version(&state.universes, &id, query.version_id) {
        Ok(v) => v,
        Err((status, code, message)) => return error(status, code, &message, Value::Null),
    };
    if query.start.is_some() != query.end.is_some() {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "start and end must be provided together",
            Value::Null,
        );
    }
    if query.start.zip(query.end).is_some_and(|(a, b)| a > b) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "start must be on or before end",
            Value::Null,
        );
    }
    let capabilities = vec![
        crate::universe::Capability {
            kind: crate::universe::CapabilityKind::MembershipReady,
            ready: definition.source_kind == SourceKind::Manual,
            blockers: if definition.source_kind == SourceKind::Manual {
                vec![]
            } else {
                vec!["historical membership provider is not configured".into()]
            },
        },
        crate::universe::Capability {
            kind: crate::universe::CapabilityKind::FactorResearchReady,
            ready: false,
            blockers: vec!["universe-filtered factor runner is not connected".into()],
        },
        {
            let mut c = strategy_capability(definition.asset_scope);
            if c.ready {
                c.ready = false;
                c.blockers
                    .push("universe member mask is not connected to the strategy runner".into());
            }
            c
        },
    ];
    let mut gaps = Vec::<Value>::new();
    let (membership_status, pit) = if definition.source_kind == SourceKind::IndexHistory {
        let covering = query.start.zip(query.end).and_then(|(start, end)| {
            definition
                .coverage
                .iter()
                .find(|segment| segment.start <= start && segment.end >= end)
        });
        match covering {
            Some(segment) if segment.status == CoverageStatus::Complete => {
                let pit = if definition.index_events.iter().all(|event| event.verified) {
                    PitStatus::VerifiedPit
                } else {
                    PitStatus::Unknown
                };
                (CoverageStatus::Complete, pit)
            }
            Some(segment) => {
                gaps.push(json!({"start":query.start,"end":query.end,"source_ref":definition.source_ref,"reason":"membership coverage segment is not complete"}));
                (segment.status, PitStatus::Unknown)
            }
            None => {
                gaps.push(json!({"start":query.start,"end":query.end,"source_ref":definition.source_ref,"reason":"historical membership provider is not configured or requested range is uncovered"}));
                (CoverageStatus::Gaps, PitStatus::Unknown)
            }
        }
    } else {
        (CoverageStatus::Unverified, PitStatus::RetrospectiveStatic)
    };

    let snapshot = match latest_verified_etf_snapshot(&state.output) {
        Ok(value) => value,
        Err(e) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "snapshot_audit_failed",
                &e.to_string(),
                Value::Null,
            );
        }
    };
    let (market_data_coverage, snapshot_identity, calendar_identity, audited_range) = match snapshot
    {
        Some((file, manifest, calendar_file)) => {
            let start = query
                .start
                .unwrap_or_else(|| manifest.first_date.parse().unwrap_or(NaiveDate::MIN));
            let end = query
                .end
                .unwrap_or_else(|| manifest.last_date.parse().unwrap_or(NaiveDate::MAX));
            let members = if definition.source_kind == SourceKind::Manual {
                definition
                    .manual_members
                    .iter()
                    .map(|m| &m.instrument)
                    .collect::<Vec<_>>()
            } else {
                definition
                    .index_events
                    .iter()
                    .map(|e| &e.instrument)
                    .collect::<Vec<_>>()
            };
            let mut symbols = members
                .into_iter()
                .filter(|instrument| {
                    definition.source_kind != SourceKind::Manual
                        || definition.manual_members.iter().any(|m| {
                            m.instrument.instrument_id == instrument.instrument_id
                                && m.effective_from <= end
                                && m.effective_to.is_none_or(|to| to > start)
                        })
                })
                .map(|instrument| {
                    crate::universe::market_bar_symbol(&instrument.exchange, &instrument.code)
                })
                .collect::<Result<Vec<_>>>();
            let audit = match symbols.as_mut() {
                Ok(symbols) => {
                    symbols.sort();
                    symbols.dedup();
                    if symbols.is_empty() {
                        MarketCoverageAudit {
                            status: "unverified",
                            calendar_basis: "observed_dates_union_diagnostic_only",
                            calendar_status: "unavailable",
                            calendar_missing_dates: vec![],
                            expected_dates: vec![],
                            symbols: vec![],
                        }
                    } else {
                        match audit_etf_market_coverage(
                            &file,
                            calendar_file.as_deref(),
                            symbols,
                            start,
                            end,
                        ) {
                            Ok(audit) => audit,
                            Err(e) => {
                                return error(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "snapshot_audit_failed",
                                    &e.to_string(),
                                    Value::Null,
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    return error(
                        StatusCode::BAD_REQUEST,
                        "invalid_instrument",
                        &e.to_string(),
                        Value::Null,
                    );
                }
            };
            let calendar_identity = calendar_file.as_ref().map(|path| {
                json!({
                    "source": manifest.trading_calendar_source,
                    "sha256": manifest.trading_calendar_sha256,
                    "rows": manifest.trading_calendar_rows,
                    "first_date": manifest.trading_calendar_first_date,
                    "last_date": manifest.trading_calendar_last_date,
                    "file": path.file_name().and_then(|name| name.to_str())
                })
            });
            (
                audit,
                Some(
                    json!({"snapshot_id":manifest.snapshot_id,"sha256":manifest.sha256,"created_at":manifest.created_at,"first_date":manifest.first_date,"last_date":manifest.last_date,"selection_rule":manifest.selection_rule}),
                ),
                calendar_identity,
                Some((start, end)),
            )
        }
        None => {
            gaps.push(json!({"start":query.start,"end":query.end,"source_ref":"research-output/snapshots","reason":"no verified ETF market-data snapshot with a manifest is available"}));
            (
                MarketCoverageAudit {
                    status: "unverified",
                    calendar_basis: "observed_dates_union_diagnostic_only",
                    calendar_status: "unavailable",
                    calendar_missing_dates: vec![],
                    expected_dates: vec![],
                    symbols: vec![],
                },
                None,
                None,
                query.start.zip(query.end),
            )
        }
    };
    for item in &market_data_coverage.symbols {
        if !item.missing_dates.is_empty()
            || !item.unexpected_dates.is_empty()
            || !item.duplicate_dates.is_empty()
            || !item.invalid_ohlc_dates.is_empty()
            || item.rows == 0
        {
            gaps.push(json!({"source_ref":snapshot_identity.as_ref().and_then(|s|s.get("snapshot_id")).cloned().unwrap_or(Value::Null),"symbol":item.symbol,"missing_dates":item.missing_dates,"unexpected_dates":item.unexpected_dates,"duplicate_dates":item.duplicate_dates,"invalid_ohlc_dates":item.invalid_ohlc_dates,"reason":if item.rows == 0 {"no rows for this member in requested range"} else {"market data has missing dates or validation anomalies"}}));
        }
    }
    if !market_data_coverage.calendar_missing_dates.is_empty() {
        gaps.push(json!({"source_ref":calendar_identity,"missing_dates":market_data_coverage.calendar_missing_dates,"reason":"official trading calendar does not cover every date in the requested range"}));
    }
    let status = if definition.source_kind == SourceKind::IndexHistory
        && membership_status != CoverageStatus::Complete
    {
        membership_status
    } else {
        match market_data_coverage.status {
            "complete" => CoverageStatus::Complete,
            "gaps" => CoverageStatus::Gaps,
            _ => CoverageStatus::Unverified,
        }
    };
    Json(json!({
        "universe_id":definition.universe_id,"version_id":definition.version_id,
        "coverage":status,"membership_coverage":if definition.source_kind == SourceKind::Manual {json!({"status":"retrospective_static"})} else {json!({"status":membership_status})},
        "market_data_coverage":market_data_coverage,"snapshot":snapshot_identity,"calendar_snapshot":calendar_identity,
        "pit_status":pit,"capabilities":capabilities,
        "sources":[{"source_ref":definition.source_ref,"source_kind":definition.source_kind}],"gaps":gaps,
        "requested_range":audited_range
    })).into_response()
}

fn latest_verified_etf_snapshot(
    output: &FsPath,
) -> Result<
    Option<(
        std::path::PathBuf,
        SnapshotManifest,
        Option<std::path::PathBuf>,
    )>,
> {
    let root = output.join("snapshots");
    if !root.is_dir() {
        return Ok(None);
    }
    let mut manifests = fs::read_dir(&root)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter_map(|dir| {
            let manifest_path = dir.join("manifest.json");
            let bytes = fs::read(&manifest_path).ok()?;
            let manifest: SnapshotManifest = serde_json::from_slice(&bytes).ok()?;
            Some((
                manifest.created_at.clone(),
                dir.join("etf_daily.parquet"),
                manifest,
            ))
        })
        .collect::<Vec<_>>();
    manifests.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, file, manifest) in manifests {
        if !file.is_file() {
            continue;
        }
        let bytes = fs::read(&file)?;
        let actual = format!("{:x}", Sha256::digest(bytes));
        if actual == manifest.sha256 {
            let calendar = if let Some(expected) = &manifest.trading_calendar_sha256 {
                let calendar = file
                    .parent()
                    .context("snapshot has no parent directory")?
                    .join("trading_calendar.parquet");
                let bytes = fs::read(&calendar)
                    .with_context(|| format!("read calendar snapshot {}", calendar.display()))?;
                let actual = format!("{:x}", Sha256::digest(bytes));
                anyhow::ensure!(actual == *expected, "calendar snapshot SHA-256 mismatch");
                Some(calendar)
            } else {
                None
            };
            return Ok(Some((file, manifest, calendar)));
        }
    }
    Ok(None)
}

fn find_version(
    store: &UniverseStore,
    id: &str,
    version_id: Uuid,
) -> Result<UniverseDefinition, (StatusCode, &'static str, String)> {
    let Ok(id) = Uuid::parse_str(id) else {
        return Err((
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid universe id".into(),
        ));
    };
    let config = store
        .get(id)
        .map_err(|e| (StatusCode::NOT_FOUND, "not_found", e.to_string()))?;
    config
        .versions
        .into_iter()
        .find(|v| v.version_id == version_id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "not_found",
                "unknown universe version".into(),
            )
        })
}
fn version_summary(v: &UniverseDefinition) -> Value {
    json!({"version_id":v.version_id,"name":v.name,"description":v.description,"asset_scope":v.asset_scope,"source_kind":v.source_kind,"source_ref":v.source_ref,"content_hash":v.content_hash})
}
fn validate_loopback_address(address: &str) -> Result<SocketAddr> {
    let socket: SocketAddr = address
        .parse()
        .context("write API requires a numeric loopback bind address")?;
    anyhow::ensure!(
        socket.ip().is_loopback(),
        "unauthenticated write API may only bind to loopback"
    );
    Ok(socket)
}
fn shanghai_close(date: NaiveDate) -> DateTime<Utc> {
    FixedOffset::east_opt(8 * 3600)
        .expect("UTC+8")
        .from_local_datetime(&date.and_hms_opt(15, 0, 0).expect("valid time"))
        .single()
        .expect("unambiguous fixed offset")
        .with_timezone(&Utc)
}

type ApiResponse = Response;
fn error(status: StatusCode, code: &str, message: &str, details: Value) -> Response {
    (
        status,
        Json(json!({"error":{"code":code,"message":message,"details":details}})),
    )
        .into_response()
}

fn load_latest_signal_report(root: &FsPath, signal_key: &str) -> Result<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::universe::{
        AssetScope, AssetType, CoverageSegment, IndexMembershipEvent, Instrument, ManualMember,
        MembershipAction, UniverseDefinition,
    };
    use tempfile::tempdir;

    fn draft() -> UniverseDraft {
        UniverseDraft {
            name: "test ETF".into(),
            description: "managed test".into(),
            asset_scope: AssetScope::Etf,
            source_kind: SourceKind::Manual,
            source_ref: "user:local-user".into(),
            members: vec![ManualMember {
                instrument: Instrument {
                    instrument_id: "ETF:510300".into(),
                    exchange: "XSHG".into(),
                    code: "510300".into(),
                    name: "沪深300ETF".into(),
                    asset_type: AssetType::Etf,
                },
                effective_from: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(),
                effective_to: None,
                source_ref: "user:local-user".into(),
            }],
        }
    }
    #[test]
    fn store_crud_publish_keeps_immutable_versions_and_reopens() {
        let dir = tempdir().unwrap();
        let store = UniverseStore::new(dir.path());
        let created = store.create(draft()).unwrap();
        let id = created.universe_id;
        let first = store.publish(id, None).unwrap();
        assert!(first.created);
        let v1 = first.version;
        let unchanged = store.preview_publish(id).unwrap();
        assert!(!unchanged.changed);
        assert!(unchanged.changed_fields.is_empty());
        let repeated = store.publish(id, Some(&unchanged.draft_hash)).unwrap();
        assert!(!repeated.created);
        assert_eq!(repeated.version.version_id, v1.version_id);
        store
            .patch_draft(id, Some("renamed draft".into()), None, None, None, None)
            .unwrap();
        let preview = store.preview_publish(id).unwrap();
        assert!(preview.changed);
        assert_eq!(preview.changed_fields, vec!["名称"]);
        store
            .patch_draft(
                id,
                None,
                Some("description changed after preview".into()),
                None,
                None,
                None,
            )
            .unwrap();
        assert!(store.publish(id, Some(&preview.draft_hash)).is_err());
        let refreshed_preview = store.preview_publish(id).unwrap();
        let v2 = store
            .publish(id, Some(&refreshed_preview.draft_hash))
            .unwrap()
            .version;
        let reopened = UniverseStore::new(dir.path()).get(id).unwrap();
        assert_eq!(reopened.versions.len(), 2);
        assert_eq!(reopened.versions[0].version_id, v1.version_id);
        assert_eq!(reopened.versions[0].content_hash, v1.content_hash);
        assert_ne!(reopened.versions[0].version_id, v2.version_id);
    }
    #[test]
    fn manual_members_date_resolution_and_stock_capability_are_explicit() {
        let dir = tempdir().unwrap();
        let store = UniverseStore::new(dir.path());
        let config = store.create(draft()).unwrap();
        let version = store.publish(config.universe_id, None).unwrap().version;
        let snapshot = resolve_members(
            &version,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            shanghai_close(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()),
            false,
        )
        .unwrap();
        assert_eq!(snapshot.pit_status, PitStatus::RetrospectiveStatic);
        assert_eq!(snapshot.members.len(), 1);
        assert!(
            strategy_capability(AssetScope::Stock).blockers[0].contains("capability_not_ready")
        );
    }

    #[test]
    fn unauthenticated_write_api_rejects_non_loopback_binds() {
        assert!(validate_loopback_address("127.0.0.1:7878").is_ok());
        assert!(validate_loopback_address("[::1]:7878").is_ok());
        assert!(validate_loopback_address("0.0.0.0:7878").is_err());
    }

    #[tokio::test]
    async fn api_handlers_create_patch_publish_query_members_and_archive() {
        let dir = tempdir().unwrap();
        let state = AppState {
            universes: Arc::new(UniverseStore::new(dir.path())),
            output: Arc::new(dir.path().to_path_buf()),
        };
        let response = create_universe(
            State(state.clone()),
            Json(CreateUniverseRequest {
                name: "managed ETF".into(),
                description: "route contract".into(),
                asset_scope: AssetScope::Etf,
                source_kind: SourceKind::Manual,
                source_ref: None,
                members: vec![],
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let id = body
            .pointer("/universe/universe_id")
            .unwrap()
            .as_str()
            .unwrap();
        let member = draft().members[0].clone();
        let response = update_draft(
            State(state.clone()),
            Path(id.to_string()),
            Json(DraftPatch {
                name: None,
                description: None,
                asset_scope: None,
                source_ref: None,
                members: Some(vec![member]),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let preview_response =
            preview_publish_version(State(state.clone()), Path(id.to_string())).await;
        assert_eq!(preview_response.status(), StatusCode::OK);
        let preview_bytes = axum::body::to_bytes(preview_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let preview: Value = serde_json::from_slice(&preview_bytes).unwrap();
        assert_eq!(preview["changed"], true);
        assert_eq!(preview["changed_fields"][0], "首次发布");
        let response = publish_version(
            State(state.clone()),
            Path(id.to_string()),
            Json(PublishVersionRequest {
                expected_draft_hash: Some(preview["draft_hash"].as_str().unwrap().to_owned()),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let published: Value = serde_json::from_slice(&bytes).unwrap();
        let version = published
            .pointer("/version/version_id")
            .unwrap()
            .as_str()
            .unwrap();
        let unchanged = state
            .universes
            .preview_publish(Uuid::parse_str(id).unwrap())
            .unwrap();
        assert!(!unchanged.changed);
        let no_change_response = publish_version(
            State(state.clone()),
            Path(id.to_string()),
            Json(PublishVersionRequest {
                expected_draft_hash: Some(unchanged.draft_hash),
            }),
        )
        .await;
        assert_eq!(no_change_response.status(), StatusCode::OK);
        let no_change_bytes = axum::body::to_bytes(no_change_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let no_change_body: Value = serde_json::from_slice(&no_change_bytes).unwrap();
        assert_eq!(no_change_body["created"], false);
        let response = universe_members(
            State(state.clone()),
            Path(id.to_string()),
            Query(MembersQuery {
                version_id: Uuid::parse_str(version).unwrap(),
                as_of: NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let members: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(members["members"].as_array().unwrap().len(), 1);
        assert_eq!(members["pit_status"], "retrospective_static");
        let response = delete_universe(State(state), Path(id.to_string())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let archived: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(archived.pointer("/universe/status").unwrap(), "archived");
    }

    #[tokio::test]
    async fn synthetic_three_security_ten_day_universe_flows_through_api_handlers() {
        // Test-only, explicitly synthetic PIT fixture. Its verified status is not evidence
        // about real index history and is never installed in the user's output directory.
        let dir = tempdir().unwrap();
        let state = AppState {
            universes: Arc::new(UniverseStore::new(dir.path())),
            output: Arc::new(dir.path().to_path_buf()),
        };
        let d = |day: u32| {
            let first = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
            let n = day - 1;
            first + chrono::Duration::days((n / 5 * 7 + n % 5) as i64)
        };
        let cutoff = |day: u32, hour: u32| {
            let local = d(day).and_hms_opt(hour, 0, 0).unwrap();
            FixedOffset::east_opt(8 * 3600)
                .unwrap()
                .from_local_datetime(&local)
                .single()
                .unwrap()
                .with_timezone(&Utc)
        };
        let instrument = |id: &str| Instrument {
            instrument_id: id.into(),
            exchange: "XSHG".into(),
            code: match id {
                "A" => "600001",
                "B" => "600002",
                _ => "600003",
            }
            .into(),
            name: format!("synthetic-{id}"),
            asset_type: AssetType::Stock,
        };
        let event = |id: &str, action, effective, published, hour| IndexMembershipEvent {
            instrument: instrument(id),
            action,
            effective_date: d(effective),
            published_at: cutoff(published, hour),
            source_ref: format!("synthetic-fixture:{id}:{effective}"),
            source_revision_hash: format!("synthetic-hash-{id}-{effective}"),
            verified: true,
        };
        let universe_id = Uuid::from_u128(101);
        let version_id = Uuid::from_u128(102);
        let mut definition = UniverseDefinition {
            universe_id,
            version_id,
            name: "Synthetic 3 security × 10 trading day fixture".into(),
            description: "API integration test only".into(),
            asset_scope: AssetScope::Stock,
            source_kind: SourceKind::IndexHistory,
            source_ref: "synthetic:test-fixture".into(),
            content_hash: String::new(),
            manual_members: vec![],
            index_events: vec![
                event("A", MembershipAction::Add, 1, 1, 14),
                event("C", MembershipAction::Add, 1, 1, 14),
                event("B", MembershipAction::Add, 4, 3, 14),
                event("C", MembershipAction::Remove, 6, 5, 16),
                event("A", MembershipAction::Remove, 8, 8, 16),
                event("B", MembershipAction::Remove, 10, 9, 14),
            ],
            coverage: vec![CoverageSegment {
                start: d(1),
                end: d(10),
                status: CoverageStatus::Complete,
                source_revision_hash: "synthetic-complete-d1-d10".into(),
            }],
        };
        definition.content_hash = definition.calculate_content_hash().unwrap();
        let config = universe_api::UniverseConfig {
            universe_id,
            name: definition.name.clone(),
            description: definition.description.clone(),
            asset_scope: definition.asset_scope,
            source_kind: definition.source_kind,
            source_ref: definition.source_ref.clone(),
            status: "active".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            draft: None,
            versions: vec![definition],
        };
        let registry = dir.path().join("universes");
        fs::create_dir_all(&registry).unwrap();
        fs::write(
            registry.join(format!("{universe_id}.json")),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();

        let d4 = universe_members(
            State(state.clone()),
            Path(universe_id.to_string()),
            Query(MembersQuery {
                version_id,
                as_of: d(4),
            }),
        )
        .await;
        assert_eq!(d4.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(d4.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["pit_status"], "verified_pit");
        assert_eq!(body["members"].as_array().unwrap().len(), 3);

        let d10 = universe_members(
            State(state.clone()),
            Path(universe_id.to_string()),
            Query(MembersQuery {
                version_id,
                as_of: d(10),
            }),
        )
        .await;
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(d10.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["members"].as_array().unwrap().len(), 0);

        let coverage = universe_coverage(
            State(state),
            Path(universe_id.to_string()),
            Query(CoverageQuery {
                version_id,
                start: Some(d(1)),
                end: Some(d(10)),
            }),
        )
        .await;
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(coverage.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["coverage"], "unverified");
        assert_eq!(body["membership_coverage"]["status"], "complete");
        assert_eq!(body["market_data_coverage"]["status"], "unverified");
        assert_eq!(body["pit_status"], "verified_pit");
        assert!(
            body["capabilities"].as_array().unwrap().iter().any(|item| {
                item["kind"] == "strategy_backtest_ready" && item["ready"] == false
            })
        );
    }

    #[tokio::test]
    async fn coverage_api_audits_version_members_against_verified_snapshot() {
        let dir = tempdir().unwrap();
        let snapshot_id = Uuid::from_u128(201);
        let snapshot_dir = dir.path().join("snapshots").join(snapshot_id.to_string());
        fs::create_dir_all(&snapshot_dir).unwrap();
        let parquet = snapshot_dir.join("etf_daily.parquet");
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT * FROM (VALUES
                ('sh513300', 'ETF-A', DATE '2026-01-05', 10.0, 11.0, 9.0, 10.5),
                ('sh513300', 'ETF-A', DATE '2026-01-06', 10.5, 11.5, 10.0, 11.0),
                ('sh518880', 'ETF-B', DATE '2026-01-05', 5.0, 5.5, 4.5, 5.2)
             ) AS bars(symbol, name, trade_date, open, high, low, close)) TO '{}' (FORMAT PARQUET)",
            parquet.to_string_lossy().replace('\'', "''")
        ))
        .unwrap();
        let calendar_file = snapshot_dir.join("trading_calendar.parquet");
        conn.execute_batch(&format!(
            "COPY (SELECT * FROM (VALUES
                (DATE '2026-01-05', TRUE),
                (DATE '2026-01-06', TRUE)
             ) AS days(trade_date, is_open)) TO '{}' (FORMAT PARQUET)",
            calendar_file.to_string_lossy().replace('\'', "''")
        ))
        .unwrap();
        let bytes = fs::read(&parquet).unwrap();
        let sha256 = format!("{:x}", Sha256::digest(bytes));
        let calendar_bytes = fs::read(&calendar_file).unwrap();
        let calendar_sha256 = format!("{:x}", Sha256::digest(calendar_bytes));
        let manifest = SnapshotManifest {
            snapshot_id,
            created_at: "2026-01-07T00:00:00Z".into(),
            source_database: PathBuf::new(),
            file: parquet,
            sha256: sha256.clone(),
            rows: 3,
            symbols: 2,
            first_date: "2026-01-05".into(),
            last_date: "2026-01-06".into(),
            benchmark_file: None,
            benchmark_sha256: None,
            benchmark_rows: 0,
            benchmark_name: None,
            trading_calendar_file: Some(calendar_file),
            trading_calendar_sha256: Some(calendar_sha256.clone()),
            trading_calendar_rows: 2,
            trading_calendar_first_date: Some("2026-01-05".into()),
            trading_calendar_last_date: Some("2026-01-06".into()),
            trading_calendar_source: Some("szse_official_month_list".into()),
            selection_rule: "synthetic test".into(),
            limitations: vec![],
        };
        fs::write(
            snapshot_dir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let universe_id = Uuid::from_u128(202);
        let version_id = Uuid::from_u128(203);
        let instrument = |code: &str, name: &str| Instrument {
            instrument_id: format!("XSHG:{code}"),
            exchange: "XSHG".into(),
            code: code.into(),
            name: name.into(),
            asset_type: AssetType::Etf,
        };
        let mut definition = UniverseDefinition {
            universe_id,
            version_id,
            name: "Coverage test".into(),
            description: String::new(),
            asset_scope: AssetScope::Etf,
            source_kind: SourceKind::Manual,
            source_ref: "user:test".into(),
            content_hash: String::new(),
            manual_members: vec![
                ManualMember {
                    instrument: instrument("513300", "ETF-A"),
                    effective_from: NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                    effective_to: None,
                    source_ref: "user:test".into(),
                },
                ManualMember {
                    instrument: instrument("518880", "ETF-B"),
                    effective_from: NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                    effective_to: None,
                    source_ref: "user:test".into(),
                },
            ],
            index_events: vec![],
            coverage: vec![],
        };
        definition.content_hash = definition.calculate_content_hash().unwrap();
        let config = universe_api::UniverseConfig {
            universe_id,
            name: definition.name.clone(),
            description: String::new(),
            asset_scope: AssetScope::Etf,
            source_kind: SourceKind::Manual,
            source_ref: "user:test".into(),
            status: "active".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            draft: None,
            versions: vec![definition],
        };
        let registry = dir.path().join("universes");
        fs::create_dir_all(&registry).unwrap();
        fs::write(
            registry.join(format!("{universe_id}.json")),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let state = AppState {
            output: Arc::new(dir.path().to_path_buf()),
            universes: Arc::new(UniverseStore::new(dir.path())),
        };
        let response = universe_coverage(
            State(state),
            Path(universe_id.to_string()),
            Query(CoverageQuery {
                version_id,
                start: NaiveDate::from_ymd_opt(2026, 1, 5),
                end: NaiveDate::from_ymd_opt(2026, 1, 6),
            }),
        )
        .await;
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["coverage"], "gaps");
        assert_eq!(
            body["membership_coverage"]["status"],
            "retrospective_static"
        );
        assert_eq!(body["market_data_coverage"]["status"], "gaps");
        assert_eq!(
            body["market_data_coverage"]["calendar_basis"],
            "official_szse_daily_calendar"
        );
        assert_eq!(body["market_data_coverage"]["calendar_status"], "complete");
        assert_eq!(body["snapshot"]["sha256"], sha256);
        assert_eq!(body["calendar_snapshot"]["sha256"], calendar_sha256);
        assert_eq!(
            body["market_data_coverage"]["symbols"][1]["missing_dates"][0],
            "2026-01-06"
        );
    }

    #[tokio::test]
    async fn saved_legacy_factor_reports_are_universe_unknown_and_never_match_a_version() {
        let dir = tempdir().unwrap();
        let state = AppState {
            universes: Arc::new(UniverseStore::new(dir.path())),
            output: Arc::new(dir.path().to_path_buf()),
        };
        let report_dir = dir.path().join("signal-reports").join("legacy-report");
        fs::create_dir_all(&report_dir).unwrap();
        fs::write(
            report_dir.join("report.json"),
            r#"{"report":{"definition":{"key":"momentum_60","display_name":"60日动量"}}}"#
                .as_bytes(),
        )
        .unwrap();
        let all = factor_reports(State(state.clone()), Query(FactorReportFilter::default())).await;
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(all.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["items"].as_array().unwrap().len(), 1);
        assert!(body["items"][0].get("universe").is_none());

        let filtered = factor_reports(
            State(state),
            Query(FactorReportFilter {
                signal_key: None,
                universe_id: Some(Uuid::from_u128(9001)),
                version_id: Some(Uuid::from_u128(9002)),
            }),
        )
        .await;
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(filtered.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["items"].as_array().unwrap().len(), 0);
    }
}
