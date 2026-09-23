use crate::core::Bar;
use anyhow::{Context, Result, ensure};
use chrono::{NaiveDate, Utc};
use duckdb::{AccessMode, Config, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
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
    pub selection_rule: String,
    pub limitations: Vec<String>,
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
    let source = "(SELECT symbol, coalesce(name, symbol) AS name, trade_date, CAST(open AS DOUBLE) AS open, CAST(high AS DOUBLE) AS high, CAST(low AS DOUBLE) AS low, CAST(close AS DOUBLE) AS close, CAST(volume_shares AS DOUBLE) AS volume, CAST(amount_cny AS DOUBLE) AS amount FROM research.etf_daily_bar QUALIFY row_number() OVER (PARTITION BY symbol, trade_date ORDER BY CASE source WHEN 'tencent' THEN 0 WHEN 'tdx' THEN 1 ELSE 2 END, observed_at DESC)=1)";
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
    let manifest = SnapshotManifest {
        snapshot_id, created_at: Utc::now().to_rfc3339(), source_database: database.to_path_buf(),
        file: file.clone(), sha256: format!("{:x}", Sha256::digest(bytes)), rows, symbols,
        first_date, last_date,
        benchmark_file: Some(benchmark_file),
        benchmark_sha256: Some(format!("{:x}", Sha256::digest(benchmark_bytes))),
        benchmark_rows,
        benchmark_name: Some("沪深300".into()),
        selection_rule: "one ETF row per symbol/date; Tencent preferred over TDX when both exist".into(),
        limitations: vec![
            "ETF universe comes from the currently observed instrument set and is not survivorship-bias free".into(),
            "daily bars cannot reproduce intraday queue position or price path".into(),
            "source observed_at is ingestion time, not historical publication time".into(),
        ],
    };
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(manifest)
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

fn sql_path(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}
