use crate::core::{Bar, ExecutionStatus, ExecutionStatusMap};
use anyhow::{Context, Result, ensure};
use chrono::{Datelike, NaiveDate, Utc};
use duckdb::{AccessMode, Config, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub snapshot_id: Uuid,
    pub created_at: String,
    pub source_database: PathBuf,
    pub file: PathBuf,
    pub sha256: String,
    pub rows: i64,
    pub symbols: i64,
    pub first_date: String,
    pub last_date: String,
    #[serde(default)]
    pub benchmark_file: Option<PathBuf>,
    #[serde(default)]
    pub benchmark_sha256: Option<String>,
    #[serde(default)]
    pub benchmark_rows: i64,
    #[serde(default)]
    pub benchmark_name: Option<String>,
    #[serde(default)]
    pub trading_calendar_file: Option<PathBuf>,
    #[serde(default)]
    pub trading_calendar_sha256: Option<String>,
    #[serde(default)]
    pub trading_calendar_rows: i64,
    #[serde(default)]
    pub trading_calendar_first_date: Option<String>,
    #[serde(default)]
    pub trading_calendar_last_date: Option<String>,
    #[serde(default)]
    pub trading_calendar_source: Option<String>,
    pub selection_rule: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailySnapshotManifest {
    #[serde(flatten)]
    pub base: SnapshotManifest,
    pub schema_version: u32,
    pub asset_types: Vec<String>,
    pub securities: Vec<String>,
    pub preheat_start: String,
    pub research_start: String,
    pub research_end: String,
    pub calendar_identity: Option<String>,
}

/// Current observed security identity. Listing/delisting dates are deliberately absent: the
/// current schema does not provide reliable evidence for them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecurityDirectoryEntry {
    pub instrument_id: String,
    pub symbol: String,
    pub code: String,
    pub name: Option<String>,
    pub exchange: String,
    pub asset_type: String,
    pub classification_source: Option<String>,
    pub identity_source: Option<String>,
    pub first_observed_date: String,
    pub last_observed_date: String,
    pub listed_date: Option<String>,
    pub delisted_date: Option<String>,
}

/// Nullable, source-bearing daily row used by shared security search and chart APIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyResearchRow {
    pub instrument_id: String,
    pub symbol: String,
    pub code: String,
    pub name: Option<String>,
    pub exchange: String,
    pub asset_type: String,
    pub trade_date: NaiveDate,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: Option<f64>,
    pub volume: Option<f64>,
    pub amount: Option<f64>,
    pub source: Option<String>,
    pub observed_at: Option<String>,
    pub trade_status: Option<String>,
    pub is_tradable: Option<bool>,
    pub status_sources: Option<String>,
    pub status_observed_at: Option<String>,
    pub execution_status_covered: Option<bool>,
}

