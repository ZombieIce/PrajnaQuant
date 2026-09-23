#![forbid(unsafe_code)]

pub mod sources;

use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, Utc};
use duckdb::{Connection, OptionalExt, params};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const SCHEMA_VERSION: i32 = 4;

#[derive(Debug)]
pub struct Warehouse {
    conn: Connection,
    _writer_lock: File,
    data_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TencentKlinePayload {
    pub code: i64,
    pub data: std::collections::BTreeMap<String, TencentKlineNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TencentKlineNode {
    pub day: Option<Vec<Vec<serde_json::Value>>>,
}

#[derive(Debug, Clone)]
pub struct DailyBar {
    pub trade_date: NaiveDate,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
    /// Standard unit is shares. Tencent day data's reported volume is lots.
    pub volume_shares: String,
    pub row_hash: String,
}

#[derive(Debug, Serialize)]
pub struct IngestResult {
    pub run_id: Uuid,
    pub source_rows: usize,
    pub new_revisions: usize,
}

#[derive(Debug, Serialize)]
pub struct WarehouseStatus {
    pub database: PathBuf,
    pub revisions: i64,
    pub latest_rows: i64,
    pub successful_runs: i64,
    pub failed_runs: i64,
    pub etf_instruments: i64,
    pub etf_daily_rows: i64,
    pub sh_etf_instruments: i64,
    pub sz_etf_instruments: i64,
    pub latest_etf_trade_date: Option<String>,
    pub etf_history_rows: i64,
    pub etf_history_symbols: i64,
    pub earliest_etf_history_date: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SnapshotResult {
    pub parquet: PathBuf,
    pub rows: i64,
    pub sha256: String,
}

struct RawArchiveRequest<'a> {
    source: &'a str,
    extension: &'a str,
    response: &'a [u8],
    metadata: serde_json::Value,
}

impl Warehouse {
    /// Opens the only allowed writer. Readers must consume immutable Parquet releases.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        fs::create_dir_all(&data_dir).context("create warehouse directory")?;
        let lock_path = data_dir.join(".writer.lock");
        let writer_lock = File::options()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open writer lock {}", lock_path.display()))?;
        writer_lock
            .try_lock_exclusive()
            .context("another warehouse writer is already active")?;

        let database = data_dir.join("market.duckdb");
        let conn = Connection::open(&database)
            .with_context(|| format!("open DuckDB database {}", database.display()))?;
        conn.execute_batch(include_str!("../sql/schema.sql"))
            .context("apply warehouse schema")?;
        Ok(Self {
            conn,
            _writer_lock: writer_lock,
            data_dir,
        })
    }

    pub fn ingest_tencent_day_response(
        &mut self,
        symbol: &str,
        start: NaiveDate,
        end: NaiveDate,
        response: &[u8],
        request_url: &str,
    ) -> Result<IngestResult> {
        validate_symbol(symbol)?;
        if start > end || (end - start).num_days() >= 700 {
            bail!("a daily ingestion window must be between 1 and 700 calendar days");
        }
        let run_id = Uuid::new_v4();
        let observed_at = Utc::now();
        self.conn.execute(
            "INSERT INTO ops.ingest_run(run_id, source, request_url, started_at, status)
             VALUES (?, 'tencent', ?, ?, 'RUNNING')",
            params![run_id.to_string(), request_url, observed_at.to_rfc3339()],
        )?;

        let outcome = (|| -> Result<IngestResult> {
            let raw_path = self.archive_raw(run_id, response, request_url, symbol, start, end)?;
            let raw_hash = sha256(response);
            self.conn.execute(
                "UPDATE ops.ingest_run SET raw_path=?, raw_sha256=? WHERE run_id=?",
                params![raw_path.display().to_string(), raw_hash, run_id.to_string()],
            )?;
            let bars = parse_tencent_day_response(response, symbol, start, end)?;
            let new_revisions = self.insert_daily_revisions(run_id, symbol, &bars, observed_at)?;
            Ok(IngestResult {
                run_id,
                source_rows: bars.len(),
                new_revisions,
            })
        })();

        match outcome {
            Ok(result) => {
                self.conn.execute(
                    "UPDATE ops.ingest_run SET status='SUCCESS', finished_at=?, row_count=?, inserted_count=? WHERE run_id=?",
                    params![
                        Utc::now().to_rfc3339(),
                        i64::try_from(result.source_rows).context("source row count exceeds i64")?,
                        i64::try_from(result.new_revisions).context("revision count exceeds i64")?,
                        run_id.to_string()
                    ],
                )?;
                Ok(result)
            }
            Err(error) => {
                self.conn.execute(
                    "UPDATE ops.ingest_run SET status='FAILED', finished_at=?, error=? WHERE run_id=?",
                    params![Utc::now().to_rfc3339(), error.to_string(), run_id.to_string()],
                )?;
                Err(error)
            }
        }
    }

    pub fn status(&self) -> Result<WarehouseStatus> {
        let revisions = self.conn.query_row(
            "SELECT count(*) FROM staging.daily_bar_revision",
            [],
            |row| row.get(0),
        )?;
        let latest_rows =
            self.conn
                .query_row("SELECT count(*) FROM staging.daily_bar_latest", [], |row| {
                    row.get(0)
                })?;
        let successful_runs = self.conn.query_row(
            "SELECT count(*) FROM ops.ingest_run WHERE status='SUCCESS'",
            [],
            |row| row.get(0),
        )?;
        let failed_runs = self.conn.query_row(
            "SELECT count(*) FROM ops.ingest_run WHERE status='FAILED'",
            [],
            |row| row.get(0),
        )?;
        let etf_instruments = self.conn.query_row(
            "SELECT count(*) FROM core.instrument WHERE asset_class='ETF'",
            [],
            |row| row.get(0),
        )?;
        let etf_daily_rows =
            self.conn
                .query_row("SELECT count(*) FROM research.etf_daily_bar", [], |row| {
                    row.get(0)
                })?;
        let sh_etf_instruments = self.conn.query_row(
            "SELECT count(*) FROM core.instrument WHERE asset_class='ETF' AND market='SH'",
            [],
            |row| row.get(0),
        )?;
        let sz_etf_instruments = self.conn.query_row(
            "SELECT count(*) FROM core.instrument WHERE asset_class='ETF' AND market='SZ'",
            [],
            |row| row.get(0),
        )?;
        let latest_etf_trade_date = self.conn.query_row(
            "SELECT CAST(max(trade_date) AS VARCHAR) FROM research.etf_daily_bar",
            [],
            |row| row.get(0),
        )?;
        let (etf_history_rows, etf_history_symbols, earliest_etf_history_date) =
            self.conn.query_row(
                "SELECT count(*), count(DISTINCT symbol), CAST(min(trade_date) AS VARCHAR)
             FROM research.etf_daily_bar WHERE source='tencent'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        Ok(WarehouseStatus {
            database: self.data_dir.join("market.duckdb"),
            revisions,
            latest_rows,
            successful_runs,
            failed_runs,
            etf_instruments,
            etf_daily_rows,
            sh_etf_instruments,
            sz_etf_instruments,
            latest_etf_trade_date,
            etf_history_rows,
            etf_history_symbols,
            earliest_etf_history_date,
        })
    }

    pub fn export_snapshot(&self) -> Result<SnapshotResult> {
        self.export_snapshot_query("staging.daily_bar_latest", "staging sample only")
    }

    pub fn export_etf_daily_snapshot(&self, trade_date: NaiveDate) -> Result<SnapshotResult> {
        let query = format!(
            "(SELECT * FROM research.etf_daily_bar WHERE trade_date=DATE '{}')",
            trade_date
        );
        let rows: i64 =
            self.conn
                .query_row(&format!("SELECT count(*) FROM {query}"), [], |row| {
                    row.get(0)
                })?;
        if rows == 0 {
            bail!("no ETF daily bars are available for {trade_date}");
        }
        self.export_snapshot_query(
            &query,
            "TDX-observed ETF daily snapshot; classified by ETF name marker and exchange fund code family",
        )
    }

    pub fn etf_symbols(&self) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare(
            "SELECT CASE market WHEN 'SH' THEN 'sh' ELSE 'sz' END || code
             FROM core.instrument WHERE asset_class='ETF' AND market IN ('SH','SZ')
             ORDER BY market, code",
        )?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?)
    }

