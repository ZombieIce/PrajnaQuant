//! Read-only security search and daily OHLCV APIs backed only by immutable published snapshots.
use crate::data::{
    DailyResearchRow, DailySnapshotManifest, SecurityDirectoryEntry, load_daily_research_rows,
    load_snapshot_security_directory, verify_snapshot_manifest,
};
use ashare_warehouse::resolve_published_daily_snapshot;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

#[derive(Clone)]
struct MarketState(Arc<PathBuf>);

pub fn router(output: Arc<PathBuf>) -> Router {
    Router::new()
        .route("/api/v1/instruments", get(instruments))
        .route("/api/v1/daily-bars", get(daily_bars))
        .with_state(MarketState(output))
}

#[derive(Debug, Clone, Deserialize)]
struct InstrumentQuery {
    q: Option<String>,
    asset_type: Option<String>,
    snapshot_id: Option<String>,
    limit: Option<String>,
    cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct BarsQuery {
    instrument_id: Option<String>,
    snapshot_id: Option<String>,
    start: Option<String>,
    end: Option<String>,
    limit: Option<String>,
    cursor: Option<String>,
    adjustment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Publication {
    status: String,
    snapshot_id: Uuid,
    published_at: String,
    data_cutoff_date: String,
    coverage_status: String,
    price_adjustment: String,
    source_status: String,
    manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
struct SnapshotIdentity {
    snapshot_id: Uuid,
    sha256: String,
    manifest_sha256: String,
    created_at: String,
    published_at: Option<String>,
    data_cutoff_date: String,
    first_date: String,
    last_date: String,
    price_adjustment: String,
    coverage_status: String,
    source_status: String,
}

#[derive(Debug, Clone, Serialize)]
struct Bar {
    trade_date: NaiveDate,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    close: Option<f64>,
    volume_shares: Option<f64>,
    amount_cny: Option<f64>,
    source: Option<String>,
    observed_at: Option<String>,
}

struct ResolvedSnapshot {
    source: SnapshotSource,
    identity: SnapshotIdentity,
}

enum SnapshotSource {
    Research(Box<DailySnapshotManifest>),
    Warehouse {
        data_path: PathBuf,
        directory: Vec<SecurityDirectoryEntry>,
    },
}

impl ResolvedSnapshot {
    fn securities(&self) -> Result<Vec<SecurityDirectoryEntry>, String> {
        match &self.source {
            SnapshotSource::Research(manifest) => {
                load_snapshot_security_directory(&manifest.base.file).map_err(|e| e.to_string())
            }
            SnapshotSource::Warehouse { directory, .. } => Ok(directory.clone()),
        }
    }

    fn bars(&self, symbol: &str, start: NaiveDate, end: NaiveDate) -> Result<Vec<Bar>, String> {
        match &self.source {
            SnapshotSource::Research(manifest) => {
                load_daily_research_rows(&manifest.base.file, &[symbol.to_owned()], start, end)
                    .map(|rows| rows.iter().map(to_bar).collect())
                    .map_err(|e| e.to_string())
            }
            SnapshotSource::Warehouse { data_path, .. } => {
                warehouse_bars(data_path, symbol, start, end).map_err(|e| e.to_string())
            }
        }
    }
}

async fn instruments(
    State(state): State<MarketState>,
    Query(query): Query<InstrumentQuery>,
) -> Response {
    let q = query.q.unwrap_or_default().trim().to_lowercase();
    if q.chars().count() < 2 {
        return Json(json!({"items": [], "next_cursor": null, "snapshot": Value::Null}))
            .into_response();
    }
    let asset_filter = match query
        .asset_type
        .as_deref()
        .map(str::to_lowercase)
        .as_deref()
    {
        None => None,
        Some("etf") => Some("ETF"),
        Some("stock") => Some("stock"),
        Some(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "asset_type must be etf or stock",
                Value::Null,
            );
        }
    };
    let limit = match bounded_limit(query.limit.as_deref(), 20, 100) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let resolved = match resolve_snapshot(&state.0, query.snapshot_id.as_deref()) {
        Ok(snapshot) => snapshot,
        Err(response) => return response,
    };
    let offset = match decode_cursor(
        query.cursor.as_deref(),
        &resolved.identity.snapshot_id.to_string(),
        &filter_hash(&[&q, asset_filter.unwrap_or("all")]),
    ) {
        Ok(value) => value,
        Err(message) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_cursor",
                &message,
                Value::Null,
            );
        }
    };
    let entries = match resolved.securities() {
        Ok(entries) => entries,
        Err(e) => {
            return error(
                StatusCode::CONFLICT,
                "snapshot_invalid",
                &e.to_string(),
                Value::Null,
            );
        }
    };
    let matches = entries
        .into_iter()
        .filter(|entry| {
            let type_match = match asset_filter {
                Some("ETF") => entry.asset_type == "ETF",
                Some("stock") => matches!(entry.asset_type.as_str(), "EQUITY" | "EQUITY_CANDIDATE"),
                _ => true,
            };
            let text_match = entry.code.contains(&q)
                || entry.symbol.to_lowercase().contains(&q)
                || entry.instrument_id.to_lowercase().contains(&q)
                || entry
                    .name
                    .as_deref()
                    .is_some_and(|name| name.to_lowercase().contains(&q));
            type_match && text_match
        })
        .collect::<Vec<_>>();
    if offset > matches.len() {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "cursor offset exceeds the result set",
            Value::Null,
        );
    }
    let items = matches.iter().skip(offset).take(limit).map(|entry| {
        json!({
            "instrument_id":entry.instrument_id,"symbol":entry.symbol,"code":entry.code,
            "name":entry.name,"exchange":entry.exchange,"asset_type":entry.asset_type,
            "identity_source":entry.identity_source,"classification_source":entry.classification_source,
            "classification_notice":if entry.asset_type == "EQUITY_CANDIDATE" {"股票候选，分类未核验"} else {"当前目录分类，不代表历史 PIT 身份"},
            "first_observed_date":entry.first_observed_date,"last_observed_date":entry.last_observed_date,
            "listed_date":entry.listed_date,"delisted_date":entry.delisted_date
        })
    }).collect::<Vec<_>>();
    let next_offset = offset + items.len();
    let next_cursor = (next_offset < matches.len()).then(|| {
        encode_cursor(
            next_offset,
            &resolved.identity.snapshot_id.to_string(),
            &filter_hash(&[&q, asset_filter.unwrap_or("all")]),
        )
    });
    Json(json!({"items":items,"snapshot":resolved.identity,"data_cutoff_date":resolved.identity.data_cutoff_date,"next_cursor":next_cursor})).into_response()
}

