//! Create an explicitly synthetic, published 10-year mixed equity/ETF daily snapshot for
//! API and browser acceptance. Never use this fixture as production or research input.
use chrono::{NaiveDate, Utc};
use duckdb::Connection;
use quant_research::data::{DailySnapshotManifest, SnapshotManifest};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};
use uuid::Uuid;

#[derive(Serialize)]
struct Publication {
    status: &'static str,
    snapshot_id: Uuid,
    published_at: String,
    data_cutoff_date: String,
    coverage_status: &'static str,
    price_adjustment: &'static str,
    source_status: &'static str,
    manifest_sha256: String,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(
        env::args()
            .nth(1)
            .unwrap_or_else(|| "/private/tmp/prajna-market-fixture".into()),
    );
    let id = Uuid::new_v4();
    let directory = output.join("snapshots").join(id.to_string());
    fs::create_dir_all(&directory)?;
    let parquet = directory.join("daily_research.parquet");
    let escaped = parquet.to_string_lossy().replace('\'', "''");
    let today = Utc::now().date_naive();
    let end = today.format("%Y-%m-%d");
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(&format!(r#"
      COPY (
        WITH dates AS (
          SELECT CAST(trade_date AS DATE) AS trade_date,
                 row_number() OVER (ORDER BY trade_date) - 1 AS i
          FROM generate_series(DATE '2016-01-01', DATE '{end}', INTERVAL 1 DAY) t(trade_date)
          WHERE dayofweek(trade_date) BETWEEN 1 AND 5
        ), universe AS (
          SELECT * FROM (VALUES
            ('SH:600000','sh600000','600000','合成样例股票','SH','EQUITY',0.0,1000000.0),
            ('SZ:510300','sz510300','510300','合成沪深300ETF','SZ','ETF',1.7,5000000.0)
          ) AS u(instrument_id,symbol,code,name,exchange,asset_type,offset_value,base_volume)
        ), bars AS (
          SELECT u.instrument_id,u.symbol,u.code,u.name,u.exchange,u.asset_type,d.trade_date,
                 CAST(10 + u.offset_value + d.i * 0.001 + sin(d.i / 15.0) * 0.12 AS DOUBLE) AS open,
                 CAST(10 + u.offset_value + d.i * 0.001 + sin(d.i / 15.0) * 0.12 + 0.18 AS DOUBLE) AS high,
                 CAST(10 + u.offset_value + d.i * 0.001 + sin(d.i / 15.0) * 0.12 - 0.16 AS DOUBLE) AS low,
                 CAST(10 + u.offset_value + d.i * 0.001 + sin(d.i / 15.0) * 0.12 + cos(d.i / 7.0) * 0.04 AS DOUBLE) AS close,
                 CAST(u.base_volume + (d.i % 53) * 1000 AS DOUBLE) AS volume,
                 CAST((u.base_volume + (d.i % 53) * 1000) * (10 + u.offset_value) AS DOUBLE) AS amount,
                 'synthetic_fixture' AS source, '2026-09-24T20:00:00+08:00' AS observed_at,
                 NULL::VARCHAR AS trade_status, false AS is_tradable,
                 NULL::VARCHAR AS status_sources, NULL::VARCHAR AS status_observed_at,
                 false AS execution_status_covered, false AS is_preheat
          FROM dates d CROSS JOIN universe u
        ) SELECT * FROM bars ORDER BY trade_date,symbol
      ) TO '{escaped}' (FORMAT PARQUET, COMPRESSION ZSTD)
    "#))?;
    let bytes = fs::read(&parquet)?;
    let rows = conn.query_row(
        &format!("SELECT count(*) FROM read_parquet('{escaped}')"),
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let first = NaiveDate::from_ymd_opt(2016, 1, 1).unwrap().to_string();
    let published_at = Utc::now().to_rfc3339();
    let manifest = DailySnapshotManifest {
        base: SnapshotManifest {
            snapshot_id: id,
            created_at: published_at.clone(),
            source_database: PathBuf::from("synthetic fixture"),
            file: parquet.clone(),
            sha256: hash(&bytes),
            rows,
            symbols: 2,
            first_date: first.clone(),
            last_date: today.to_string(),
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
            selection_rule: "deterministic generated weekday fixtures; not market observations"
                .into(),
            limitations: vec![
                "fully synthetic prices and weekday calendar; no production data coverage".into(),
            ],
        },
        schema_version: 1,
        asset_types: vec!["EQUITY".into(), "ETF".into()],
        securities: vec!["sh600000".into(), "sz510300".into()],
        preheat_start: first.clone(),
        research_start: first.clone(),
        research_end: today.to_string(),
        calendar_identity: None,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    fs::write(directory.join("manifest.json"), &manifest_bytes)?;
    let publication = Publication {
        status: "published",
        snapshot_id: id,
        published_at,
        data_cutoff_date: today.to_string(),
        coverage_status: "unverified",
        price_adjustment: "none",
        source_status: "synthetic",
        manifest_sha256: hash(&manifest_bytes),
    };
    fs::write(
        directory.join("publication.json"),
        serde_json::to_vec_pretty(&publication)?,
    )?;
    println!("snapshot_id={id}");
    println!("snapshot_dir={}", directory.display());
    println!("rows={rows}");
    Ok(())
}