    pub fn backfill_window_done(
        &self,
        symbol: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<bool> {
        let dataset = backfill_dataset(symbol);
        Ok(self.conn.query_row(
            "SELECT count(*)>0 FROM ops.history_backfill_window
             WHERE dataset=? AND symbol=? AND window_start=? AND window_end=?
               AND source='tencent' AND status IN ('SUCCESS','EMPTY')",
            params![dataset, symbol, start.to_string(), end.to_string()],
            |row| row.get(0),
        )?)
    }

    pub fn record_backfill_window(
        &self,
        symbol: &str,
        start: NaiveDate,
        end: NaiveDate,
        status: &str,
        rows: usize,
        error: Option<&str>,
    ) -> Result<()> {
        if !matches!(status, "SUCCESS" | "EMPTY" | "FAILED") {
            bail!("invalid backfill status");
        }
        let dataset = backfill_dataset(symbol);
        self.conn.execute(
            "INSERT INTO ops.history_backfill_window VALUES (?, ?, ?, ?, 'tencent', ?, ?, ?, ?)
             ON CONFLICT(dataset,symbol,window_start,window_end,source) DO UPDATE SET
             status=excluded.status,row_count=excluded.row_count,error=excluded.error,updated_at=excluded.updated_at",
            params![dataset, symbol, start.to_string(), end.to_string(), status, i64::try_from(rows)?, error, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn archive_empty_tencent_window(
        &self,
        symbol: &str,
        start: NaiveDate,
        end: NaiveDate,
        request_url: &str,
        response: &[u8],
    ) -> Result<()> {
        let run_id = self.create_run("tencent", request_url)?;
        let path = self.archive_raw_source(
            run_id,
            RawArchiveRequest {
                source: "tencent",
                extension: "json",
                response,
                metadata: serde_json::json!({
                    "adapter_version":"tencent-day-v1",
                    "url":request_url,
                    "symbol":symbol,
                    "start":start.to_string(),
                    "end":end.to_string(),
                    "empty":"confirmed source response"
                }),
            },
        )?;
        self.conn.execute(
            "UPDATE ops.ingest_run SET raw_path=?, raw_sha256=? WHERE run_id=?",
            params![
                path.display().to_string(),
                sha256(response),
                run_id.to_string()
            ],
        )?;
        self.finish_run(run_id, 0, 0)
    }

    fn export_daily_snapshot(&self, trade_date: NaiveDate) -> Result<SnapshotResult> {
        let query = format!(
            "(SELECT * FROM staging.daily_bar_latest WHERE source='tdx' AND trade_date=DATE '{}')",
            trade_date
        );
        self.export_snapshot_query(&query, "TDX market-package daily snapshot")
    }

    fn export_snapshot_query(&self, query: &str, scope: &str) -> Result<SnapshotResult> {
        let directory = self
            .data_dir
            .join("snapshots")
            .join(Uuid::new_v4().to_string());
        fs::create_dir_all(&directory)?;
        let parquet = directory.join("daily_latest.parquet");
        let escaped = parquet.display().to_string().replace('\'', "''");
        self.conn.execute_batch(&format!(
            "COPY {query} TO '{escaped}' (FORMAT PARQUET, COMPRESSION ZSTD)"
        ))?;
        let rows = self
            .conn
            .query_row(&format!("SELECT count(*) FROM {query}"), [], |row| {
                row.get(0)
            })?;
        let read_back = self.conn.query_row(
            "SELECT count(*) FROM read_parquet(?)",
            params![parquet.display().to_string()],
            |row| row.get::<_, i64>(0),
        )?;
        if rows != read_back {
            bail!("Parquet read-back row count differs from source table");
        }
        let hash = sha256(&fs::read(&parquet)?);
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": SCHEMA_VERSION,
                "created_at": Utc::now().to_rfc3339(),
                "scope": scope,
                "rows": rows,
                "file": "daily_latest.parquet",
                "sha256": hash,
            }))?,
        )?;
        Ok(SnapshotResult {
            parquet,
            rows,
            sha256: hash,
        })
    }