async fn daily_bars(State(state): State<MarketState>, Query(query): Query<BarsQuery>) -> Response {
    let Some(instrument_id) = query.instrument_id.as_deref().map(str::trim) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "instrument_id is required",
            Value::Null,
        );
    };
    if instrument_id.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "instrument_id is required",
            Value::Null,
        );
    }
    if query
        .adjustment
        .as_deref()
        .is_some_and(|adjustment| adjustment != "none")
    {
        return error(
            StatusCode::BAD_REQUEST,
            "unsupported_adjustment",
            "only raw unadjusted prices (adjustment=none) are supported",
            json!({"supported":["none"]}),
        );
    }
    let Some(start_value) = query.start.as_deref() else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "start is required",
            Value::Null,
        );
    };
    let start = match NaiveDate::parse_from_str(start_value, "%Y-%m-%d") {
        Ok(date) => date,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "start must be YYYY-MM-DD",
                Value::Null,
            );
        }
    };
    let Some(end_value) = query.end.as_deref() else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "end is required",
            Value::Null,
        );
    };
    let end = match NaiveDate::parse_from_str(end_value, "%Y-%m-%d") {
        Ok(date) => date,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "end must be YYYY-MM-DD",
                Value::Null,
            );
        }
    };
    if start > end || (end - start).num_days() > 3660 {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_range",
            "date range must be ordered and no longer than 3661 calendar days",
            Value::Null,
        );
    }
    let limit = match bounded_limit(query.limit.as_deref(), 250, 1000) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let resolved = match resolve_snapshot(&state.0, query.snapshot_id.as_deref()) {
        Ok(snapshot) => snapshot,
        Err(response) => return response,
    };
    let manifest_start = match NaiveDate::parse_from_str(&resolved.identity.first_date, "%Y-%m-%d")
    {
        Ok(date) => date,
        Err(_) => {
            return error(
                StatusCode::CONFLICT,
                "snapshot_invalid",
                "manifest research_start is invalid",
                Value::Null,
            );
        }
    };
    let manifest_end = match NaiveDate::parse_from_str(&resolved.identity.last_date, "%Y-%m-%d") {
        Ok(date) => date,
        Err(_) => {
            return error(
                StatusCode::CONFLICT,
                "snapshot_invalid",
                "manifest research_end is invalid",
                Value::Null,
            );
        }
    };
    if start < manifest_start || end > manifest_end {
        return error(
            StatusCode::BAD_REQUEST,
            "range_outside_snapshot",
            "requested range exceeds the published research range",
            json!({"start":resolved.identity.first_date,"end":resolved.identity.last_date}),
        );
    }
    let entries = match resolved.securities() {
        Ok(entries) => entries,
        Err(e) => {
            return error(
                StatusCode::CONFLICT,
                "snapshot_invalid",
                &e.to_string(),
                Value::Null,
            );
        }
    };
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.instrument_id == instrument_id)
    else {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_instrument",
            "instrument_id is not present in this snapshot",
            json!({"instrument_id":instrument_id}),
        );
    };
    let symbol = entry.symbol.clone();
    let cursor_scope = filter_hash(&[instrument_id, &start.to_string(), &end.to_string()]);
    let offset = match decode_cursor(
        query.cursor.as_deref(),
        &resolved.identity.snapshot_id.to_string(),
        &cursor_scope,
    ) {
        Ok(value) => value,
        Err(message) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_cursor",
                &message,
                Value::Null,
            );
        }
    };
    let rows = match resolved.bars(&symbol, start, end) {
        Ok(rows) => rows,
        Err(e) => {
            return error(
                StatusCode::CONFLICT,
                "snapshot_invalid",
                &e.to_string(),
                Value::Null,
            );
        }
    };
    if offset > rows.len() {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "cursor offset exceeds the result set",
            Value::Null,
        );
    }
    let bars = rows
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let next_offset = offset + bars.len();
    let next_cursor = (next_offset < rows.len()).then(|| {
        encode_cursor(
            next_offset,
            &resolved.identity.snapshot_id.to_string(),
            &cursor_scope,
        )
    });
    let price_unit = match entry.asset_type.as_str() {
        "ETF" => "CNY/份",
        "EQUITY" => "CNY/股",
        _ => "unknown",
    };
    Json(json!({
        "snapshot":resolved.identity,"instrument":{"instrument_id":entry.instrument_id,"symbol":entry.symbol,"code":entry.code,"name":entry.name,"exchange":entry.exchange,"asset_type":entry.asset_type},
        "data_cutoff_date":resolved.identity.data_cutoff_date,"range":{"start":start,"end":end},"price_adjustment":"none","price_unit":price_unit,"volume_unit":"股/份","amount_unit":"CNY","items":bars,"next_cursor":next_cursor
    })).into_response()
}

fn to_bar(row: &DailyResearchRow) -> Bar {
    Bar {
        trade_date: row.trade_date,
        open: row.open,
        high: row.high,
        low: row.low,
        close: row.close,
        volume_shares: row.volume,
        amount_cny: row.amount,
        source: row.source.clone(),
        observed_at: row.observed_at.clone(),
    }
}