/// Minimal sidecar for the execution gate. `covered=false` or `status=None` means UNKNOWN;
/// `observed_at` is collection time and is not evidence of public availability time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionStateInput {
    pub instrument_id: String,
    pub symbol: String,
    pub trade_date: NaiveDate,
    pub status: Option<String>,
    pub source: Option<String>,
    pub covered: bool,
    pub status_observed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolCoverage {
    pub symbol: String,
    pub rows: usize,
    pub first_date: Option<String>,
    pub last_date: Option<String>,
    pub missing_dates: Vec<String>,
    pub unexpected_dates: Vec<String>,
    pub duplicate_dates: Vec<String>,
    pub invalid_ohlc_dates: Vec<String>,
    pub missing_source_dates: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MarketCoverageAudit {
    pub status: &'static str,
    pub calendar_basis: &'static str,
    pub calendar_status: &'static str,
    pub calendar_missing_dates: Vec<String>,
    pub expected_dates: Vec<String>,
    pub symbols: Vec<SymbolCoverage>,
}

/// Audit selected instruments against an independent official calendar when available. The ETF
/// date union is only diagnostic fallback data and never proves market-wide completeness.
pub fn audit_etf_market_coverage(
    snapshot: &Path,
    calendar_snapshot: Option<&Path>,
    symbols: &[String],
    start: NaiveDate,
    end: NaiveDate,
) -> Result<MarketCoverageAudit> {
    audit_daily_market_coverage(snapshot, calendar_snapshot, symbols, start, end)
}

/// Shared audit for an explicit equity/ETF snapshot. A date-union fallback is diagnostic only.
pub fn audit_daily_market_coverage(
    snapshot: &Path,
    calendar_snapshot: Option<&Path>,
    symbols: &[String],
    start: NaiveDate,
    end: NaiveDate,
) -> Result<MarketCoverageAudit> {
    ensure!(start <= end, "coverage start must be on or before end");
    ensure!(
        !symbols.is_empty(),
        "coverage audit needs at least one symbol"
    );
    ensure!(
        snapshot.is_file(),
        "snapshot does not exist: {}",
        snapshot.display()
    );

    let escaped_path = sql_path(snapshot);
    let quoted_symbols = symbols
        .iter()
        .map(|symbol| format!("'{}'", symbol.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let conn = Connection::open_in_memory()?;
    let mut schema = conn.prepare(&format!(
        "DESCRIBE SELECT * FROM read_parquet('{escaped_path}')"
    ))?;
    let columns = schema
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let source_provenance_available = columns.iter().any(|column| column == "source");
    let missing_source = if source_provenance_available {
        "count(*) FILTER (WHERE source IS NULL OR source='')"
    } else {
        "0"
    };
    let sql = format!(
        "SELECT symbol, CAST(trade_date AS VARCHAR), count(*), count(*) FILTER (WHERE open IS NULL OR high IS NULL OR low IS NULL OR close IS NULL OR open <= 0 OR high <= 0 OR low <= 0 OR close <= 0 OR high < low OR high < open OR high < close OR low > open OR low > close), {missing_source} FROM read_parquet('{escaped_path}') WHERE symbol IN ({quoted_symbols}) AND trade_date BETWEEN DATE '{start}' AND DATE '{end}' GROUP BY symbol, trade_date ORDER BY trade_date, symbol"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    let mut grouped: BTreeMap<String, BTreeMap<String, (i64, i64, i64)>> = BTreeMap::new();
    for row in rows {
        let (symbol, date, count, invalid, no_source) = row?;
        grouped
            .entry(symbol)
            .or_default()
            .insert(date, (count, invalid, no_source));
    }
    let observed_union: BTreeSet<String> = grouped
        .values()
        .flat_map(|dates| dates.keys().cloned())
        .collect();
    let mut official_calendar = BTreeMap::<String, bool>::new();
    let has_calendar_snapshot = calendar_snapshot.is_some_and(Path::is_file);
    if let Some(calendar_snapshot) = calendar_snapshot.filter(|path| path.is_file()) {
        let calendar_sql = format!(
            "SELECT CAST(trade_date AS VARCHAR), is_open FROM read_parquet('{}') WHERE trade_date BETWEEN DATE '{start}' AND DATE '{end}' ORDER BY trade_date",
            sql_path(calendar_snapshot)
        );
        let mut calendar_statement = conn.prepare(&calendar_sql)?;
        let calendar_rows = calendar_statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
        })?;
        for row in calendar_rows {
            let (date, is_open) = row?;
            official_calendar.insert(date, is_open);
        }
    }
    let mut calendar_missing_dates = Vec::new();
    let mut day = start;
    loop {
        let key = day.to_string();
        if !official_calendar.contains_key(&key) {
            calendar_missing_dates.push(key);
        }
        if day == end {
            break;
        }
        day += chrono::Duration::days(1);
    }
    let calendar_available = has_calendar_snapshot && calendar_missing_dates.is_empty();
    let expected: BTreeSet<String> = if has_calendar_snapshot {
        official_calendar
            .iter()
            .filter(|(_, is_open)| **is_open)
            .map(|(date, _)| date.clone())
            .collect()
    } else {
        observed_union.clone()
    };
    let expected_dates = expected.iter().cloned().collect::<Vec<_>>();
    let mut any_gap = false;
    let mut results = Vec::with_capacity(symbols.len());
    for symbol in symbols {
        let dates = grouped.get(symbol);
        let observed = dates
            .map(|dates| dates.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let observed_set: BTreeSet<String> = observed.iter().cloned().collect();
        let missing_dates = expected
            .difference(&observed_set)
            .cloned()
            .collect::<Vec<_>>();
        let unexpected_dates = if has_calendar_snapshot {
            observed_set
                .iter()
                .filter(|date| official_calendar.get(*date) == Some(&false))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let duplicate_dates = dates
            .into_iter()
            .flat_map(|dates| dates.iter())
            .filter(|(_, (count, _, _))| *count > 1)
            .map(|(date, _)| date.clone())
            .collect::<Vec<_>>();
        let invalid_ohlc_dates = dates
            .into_iter()
            .flat_map(|dates| dates.iter())
            .filter(|(_, (_, invalid, _))| *invalid > 0)
            .map(|(date, _)| date.clone())
            .collect::<Vec<_>>();
        let missing_source_dates = dates
            .into_iter()
            .flat_map(|dates| dates.iter())
            .filter(|(_, (_, _, missing))| *missing > 0)
            .map(|(date, _)| date.clone())
            .collect::<Vec<_>>();
        any_gap |= !missing_dates.is_empty()
            || !unexpected_dates.is_empty()
            || !duplicate_dates.is_empty()
            || !invalid_ohlc_dates.is_empty()
            || !missing_source_dates.is_empty();
        results.push(SymbolCoverage {
            symbol: symbol.clone(),
            rows: dates.map_or(0, |dates| {
                dates.values().map(|(count, _, _)| *count as usize).sum()
            }),
            first_date: observed.first().cloned(),
            last_date: observed.last().cloned(),
            missing_dates,
            unexpected_dates,
            duplicate_dates,
            invalid_ohlc_dates,
            missing_source_dates,
        });
    }
    let calendar_status = if !has_calendar_snapshot {
        "unavailable"
    } else if calendar_available {
        "complete"
    } else if official_calendar.is_empty() {
        "unverified"
    } else {
        "gaps"
    };
    let status = if any_gap {
        "gaps"
    } else if !calendar_available || !source_provenance_available {
        "unverified"
    } else {
        "complete"
    };
    Ok(MarketCoverageAudit {
        status,
        calendar_basis: if has_calendar_snapshot {
            "official_szse_daily_calendar"
        } else {
            "observed_dates_union_diagnostic_only"
        },
        calendar_status,
        calendar_missing_dates,
        expected_dates,
        symbols: results,
    })
}

pub fn create_etf_snapshot(database: &Path, output_root: &Path) -> Result<SnapshotManifest> {
    ensure!(
        database.is_file(),
        "warehouse database does not exist: {}",
        database.display()
    );
    let read_only = Config::default().access_mode(AccessMode::ReadOnly)?;
    let conn = Connection::open_with_flags(database, read_only)
        .context("open warehouse database read-only")?;
    let snapshot_id = Uuid::new_v4();
    let dir = output_root.join("snapshots").join(snapshot_id.to_string());
    fs::create_dir_all(&dir).context("create snapshot directory")?;
    let file = dir.join("etf_daily.parquet");
    let escaped = sql_path(&file);
    let source = etf_snapshot_source_sql();
    let (rows, symbols, first_date, last_date): (i64, i64, String, String) = conn.query_row(
        &format!("SELECT count(*), count(DISTINCT symbol), CAST(min(trade_date) AS VARCHAR), CAST(max(trade_date) AS VARCHAR) FROM {source}"), [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).context("inspect ETF history")?;
    ensure!(rows > 0, "warehouse contains no ETF history");
    conn.execute_batch(&format!(
        "COPY {source} TO '{escaped}' (FORMAT PARQUET, COMPRESSION ZSTD)"
    ))
    .context("write research snapshot")?;
    let bytes = fs::read(&file).context("read snapshot for hashing")?;
    let benchmark_file = dir.join("benchmark_hs300.parquet");
    let benchmark_source = "(SELECT symbol, '沪深300' AS name, trade_date, CAST(open AS DOUBLE) AS open, CAST(high AS DOUBLE) AS high, CAST(low AS DOUBLE) AS low, CAST(close AS DOUBLE) AS close, CAST(volume_shares AS DOUBLE) AS volume, CAST(amount_cny AS DOUBLE) AS amount FROM staging.daily_bar_latest WHERE symbol='sh000300' QUALIFY row_number() OVER (PARTITION BY trade_date ORDER BY CASE source WHEN 'tencent' THEN 0 WHEN 'tdx' THEN 1 ELSE 2 END, observed_at DESC)=1)";
    let benchmark_rows: i64 = conn.query_row(
        &format!("SELECT count(*) FROM {benchmark_source}"),
        [],
        |row| row.get(0),
    )?;
    ensure!(
        benchmark_rows >= 1_000,
        "沪深300 benchmark history is incomplete: only {benchmark_rows} rows"
    );
    conn.execute_batch(&format!(
        "COPY {benchmark_source} TO '{}' (FORMAT PARQUET, COMPRESSION ZSTD)",
        sql_path(&benchmark_file)
    ))
    .context("write HS300 benchmark snapshot")?;
    let benchmark_bytes = fs::read(&benchmark_file).context("read benchmark for hashing")?;
    let calendar_rows = load_complete_calendar_months(&conn)?;
    let (
        trading_calendar_file,
        trading_calendar_sha256,
        trading_calendar_rows,
        trading_calendar_first_date,
        trading_calendar_last_date,
        trading_calendar_source,
    ) = if calendar_rows.is_empty() {
        (None, None, 0, None, None, None)
    } else {
        let calendar_file = dir.join("trading_calendar.parquet");
        let mut monthly_ranges = BTreeMap::<(i32, u32), (NaiveDate, NaiveDate)>::new();
        for date in calendar_rows.keys() {
            monthly_ranges
                .entry((date.year(), date.month()))
                .and_modify(|range| {
                    range.0 = range.0.min(*date);
                    range.1 = range.1.max(*date);
                })
                .or_insert((*date, *date));
        }
        let month_predicate = monthly_ranges
            .values()
            .map(|(first, last)| format!("(trade_date BETWEEN DATE '{first}' AND DATE '{last}')"))
            .collect::<Vec<_>>()
            .join(" OR ");
        conn.execute_batch(&format!(
                "COPY (SELECT trade_date, is_open FROM core.trading_calendar_latest WHERE market='CN' AND source='szse' AND ({month_predicate}) ORDER BY trade_date) TO '{}' (FORMAT PARQUET, COMPRESSION ZSTD)",
                sql_path(&calendar_file)
            )).context("write official trading calendar snapshot")?;
        let bytes =
            fs::read(&calendar_file).context("read trading calendar snapshot for hashing")?;
        (
            Some(calendar_file),
            Some(format!("{:x}", Sha256::digest(bytes))),
            calendar_rows.len() as i64,
            calendar_rows.keys().next().map(ToString::to_string),
            calendar_rows.keys().next_back().map(ToString::to_string),
            Some("szse_official_month_list".into()),
        )
    };
    let manifest = SnapshotManifest {
        snapshot_id, created_at: Utc::now().to_rfc3339(), source_database: database.to_path_buf(),
        file: file.clone(), sha256: format!("{:x}", Sha256::digest(bytes)), rows, symbols,
        first_date, last_date,
        benchmark_file: Some(benchmark_file),
        benchmark_sha256: Some(format!("{:x}", Sha256::digest(benchmark_bytes))),
        benchmark_rows,
        benchmark_name: Some("沪深300".into()),
        trading_calendar_file,
        trading_calendar_sha256,
        trading_calendar_rows,
        trading_calendar_first_date,
        trading_calendar_last_date,
        trading_calendar_source,
        selection_rule: "one ETF row per symbol/date; Tencent preferred over TDX when both exist".into(),
        limitations: vec![
            "ETF universe comes from the currently observed instrument set and is not survivorship-bias free".into(),
            "daily bars cannot reproduce intraday queue position or price path".into(),
            "source observed_at is ingestion time, not historical publication time".into(),
            "missing, unknown, halted, or conflicting execution status blocks orders in status-aware backtests".into(),
        ],
    };
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(manifest)
}

/// Read the shared, current observed security directory. Unknown and candidate classifications
/// are returned verbatim; no classification is inferred from a symbol pattern.
pub fn load_security_directory(database: &Path) -> Result<Vec<SecurityDirectoryEntry>> {
    ensure!(
        database.is_file(),
        "warehouse database does not exist: {}",
        database.display()
    );
    let read_only = Config::default().access_mode(AccessMode::ReadOnly)?;
    let conn = Connection::open_with_flags(database, read_only)
        .context("open warehouse database read-only")?;
    let mut statement = conn.prepare(
        "SELECT i.instrument_id,
                lower(i.market) || i.code AS symbol,
                i.code,
                n.name,
                i.market AS exchange,
                i.asset_class,
                c.source AS classification_source,
                n.source AS identity_source,
                CAST(i.first_observed_date AS VARCHAR),
                CAST(i.last_observed_date AS VARCHAR)
         FROM core.instrument i
         LEFT JOIN LATERAL (
             SELECT name, source FROM core.instrument_symbol_latest s
             WHERE s.instrument_id=i.instrument_id
             ORDER BY s.valid_from DESC, s.observed_at DESC, s.revision_id DESC LIMIT 1
         ) n ON true
         LEFT JOIN LATERAL (
             SELECT string_agg(DISTINCT source, ',' ORDER BY source) AS source
             FROM core.instrument_classification_latest c
             WHERE c.instrument_id=i.instrument_id
         ) c ON true
         ORDER BY i.market, i.code",
    )?;
    let rows = statement.query_map([], |row| {
        let exchange: String = row.get(4)?;
        Ok(SecurityDirectoryEntry {
            instrument_id: row.get(0)?,
            symbol: row.get(1)?,
            code: row.get(2)?,
            name: row.get(3)?,
            exchange,
            asset_type: row.get(5)?,
            classification_source: row.get(6)?,
            identity_source: row.get(7)?,
            first_observed_date: row.get(8)?,
            last_observed_date: row.get(9)?,
            listed_date: None,
            delisted_date: None,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Create an immutable daily snapshot for an explicit set of observed securities. `preheat_start`
/// is inclusive and may precede `research_start`; only the latter bounds formal research rows.
/// A security is eligible only when its identity is in `core.instrument`; current source labels
/// remain unchanged. This utility does not make stock runs eligible in ETF-only entry points.
pub fn create_daily_snapshot(
    database: &Path,
    output_root: &Path,
    symbols: &[String],
    preheat_start: NaiveDate,
    research_start: NaiveDate,
    research_end: NaiveDate,
) -> Result<DailySnapshotManifest> {
    ensure!(
        database.is_file(),
        "warehouse database does not exist: {}",
        database.display()
    );
    ensure!(
        !symbols.is_empty(),
        "daily snapshot needs an explicit security list"
    );
    ensure!(
        preheat_start <= research_start && research_start <= research_end,
        "expected preheat_start <= research_start <= research_end"
    );
    let unique = symbols.iter().cloned().collect::<BTreeSet<_>>();
    ensure!(
        unique.len() == symbols.len(),
        "duplicate symbols in explicit security list"
    );

    let read_only = Config::default().access_mode(AccessMode::ReadOnly)?;
    let conn = Connection::open_with_flags(database, read_only)
        .context("open warehouse database read-only")?;
    let requested = symbols
        .iter()
        .map(|s| format!("'{}'", s.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let file_id = Uuid::new_v4();
    let dir = output_root.join("snapshots").join(file_id.to_string());
    fs::create_dir_all(&dir).context("create immutable daily snapshot directory")?;
    let file = dir.join("daily_research.parquet");
    let source = generic_daily_source_sql(&requested, preheat_start, research_end);
    let missing = conn.query_row(
        &format!("SELECT count(*) FROM (SELECT unnest([{requested}]) AS symbol) r WHERE NOT EXISTS (SELECT 1 FROM core.instrument i WHERE lower(i.market)||i.code=r.symbol)"),
        [], |row| row.get::<_, i64>(0),
    )?;
    ensure!(
        missing == 0,
        "{} requested securities are absent from the observed instrument directory",
        missing
    );
    let rows: i64 = conn.query_row(&format!("SELECT count(*) FROM {source}"), [], |row| {
        row.get(0)
    })?;
    ensure!(
        rows > 0,
        "selected securities have no daily bars in requested snapshot range"
    );
    let (first_date, last_date): (String, String) = conn.query_row(
        &format!("SELECT CAST(min(trade_date) AS VARCHAR), CAST(max(trade_date) AS VARCHAR) FROM {source}"),
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut asset_statement = conn.prepare(&format!(
        "SELECT DISTINCT asset_type FROM {source} ORDER BY asset_type"
    ))?;
    let asset_types = asset_statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let escaped = sql_path(&file);
    conn.execute_batch(&format!(
        "COPY {source} TO '{escaped}' (FORMAT PARQUET, COMPRESSION ZSTD)"
    ))
    .context("write immutable daily research snapshot")?;
    let sha256 = hash_file(&file)?;

    let calendar_rows = load_complete_calendar_months(&conn)?;
    let calendar_rows = calendar_rows
        .into_iter()
        .filter(|(date, _)| *date >= preheat_start && *date <= research_end)
        .collect::<BTreeMap<_, _>>();
    let (
        trading_calendar_file,
        trading_calendar_sha256,
        trading_calendar_rows,
        trading_calendar_first_date,
        trading_calendar_last_date,
        trading_calendar_source,
        calendar_identity,
    ) = if calendar_rows.is_empty() {
        (None, None, 0, None, None, None, None)
    } else {
        let calendar_file = dir.join("trading_calendar.parquet");
        let complete_months = calendar_rows
            .keys()
            .map(|date| date.format("%Y-%m").to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|month| format!("'{month}'"))
            .collect::<Vec<_>>()
            .join(",");
        conn.execute_batch(&format!(
                "COPY (SELECT trade_date, is_open FROM core.trading_calendar_latest WHERE market='CN' AND source='szse' AND trade_date BETWEEN DATE '{preheat_start}' AND DATE '{research_end}' AND strftime(trade_date, '%Y-%m') IN ({complete_months}) QUALIFY row_number() OVER(PARTITION BY trade_date ORDER BY observed_at DESC, revision_id DESC)=1 ORDER BY trade_date) TO '{}' (FORMAT PARQUET, COMPRESSION ZSTD)",
                sql_path(&calendar_file)))?;
        let calendar_hash = hash_file(&calendar_file)?;
        let first = calendar_rows.keys().next().map(ToString::to_string);
        let last = calendar_rows.keys().next_back().map(ToString::to_string);
        (
            Some(calendar_file),
            Some(calendar_hash.clone()),
            calendar_rows.len() as i64,
            first,
            last,
            Some("szse_official_month_list".to_owned()),
            Some(format!("szse_official_month_list:{}", calendar_hash)),
        )
    };
    let snapshot = SnapshotManifest {
        snapshot_id: file_id,
        created_at: Utc::now().to_rfc3339(),
        source_database: database.to_path_buf(),
        file: file.clone(),
        sha256,
        rows,
        symbols: symbols.len() as i64,
        first_date,
        last_date,
        benchmark_file: None,
        benchmark_sha256: None,
        benchmark_rows: 0,
        benchmark_name: None,
        trading_calendar_file,
        trading_calendar_sha256,
        trading_calendar_rows,
        trading_calendar_first_date,
        trading_calendar_last_date,
        trading_calendar_source,
        selection_rule: "explicit observed instrument symbols; per symbol/date prefer Tencent, then TDX, then other sources; within source select latest observed revision; preserve nullable fields".into(),
        limitations: vec![
            "本机已观察集合，不是完整历史市场证券全集".into(),
            "asset_type reflects current recorded classification; EQUITY_CANDIDATE is not verified equity".into(),
            "listing and delisting dates are unknown because reliable evidence is unavailable".into(),
            "observed_at is local ingestion time, not historical publication or availability time".into(),
            "calendar is exported only for complete recorded SZSE natural months; absent calendar coverage remains unknown".into(),
        ],
    };
    let manifest = DailySnapshotManifest {
        base: snapshot,
        schema_version: 1,
        asset_types,
        securities: symbols.to_vec(),
        preheat_start: preheat_start.to_string(),
        research_start: research_start.to_string(),
        research_end: research_end.to_string(),
        calendar_identity,
    };
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(manifest)
}

fn generic_daily_source_sql(symbols: &str, start: NaiveDate, end: NaiveDate) -> String {
    format!("(WITH daily_status AS (
        SELECT symbol, effective_date,
               CASE WHEN count(DISTINCT trade_status)=1 THEN min(trade_status) ELSE 'CONFLICT' END AS trade_status,
               bool_and(trade_status='TRADABLE') AS is_tradable,
               string_agg(DISTINCT source, ',' ORDER BY source) AS status_sources,
               CAST(max(observed_at) AS VARCHAR) AS status_observed_at
        FROM core.security_status_latest GROUP BY symbol, effective_date
    ), selected AS (
        SELECT lower(i.market)||i.code AS symbol, i.code, i.market AS exchange,
               i.instrument_id, i.asset_class AS asset_type, n.name,
               b.trade_date, CAST(b.open AS DOUBLE) AS open, CAST(b.high AS DOUBLE) AS high,
               CAST(b.low AS DOUBLE) AS low, CAST(b.close AS DOUBLE) AS close,
               CAST(b.volume_shares AS DOUBLE) AS volume, CAST(b.amount_cny AS DOUBLE) AS amount,
               b.source, CAST(b.observed_at AS VARCHAR) AS observed_at,
               s.trade_status, coalesce(s.is_tradable, false) AS is_tradable,
               s.status_sources, s.status_observed_at,
               (s.status_sources IS NOT NULL) AS execution_status_covered,
               b.trade_date < DATE '{start}' AS is_preheat,
               row_number() OVER(PARTITION BY b.symbol,b.trade_date ORDER BY
                 CASE b.source WHEN 'tencent' THEN 0 WHEN 'tdx' THEN 1 ELSE 2 END,
                 b.observed_at DESC, b.revision_id DESC) AS selection_rank
        FROM staging.daily_bar_revision b
        JOIN core.instrument i ON lower(i.market)||i.code=b.symbol
        LEFT JOIN LATERAL (
            SELECT name FROM core.instrument_symbol_latest n
            WHERE n.instrument_id=i.instrument_id AND n.valid_from<=b.trade_date
            ORDER BY n.valid_from DESC,n.observed_at DESC,n.revision_id DESC LIMIT 1
        ) n ON true
        LEFT JOIN daily_status s ON s.symbol=b.symbol AND s.effective_date=b.trade_date
        WHERE b.symbol IN ({symbols}) AND b.adjustment='none'
          AND b.trade_date BETWEEN DATE '{start}' AND DATE '{end}'
    ) SELECT * EXCLUDE(selection_rank) FROM selected WHERE selection_rank=1 ORDER BY trade_date,symbol)")
}

fn hash_file(path: &Path) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            fs::read(path).with_context(|| format!("read {} for hashing", path.display()))?
        )
    ))
}

pub fn verify_snapshot_manifest(manifest: &SnapshotManifest) -> Result<()> {
    ensure!(
        hash_file(&manifest.file)? == manifest.sha256,
        "daily snapshot hash mismatch"
    );
    if let (Some(path), Some(expected)) = (
        &manifest.trading_calendar_file,
        &manifest.trading_calendar_sha256,
    ) {
        ensure!(
            hash_file(path)? == *expected,
            "trading calendar snapshot hash mismatch"
        );
    }
    if let (Some(path), Some(expected)) = (&manifest.benchmark_file, &manifest.benchmark_sha256) {
        ensure!(
            hash_file(path)? == *expected,
            "benchmark snapshot hash mismatch"
        );
    }
    Ok(())
}

/// Load nullable daily rows from a generic snapshot for shared search/chart readers.
pub fn load_daily_research_rows(
    snapshot: &Path,
    symbols: &[String],
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<DailyResearchRow>> {
    ensure!(
        snapshot.is_file(),
        "snapshot does not exist: {}",
        snapshot.display()
    );
    ensure!(start <= end, "daily row start must be on or before end");
    let quoted = symbols
        .iter()
        .map(|s| format!("'{}'", s.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let filter = if symbols.is_empty() {
        "TRUE".to_owned()
    } else {
        format!("symbol IN ({quoted})")
    };
    let conn = Connection::open_in_memory()?;
    let path = sql_path(snapshot);
    let sql = format!(
        "SELECT instrument_id,symbol,code,name,exchange,asset_type,CAST(trade_date AS VARCHAR),open,high,low,close,volume,amount,source,observed_at,trade_status,is_tradable,status_sources,status_observed_at,execution_status_covered FROM read_parquet('{path}') WHERE {filter} AND trade_date BETWEEN DATE '{start}' AND DATE '{end}' ORDER BY trade_date,symbol"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([], |r| {
        let date: String = r.get(6)?;
        Ok(DailyResearchRow {
            instrument_id: r.get(0)?,
            symbol: r.get(1)?,
            code: r.get(2)?,
            name: r.get(3)?,
            exchange: r.get(4)?,
            asset_type: r.get(5)?,
            trade_date: date
                .parse()
                .map_err(|e| duckdb::Error::ToSqlConversionFailure(Box::new(e)))?,
            open: r.get(7)?,
            high: r.get(8)?,
            low: r.get(9)?,
            close: r.get(10)?,
            volume: r.get(11)?,
            amount: r.get(12)?,
            source: r.get(13)?,
            observed_at: r.get(14)?,
            trade_status: r.get(15)?,
            is_tradable: r.get(16)?,
            status_sources: r.get(17)?,
            status_observed_at: r.get(18)?,
            execution_status_covered: r.get(19)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn load_snapshot_execution_state_inputs(snapshot: &Path) -> Result<Vec<ExecutionStateInput>> {
    ensure!(
        snapshot.is_file(),
        "snapshot does not exist: {}",
        snapshot.display()
    );
    let conn = Connection::open_in_memory()?;
    let path = sql_path(snapshot);
    let mut statement = conn.prepare(&format!(
        "SELECT instrument_id,symbol,CAST(trade_date AS VARCHAR),trade_status,status_sources,
                coalesce(execution_status_covered, status_sources IS NOT NULL),status_observed_at
         FROM read_parquet('{path}') ORDER BY trade_date,symbol"
    ))?;
    let rows = statement.query_map([], |row| {
        let date: String = row.get(2)?;
        Ok(ExecutionStateInput {
            instrument_id: row.get(0)?,
            symbol: row.get(1)?,
            trade_date: date
                .parse()
                .map_err(|error| duckdb::Error::ToSqlConversionFailure(Box::new(error)))?,
            status: row.get(3)?,
            source: row.get(4)?,
            covered: row.get(5)?,
            status_observed_at: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn etf_snapshot_source_sql() -> &'static str {
    "(WITH daily_status AS (SELECT symbol, effective_date, CASE WHEN count(DISTINCT trade_status)=1 THEN min(trade_status) ELSE 'CONFLICT' END AS trade_status, bool_and(trade_status='TRADABLE') AS is_tradable, string_agg(DISTINCT source, ',' ORDER BY source) AS status_sources FROM core.security_status_latest GROUP BY symbol, effective_date) SELECT b.symbol, coalesce(b.name, b.symbol) AS name, b.trade_date, CAST(b.open AS DOUBLE) AS open, CAST(b.high AS DOUBLE) AS high, CAST(b.low AS DOUBLE) AS low, CAST(b.close AS DOUBLE) AS close, CAST(b.volume_shares AS DOUBLE) AS volume, CAST(b.amount_cny AS DOUBLE) AS amount, s.trade_status, coalesce(s.is_tradable, false) AS is_tradable, s.status_sources FROM research.etf_daily_bar b LEFT JOIN daily_status s ON s.symbol=b.symbol AND s.effective_date=b.trade_date QUALIFY row_number() OVER (PARTITION BY b.symbol, b.trade_date ORDER BY CASE b.source WHEN 'tencent' THEN 0 WHEN 'tdx' THEN 1 ELSE 2 END, b.observed_at DESC)=1)"
}

fn load_complete_calendar_months(conn: &Connection) -> Result<BTreeMap<NaiveDate, bool>> {
    let mut statement = conn.prepare(
        "SELECT CAST(trade_date AS VARCHAR), is_open FROM core.trading_calendar_latest WHERE market='CN' AND source='szse' ORDER BY trade_date",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
    })?;
    let mut monthly = BTreeMap::<(i32, u32), BTreeMap<NaiveDate, bool>>::new();
    for row in rows {
        let (date, is_open) = row?;
        let date: NaiveDate = date.parse()?;
        monthly
            .entry((date.year(), date.month()))
            .or_default()
            .insert(date, is_open);
    }

    let mut complete = BTreeMap::new();
    for ((year, month), dates) in monthly {
        let next_month = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)
        }
        .context("calendar month has an invalid year")?;
        let last = next_month
            .pred_opt()
            .context("calendar month has no previous date")?;
        if dates.len() != last.day() as usize
            || dates.keys().next().is_none_or(|date| date.day() != 1)
            || dates.keys().next_back().is_none_or(|date| *date != last)
        {
            continue;
        }
        let mut date = NaiveDate::from_ymd_opt(year, month, 1).context("invalid calendar month")?;
        while date <= last {
            if !dates.contains_key(&date) {
                break;
            }
            complete.insert(date, dates[&date]);
            date += chrono::Duration::days(1);
        }
    }
    Ok(complete)
}

pub fn load_bars(
    snapshot: &Path,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> Result<Vec<Bar>> {
    ensure!(
        snapshot.is_file(),
        "snapshot does not exist: {}",
        snapshot.display()
    );
    let conn = Connection::open_in_memory()?;
    let mut schema = conn.prepare(&format!(
        "DESCRIBE SELECT * FROM read_parquet('{}')",
        sql_path(snapshot)
    ))?;
    let columns = schema
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if columns.iter().any(|column| column == "asset_type") {
        let assets = conn
            .prepare(&format!(
                "SELECT DISTINCT asset_type FROM read_parquet('{}')",
                sql_path(snapshot)
            ))?
            .query_map([], |row| row.get::<_, Option<String>>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(
            assets.iter().all(|asset| asset.as_deref() == Some("ETF")),
            "ETF-only reader rejects non-ETF or unknown-classification snapshots"
        );
    }
    let mut sql = format!(
        "SELECT symbol,name,CAST(trade_date AS VARCHAR),open,high,low,close,volume,amount FROM read_parquet('{}') WHERE 1=1",
        sql_path(snapshot)
    );
    if let Some(v) = start {
        sql.push_str(&format!(" AND trade_date >= DATE '{v}'"));
    }
    if let Some(v) = end {
        sql.push_str(&format!(" AND trade_date <= DATE '{v}'"));
    }
    sql.push_str(" ORDER BY trade_date,symbol");
    let mut stmt = conn.prepare(&sql)?;
    let bars = stmt
        .query_map([], |r| {
            let date: String = r.get(2)?;
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                date,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, f64>(5)?,
                r.get::<_, f64>(6)?,
                r.get::<_, f64>(7)?,
                r.get::<_, Option<f64>>(8)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|r| {
            Ok(Bar {
                symbol: r.0,
                name: r.1,
                trade_date: r.2.parse()?,
                open: r.3,
                high: r.4,
                low: r.5,
                close: r.6,
                volume: r.7,
                amount: r.8,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        !bars.is_empty(),
        "snapshot has no bars in the requested range"
    );
    Ok(bars)
}

/// Load the execution-status columns embedded in new ETF snapshots. Snapshots
/// created before those columns existed return `None` and retain legacy behavior.
pub fn load_snapshot_execution_statuses(snapshot: &Path) -> Result<Option<ExecutionStatusMap>> {
    ensure!(
        snapshot.is_file(),
        "snapshot does not exist: {}",
        snapshot.display()
    );
    let conn = Connection::open_in_memory()?;
    let escaped = sql_path(snapshot);
    let mut schema = conn.prepare(&format!("DESCRIBE SELECT * FROM read_parquet('{escaped}')"))?;
    let columns = schema
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !columns.iter().any(|column| column == "trade_status")
        || !columns.iter().any(|column| column == "is_tradable")
    {
        return Ok(None);
    }
    let mut statement = conn.prepare(&format!(
        "SELECT symbol, CAST(trade_date AS VARCHAR), trade_status, is_tradable, status_sources FROM read_parquet('{escaped}') ORDER BY trade_date, symbol"
    ))?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, bool>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut statuses = ExecutionStatusMap::new();
    for row in rows {
        let (symbol, date, trade_status, is_tradable, sources) = row?;
        statuses.insert(
            (date.parse()?, symbol),
            ExecutionStatus {
                trade_status,
                is_tradable,
                sources,
            },
        );
    }
    Ok(Some(statuses))
}

pub fn load_snapshot_trading_calendar(
    snapshot: &SnapshotManifest,
) -> Result<Option<Vec<NaiveDate>>> {
    let Some(path) = snapshot.trading_calendar_file.as_deref() else {
        return Ok(None);
    };
    let escaped = path.to_string_lossy().replace('\'', "''");
    let conn = Connection::open_in_memory()?;
    let mut statement = conn.prepare(&format!(
        "SELECT CAST(trade_date AS VARCHAR) FROM read_parquet('{escaped}') WHERE is_open ORDER BY trade_date"
    ))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let dates = rows
        .map(|row| -> Result<NaiveDate> { Ok(row?.parse()?) })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        !dates.is_empty(),
        "snapshot trading calendar has no open dates"
    );
    Ok(Some(dates))
}

fn sql_path(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn daily_fixture_database(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE SCHEMA core; CREATE SCHEMA staging;
             CREATE TABLE core.instrument(instrument_id VARCHAR, market VARCHAR, code VARCHAR, asset_class VARCHAR,
                 first_observed_date DATE, last_observed_date DATE);
             CREATE TABLE core.instrument_symbol_latest(instrument_id VARCHAR, name VARCHAR, source VARCHAR, valid_from DATE, observed_at TIMESTAMP, revision_id VARCHAR);
             CREATE TABLE core.instrument_classification_latest(instrument_id VARCHAR, source VARCHAR);
             CREATE TABLE core.security_status_latest(symbol VARCHAR, effective_date DATE, trade_status VARCHAR, source VARCHAR, observed_at TIMESTAMP);
             CREATE TABLE core.trading_calendar_latest(market VARCHAR, source VARCHAR, trade_date DATE, is_open BOOLEAN, observed_at TIMESTAMP, revision_id VARCHAR);
             CREATE TABLE staging.daily_bar_revision(revision_id VARCHAR, symbol VARCHAR, trade_date DATE, source VARCHAR,
                 adjustment VARCHAR, open DOUBLE, high DOUBLE, low DOUBLE, close DOUBLE, volume_shares DOUBLE,
                 amount_cny DOUBLE, observed_at TIMESTAMP);
             INSERT INTO core.instrument VALUES
                 ('stock-1','SH','600000','EQUITY_CANDIDATE',DATE '2026-01-05',DATE '2026-01-07'),
                 ('etf-1','SH','510300','ETF',DATE '2026-01-05',DATE '2026-01-07');
             INSERT INTO core.instrument_symbol_latest VALUES
                 ('stock-1','浦发银行','fixture',DATE '2020-01-01',TIMESTAMP '2026-01-08 00:00:00','name-1'),
                 ('etf-1','沪深300ETF','fixture',DATE '2012-01-01',TIMESTAMP '2026-01-08 00:00:00','name-2');
             INSERT INTO staging.daily_bar_revision VALUES
                 ('old','sh600000',DATE '2026-01-06','tencent','none',10,11,9,10,100,NULL,TIMESTAMP '2026-01-06 16:00:00'),
                 ('new','sh600000',DATE '2026-01-06','tencent','none',10,12,9,11,120,NULL,TIMESTAMP '2026-01-07 16:00:00'),
                 ('stock-2','sh600000',DATE '2026-01-07','tencent','none',11,12,10,11.5,130,1495,TIMESTAMP '2026-01-07 16:00:00'),
                 ('etf-1','sh510300',DATE '2026-01-05','tdx','none',4,5,3,4.5,1000,4500,TIMESTAMP '2026-01-05 16:00:00'),
                 ('etf-2','sh510300',DATE '2026-01-06','tdx','none',4.5,5,4,4.8,1100,5280,TIMESTAMP '2026-01-06 16:00:00'),
                 ('etf-3','sh510300',DATE '2026-01-07','tdx','none',4.8,5.2,4.5,5,1200,6000,TIMESTAMP '2026-01-07 16:00:00');",
        ).unwrap();
    }

    #[test]
    fn snapshot_coverage_finds_symbol_gaps_and_bad_rows() {
        let dir = tempdir().unwrap();
        let parquet = dir.path().join("coverage.parquet");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT * FROM (VALUES
                ('sh510300', DATE '2026-01-05', 10.0, 11.0, 9.0, 10.5),
                ('sh510300', DATE '2026-01-06', 10.5, 11.5, 10.0, 11.0),
                ('sh518880', DATE '2026-01-05', 5.0, 5.5, 4.5, 5.2)
             ) AS bars(symbol, trade_date, open, high, low, close)) TO '{}' (FORMAT PARQUET)",
            sql_path(&parquet)
        ))
        .unwrap();
        let audit = audit_etf_market_coverage(
            &parquet,
            None,
            &["sh510300".into(), "sh518880".into()],
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
        )
        .unwrap();
        assert_eq!(audit.status, "gaps");
        assert_eq!(audit.expected_dates, vec!["2026-01-05", "2026-01-06"]);
        assert_eq!(audit.symbols[0].missing_dates, Vec::<String>::new());
        assert_eq!(audit.symbols[1].missing_dates, vec!["2026-01-06"]);
        assert_eq!(audit.symbols[1].rows, 1);
    }

    #[test]
    fn execution_status_loader_reads_new_snapshot_columns_and_accepts_legacy_snapshot() {
        let dir = tempdir().unwrap();
        let current = dir.path().join("status.parquet");
        let legacy = dir.path().join("legacy.parquet");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT * FROM (VALUES
                ('sh510300', DATE '2026-01-05', 'TRADABLE', TRUE, 'fixture'),
                ('sh518880', DATE '2026-01-05', 'HALTED', FALSE, 'fixture'),
                ('sz159919', DATE '2026-01-05', NULL::VARCHAR, FALSE, NULL::VARCHAR)
             ) AS bars(symbol, trade_date, trade_status, is_tradable, status_sources)) TO '{}' (FORMAT PARQUET);
             COPY (SELECT 'sh510300' AS symbol, DATE '2026-01-05' AS trade_date) TO '{}' (FORMAT PARQUET)",
            sql_path(&current),
            sql_path(&legacy),
        ))
        .unwrap();

        let statuses = load_snapshot_execution_statuses(&current).unwrap().unwrap();
        assert!(
            statuses[&(
                NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                "sh510300".into()
            )]
                .is_tradable
        );
        assert_eq!(
            statuses[&(
                NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                "sh518880".into()
            )]
                .trade_status
                .as_deref(),
            Some("HALTED")
        );
        assert_eq!(
            statuses[&(
                NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                "sh518880".into()
            )]
                .sources
                .as_deref(),
            Some("fixture")
        );
        assert!(
            !statuses[&(
                NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                "sz159919".into()
            )]
                .is_tradable
        );
        assert!(load_snapshot_execution_statuses(&legacy).unwrap().is_none());
    }

    #[test]
    fn etf_snapshot_query_fails_closed_on_missing_or_conflicting_status_sources() {
        let dir = tempdir().unwrap();
        let parquet = dir.path().join("snapshot.parquet");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE SCHEMA core;
             CREATE SCHEMA research;
             CREATE TABLE research.etf_daily_bar(
                 symbol VARCHAR, name VARCHAR, trade_date DATE, open DOUBLE, high DOUBLE,
                 low DOUBLE, close DOUBLE, volume_shares DOUBLE, amount_cny DOUBLE,
                 source VARCHAR, observed_at TIMESTAMP
             );
             INSERT INTO research.etf_daily_bar VALUES
                 ('sh510300','沪深300ETF',DATE '2026-01-05',10,11,9,10,1000,10000,'tdx',TIMESTAMP '2026-01-05 16:00:00'),
                 ('sh518880','黄金ETF',DATE '2026-01-05',5,6,4,5,1000,5000,'tdx',TIMESTAMP '2026-01-05 16:00:00'),
                 ('sz159919','沪深300ETF',DATE '2026-01-05',4,5,3,4,1000,4000,'tdx',TIMESTAMP '2026-01-05 16:00:00');
             CREATE TABLE core.security_status_latest(
                 symbol VARCHAR, effective_date DATE, trade_status VARCHAR, source VARCHAR
             );
             INSERT INTO core.security_status_latest VALUES
                 ('sh510300',DATE '2026-01-05','TRADABLE','official-a'),
                 ('sh518880',DATE '2026-01-05','TRADABLE','source-a'),
                 ('sh518880',DATE '2026-01-05','HALTED','source-b');",
        )
        .unwrap();
        conn.execute_batch(&format!(
            "COPY {} TO '{}' (FORMAT PARQUET)",
            etf_snapshot_source_sql(),
            sql_path(&parquet)
        ))
        .unwrap();

        let statuses = load_snapshot_execution_statuses(&parquet).unwrap().unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        assert_eq!(
            statuses[&(day, "sh510300".into())].trade_status.as_deref(),
            Some("TRADABLE")
        );
        assert!(statuses[&(day, "sh510300".into())].is_tradable);
        assert_eq!(
            statuses[&(day, "sh510300".into())].sources.as_deref(),
            Some("official-a")
        );
        assert_eq!(
            statuses[&(day, "sh518880".into())].trade_status.as_deref(),
            Some("CONFLICT")
        );
        assert!(!statuses[&(day, "sh518880".into())].is_tradable);
        assert_eq!(
            statuses[&(day, "sh518880".into())].sources.as_deref(),
            Some("source-a,source-b")
        );
        assert_eq!(statuses[&(day, "sz159919".into())].trade_status, None);
        assert!(!statuses[&(day, "sz159919".into())].is_tradable);
    }

    #[test]
    fn no_observed_member_dates_are_unverified_not_complete() {
        let dir = tempdir().unwrap();
        let parquet = dir.path().join("empty.parquet");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT 'sh510300' AS symbol, DATE '2026-01-05' AS trade_date, 1.0 AS open, 1.0 AS high, 1.0 AS low, 1.0 AS close WHERE FALSE) TO '{}' (FORMAT PARQUET)",
            sql_path(&parquet)
        )).unwrap();
        let audit = audit_etf_market_coverage(
            &parquet,
            None,
            &["sh510300".into()],
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
        )
        .unwrap();
        assert_eq!(audit.status, "unverified");
        assert!(audit.expected_dates.is_empty());
        assert_eq!(audit.symbols[0].rows, 0);
    }

    #[test]
    fn official_calendar_detects_a_date_missing_from_every_member() {
        let dir = tempdir().unwrap();
        let parquet = dir.path().join("bars.parquet");
        let calendar = dir.path().join("calendar.parquet");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT * FROM (VALUES
                ('sh513300', DATE '2026-01-05', 10.0, 11.0, 9.0, 10.5),
                ('sh518880', DATE '2026-01-05', 5.0, 5.5, 4.5, 5.2)
             ) AS bars(symbol, trade_date, open, high, low, close)) TO '{}' (FORMAT PARQUET);
             COPY (SELECT * FROM (VALUES
                (DATE '2026-01-05', TRUE),
                (DATE '2026-01-06', TRUE),
                (DATE '2026-01-07', FALSE)
             ) AS days(trade_date, is_open)) TO '{}' (FORMAT PARQUET)",
            sql_path(&parquet),
            sql_path(&calendar),
        ))
        .unwrap();
        let audit = audit_etf_market_coverage(
            &parquet,
            Some(&calendar),
            &["sh513300".into(), "sh518880".into()],
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 7).unwrap(),
        )
        .unwrap();
        assert_eq!(audit.status, "gaps");
        assert_eq!(audit.calendar_status, "complete");
        assert_eq!(audit.calendar_basis, "official_szse_daily_calendar");
        assert_eq!(audit.expected_dates, vec!["2026-01-05", "2026-01-06"]);
        assert_eq!(audit.symbols[0].missing_dates, vec!["2026-01-06"]);
        assert_eq!(audit.symbols[1].missing_dates, vec!["2026-01-06"]);
        assert!(
            audit
                .symbols
                .iter()
                .all(|symbol| symbol.unexpected_dates.is_empty())
        );
    }

    #[test]
    fn snapshot_calendar_only_keeps_complete_natural_months() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE SCHEMA core;
             CREATE TABLE core.trading_calendar_latest(market VARCHAR, trade_date DATE, is_open BOOLEAN, source VARCHAR);
             INSERT INTO core.trading_calendar_latest
             SELECT 'CN', CAST(day AS DATE), TRUE, 'szse'
             FROM generate_series(DATE '2026-03-01', DATE '2026-03-31', INTERVAL 1 DAY) AS dates(day);
             INSERT INTO core.trading_calendar_latest
             SELECT 'CN', CAST(day AS DATE), TRUE, 'szse'
             FROM generate_series(DATE '2026-04-01', DATE '2026-04-29', INTERVAL 1 DAY) AS dates(day);",
        )
        .unwrap();
        let complete = load_complete_calendar_months(&conn).unwrap();
        assert_eq!(complete.len(), 31);
        assert_eq!(complete.keys().next().unwrap().to_string(), "2026-03-01");
        assert_eq!(
            complete.keys().next_back().unwrap().to_string(),
            "2026-03-31"
        );
    }

    #[test]
    fn mixed_daily_snapshot_freezes_selected_assets_revisions_and_prewarm_range() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("fixture.duckdb");
        daily_fixture_database(&database);
        let symbols = vec!["sh600000".into(), "sh510300".into()];
        let manifest = create_daily_snapshot(
            &database,
            dir.path(),
            &symbols,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 7).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.asset_types, vec!["EQUITY_CANDIDATE", "ETF"]);
        assert_eq!(manifest.base.rows, 5);
        assert_eq!(manifest.base.trading_calendar_source, None);
        verify_snapshot_manifest(&manifest.base).unwrap();
        let rows = load_daily_research_rows(
            &manifest.base.file,
            &[],
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 7).unwrap(),
        )
        .unwrap();
        assert_eq!(rows.len(), 5);
        let revised = rows
            .iter()
            .find(|r| {
                r.symbol == "sh600000"
                    && r.trade_date == NaiveDate::from_ymd_opt(2026, 1, 6).unwrap()
            })
            .unwrap();
        assert_eq!(revised.close, Some(11.0));
        assert_eq!(revised.asset_type, "EQUITY_CANDIDATE");
        assert_eq!(revised.amount, None);
        assert_eq!(revised.trade_status, None);
        assert_eq!(revised.is_tradable, Some(false));
        assert_eq!(revised.execution_status_covered, Some(false));
        let state = load_snapshot_execution_state_inputs(&manifest.base.file).unwrap();
        let unknown = state
            .iter()
            .find(|row| {
                row.symbol == "sh600000"
                    && row.trade_date == NaiveDate::from_ymd_opt(2026, 1, 6).unwrap()
            })
            .unwrap();
        assert_eq!(unknown.status, None);
        assert_eq!(unknown.source, None);
        assert!(!unknown.covered);
        let etf_runner_error = load_bars(&manifest.base.file, None, None)
            .unwrap_err()
            .to_string();
        assert!(etf_runner_error.contains("ETF-only reader rejects"));
        fs::write(&manifest.base.file, b"tampered").unwrap();
        assert!(verify_snapshot_manifest(&manifest.base).is_err());
    }

    #[test]
    fn daily_snapshot_calendar_file_excludes_incomplete_month_rows() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("calendar-fixture.duckdb");
        daily_fixture_database(&database);
        let conn = Connection::open(&database).unwrap();
        conn.execute_batch(
            "INSERT INTO core.trading_calendar_latest
             SELECT 'CN', 'szse', CAST(day AS DATE), TRUE, TIMESTAMP '2026-02-02 00:00:00', CAST(day AS VARCHAR)
             FROM generate_series(DATE '2026-01-01', DATE '2026-01-31', INTERVAL 1 DAY) AS dates(day);
             INSERT INTO core.trading_calendar_latest VALUES
             ('CN','szse',DATE '2026-02-01',FALSE,TIMESTAMP '2026-02-02 00:00:00','partial-feb');",
        ).unwrap();
        drop(conn);
        let manifest = create_daily_snapshot(
            &database,
            dir.path(),
            &["sh510300".into()],
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
            NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.base.trading_calendar_rows, 27);
        let calendar = manifest.base.trading_calendar_file.unwrap();
        let conn = Connection::open_in_memory().unwrap();
        let count: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM read_parquet('{}')",
                    sql_path(&calendar)
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 27);
        let feb: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM read_parquet('{}') WHERE trade_date=DATE '2026-02-01'",
                    sql_path(&calendar)
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(feb, 0);
    }

    #[test]
    fn legacy_etf_parquet_without_asset_type_remains_readable() {
        let dir = tempdir().unwrap();
        let parquet = dir.path().join("legacy-etf.parquet");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT 'sh510300' AS symbol, '沪深300ETF' AS name, DATE '2026-01-05' AS trade_date,
                4.0 AS open, 5.0 AS high, 3.0 AS low, 4.5 AS close, 1000.0 AS volume, NULL::DOUBLE AS amount)
             TO '{}' (FORMAT PARQUET)", sql_path(&parquet)
        )).unwrap();
        let bars = load_bars(&parquet, None, None).unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].amount, None);
    }

    #[test]
    fn mixed_snapshot_directory_preserves_unknown_class_and_missing_identity_fields() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("fixture.duckdb");
        daily_fixture_database(&database);
        let conn = Connection::open(&database).unwrap();
        conn.execute_batch("INSERT INTO core.instrument VALUES ('unknown-1','SZ','300001','UNKNOWN',DATE '2026-01-01',DATE '2026-01-01');").unwrap();
        drop(conn);
        let directory = load_security_directory(&database).unwrap();
        let unknown = directory
            .iter()
            .find(|entry| entry.symbol == "sz300001")
            .unwrap();
        assert_eq!(unknown.asset_type, "UNKNOWN");
        assert_eq!(unknown.name, None);
        assert_eq!(unknown.listed_date, None);
        assert_eq!(unknown.delisted_date, None);
    }

    #[test]
    fn shared_coverage_audit_finds_duplicates_common_gaps_and_non_open_rows() {
        let dir = tempdir().unwrap();
        let parquet = dir.path().join("audit.parquet");
        let calendar = dir.path().join("calendar.parquet");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT * FROM (VALUES
                ('sh600000', DATE '2026-01-05', 10.0, 11.0, 9.0, 10.0, 'tencent'),
                ('sh600000', DATE '2026-01-05', 10.0, 11.0, 9.0, 10.0, 'tencent'),
                ('sh510300', DATE '2026-01-05', 4.0, 3.0, 3.5, 4.0, NULL),
                ('sh600000', DATE '2026-01-07', 10.0, 11.0, 9.0, 10.0, 'tencent'),
                ('sh510300', DATE '2026-01-07', 4.0, 5.0, 3.0, 4.5, 'tdx')
            ) AS bars(symbol, trade_date, open, high, low, close, source)) TO '{}' (FORMAT PARQUET);
            COPY (SELECT * FROM (VALUES
                (DATE '2026-01-05', TRUE), (DATE '2026-01-06', TRUE), (DATE '2026-01-07', FALSE)
            ) AS days(trade_date, is_open)) TO '{}' (FORMAT PARQUET)",
            sql_path(&parquet),
            sql_path(&calendar),
        ))
        .unwrap();
        let audit = audit_daily_market_coverage(
            &parquet,
            Some(&calendar),
            &["sh600000".into(), "sh510300".into()],
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 7).unwrap(),
        )
        .unwrap();
        assert_eq!(audit.symbols[0].duplicate_dates, vec!["2026-01-05"]);
        assert_eq!(audit.symbols[0].missing_dates, vec!["2026-01-06"]);
        assert_eq!(audit.symbols[1].invalid_ohlc_dates, vec!["2026-01-05"]);
        assert_eq!(audit.symbols[1].missing_source_dates, vec!["2026-01-05"]);
        assert_eq!(audit.symbols[1].unexpected_dates, vec!["2026-01-07"]);
        assert_eq!(audit.calendar_status, "complete");
    }
}