    fn archive_raw(
        &self,
        run_id: Uuid,
        response: &[u8],
        request_url: &str,
        symbol: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<PathBuf> {
        let directory = self
            .data_dir
            .join("raw")
            .join("tencent")
            .join(run_id.to_string());
        fs::create_dir_all(&directory)?;
        let raw_path = directory.join("response.json");
        fs::write(&raw_path, response)?;
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "adapter_version": "tencent-day-v1",
                "fetched_at": Utc::now().to_rfc3339(),
                "url": request_url,
                "symbol": symbol,
                "start": start.to_string(),
                "end": end.to_string(),
                "sha256": sha256(response),
            }))?,
        )?;
        Ok(raw_path.canonicalize()?)
    }

    fn insert_daily_revisions(
        &mut self,
        run_id: Uuid,
        symbol: &str,
        bars: &[DailyBar],
        observed_at: DateTime<Utc>,
    ) -> Result<usize> {
        self.conn.execute_batch(
            "CREATE OR REPLACE TEMP TABLE import_daily_bar AS
             SELECT * FROM staging.daily_bar_revision WHERE false",
        )?;
        {
            let mut appender =
                self.conn
                    .appender_to_catalog_and_db("import_daily_bar", "temp", "main")?;
            for bar in bars {
                appender.append_row(params![
                    Uuid::new_v4().to_string(),
                    symbol,
                    bar.trade_date.to_string(),
                    "tencent",
                    "none",
                    bar.open,
                    bar.high,
                    bar.low,
                    bar.close,
                    bar.volume_shares,
                    Option::<String>::None,
                    observed_at.to_rfc3339(),
                    run_id.to_string(),
                    bar.row_hash
                ])?;
            }
            appender.flush()?;
        }
        let transaction = self.conn.transaction()?;
        let inserted = transaction.execute(
            "WITH latest AS (
               SELECT trade_date, row_hash
               FROM staging.daily_bar_revision
               WHERE symbol=? AND source='tencent' AND adjustment='none'
               QUALIFY row_number() OVER(
                 PARTITION BY trade_date ORDER BY observed_at DESC, revision_id DESC
               )=1
             )
             INSERT INTO staging.daily_bar_revision
             SELECT incoming.* FROM import_daily_bar incoming
             LEFT JOIN latest
               ON latest.trade_date=incoming.trade_date
             WHERE latest.row_hash IS NULL OR latest.row_hash<>incoming.row_hash",
            params![symbol],
        )?;
        transaction.commit()?;
        self.conn.execute_batch("DROP TABLE import_daily_bar")?;
        Ok(inserted)
    }

    fn create_run(&self, source: &str, request_url: &str) -> Result<Uuid> {
        let run_id = Uuid::new_v4();
        self.conn.execute(
            "INSERT INTO ops.ingest_run(run_id, source, request_url, started_at, status) VALUES (?, ?, ?, ?, 'RUNNING')",
            params![run_id.to_string(), source, request_url, Utc::now().to_rfc3339()],
        )?;
        Ok(run_id)
    }

    pub fn record_fetch_failure(
        &self,
        source: &str,
        request_url: &str,
        error: &anyhow::Error,
    ) -> Result<()> {
        let run_id = self.create_run(source, request_url)?;
        self.fail_run(run_id, error)
    }

    fn finish_run(&self, run_id: Uuid, rows: usize, inserted: usize) -> Result<()> {
        self.conn.execute(
            "UPDATE ops.ingest_run SET status='SUCCESS', finished_at=?, row_count=?, inserted_count=? WHERE run_id=?",
            params![Utc::now().to_rfc3339(), i64::try_from(rows)?, i64::try_from(inserted)?, run_id.to_string()],
        )?;
        Ok(())
    }

    fn fail_run(&self, run_id: Uuid, error: &anyhow::Error) -> Result<()> {
        self.conn.execute(
            "UPDATE ops.ingest_run SET status='FAILED', finished_at=?, error=? WHERE run_id=?",
            params![
                Utc::now().to_rfc3339(),
                error.to_string(),
                run_id.to_string()
            ],
        )?;
        Ok(())
    }

    fn archive_raw_source(&self, run_id: Uuid, request: RawArchiveRequest<'_>) -> Result<PathBuf> {
        let directory = self
            .data_dir
            .join("raw")
            .join(request.source)
            .join(run_id.to_string());
        fs::create_dir_all(&directory)?;
        let response_path = directory.join(format!("response.{}", request.extension));
        fs::write(&response_path, request.response)?;
        let mut metadata = request.metadata;
        metadata["fetched_at"] = serde_json::Value::String(Utc::now().to_rfc3339());
        metadata["sha256"] = serde_json::Value::String(sha256(request.response));
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec_pretty(&metadata)?,
        )?;
        Ok(response_path.canonicalize()?)
    }
}