fn warehouse_bars(
    data_path: &Path,
    symbol: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> anyhow::Result<Vec<Bar>> {
    let connection = duckdb::Connection::open_in_memory()?;
    let escaped_path = data_path.to_string_lossy().replace('\'', "''");
    let escaped_symbol = symbol.replace('\'', "''");
    let sql = format!(
        "SELECT trade_date::VARCHAR, CAST(open AS DOUBLE), CAST(high AS DOUBLE), \
         CAST(low AS DOUBLE), CAST(close AS DOUBLE), CAST(volume_shares AS DOUBLE), \
         CAST(amount_cny AS DOUBLE), source, CAST(observed_at AS VARCHAR) \
         FROM read_parquet('{escaped_path}') WHERE symbol='{escaped_symbol}' \
         AND trade_date BETWEEN DATE '{start}' AND DATE '{end}' ORDER BY trade_date"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        let date: String = row.get(0)?;
        Ok((
            date,
            row.get::<_, Option<f64>>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, Option<f64>>(3)?,
            row.get::<_, Option<f64>>(4)?,
            row.get::<_, Option<f64>>(5)?,
            row.get::<_, Option<f64>>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
        ))
    })?;
    rows.map(|row| {
        let (date, open, high, low, close, volume_shares, amount_cny, source, observed_at) = row?;
        Ok(Bar {
            trade_date: NaiveDate::parse_from_str(&date, "%Y-%m-%d")?,
            open,
            high,
            low,
            close,
            volume_shares,
            amount_cny,
            source,
            observed_at,
        })
    })
    .collect()
}

#[allow(clippy::result_large_err)]
fn bounded_limit(value: Option<&str>, default: usize, maximum: usize) -> Result<usize, Response> {
    let limit = match value {
        None => default,
        Some(raw) => match raw.parse::<usize>() {
            Ok(limit) => limit,
            Err(_) => {
                return Err(error(
                    StatusCode::BAD_REQUEST,
                    "invalid_limit",
                    "limit must be a positive integer",
                    Value::Null,
                ));
            }
        },
    };
    if !(1..=maximum).contains(&limit) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_limit",
            &format!("limit must be between 1 and {maximum}"),
            Value::Null,
        ));
    }
    Ok(limit)
}

#[allow(clippy::result_large_err)]
fn resolve_snapshot(output: &Path, requested: Option<&str>) -> Result<ResolvedSnapshot, Response> {
    let root = output.join("snapshots");
    let requested_id = match requested {
        Some(value) => Some(Uuid::parse_str(value).map_err(|_| {
            error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "snapshot_id must be a UUID",
                Value::Null,
            )
        })?),
        None => None,
    };
    if requested_id.is_none() && root.join("current.json").is_file() {
        return read_warehouse_snapshot(output, None);
    }
    if let Some(id) = requested_id {
        let manifest_path = root.join(id.to_string()).join("manifest.json");
        if manifest_path.is_file() {
            let bytes = fs::read(&manifest_path).map_err(|_| {
                error(
                    StatusCode::CONFLICT,
                    "snapshot_invalid",
                    "snapshot manifest is unreadable",
                    Value::Null,
                )
            })?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                error(
                    StatusCode::CONFLICT,
                    "snapshot_invalid",
                    "snapshot manifest is malformed",
                    Value::Null,
                )
            })?;
            if value.get("data_file").is_some() {
                return read_warehouse_snapshot(output, Some(id));
            }
        }
    }
    let dirs = if let Some(id) = requested_id {
        let dir = root.join(id.to_string());
        if !dir.exists() {
            return Err(error(
                StatusCode::NOT_FOUND,
                "snapshot_not_found",
                "snapshot_id was not found",
                json!({"snapshot_id":id}),
            ));
        }
        vec![dir]
    } else {
        fs::read_dir(&root)
            .map_err(|_| {
                error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "snapshot_unavailable",
                    "no published daily snapshot is available",
                    Value::Null,
                )
            })?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>()
    };
    let directory = if requested_id.is_some() {
        dirs.into_iter().next().expect("requested directory")
    } else {
        let mut published = Vec::new();
        for directory in dirs {
            let path = directory.join("publication.json");
            if !path.exists() {
                continue;
            }
            let publication: Publication =
                serde_json::from_slice(&fs::read(&path).map_err(|_| {
                    error(
                        StatusCode::CONFLICT,
                        "snapshot_invalid",
                        "publication record is unreadable",
                        Value::Null,
                    )
                })?)
                .map_err(|_| {
                    error(
                        StatusCode::CONFLICT,
                        "snapshot_invalid",
                        "publication record is malformed",
                        Value::Null,
                    )
                })?;
            if publication.status == "published" {
                let published_time =
                    chrono::DateTime::parse_from_rfc3339(&publication.published_at)
                        .map_err(|_| {
                            error(
                                StatusCode::CONFLICT,
                                "snapshot_invalid",
                                "published_at must be RFC 3339",
                                Value::Null,
                            )
                        })?
                        .timestamp_millis();
                published.push((published_time, publication.snapshot_id, directory));
            }
        }
        published.sort_by_key(|a| (a.0, a.1));
        published
            .pop()
            .map(|(_, _, directory)| directory)
            .ok_or_else(|| {
                error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "snapshot_unavailable",
                    "no published daily snapshot is available",
                    Value::Null,
                )
            })?
    };
    read_snapshot(&directory)
}

#[allow(clippy::result_large_err)]
fn read_snapshot(directory: &Path) -> Result<ResolvedSnapshot, Response> {
    let manifest_path = directory.join("manifest.json");
    let publication_path = directory.join("publication.json");
    if !manifest_path.is_file() || !publication_path.is_file() {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "snapshot_not_published",
            "snapshot has no complete publication record",
            Value::Null,
        ));
    }
    let manifest_bytes = fs::read(&manifest_path).map_err(|_| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "snapshot_unavailable",
            "cannot read snapshot manifest",
            Value::Null,
        )
    })?;
    let manifest_sha256 = sha256(&manifest_bytes);
    let manifest: DailySnapshotManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| {
            error(
                StatusCode::CONFLICT,
                "snapshot_invalid",
                "snapshot manifest is malformed",
                Value::Null,
            )
        })?;
    let publication: Publication =
        serde_json::from_slice(&fs::read(&publication_path).map_err(|_| {
            error(
                StatusCode::CONFLICT,
                "snapshot_invalid",
                "publication record is unreadable",
                Value::Null,
            )
        })?)
        .map_err(|_| {
            error(
                StatusCode::CONFLICT,
                "snapshot_invalid",
                "publication record is malformed",
                Value::Null,
            )
        })?;
    if publication.status != "published"
        || publication.snapshot_id != manifest.base.snapshot_id
        || publication.manifest_sha256 != manifest_sha256
    {
        return Err(error(
            StatusCode::CONFLICT,
            "snapshot_identity_mismatch",
            "publication identity or manifest hash does not match",
            Value::Null,
        ));
    }
    if manifest.schema_version != 1
        || manifest.base.file != directory.join("daily_research.parquet")
    {
        return Err(error(
            StatusCode::CONFLICT,
            "snapshot_invalid",
            "unsupported schema or manifest file path",
            Value::Null,
        ));
    }
    verify_snapshot_manifest(&manifest.base).map_err(|e| {
        error(
            StatusCode::CONFLICT,
            "snapshot_hash_mismatch",
            &e.to_string(),
            Value::Null,
        )
    })?;
    if publication.price_adjustment != "none" {
        return Err(error(
            StatusCode::CONFLICT,
            "snapshot_invalid",
            "only raw unadjusted snapshots are supported",
            Value::Null,
        ));
    }
    let identity = SnapshotIdentity {
        snapshot_id: manifest.base.snapshot_id,
        sha256: manifest.base.sha256.clone(),
        manifest_sha256,
        created_at: manifest.base.created_at.clone(),
        published_at: Some(publication.published_at),
        data_cutoff_date: publication.data_cutoff_date,
        first_date: manifest.research_start.clone(),
        last_date: manifest.research_end.clone(),
        price_adjustment: publication.price_adjustment,
        coverage_status: publication.coverage_status,
        source_status: publication.source_status,
    };
    Ok(ResolvedSnapshot {
        source: SnapshotSource::Research(Box::new(manifest)),
        identity,
    })
}