impl Warehouse {
    pub fn ingest_calendar_response(
        &mut self,
        year: i32,
        month: u32,
        request_url: &str,
        response: &[u8],
        days: &[sources::CalendarDay],
    ) -> Result<IngestResult> {
        let run_id = self.create_run("szse", request_url)?;
        let outcome = (|| -> Result<IngestResult> {
            let path = self.archive_raw_source(run_id, RawArchiveRequest {
                source: "szse", extension: "json", response,
                metadata: serde_json::json!({"adapter_version":"szse-calendar-v1","url":request_url,"year":year,"month":month}),
            })?;
            self.conn.execute(
                "UPDATE ops.ingest_run SET raw_path=?, raw_sha256=? WHERE run_id=?",
                params![
                    path.display().to_string(),
                    sha256(response),
                    run_id.to_string()
                ],
            )?;
            let transaction = self.conn.transaction()?;
            let observed = Utc::now();
            let mut inserted = 0;
            for day in days {
                let hash = sha256(format!("CN|{}|{}", day.trade_date, day.is_open).as_bytes());
                let existing: Option<String> = transaction.query_row(
                    "SELECT row_hash FROM core.trading_calendar_latest WHERE market='CN' AND trade_date=? AND source='szse'",
                    params![day.trade_date.to_string()], |row| row.get(0)).optional()?;
                if existing.as_deref() == Some(&hash) {
                    continue;
                }
                transaction.execute(
                    "INSERT INTO core.trading_calendar_revision VALUES (?, 'CN', ?, ?, 'szse', ?, ?, ?)",
                    params![Uuid::new_v4().to_string(), day.trade_date.to_string(), day.is_open, observed.to_rfc3339(), run_id.to_string(), hash],
                )?;
                inserted += 1;
            }
            transaction.commit()?;
            Ok(IngestResult {
                run_id,
                source_rows: days.len(),
                new_revisions: inserted,
            })
        })();
        match outcome {
            Ok(result) => {
                self.finish_run(run_id, result.source_rows, result.new_revisions)?;
                Ok(result)
            }
            Err(error) => {
                self.fail_run(run_id, &error)?;
                Err(error)
            }
        }
    }