#[allow(clippy::result_large_err)]
fn read_warehouse_snapshot(
    data_dir: &Path,
    id: Option<Uuid>,
) -> Result<ResolvedSnapshot, Response> {
    let snapshot = resolve_published_daily_snapshot(data_dir, id).map_err(|e| {
        let message = e.to_string();
        let (status, code) = if id.is_none() && message.contains("no current published") {
            (StatusCode::SERVICE_UNAVAILABLE, "snapshot_unavailable")
        } else if message.contains("not published") {
            (StatusCode::SERVICE_UNAVAILABLE, "snapshot_not_published")
        } else {
            (StatusCode::CONFLICT, "snapshot_invalid")
        };
        error(status, code, &message, Value::Null)
    })?;
    let directory = snapshot
        .manifest_path
        .parent()
        .expect("manifest has parent");
    let manifest: Value =
        serde_json::from_slice(&fs::read(&snapshot.manifest_path).map_err(|_| {
            error(
                StatusCode::CONFLICT,
                "snapshot_invalid",
                "manifest is unreadable",
                Value::Null,
            )
        })?)
        .map_err(|_| {
            error(
                StatusCode::CONFLICT,
                "snapshot_invalid",
                "manifest is malformed",
                Value::Null,
            )
        })?;
    if manifest["price_adjustment"] != "none" || manifest["coverage_status"] != "complete" {
        return Err(error(
            StatusCode::CONFLICT,
            "snapshot_invalid",
            "published daily snapshot has an unsupported price or coverage status",
            Value::Null,
        ));
    }
    let catalog: Vec<Value> =
        serde_json::from_slice(&fs::read(directory.join("securities.json")).map_err(|_| {
            error(
                StatusCode::CONFLICT,
                "snapshot_invalid",
                "security catalog is unreadable",
                Value::Null,
            )
        })?)
        .map_err(|_| {
            error(
                StatusCode::CONFLICT,
                "snapshot_invalid",
                "security catalog is malformed",
                Value::Null,
            )
        })?;
    let directory = catalog
        .into_iter()
        .filter_map(|item| {
            let instrument_id = item["instrument_id"].as_str()?;
            let market = item["market"].as_str()?;
            let first = item["first_observed_date"].as_str().unwrap_or("");
            let last = item["last_observed_date"].as_str().unwrap_or("");
            Some(SecurityDirectoryEntry {
                instrument_id: instrument_id.to_owned(),
                symbol: item["symbol"].as_str()?.to_owned(),
                code: item["code"].as_str()?.to_owned(),
                name: item["name"].as_str().map(str::to_owned),
                exchange: market.to_owned(),
                asset_type: item["asset_type"].as_str()?.to_owned(),
                classification_source: None,
                identity_source: item["name_source"].as_str().map(str::to_owned),
                first_observed_date: first.to_owned(),
                last_observed_date: last.to_owned(),
                listed_date: None,
                delisted_date: None,
            })
        })
        .collect::<Vec<_>>();
    let created_at = manifest["created_at"].as_str().unwrap_or("").to_owned();
    let first_date = manifest["requested_start"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let last_date = manifest["as_of_date"].as_str().unwrap_or("").to_owned();
    if chrono::DateTime::parse_from_rfc3339(&created_at).is_err()
        || NaiveDate::parse_from_str(&first_date, "%Y-%m-%d").is_err()
        || NaiveDate::parse_from_str(&last_date, "%Y-%m-%d").is_err()
    {
        return Err(error(
            StatusCode::CONFLICT,
            "snapshot_invalid",
            "published manifest has invalid time or range",
            Value::Null,
        ));
    }
    Ok(ResolvedSnapshot {
        source: SnapshotSource::Warehouse {
            data_path: snapshot.data_path,
            directory,
        },
        identity: SnapshotIdentity {
            snapshot_id: snapshot.snapshot_id,
            sha256: snapshot.data_sha256,
            manifest_sha256: snapshot.manifest_sha256,
            created_at: created_at.clone(),
            published_at: None,
            data_cutoff_date: snapshot.data_cutoff_date,
            first_date,
            last_date,
            price_adjustment: "none".into(),
            coverage_status: snapshot.coverage_status,
            source_status: "recorded_per_bar".into(),
        },
    })
}

fn filter_hash(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    format!("{:x}", digest.finalize())[..16].to_owned()
}
fn encode_cursor(offset: usize, snapshot: &str, filter: &str) -> String {
    format!("v1.{snapshot}.{offset}.{filter}")
}
fn decode_cursor(cursor: Option<&str>, snapshot: &str, filter: &str) -> Result<usize, String> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let parts = cursor.split('.').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != "v1" || parts[1] != snapshot || parts[3] != filter {
        return Err("cursor belongs to a different snapshot or query".into());
    }
    parts[2]
        .parse::<usize>()
        .map_err(|_| "cursor offset is invalid".into())
}
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn error(status: StatusCode, code: &str, message: &str, details: Value) -> Response {
    (
        status,
        Json(json!({"error":{"code":code,"message":message,"details":details}})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::SnapshotManifest;
    use axum::body::to_bytes;
    use duckdb::Connection;
    use std::fs;
    use tempfile::tempdir;

    pub(super) fn published_fixture(
        root: &Path,
        id: Uuid,
        published_at: &str,
    ) -> DailySnapshotManifest {
        let directory = root.join("snapshots").join(id.to_string());
        fs::create_dir_all(&directory).unwrap();
        let parquet = directory.join("daily_research.parquet");
        let escaped = parquet.to_string_lossy().replace('\'', "''");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (
               SELECT 'SH:600000' instrument_id, 'sh600000' symbol, '600000' code, '浦发银行' AS \"name\", 'SH' AS exchange, 'EQUITY' asset_type,
                 DATE '2024-01-01' trade_date, 9.0 AS \"open\", 10.0 AS \"high\", 8.0 AS \"low\", 9.5 AS \"close\", 900.0 volume, 9000.0 amount,
                 'tencent' AS \"source\", '2024-01-01T17:00:00+08:00' AS observed_at, NULL::VARCHAR AS trade_status, false AS is_tradable,
                 NULL::VARCHAR AS status_sources, NULL::VARCHAR AS status_observed_at, false AS execution_status_covered, true AS is_preheat
               UNION ALL SELECT 'SH:600000','sh600000','600000','浦发银行','SH','EQUITY',DATE '2024-01-02',10.0,12.0,9.0,11.0,1000.0,11000.0,'tencent','2024-01-02T17:00:00+08:00',NULL::VARCHAR,false,NULL::VARCHAR,NULL::VARCHAR,false,false
               UNION ALL SELECT 'SH:600000','sh600000','600000','浦发银行','SH','EQUITY',DATE '2024-01-03',11.0,13.0,10.0,12.0,1200.0,NULL::DOUBLE,'tdx','2024-01-03T17:00:00+08:00',NULL::VARCHAR,false,NULL::VARCHAR,NULL::VARCHAR,false,false
               UNION ALL SELECT 'SZ:510300','sz510300','510300','沪深300ETF','SZ','ETF',DATE '2024-01-02',3.0,3.2,2.9,3.1,50000.0,155000.0,'tencent','2024-01-02T17:00:00+08:00',NULL::VARCHAR,false,NULL::VARCHAR,NULL::VARCHAR,false,false
             ) TO '{escaped}' (FORMAT PARQUET)"
        )).unwrap();
        let manifest = DailySnapshotManifest {
            base: SnapshotManifest {
                snapshot_id: id,
                created_at: published_at.to_owned(),
                source_database: PathBuf::new(),
                file: parquet.clone(),
                sha256: sha256(&fs::read(&parquet).unwrap()),
                rows: 4,
                symbols: 2,
                first_date: "2024-01-01".into(),
                last_date: "2024-01-03".into(),
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
                selection_rule: "fixture".into(),
                limitations: vec!["synthetic".into()],
            },
            schema_version: 1,
            asset_types: vec!["EQUITY".into(), "ETF".into()],
            securities: vec!["sh600000".into(), "sz510300".into()],
            preheat_start: "2024-01-01".into(),
            research_start: "2024-01-02".into(),
            research_end: "2024-01-03".into(),
            calendar_identity: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        fs::write(directory.join("manifest.json"), &manifest_bytes).unwrap();
        let publication = Publication {
            status: "published".into(),
            snapshot_id: id,
            published_at: published_at.into(),
            data_cutoff_date: "2024-01-03".into(),
            coverage_status: "unverified".into(),
            price_adjustment: "none".into(),
            source_status: "unknown".into(),
            manifest_sha256: sha256(&manifest_bytes),
        };
        fs::write(
            directory.join("publication.json"),
            serde_json::to_vec_pretty(&publication).unwrap(),
        )
        .unwrap();
        manifest
    }

    pub(super) async fn body(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn mixed_catalog_search_preserves_ids_and_asset_types() {
        let dir = tempdir().unwrap();
        let manifest =
            published_fixture(dir.path(), Uuid::from_u128(1), "2024-01-04T09:00:00+08:00");
        let response = instruments(
            State(MarketState(Arc::new(dir.path().to_path_buf()))),
            Query(InstrumentQuery {
                q: Some("600000".into()),
                asset_type: Some("stock".into()),
                snapshot_id: Some(manifest.base.snapshot_id.to_string()),
                limit: Some("20".into()),
                cursor: None,
            }),
        )
        .await;
        let (status, json) = body(response).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["items"][0]["instrument_id"], "SH:600000");
        assert_eq!(json["items"][0]["asset_type"], "EQUITY");
        assert_eq!(
            json["snapshot"]["snapshot_id"],
            manifest.base.snapshot_id.to_string()
        );
        let response = instruments(
            State(MarketState(Arc::new(dir.path().to_path_buf()))),
            Query(InstrumentQuery {
                q: Some("510300".into()),
                asset_type: Some("etf".into()),
                snapshot_id: None,
                limit: None,
                cursor: None,
            }),
        )
        .await;
        let (status, json) = body(response).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["items"][0]["instrument_id"], "SZ:510300");
        assert_eq!(json["items"][0]["exchange"], "SZ");
    }

    #[tokio::test]
    async fn bars_are_bounded_sorted_paged_and_exclude_preheat() {
        let dir = tempdir().unwrap();
        let manifest =
            published_fixture(dir.path(), Uuid::from_u128(2), "2024-01-04T09:00:00+08:00");
        let state = State(MarketState(Arc::new(dir.path().to_path_buf())));
        let base = BarsQuery {
            instrument_id: Some("SH:600000".into()),
            snapshot_id: Some(manifest.base.snapshot_id.to_string()),
            start: Some("2024-01-02".into()),
            end: Some("2024-01-03".into()),
            limit: Some("1".into()),
            cursor: None,
            adjustment: None,
        };
        let (status, first) = body(daily_bars(state.clone(), Query(base.clone())).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first["items"][0]["trade_date"], "2024-01-02");
        assert_eq!(first["items"][0]["open"], 10.0);
        assert_eq!(first["items"][0]["high"], 12.0);
        assert_eq!(first["items"][0]["low"], 9.0);
        assert_eq!(first["items"][0]["close"], 11.0);
        assert_eq!(first["items"][0]["volume_shares"], 1000.0);
        assert_eq!(first["items"][0]["amount_cny"], 11000.0);
        assert_eq!(first["price_unit"], "CNY/股");
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();
        let second = BarsQuery {
            cursor: Some(cursor),
            ..base.clone()
        };
        let (status, next) = body(daily_bars(state.clone(), Query(second)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(next["items"][0]["trade_date"], "2024-01-03");
        assert_eq!(next["items"][0]["open"], 11.0);
        assert!(next["items"][0]["amount_cny"].is_null());
        assert!(next["next_cursor"].is_null());
        let changed_range = BarsQuery {
            start: Some("2024-01-03".into()),
            cursor: first["next_cursor"].as_str().map(str::to_owned),
            ..base
        };
        let (status, err) = body(daily_bars(state, Query(changed_range)).await).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(err["error"]["code"], "invalid_cursor");
    }

    #[tokio::test]
    async fn empty_unknown_unpublished_and_corrupt_snapshots_are_distinguished() {
        let dir = tempdir().unwrap();
        let manifest =
            published_fixture(dir.path(), Uuid::from_u128(3), "2024-01-04T09:00:00+08:00");
        let state = State(MarketState(Arc::new(dir.path().to_path_buf())));
        let query = BarsQuery {
            instrument_id: Some("SZ:510300".into()),
            snapshot_id: Some(manifest.base.snapshot_id.to_string()),
            start: Some("2024-01-03".into()),
            end: Some("2024-01-03".into()),
            limit: None,
            cursor: None,
            adjustment: None,
        };
        let (status, empty) = body(daily_bars(state.clone(), Query(query.clone())).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(empty["items"].as_array().unwrap().len(), 0);
        let unknown = BarsQuery {
            instrument_id: Some("SH:999999".into()),
            ..query.clone()
        };
        let (status, err) = body(daily_bars(state.clone(), Query(unknown)).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(err["error"]["code"], "unknown_instrument");
        let missing_id = Uuid::from_u128(99);
        let missing = BarsQuery {
            snapshot_id: Some(missing_id.to_string()),
            ..query.clone()
        };
        let (status, err) = body(daily_bars(state.clone(), Query(missing)).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(err["error"]["code"], "snapshot_not_found");
        let unpub_dir = dir
            .path()
            .join("snapshots")
            .join(Uuid::from_u128(4).to_string());
        fs::create_dir_all(&unpub_dir).unwrap();
        fs::write(unpub_dir.join("manifest.json"), "{}").unwrap();
        let unpub = BarsQuery {
            snapshot_id: Some(Uuid::from_u128(4).to_string()),
            ..query.clone()
        };
        let (status, err) = body(daily_bars(state.clone(), Query(unpub)).await).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err["error"]["code"], "snapshot_not_published");
        fs::write(manifest.base.file.clone(), b"corrupt parquet").unwrap();
        let (status, err) = body(daily_bars(state, Query(query)).await).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(err["error"]["code"], "snapshot_hash_mismatch");
    }

    #[tokio::test]
    async fn explicit_snapshot_remains_pinned_after_new_snapshot_publish() {
        let dir = tempdir().unwrap();
        let old = published_fixture(dir.path(), Uuid::from_u128(10), "2024-01-04T09:00:00+08:00");
        let newer = published_fixture(dir.path(), Uuid::from_u128(11), "2024-01-05T09:00:00+08:00");
        let state = State(MarketState(Arc::new(dir.path().to_path_buf())));
        let search = InstrumentQuery {
            q: Some("600000".into()),
            asset_type: None,
            snapshot_id: None,
            limit: None,
            cursor: None,
        };
        let (status, latest) = body(instruments(state.clone(), Query(search)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            latest["snapshot"]["snapshot_id"],
            newer.base.snapshot_id.to_string()
        );
        let query = BarsQuery {
            instrument_id: Some("SH:600000".into()),
            snapshot_id: Some(old.base.snapshot_id.to_string()),
            start: Some("2024-01-02".into()),
            end: Some("2024-01-03".into()),
            limit: None,
            cursor: None,
            adjustment: None,
        };
        let (status, fixed) = body(daily_bars(state, Query(query)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            fixed["snapshot"]["snapshot_id"],
            old.base.snapshot_id.to_string()
        );
    }
}

#[cfg(test)]
mod additional_tests {
    use super::*;
    use axum::body::to_bytes;
    use duckdb::Connection;
    use std::fs;
    use std::time::Instant;
    use tempfile::tempdir;

    fn published_warehouse_fixture(root: &Path, id: Uuid, price: f64) {
        let directory = root.join("snapshots").join(id.to_string());
        fs::create_dir_all(&directory).unwrap();
        let parquet = directory.join("daily.parquet");
        let escaped = parquet.to_string_lossy().replace('\'', "''");
        duckdb::Connection::open_in_memory()
            .unwrap()
            .execute_batch(&format!(
                "COPY (SELECT 'sh600000' symbol, DATE '2024-01-02' trade_date, \
             {price}::DOUBLE AS \"open\", {price}::DOUBLE AS high, \
             {price}::DOUBLE AS low, {price}::DOUBLE AS close, \
             1000.0::DOUBLE volume_shares, 11000.0::DOUBLE amount_cny, \
             'tencent' AS \"source\", '2024-01-02T17:00:00+08:00' observed_at) \
             TO '{escaped}' (FORMAT PARQUET)"
            ))
            .unwrap();
        let catalog = serde_json::json!([{"instrument_id":"SH:600000","symbol":"sh600000",
            "code":"600000","market":"SH","asset_type":"EQUITY_CANDIDATE",
            "name":"合成股票","name_source":"fixture","first_observed_date":"2024-01-02",
            "last_observed_date":"2024-01-02","listed_date":null,"delisted_date":null}]);
        let catalog_bytes = serde_json::to_vec_pretty(&catalog).unwrap();
        fs::write(directory.join("securities.json"), &catalog_bytes).unwrap();
        fs::write(directory.join("trading_calendar.txt"), b"2024-01-02:true").unwrap();
        let manifest = serde_json::json!({"schema_version":7,"snapshot_id":id,
            "created_at":"2024-01-03T09:00:00+08:00","requested_start":"2024-01-02",
            "as_of_date":"2024-01-02","data_cutoff_date":"2024-01-02",
            "coverage_status":"complete","price_adjustment":"none",
            "data_file":"daily.parquet","data_sha256":sha256(&fs::read(&parquet).unwrap()),
            "catalog_file":"securities.json","catalog_sha256":sha256(&catalog_bytes),
            "calendar_file":"trading_calendar.txt","calendar_sha256":sha256(b"2024-01-02:true")});
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        fs::write(directory.join("manifest.json"), &manifest_bytes).unwrap();
        fs::write(
            root.join("snapshots/current.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"snapshot_id":id,"manifest_sha256":sha256(&manifest_bytes)}),
            )
            .unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn warehouse_publication_flows_into_search_and_daily_bars_without_live_database() {
        let root = tempdir().unwrap();
        let old_id = Uuid::from_u128(500);
        published_warehouse_fixture(root.path(), old_id, 10.0);
        let state = State(MarketState(Arc::new(root.path().to_path_buf())));
        let search = InstrumentQuery {
            q: Some("600000".into()),
            asset_type: Some("stock".into()),
            snapshot_id: None,
            limit: None,
            cursor: None,
        };
        let response = instruments(state.clone(), Query(search)).await;
        let (status, body) = tests::body(response).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["snapshot"]["snapshot_id"], old_id.to_string());
        assert!(body["snapshot"]["published_at"].is_null());
        assert_eq!(body["items"][0]["asset_type"], "EQUITY_CANDIDATE");
        let newer_id = Uuid::from_u128(501);
        published_warehouse_fixture(root.path(), newer_id, 20.0);
        let fixed = BarsQuery {
            instrument_id: Some("SH:600000".into()),
            snapshot_id: Some(old_id.to_string()),
            start: Some("2024-01-02".into()),
            end: Some("2024-01-02".into()),
            limit: None,
            cursor: None,
            adjustment: None,
        };
        let (status, old) = tests::body(daily_bars(state.clone(), Query(fixed)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(old["items"][0]["open"], 10.0);
        assert_eq!(old["items"][0]["high"], 10.0);
        assert_eq!(old["items"][0]["low"], 10.0);
        assert_eq!(old["items"][0]["volume_shares"], 1000.0);
        let latest = BarsQuery {
            instrument_id: Some("SH:600000".into()),
            snapshot_id: None,
            start: Some("2024-01-02".into()),
            end: Some("2024-01-02".into()),
            limit: None,
            cursor: None,
            adjustment: None,
        };
        let (status, current) = tests::body(daily_bars(state, Query(latest)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(current["snapshot"]["snapshot_id"], newer_id.to_string());
        assert_eq!(current["items"][0]["open"], 20.0);
    }

    #[tokio::test]
    async fn instrument_search_is_cursor_paged_and_cursor_is_query_bound() {
        let dir = tempdir().unwrap();
        let snapshot =
            tests::published_fixture(dir.path(), Uuid::from_u128(20), "2024-01-04T09:00:00+08:00");
        let state = State(MarketState(Arc::new(dir.path().to_path_buf())));
        let first_query = InstrumentQuery {
            q: Some("00".into()),
            asset_type: None,
            snapshot_id: Some(snapshot.base.snapshot_id.to_string()),
            limit: Some("1".into()),
            cursor: None,
        };
        let first_response = instruments(state.clone(), Query(first_query.clone())).await;
        let first_bytes = to_bytes(first_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let first: Value = serde_json::from_slice(&first_bytes).unwrap();
        assert_eq!(first["items"].as_array().unwrap().len(), 1);
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();
        let next_query = InstrumentQuery {
            cursor: Some(cursor.clone()),
            ..first_query.clone()
        };
        let next_response = instruments(state.clone(), Query(next_query)).await;
        let next_bytes = to_bytes(next_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let next: Value = serde_json::from_slice(&next_bytes).unwrap();
        assert_eq!(next["items"].as_array().unwrap().len(), 1);
        assert_ne!(
            next["items"][0]["instrument_id"],
            first["items"][0]["instrument_id"]
        );
        let changed = InstrumentQuery {
            q: Some("600000".into()),
            cursor: Some(cursor),
            ..first_query
        };
        let response = instruments(state, Query(changed)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn missing_snapshot_fields_and_latest_corruption_fail_closed() {
        let dir = tempdir().unwrap();
        let valid =
            tests::published_fixture(dir.path(), Uuid::from_u128(30), "2024-01-04T09:00:00+08:00");
        let corrupt =
            tests::published_fixture(dir.path(), Uuid::from_u128(31), "2024-01-05T09:00:00+08:00");
        let parquet = corrupt.base.file.clone();
        let escaped = parquet.to_string_lossy().replace('\'', "''");
        Connection::open_in_memory().unwrap().execute_batch(&format!(
            "COPY (SELECT 'SH:600000' instrument_id, 'sh600000' symbol, '600000' code, '浦发银行' AS \"name\", 'SH' AS exchange, 'EQUITY' asset_type, DATE '2024-01-02' trade_date, 10.0 AS \"open\", 9.0 AS \"low\", 11.0 AS \"close\", 1000.0 volume, 11000.0 amount, 'tencent' AS \"source\", '2024-01-02T17:00:00+08:00' observed_at, NULL::VARCHAR trade_status, false is_tradable, NULL::VARCHAR status_sources, NULL::VARCHAR status_observed_at, false execution_status_covered, false is_preheat) TO '{escaped}' (FORMAT PARQUET)"
        )).unwrap();
        let mut broken_manifest = corrupt;
        broken_manifest.base.sha256 = sha256(&fs::read(&parquet).unwrap());
        let manifest_path = parquet.parent().unwrap().join("manifest.json");
        let manifest_bytes = serde_json::to_vec_pretty(&broken_manifest).unwrap();
        fs::write(&manifest_path, &manifest_bytes).unwrap();
        let mut publication: Publication = serde_json::from_slice(
            &fs::read(parquet.parent().unwrap().join("publication.json")).unwrap(),
        )
        .unwrap();
        publication.manifest_sha256 = sha256(&manifest_bytes);
        fs::write(
            parquet.parent().unwrap().join("publication.json"),
            serde_json::to_vec_pretty(&publication).unwrap(),
        )
        .unwrap();
        let state = State(MarketState(Arc::new(dir.path().to_path_buf())));
        let query = BarsQuery {
            instrument_id: Some("SH:600000".into()),
            snapshot_id: Some(broken_manifest.base.snapshot_id.to_string()),
            start: Some("2024-01-02".into()),
            end: Some("2024-01-03".into()),
            limit: None,
            cursor: None,
            adjustment: None,
        };
        let response = daily_bars(state.clone(), Query(query.clone())).await;
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "snapshot_invalid");
        let latest = BarsQuery {
            snapshot_id: None,
            ..query
        };
        let response = daily_bars(state, Query(latest)).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_ne!(valid.base.snapshot_id, broken_manifest.base.snapshot_id);
    }

    #[tokio::test]
    async fn ten_year_page_size_and_latency_observation() {
        let dir = tempdir().unwrap();
        let mut snapshot =
            tests::published_fixture(dir.path(), Uuid::from_u128(40), "2024-01-04T09:00:00+08:00");
        let parquet = snapshot.base.file.clone();
        let escaped = parquet.to_string_lossy().replace('\'', "''");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (
               WITH days AS (
                 SELECT CAST(trade_date AS DATE) trade_date,
                        row_number() OVER (ORDER BY trade_date)-1 i
                 FROM generate_series(DATE '2016-09-24', DATE '2026-09-24', INTERVAL 1 DAY) t(trade_date)
                 WHERE dayofweek(trade_date) BETWEEN 1 AND 5
               )
               SELECT 'SH:600000' instrument_id, 'sh600000' symbol, '600000' code, '合成十年股票' AS \"name\", 'SH' exchange, 'EQUITY' asset_type,
                 trade_date, CAST(10+i*0.001 AS DOUBLE) AS \"open\", CAST(10.2+i*0.001 AS DOUBLE) AS \"high\",
                 CAST(9.8+i*0.001 AS DOUBLE) AS \"low\", CAST(10.1+i*0.001 AS DOUBLE) AS \"close\",
                 CAST(1000000+i AS DOUBLE) volume, CAST(10000000+i AS DOUBLE) amount,
                 'synthetic_fixture' AS \"source\", '2026-09-24T20:00:00+08:00' observed_at,
                 NULL::VARCHAR trade_status, false is_tradable, NULL::VARCHAR status_sources,
                 NULL::VARCHAR status_observed_at, false execution_status_covered, false is_preheat
               FROM days ORDER BY trade_date
             ) TO '{escaped}' (FORMAT PARQUET)"
        )).unwrap();
        let rows = conn
            .query_row(
                &format!("SELECT count(*) FROM read_parquet('{escaped}')"),
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        snapshot.base.rows = rows;
        snapshot.base.first_date = "2016-09-26".into();
        snapshot.base.last_date = "2026-09-24".into();
        snapshot.base.sha256 = sha256(&fs::read(&parquet).unwrap());
        snapshot.research_start = "2016-09-26".into();
        snapshot.research_end = "2026-09-24".into();
        let manifest_path = parquet.parent().unwrap().join("manifest.json");
        let manifest_bytes = serde_json::to_vec_pretty(&snapshot).unwrap();
        fs::write(&manifest_path, &manifest_bytes).unwrap();
        let publication_path = parquet.parent().unwrap().join("publication.json");
        let mut publication: Publication =
            serde_json::from_slice(&fs::read(&publication_path).unwrap()).unwrap();
        publication.data_cutoff_date = "2026-09-24".into();
        publication.manifest_sha256 = sha256(&manifest_bytes);
        fs::write(
            &publication_path,
            serde_json::to_vec_pretty(&publication).unwrap(),
        )
        .unwrap();
        let query = BarsQuery {
            instrument_id: Some("SH:600000".into()),
            snapshot_id: Some(snapshot.base.snapshot_id.to_string()),
            start: Some("2016-09-26".into()),
            end: Some("2026-09-24".into()),
            limit: Some("1000".into()),
            cursor: None,
            adjustment: None,
        };
        let began = Instant::now();
        let response = daily_bars(
            State(MarketState(Arc::new(dir.path().to_path_buf()))),
            Query(query),
        )
        .await;
        let elapsed = began.elapsed();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        println!(
            "synthetic daily-bars max page: rows={}, response_bytes={}, handler_elapsed_ms={}",
            body["items"].as_array().unwrap().len(),
            bytes.len(),
            elapsed.as_millis()
        );
        assert_eq!(body["items"].as_array().unwrap().len(), 1000);
        assert!(bytes.len() < 300_000);
    }
}