    pub fn ingest_tdx_package_response(
        &mut self,
        trade_date: NaiveDate,
        request_url: &str,
        response: &[u8],
        bars: &[sources::TdxDailyBar],
    ) -> Result<IngestResult> {
        let run_id = self.create_run("tdx", request_url)?;
        let outcome = (|| -> Result<IngestResult> {
            let path = self.archive_raw_source(run_id, RawArchiveRequest {
                source: "tdx", extension: "zip", response,
                metadata: serde_json::json!({"adapter_version":"tdx-g4day-v1","url":request_url,"trade_date":trade_date.to_string()}),
            })?;
            self.conn.execute(
                "UPDATE ops.ingest_run SET raw_path=?, raw_sha256=? WHERE run_id=?",
                params![
                    path.display().to_string(),
                    sha256(response),
                    run_id.to_string()
                ],
            )?;
            let transaction = self.conn.transaction()?;
            let observed = Utc::now();
            let mut inserted = 0;
            let mut coverage = std::collections::BTreeMap::<&str, usize>::new();
            for bar in bars {
                *coverage.entry(bar.market).or_default() += 1;
                let is_etf = is_etf_instrument(bar.market, &bar.code, &bar.name);
                let instrument_id: String = match transaction
                    .query_row(
                        "SELECT instrument_id FROM core.instrument WHERE market=? AND code=?",
                        params![bar.market, bar.code],
                        |row| row.get(0),
                    )
                    .optional()?
                {
                    Some(id) => {
                        transaction.execute(
                            "UPDATE core.instrument SET asset_class=CASE WHEN ? THEN 'ETF' ELSE asset_class END, last_observed_date=greatest(last_observed_date, ?), updated_at=? WHERE instrument_id=?",
                            params![is_etf, trade_date.to_string(), observed.to_rfc3339(), id],
                        )?;
                        id
                    }
                    None => {
                        let id = Uuid::new_v4().to_string();
                        let asset_class = if is_etf {
                            "ETF"
                        } else if is_equity_candidate(bar.market, &bar.code) {
                            "EQUITY_CANDIDATE"
                        } else {
                            "UNKNOWN"
                        };
                        transaction.execute(
                            "INSERT INTO core.instrument VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                            params![
                                id,
                                bar.market,
                                bar.code,
                                asset_class,
                                trade_date.to_string(),
                                trade_date.to_string(),
                                observed.to_rfc3339(),
                                observed.to_rfc3339()
                            ],
                        )?;
                        id
                    }
                };
                if is_etf {
                    let method = "tdx-name-etf-and-exchange-fund-code-v1";
                    let class_hash = sha256(format!("{instrument_id}|ETF|{method}").as_bytes());
                    let existing_class: Option<String> = transaction.query_row(
                        "SELECT row_hash FROM core.instrument_classification_latest WHERE instrument_id=? AND source='tdx'",
                        params![instrument_id],
                        |row| row.get(0),
                    ).optional()?;
                    if existing_class.as_deref() != Some(&class_hash) {
                        transaction.execute(
                            "INSERT INTO core.instrument_classification_revision VALUES (?, ?, 'ETF', ?, 'tdx', ?, ?, ?, ?)",
                            params![Uuid::new_v4().to_string(), instrument_id, trade_date.to_string(), method, observed.to_rfc3339(), run_id.to_string(), class_hash],
                        )?;
                    }
                }
                let symbol = bar.symbol();
                let symbol_hash =
                    sha256(format!("{}|{}|{}", symbol, bar.name, trade_date).as_bytes());
                let old_symbol: Option<String> = transaction.query_row(
                    "SELECT row_hash FROM core.instrument_symbol_latest WHERE instrument_id=? AND symbol=? AND source='tdx' AND valid_from=?",
                    params![instrument_id, symbol, trade_date.to_string()], |row| row.get(0)).optional()?;
                if old_symbol.as_deref() != Some(&symbol_hash) {
                    transaction.execute("INSERT INTO core.instrument_symbol_revision VALUES (?, ?, ?, ?, ?, NULL, 'tdx', ?, ?, ?)", params![Uuid::new_v4().to_string(), instrument_id, symbol, bar.name, trade_date.to_string(), observed.to_rfc3339(), run_id.to_string(), symbol_hash])?;
                }
                let row_hash = sha256(
                    format!(
                        "{}|{}|{}|{}|{}|{}|{}|{}",
                        symbol,
                        trade_date,
                        bar.open,
                        bar.high,
                        bar.low,
                        bar.close,
                        bar.volume_shares,
                        bar.amount_cny
                    )
                    .as_bytes(),
                );
                let existing: Option<String> = transaction.query_row(
                    "SELECT row_hash FROM staging.daily_bar_latest WHERE symbol=? AND trade_date=? AND source='tdx' AND adjustment='none'", params![symbol, trade_date.to_string()], |row| row.get(0)).optional()?;
                if existing.as_deref() == Some(&row_hash) {
                    continue;
                }
                transaction.execute(
                    "INSERT INTO staging.daily_bar_revision(revision_id,symbol,trade_date,source,adjustment,open,high,low,close,volume_shares,amount_cny,observed_at,run_id,row_hash) VALUES (?,?,?,'tdx','none',?,?,?,?,?,?,?, ?,?)",
                    params![Uuid::new_v4().to_string(), symbol, trade_date.to_string(), bar.open, bar.high, bar.low, bar.close, bar.volume_shares.to_string(), bar.amount_cny, observed.to_rfc3339(), run_id.to_string(), row_hash])?;
                inserted += 1;
            }
            for (market, minimum) in [("SH", 10_000_usize), ("SZ", 3_000), ("BJ", 50)] {
                let count = coverage.get(market).copied().unwrap_or_default();
                transaction.execute(
                    "INSERT INTO ops.market_daily_coverage VALUES (?, ?, ?, ?, ?, ?)",
                    params![
                        run_id.to_string(),
                        trade_date.to_string(),
                        market,
                        i64::try_from(count)?,
                        i64::try_from(minimum)?,
                        count >= minimum
                    ],
                )?;
            }
            transaction.commit()?;
            Ok(IngestResult {
                run_id,
                source_rows: bars.len(),
                new_revisions: inserted,
            })
        })();
        match outcome {
            Ok(result) => {
                self.finish_run(run_id, result.source_rows, result.new_revisions)?;
                Ok(result)
            }
            Err(error) => {
                self.fail_run(run_id, &error)?;
                Err(error)
            }
        }
    }

    pub fn ingest_adjustment_response(
        &mut self,
        symbol: &str,
        kind: &str,
        request_url: &str,
        response: &[u8],
        factors: &[sources::AdjustmentFactor],
    ) -> Result<IngestResult> {
        sources::validate_explicit_symbol(symbol, true)?;
        let run_id = self.create_run("sina", request_url)?;
        let outcome = (|| -> Result<IngestResult> {
            let path = self.archive_raw_source(run_id, RawArchiveRequest { source:"sina", extension:"js", response,
                metadata:serde_json::json!({"adapter_version":"sina-adjustment-v1","url":request_url,"symbol":symbol,"kind":kind}) })?;
            self.conn.execute(
                "UPDATE ops.ingest_run SET raw_path=?, raw_sha256=? WHERE run_id=?",
                params![
                    path.display().to_string(),
                    sha256(response),
                    run_id.to_string()
                ],
            )?;
            let transaction = self.conn.transaction()?;
            let observed = Utc::now();
            let mut inserted = 0;
            for factor in factors {
                let hash = sha256(
                    format!(
                        "{symbol}|{kind}|{}|{}",
                        factor.effective_date, factor.factor
                    )
                    .as_bytes(),
                );
                let existing: Option<String> = transaction.query_row("SELECT row_hash FROM core.adjustment_factor_latest WHERE symbol=? AND effective_date=? AND adjustment_kind=? AND source='sina'", params![symbol, factor.effective_date.to_string(), kind], |row| row.get(0)).optional()?;
                if existing.as_deref() == Some(&hash) {
                    continue;
                }
                transaction.execute("INSERT INTO core.adjustment_factor_revision VALUES (?, ?, ?, ?, ?, 'sina', ?, ?, ?)", params![Uuid::new_v4().to_string(), symbol, factor.effective_date.to_string(), kind, factor.factor, observed.to_rfc3339(), run_id.to_string(), hash])?;
                inserted += 1;
            }
            transaction.commit()?;
            Ok(IngestResult {
                run_id,
                source_rows: factors.len(),
                new_revisions: inserted,
            })
        })();
        match outcome {
            Ok(result) => {
                self.finish_run(run_id, result.source_rows, result.new_revisions)?;
                Ok(result)
            }
            Err(error) => {
                self.fail_run(run_id, &error)?;
                Err(error)
            }
        }
    }

    pub fn import_security_status_csv(
        &mut self,
        csv_bytes: &[u8],
        source_name: &str,
        source_url: &str,
    ) -> Result<IngestResult> {
        let run_id = self.create_run(source_name, source_url)?;
        let outcome = (|| -> Result<IngestResult> {
            let path = self.archive_raw_source(run_id, RawArchiveRequest { source:source_name, extension:"csv", response:csv_bytes,
                metadata:serde_json::json!({"adapter_version":"security-status-csv-v1","url":source_url}) })?;
            self.conn.execute(
                "UPDATE ops.ingest_run SET raw_path=?, raw_sha256=? WHERE run_id=?",
                params![
                    path.display().to_string(),
                    sha256(csv_bytes),
                    run_id.to_string()
                ],
            )?;
            let mut reader = csv::Reader::from_reader(csv_bytes);
            let headers = reader.headers()?.clone();
            for expected in [
                "symbol",
                "effective_date",
                "trade_status",
                "is_st",
                "limit_rule_id",
            ] {
                if !headers.iter().any(|header| header == expected) {
                    bail!("status CSV lacks required {expected} column");
                }
            }
            let transaction = self.conn.transaction()?;
            let observed = Utc::now();
            let mut rows = 0;
            let mut inserted = 0;
            for record in reader.deserialize::<StatusCsvRow>() {
                let row = record?;
                sources::validate_explicit_symbol(&row.symbol, true)?;
                let date = NaiveDate::parse_from_str(&row.effective_date, "%Y-%m-%d")?;
                if !matches!(row.trade_status.as_str(), "TRADABLE" | "HALTED" | "UNKNOWN") {
                    bail!("invalid trade_status");
                }
                let hash = sha256(
                    format!(
                        "{}|{}|{}|{:?}|{:?}",
                        row.symbol, date, row.trade_status, row.is_st, row.limit_rule_id
                    )
                    .as_bytes(),
                );
                let existing: Option<String> = transaction.query_row("SELECT row_hash FROM core.security_status_latest WHERE symbol=? AND effective_date=? AND source=?", params![row.symbol,date.to_string(),source_name], |query| query.get(0)).optional()?;
                rows += 1;
                if existing.as_deref() == Some(&hash) {
                    continue;
                }
                transaction.execute("INSERT INTO core.security_status_revision VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", params![Uuid::new_v4().to_string(),row.symbol,date.to_string(),row.trade_status,row.is_st,row.limit_rule_id,source_name,observed.to_rfc3339(),run_id.to_string(),hash])?;
                inserted += 1;
            }
            transaction.commit()?;
            if rows == 0 {
                bail!("status CSV has no records");
            }
            Ok(IngestResult {
                run_id,
                source_rows: rows,
                new_revisions: inserted,
            })
        })();
        match outcome {
            Ok(result) => {
                self.finish_run(run_id, result.source_rows, result.new_revisions)?;
                Ok(result)
            }
            Err(error) => {
                self.fail_run(run_id, &error)?;
                Err(error)
            }
        }
    }

    pub fn publish_daily(&mut self, trade_date: NaiveDate) -> Result<SnapshotResult> {
        let run = self.conn.query_row("SELECT c.run_id FROM ops.market_daily_coverage c JOIN ops.ingest_run r ON r.run_id=c.run_id WHERE c.trade_date=? AND r.source='tdx' AND r.status='SUCCESS' ORDER BY r.finished_at DESC LIMIT 1", params![trade_date.to_string()], |row| row.get::<_, String>(0)).optional()?;
        let Some(run_id) = run else {
            bail!("no successful TDX package is available for publication");
        };
        let calendar_open: Option<bool> = self.conn.query_row("SELECT is_open FROM core.trading_calendar_latest WHERE market='CN' AND trade_date=? AND source='szse'", params![trade_date.to_string()], |row| row.get(0)).optional()?;
        if calendar_open != Some(true) {
            self.record_quality_issue(
                "daily_bar",
                Some(trade_date),
                "ERROR",
                "CALENDAR_NOT_OPEN",
                "No confirmed open CN calendar row",
                Some(&run_id),
            )?;
            bail!("publication blocked: calendar does not confirm an open trading day");
        }
        let mut blocked = false;
        for market in ["SH", "SZ", "BJ"] {
            let complete: Option<bool> = self
                .conn
                .query_row(
                    "SELECT is_complete FROM ops.market_daily_coverage WHERE run_id=? AND market=?",
                    params![run_id, market],
                    |row| row.get(0),
                )
                .optional()?;
            if complete != Some(true) {
                blocked = true;
                self.record_quality_issue(
                    "daily_bar",
                    Some(trade_date),
                    "ERROR",
                    "MARKET_COVERAGE",
                    "TDX package does not meet the minimum market row count",
                    Some(&run_id),
                )?;
            }
        }
        if blocked {
            bail!("publication blocked: one or more market coverage checks failed");
        }
        let snapshot = self.export_daily_snapshot(trade_date)?;
        self.conn.execute(
            "INSERT INTO ops.dataset_release VALUES (?, 'daily_bar', ?, 'PUBLISHED', ?, ?, ?)",
            params![
                Uuid::new_v4().to_string(),
                trade_date.to_string(),
                snapshot.parquet.display().to_string(),
                "TDX package minimum coverage and SZSE calendar checks passed",
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(snapshot)
    }

    fn record_quality_issue(
        &self,
        dataset: &str,
        trade_date: Option<NaiveDate>,
        severity: &str,
        code: &str,
        detail: &str,
        run_id: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO ops.quality_issue VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                Uuid::new_v4().to_string(),
                dataset,
                trade_date.map(|date| date.to_string()),
                severity,
                code,
                detail,
                run_id,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct StatusCsvRow {
    symbol: String,
    effective_date: String,
    trade_status: String,
    is_st: Option<bool>,
    limit_rule_id: Option<String>,
}

pub fn parse_tencent_day_response(
    response: &[u8],
    symbol: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<DailyBar>> {
    validate_symbol(symbol)?;
    let payload: TencentKlinePayload =
        serde_json::from_slice(response).context("response is not valid JSON")?;
    if payload.code != 0 {
        bail!("Tencent response has non-zero status code {}", payload.code);
    }
    let node = payload
        .data
        .get(symbol)
        .context("response does not contain the requested symbol")?;
    let days = node
        .day
        .as_ref()
        .context("response has no unadjusted day field; refusing adjusted substitution")?;
    if days.is_empty() {
        bail!("response contains an empty day series; it is not a successful import");
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut bars = Vec::with_capacity(days.len());
    for values in days {
        if values.len() < 6 {
            bail!("day row has fewer than six fields")
        }
        let trade_date = values[0]
            .as_str()
            .context("day row date is not a string")?
            .parse::<NaiveDate>()
            .context("day row date is invalid")?;
        if !(start..=end).contains(&trade_date) || !seen.insert(trade_date) {
            bail!("day row date is outside request window or duplicated: {trade_date}");
        }
        let open = decimal_text(&values[1])?;
        let close = decimal_text(&values[2])?;
        let high = decimal_text(&values[3])?;
        let low = decimal_text(&values[4])?;
        let volume_lots = decimal_text(&values[5])?;
        let (o, c, h, l, lots) = (
            parse_decimal(&open)?,
            parse_decimal(&close)?,
            parse_decimal(&high)?,
            parse_decimal(&low)?,
            parse_decimal(&volume_lots)?,
        );
        if l <= 0.0 || l > o || l > c || h < o || h < c || lots < 0.0 {
            bail!("invalid OHLC or volume for {trade_date}");
        }
        let volume_shares = canonical_decimal(lots * 100.0)?;
        let hash_input = format!("{trade_date}|{open}|{high}|{low}|{close}|{volume_shares}");
        bars.push(DailyBar {
            trade_date,
            open,
            high,
            low,
            close,
            volume_shares,
            row_hash: sha256(hash_input.as_bytes()),
        });
    }
    Ok(bars)
}

pub fn validate_symbol(symbol: &str) -> Result<()> {
    let valid = symbol.len() == 8
        && (symbol == "sh000300"
            || symbol.starts_with("sh6")
            || symbol.starts_with("sh5")
            || symbol.starts_with("sz0")
            || symbol.starts_with("sz3")
            || symbol.starts_with("sz15"))
        && symbol.as_bytes()[2..].iter().all(u8::is_ascii_digit);
    if !valid {
        bail!(
            "daily-bar symbols must use an explicit supported Shanghai/Shenzhen stock or ETF prefix"
        );
    }
    Ok(())
}

fn backfill_dataset(symbol: &str) -> &'static str {
    if symbol == "sh000300" {
        "benchmark_daily_bar"
    } else {
        "etf_daily_bar"
    }
}

fn decimal_text(value: &serde_json::Value) -> Result<String> {
    let text = match value {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Number(value) => value.to_string(),
        _ => bail!("a numeric day field is missing or not numeric"),
    };
    let parsed = parse_decimal(&text)?;
    canonical_decimal(parsed)
}

fn parse_decimal(text: &str) -> Result<f64> {
    let parsed: f64 = text.parse().context("numeric field cannot be parsed")?;
    if !parsed.is_finite() {
        bail!("NaN and infinity are prohibited")
    }
    Ok(parsed)
}

fn canonical_decimal(value: f64) -> Result<String> {
    if !value.is_finite() {
        bail!("NaN and infinity are prohibited")
    }
    Ok(format!("{value:.6}"))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_equity_candidate(market: &str, code: &str) -> bool {
    match market {
        "SH" => ["600", "601", "603", "605", "688", "689"]
            .iter()
            .any(|prefix| code.starts_with(prefix)),
        "SZ" => ["000", "001", "002", "003", "300", "301"]
            .iter()
            .any(|prefix| code.starts_with(prefix)),
        "BJ" => ["4", "8", "92"]
            .iter()
            .any(|prefix| code.starts_with(prefix)),
        _ => false,
    }
}

fn is_etf_instrument(market: &str, code: &str, name: &str) -> bool {
    let has_etf_marker = name.to_ascii_uppercase().contains("ETF");
    has_etf_marker
        && match market {
            // Shanghai exchange-traded funds use the 5xxxxx fund family.
            "SH" => code.starts_with('5'),
            // Shenzhen ETFs are in the 15xxxx fund family. The name condition
            // separates them from historical structured funds in nearby ranges.
            "SZ" => code.starts_with("15"),
            "BJ" => false,
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn payload(rows: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"code": 0, "data": {"sh600519": {"day": rows}}}))
            .unwrap()
    }

    #[test]
    fn converts_lots_to_shares_and_orders_ohlc() {
        let bars = parse_tencent_day_response(
            &payload(serde_json::json!([[
                "2026-09-18",
                "10",
                "11",
                "12",
                "9",
                "123.45"
            ]])),
            "sh600519",
            NaiveDate::from_ymd_opt(2026, 9, 14).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
        )
        .unwrap();
        assert_eq!(bars[0].volume_shares, "12345.000000");
        assert_eq!(bars[0].high, "12.000000");
    }

    #[test]
    fn rejects_adjusted_substitution_duplicates_and_bad_ohlc() {
        let start = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        for rows in [
            serde_json::json!([]),
            serde_json::json!([
                ["2026-09-18", "10", "11", "12", "9", "123"],
                ["2026-09-18", "10", "11", "12", "9", "123"]
            ]),
            serde_json::json!([["2026-09-18", "10", "11", "8", "9", "123"]]),
        ] {
            assert!(parse_tencent_day_response(&payload(rows), "sh600519", start, end).is_err());
        }
        let raw = br#"{"code":0,"data":{"sh600519":{"qfqday":[]}}}"#;
        assert!(parse_tencent_day_response(raw, "sh600519", start, end).is_err());
    }

    #[test]
    fn revisions_are_idempotent_and_snapshot_reads_back() {
        let temporary = TempDir::new().unwrap();
        let mut warehouse = Warehouse::open(temporary.path()).unwrap();
        let start = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        let first = payload(serde_json::json!([[
            "2026-09-18",
            "10",
            "11",
            "12",
            "9",
            "123.45"
        ]]));
        let changed = payload(serde_json::json!([[
            "2026-09-18",
            "10",
            "10.5",
            "12",
            "9",
            "123.45"
        ]]));
        assert_eq!(
            warehouse
                .ingest_tencent_day_response("sh600519", start, end, &first, "fixture://one")
                .unwrap()
                .new_revisions,
            1
        );
        assert_eq!(
            warehouse
                .ingest_tencent_day_response("sh600519", start, end, &first, "fixture://two")
                .unwrap()
                .new_revisions,
            0
        );
        assert_eq!(
            warehouse
                .ingest_tencent_day_response("sh600519", start, end, &changed, "fixture://three")
                .unwrap()
                .new_revisions,
            1
        );
        assert_eq!(
            warehouse
                .ingest_tencent_day_response("sh600519", start, end, &first, "fixture://four")
                .unwrap()
                .new_revisions,
            1
        );
        assert_eq!(warehouse.status().unwrap().revisions, 3);
        let snapshot = warehouse.export_snapshot().unwrap();
        assert_eq!(snapshot.rows, 1);
        assert!(snapshot.parquet.exists());
    }

    #[test]
    fn one_writer_lock_is_enforced() {
        let temporary = TempDir::new().unwrap();
        let _first = Warehouse::open(temporary.path()).unwrap();
        assert!(Warehouse::open(temporary.path()).is_err());
    }

    #[test]
    fn calendar_factors_and_status_are_versioned_for_research_queries() {
        let temporary = TempDir::new().unwrap();
        let mut warehouse = Warehouse::open(temporary.path()).unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        let calendar = [sources::CalendarDay {
            trade_date: date,
            is_open: true,
        }];
        assert_eq!(
            warehouse
                .ingest_calendar_response(2026, 9, "fixture://szse", b"{}", &calendar)
                .unwrap()
                .new_revisions,
            1
        );
        let raw = payload(serde_json::json!([[
            "2026-09-18",
            "10",
            "11",
            "12",
            "9",
            "123.45"
        ]]));
        warehouse
            .ingest_tencent_day_response("sh600519", date, date, &raw, "fixture://tencent")
            .unwrap();
        let factors = [sources::AdjustmentFactor {
            effective_date: date,
            factor: "2.0000000000".to_string(),
        }];
        assert_eq!(
            warehouse
                .ingest_adjustment_response(
                    "sh600519",
                    "qfq",
                    "fixture://sina",
                    b"var x={};",
                    &factors
                )
                .unwrap()
                .new_revisions,
            1
        );
        let adjusted: f64 = warehouse
            .conn
            .query_row(
                "SELECT adjusted_close FROM research.daily_bar_qfq WHERE symbol='sh600519'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(adjusted, 5.5);
        let status = b"symbol,effective_date,trade_status,is_st,limit_rule_id\nsh600519,2026-09-18,TRADABLE,false,mainboard\n";
        warehouse
            .import_security_status_csv(status, "fixture", "fixture://status")
            .unwrap();
        let tradable: bool = warehouse
            .conn
            .query_row(
                "SELECT is_tradable FROM research.daily_bar_execution WHERE symbol='sh600519'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(tradable);
    }

    #[test]
    fn etf_classification_requires_name_and_exchange_fund_family() {
        assert!(validate_symbol("sh510300").is_ok());
        assert!(validate_symbol("sz159919").is_ok());
        assert!(validate_symbol("sh000300").is_ok());
        assert!(is_etf_instrument("SH", "510300", "300ETF"));
        assert!(is_etf_instrument("SZ", "159919", "沪深300ETF"));
        assert!(!is_etf_instrument("SH", "501018", "南方原油LOF"));
        assert!(!is_etf_instrument("SZ", "150001", "瑞福进取"));
        assert!(!is_etf_instrument("SH", "600000", "浦发ETF公司"));
    }
}
