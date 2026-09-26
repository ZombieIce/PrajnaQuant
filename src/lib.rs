#![forbid(unsafe_code)]

pub mod sources;

use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, Timelike, Utc};
use duckdb::{AccessMode, Config, Connection, OptionalExt, params};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const SCHEMA_VERSION: i32 = 7;

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
pub struct HistoryLengthAudit {
    pub symbol: String,
    pub expected_windows: usize,
    pub recorded_windows: usize,
    pub failed_windows: usize,
    pub source_rows: i64,
    pub stored_days: i64,
    pub first_date: Option<String>,
    pub last_date: Option<String>,
    pub row_count_matches: bool,
    pub coverage_complete: bool,
}

#[derive(Debug, Serialize)]
pub struct SymbolCalendarAudit {
    pub symbol: String,
    pub observed_bar_days: usize,
    pub expected_open_days: usize,
    pub bars_on_uncovered_calendar_dates: usize,
    pub missing_open_dates: Vec<String>,
    pub bars_on_closed_dates: Vec<String>,
    pub matches_calendar_on_covered_dates: bool,
}

#[derive(Debug, Serialize)]
pub struct TradingCalendarAudit {
    pub start: String,
    pub end: String,
    pub natural_days: usize,
    pub calendar_days: usize,
    pub open_days: usize,
    pub missing_calendar_dates: Vec<String>,
    pub calendar_complete: bool,
    pub symbols: Vec<SymbolCalendarAudit>,
}

#[derive(Debug, Serialize)]
pub struct SnapshotResult {
    pub parquet: PathBuf,
    pub rows: i64,
    pub sha256: String,
}

#[derive(Debug, Serialize)]
pub struct DailySyncResult {
    pub job_id: Uuid,
    pub request_fingerprint: String,
    pub symbols: Vec<String>,
    pub requested_start: String,
    pub requested_end: String,
    pub attempts: usize,
    pub source_rows: usize,
    pub inserted_revisions: usize,
    pub duplicate_rows: usize,
    pub empty_responses: usize,
    pub snapshot: Option<DailySnapshotResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DailySnapshotResult {
    pub snapshot_id: Uuid,
    pub manifest_path: PathBuf,
    pub data_path: PathBuf,
    pub manifest_sha256: String,
    pub data_sha256: String,
    pub coverage_status: String,
    pub data_cutoff_date: String,
}

#[derive(Debug, Clone)]
pub struct StatusCoverageEvidence {
    pub symbol: String,
    pub coverage_start: NaiveDate,
    pub coverage_end: NaiveDate,
    pub declared_coverage: String,
    pub verification_status: String,
    pub source_ref: String,
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SecurityDirectoryCsvRow {
    market: String,
    code: String,
    asset_class: String,
    name: String,
    effective_date: String,
    source_ref: String,
}

type SecurityDirectoryDbRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
);

/// Read-only snapshot resolver for research/API processes. It never opens the
/// DuckDB and fixes the selected ID before any Parquet read begins.
pub fn resolve_published_daily_snapshot(
    data_dir: &Path,
    requested_id: Option<Uuid>,
) -> Result<DailySnapshotResult> {
    let root = data_dir.join("snapshots");
    let (id, pointer_manifest_hash) = match requested_id {
        Some(id) => (id, None),
        None => {
            let pointer: serde_json::Value = serde_json::from_slice(
                &fs::read(root.join("current.json"))
                    .context("no current published daily snapshot")?,
            )?;
            let id = Uuid::parse_str(
                pointer["snapshot_id"]
                    .as_str()
                    .context("current pointer has no snapshot_id")?,
            )?;
            let manifest_hash = pointer["manifest_sha256"]
                .as_str()
                .context("current pointer lacks manifest hash")?
                .to_owned();
            (id, Some(manifest_hash))
        }
    };
    let directory = root.join(id.to_string());
    let manifest_path = directory.join("manifest.json");
    let manifest_bytes =
        fs::read(&manifest_path).context("requested daily snapshot is not published")?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
    if manifest["snapshot_id"].as_str() != Some(&id.to_string()) {
        bail!("snapshot ID does not match manifest");
    }
    let manifest_hash = sha256(&manifest_bytes);
    let data_path = directory.join("daily.parquet");
    let data_hash = manifest["data_sha256"]
        .as_str()
        .context("manifest lacks data hash")?
        .to_owned();
    if sha256(&fs::read(&data_path)?) != data_hash {
        bail!("daily snapshot Parquet hash mismatch");
    }
    let catalog_file = manifest["catalog_file"]
        .as_str()
        .context("manifest lacks securities catalog")?;
    let catalog_path = directory.join(catalog_file);
    let catalog_hash = manifest["catalog_sha256"]
        .as_str()
        .context("manifest lacks catalog hash")?;
    if sha256(&fs::read(catalog_path)?) != catalog_hash {
        bail!("daily snapshot securities catalog hash mismatch");
    }
    let calendar_file = manifest["calendar_file"]
        .as_str()
        .context("manifest lacks calendar sidecar")?;
    let calendar_path = directory.join(calendar_file);
    let expected_calendar = manifest["calendar_sha256"]
        .as_str()
        .context("manifest lacks calendar hash")?;
    if sha256(&fs::read(calendar_path)?) != expected_calendar {
        bail!("daily snapshot calendar hash mismatch");
    }
    if let Some(expected_hash) = pointer_manifest_hash {
        if expected_hash != manifest_hash {
            bail!("current pointer manifest hash mismatch");
        }
    }
    Ok(DailySnapshotResult {
        snapshot_id: id,
        manifest_path,
        data_path,
        manifest_sha256: manifest_hash,
        data_sha256: data_hash,
        coverage_status: manifest["coverage_status"]
            .as_str()
            .unwrap_or("unverified")
            .into(),
        data_cutoff_date: manifest["data_cutoff_date"].as_str().unwrap_or("").into(),
    })
}

/// Membership fact parsed from a historical index source. Generic imports are
/// always stored as `unknown`; only a source-specific validator may certify PIT.
#[derive(Debug, Clone)]
pub struct IndexMembershipFact {
    pub index_code: String,
    pub instrument_id: String,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub published_at: Option<DateTime<Utc>>,
    pub effective_to_published_at: Option<DateTime<Utc>>,
    pub publication_time_method: String,
    pub source_ref: String,
    pub source_revision_hash: String,
}

#[derive(Debug, Clone)]
pub struct IndexMembershipCoverage {
    pub index_code: String,
    pub coverage_start: NaiveDate,
    pub coverage_end: NaiveDate,
    /// Claim made by the source/importer. `complete` is not trusted by the
    /// generic warehouse importer and is persisted as `unverified`.
    pub declared_status: String,
    pub gap_detail: Option<String>,
    pub source_ref: String,
    pub source_revision_hash: String,
}

struct RawArchiveRequest<'a> {
    source: &'a str,
    extension: &'a str,
    response: &'a [u8],
    metadata: serde_json::Value,
}

impl Warehouse {
    /// Opens an existing warehouse for status inspection without creating files,
    /// migrating schema, or taking the exclusive writer lock. The shared lock
    /// ensures the report cannot observe a concurrent app writer.
    pub fn open_readonly_status(data_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        let lock_path = data_dir.join(".writer.lock");
        let writer_lock = File::options()
            .read(true)
            .open(&lock_path)
            .with_context(|| format!("open existing warehouse lock {}", lock_path.display()))?;
        FileExt::try_lock_shared(&writer_lock)
            .context("another warehouse writer is active; status read deferred")?;

        let database = data_dir.join("market.duckdb");
        let config = Config::default().access_mode(AccessMode::ReadOnly)?;
        let conn = Connection::open_with_flags(&database, config)
            .with_context(|| format!("open warehouse read-only {}", database.display()))?;
        Ok(Self {
            conn,
            _writer_lock: writer_lock,
            data_dir,
        })
    }

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
        conn.execute_batch(include_str!("../sql/migrations/005_index_membership.sql"))
            .context("apply index membership schema migration 5")?;
        conn.execute_batch(include_str!(
            "../sql/migrations/006_membership_simulated_exit_known_at.sql"
        ))
        .context("apply index membership schema migration 6")?;
        conn.execute_batch(include_str!("../sql/migrations/007_daily_sync.sql"))
            .context("apply daily sync schema migration 7")?;
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
        if start > end || (end - start).num_days() >= 800 {
            bail!("a daily ingestion window must be between 1 and 800 calendar days");
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

    /// Runs an explicit, bounded Tencent daily sync. Every invocation re-reads
    /// the requested lookback so source revisions are not hidden by a high-water mark.
    pub fn sync_daily_tencent(
        &mut self,
        symbols: &[String],
        start: NaiveDate,
        end: NaiveDate,
        lookback_days: u32,
        max_retries: u32,
    ) -> Result<DailySyncResult> {
        self.sync_daily_tencent_with_fetch(
            symbols,
            start,
            end,
            lookback_days,
            max_retries,
            sources::fetch_tencent_raw_daily,
        )
    }

    fn sync_daily_tencent_with_fetch<F>(
        &mut self,
        symbols: &[String],
        start: NaiveDate,
        end: NaiveDate,
        lookback_days: u32,
        max_retries: u32,
        mut fetch: F,
    ) -> Result<DailySyncResult>
    where
        F: FnMut(&str, NaiveDate, NaiveDate) -> Result<(String, Vec<u8>)>,
    {
        if symbols.is_empty()
            || start > end
            || lookback_days > 799
            || max_retries > 5
            || (end - start).num_days() + i64::from(lookback_days) > 799
        {
            bail!(
                "sync requires symbols, start <= end, request window <= 800 days, lookback <= 799 days, retries <= 5"
            );
        }
        let mut canonical_symbols = symbols.to_vec();
        canonical_symbols.sort();
        canonical_symbols.dedup();
        if canonical_symbols.len() != symbols.len() {
            bail!("sync securities must be unique");
        }
        for symbol in &canonical_symbols {
            validate_symbol(symbol)?;
        }
        let mut digest = Sha256::new();
        digest.update(canonical_symbols.join(",").as_bytes());
        digest.update(start.to_string().as_bytes());
        digest.update(end.to_string().as_bytes());
        digest.update(lookback_days.to_le_bytes());
        let fingerprint = format!("{:x}", digest.finalize());
        // A same-day bar is eligible only after the exchange session's close
        // and only when the independent calendar confirms an open date.
        let shanghai_now = Utc::now() + chrono::Duration::hours(8);
        let shanghai_today = shanghai_now.date_naive();
        if end > shanghai_today {
            bail!("target end cannot be later than the current Asia/Shanghai date");
        }
        if end == shanghai_today {
            let calendar_open:Option<bool>=self.conn.query_row("SELECT is_open FROM core.trading_calendar_latest WHERE market='CN' AND source='szse' AND trade_date=?",params![end.to_string()],|row|row.get(0)).optional()?;
            if calendar_open != Some(true)
                || shanghai_now.hour() < 18
                || (shanghai_now.hour() == 18 && shanghai_now.minute() < 30)
            {
                self.record_not_published_job(
                    &fingerprint,
                    &canonical_symbols,
                    start,
                    end,
                    lookback_days,
                    "same-day daily bars remain non-final until 18:30 Asia/Shanghai and a confirmed open-calendar record exists",
                )?;
                bail!(
                    "same-day daily bars remain non-final until 18:30 Asia/Shanghai and a confirmed open-calendar record exists"
                );
            }
        }
        let prior: Option<(String, String)> = self.conn.query_row(
            "SELECT job_id,status FROM ops.daily_sync_job WHERE request_fingerprint=? ORDER BY started_at DESC LIMIT 1",
            params![fingerprint], |row| Ok((row.get(0)?,row.get(1)?)),
        ).optional()?;
        let job_id = match prior {
            Some((id, status)) if status == "RUNNING" || status == "FAILED" => {
                let id = Uuid::parse_str(&id)?;
                self.conn.execute("UPDATE ops.daily_sync_job SET status='RUNNING',error=NULL,error_class=NULL,updated_at=? WHERE job_id=?",params![Utc::now().to_rfc3339(),id.to_string()])?;
                id
            }
            _ => {
                let id = Uuid::new_v4();
                let now = Utc::now();
                self.conn.execute(
                    "INSERT INTO ops.daily_sync_job(job_id,request_fingerprint,dataset,source,symbols_json,range_start,range_end,lookback_days,status,started_at,updated_at) VALUES (?,?, 'daily_bar','tencent',?,?,?,?,'RUNNING',?,?)",
                    params![id.to_string(), fingerprint, serde_json::to_string(&canonical_symbols)?, start.to_string(), end.to_string(), i64::from(lookback_days), now.to_rfc3339(), now.to_rfc3339()],
                )?;
                id
            }
        };
        let (prior_rows,prior_inserted,prior_empty): (i64,i64,i64)=self.conn.query_row(
            "SELECT coalesce(sum(source_rows),0),coalesce(sum(inserted_revisions),0),count(*) FILTER(WHERE status='EMPTY') FROM ops.daily_sync_attempt WHERE job_id=?",
            params![job_id.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        )?;

        let mut result = DailySyncResult {
            job_id,
            request_fingerprint: fingerprint,
            symbols: canonical_symbols.clone(),
            requested_start: start.to_string(),
            requested_end: end.to_string(),
            attempts: 0,
            source_rows: usize::try_from(prior_rows)?,
            inserted_revisions: usize::try_from(prior_inserted)?,
            duplicate_rows: usize::try_from(prior_rows.saturating_sub(prior_inserted))?,
            empty_responses: usize::try_from(prior_empty)?,
            snapshot: None,
        };
        let mut cursor_error: Option<(String, String)> = None;
        for symbol in &canonical_symbols {
            let window_start = start - chrono::Duration::days(i64::from(lookback_days));
            let prior_status: Option<String>=self.conn.query_row("SELECT status FROM ops.daily_sync_attempt WHERE job_id=? AND symbol=? AND window_start=? AND window_end=? ORDER BY attempt_no DESC LIMIT 1",params![job_id.to_string(),symbol,window_start.to_string(),end.to_string()],|row|row.get(0)).optional()?;
            if matches!(prior_status.as_deref(), Some("SUCCESS" | "EMPTY")) {
                continue;
            }
            let prior_attempts: i64=self.conn.query_row("SELECT count(*) FROM ops.daily_sync_attempt WHERE job_id=? AND symbol=? AND window_start=? AND window_end=?",params![job_id.to_string(),symbol,window_start.to_string(),end.to_string()],|row|row.get(0))?;
            self.conn.execute(
                "UPDATE ops.daily_sync_job SET cursor_symbol=?,cursor_start=?,cursor_end=?,updated_at=? WHERE job_id=?",
                params![symbol, window_start.to_string(), end.to_string(), Utc::now().to_rfc3339(), job_id.to_string()],
            )?;
            let mut completed = false;
            for attempt_no in u32::try_from(prior_attempts)? + 1
                ..=u32::try_from(prior_attempts)? + max_retries + 1
            {
                result.attempts += 1;
                let attempt_id = Uuid::new_v4();
                let attempt_started = Utc::now();
                let request = fetch(symbol, window_start, end);
                match request {
                    Ok((url, body)) => {
                        let is_empty = sources::tencent_raw_daily_is_empty(&body, symbol);
                        match is_empty {
                            Ok(true) => {
                                let raw_path = self.archive_empty_tencent_window(
                                    symbol,
                                    window_start,
                                    end,
                                    &url,
                                    &body,
                                )?;
                                self.record_daily_sync_attempt(
                                    &attempt_id,
                                    &job_id,
                                    symbol,
                                    window_start,
                                    end,
                                    attempt_no,
                                    "EMPTY",
                                    0,
                                    0,
                                    Some(&raw_path.display().to_string()),
                                    Some(&url),
                                    None,
                                    None,
                                    attempt_started,
                                )?;
                                result.empty_responses += 1;
                                completed = true;
                                break;
                            }
                            Ok(false) => {
                                match self.ingest_tencent_day_response(
                                    symbol,
                                    window_start,
                                    end,
                                    &body,
                                    &url,
                                ) {
                                    Ok(ingest) => {
                                        let raw_path: Option<String> = self.conn.query_row("SELECT raw_path FROM ops.ingest_run WHERE run_id=?", params![ingest.run_id.to_string()], |row| row.get(0)).optional()?;
                                        let raw_hash: Option<String> = self.conn.query_row("SELECT raw_sha256 FROM ops.ingest_run WHERE run_id=?", params![ingest.run_id.to_string()], |row| row.get(0)).optional()?;
                                        self.record_daily_sync_attempt(
                                            &attempt_id,
                                            &job_id,
                                            symbol,
                                            window_start,
                                            end,
                                            attempt_no,
                                            "SUCCESS",
                                            ingest.source_rows,
                                            ingest.new_revisions,
                                            raw_path.as_deref(),
                                            Some(&url),
                                            None,
                                            None,
                                            attempt_started,
                                        )?;
                                        result.source_rows += ingest.source_rows;
                                        result.inserted_revisions += ingest.new_revisions;
                                        result.duplicate_rows +=
                                            ingest.source_rows.saturating_sub(ingest.new_revisions);
                                        let _ = raw_hash; // Raw hash remains indexed by the ingest run and archive manifest.
                                        completed = true;
                                        break;
                                    }
                                    Err(error) => {
                                        let failed_path:Option<String>=self.conn.query_row("SELECT raw_path FROM ops.ingest_run WHERE source='tencent' AND request_url=? AND started_at>=? ORDER BY started_at DESC LIMIT 1",params![url,attempt_started.to_rfc3339()],|row|row.get(0)).optional()?;
                                        self.record_daily_sync_attempt(
                                            &attempt_id,
                                            &job_id,
                                            symbol,
                                            window_start,
                                            end,
                                            attempt_no,
                                            "FAILED",
                                            0,
                                            0,
                                            failed_path.as_deref(),
                                            Some(&url),
                                            Some("validation_or_write"),
                                            Some(&error.to_string()),
                                            attempt_started,
                                        )?
                                    }
                                }
                            }
                            Err(error) => self.record_daily_sync_attempt(
                                &attempt_id,
                                &job_id,
                                symbol,
                                window_start,
                                end,
                                attempt_no,
                                "FAILED",
                                0,
                                0,
                                None,
                                Some(&url),
                                Some("source_format"),
                                Some(&error.to_string()),
                                attempt_started,
                            )?,
                        }
                    }
                    Err(error) => self.record_daily_sync_attempt(
                        &attempt_id,
                        &job_id,
                        symbol,
                        window_start,
                        end,
                        attempt_no,
                        "FAILED",
                        0,
                        0,
                        None,
                        None,
                        Some("network_or_rate_limit"),
                        Some(&error.to_string()),
                        attempt_started,
                    )?,
                }
                if !completed && attempt_no < u32::try_from(prior_attempts)? + max_retries + 1 {
                    std::thread::sleep(std::time::Duration::from_millis(
                        (250_u64.saturating_mul(2_u64.saturating_pow(attempt_no - 1))).min(2_000),
                    ));
                }
            }
            if !completed {
                cursor_error = Some((symbol.clone(), "bounded retry limit exhausted".into()));
                break;
            }
        }
        if let Some((symbol, error)) = cursor_error {
            self.conn.execute(
                "UPDATE ops.daily_sync_job SET status='FAILED',error_class='retry_exhausted',error=?,attempted_count=(SELECT count(*) FROM ops.daily_sync_attempt WHERE job_id=?),finished_at=?,updated_at=? WHERE job_id=?",
                params![format!("{symbol}: {error}"),job_id.to_string(), Utc::now().to_rfc3339(), Utc::now().to_rfc3339(), job_id.to_string()],
            )?;
            bail!("daily sync failed: {symbol}: {error}");
        }
        self.conn.execute(
            "UPDATE ops.daily_sync_job SET status='SUCCESS',source_rows=?,inserted_revisions=?,duplicate_rows=?,empty_responses=?,attempted_count=(SELECT count(*) FROM ops.daily_sync_attempt WHERE job_id=?),finished_at=?,updated_at=? WHERE job_id=?",
            params![i64::try_from(result.source_rows)?, i64::try_from(result.inserted_revisions)?, i64::try_from(result.duplicate_rows)?, i64::try_from(result.empty_responses)?, job_id.to_string(), Utc::now().to_rfc3339(), Utc::now().to_rfc3339(), job_id.to_string()],
        )?;
        Ok(result)
    }

    // These fields map one-for-one to the durable attempt ledger row.
    #[allow(clippy::too_many_arguments)]
    fn record_daily_sync_attempt(
        &self,
        attempt_id: &Uuid,
        job_id: &Uuid,
        symbol: &str,
        start: NaiveDate,
        end: NaiveDate,
        attempt_no: u32,
        status: &str,
        rows: usize,
        inserted: usize,
        raw_path: Option<&str>,
        request_url: Option<&str>,
        error_class: Option<&str>,
        error: Option<&str>,
        started: DateTime<Utc>,
    ) -> Result<()> {
        let raw_hash = raw_path
            .and_then(|path| fs::read(path).ok())
            .map(|bytes| sha256(&bytes));
        self.conn.execute(
            "INSERT INTO ops.daily_sync_attempt VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                attempt_id.to_string(),
                job_id.to_string(),
                symbol,
                start.to_string(),
                end.to_string(),
                i64::from(attempt_no),
                status,
                i64::try_from(rows)?,
                i64::try_from(inserted)?,
                raw_path,
                raw_hash,
                request_url,
                error_class,
                error,
                started.to_rfc3339(),
                Utc::now().to_rfc3339()
            ],
        )?;
        self.conn.execute(
            "UPDATE ops.daily_sync_job SET attempted_count=attempted_count+1,updated_at=? WHERE job_id=?",
            params![Utc::now().to_rfc3339(),job_id.to_string()],
        )?;
        Ok(())
    }

    fn record_not_published_job(
        &self,
        fingerprint: &str,
        symbols: &[String],
        start: NaiveDate,
        end: NaiveDate,
        lookback_days: u32,
        reason: &str,
    ) -> Result<()> {
        let job_id = Uuid::new_v4();
        let now = Utc::now();
        self.conn.execute(
            "INSERT INTO ops.daily_sync_job(job_id,request_fingerprint,dataset,source,symbols_json,range_start,range_end,lookback_days,status,error_class,error,started_at,finished_at,updated_at) VALUES (?,?,'daily_bar','tencent',?,?,?,?,'NOT_PUBLISHED','publication_condition',?,?,?,?)",
            params![job_id.to_string(),fingerprint,serde_json::to_string(symbols)?,start.to_string(),end.to_string(),i64::from(lookback_days),reason,now.to_rfc3339(),now.to_rfc3339(),now.to_rfc3339()],
        )?;
        let window_start = start - chrono::Duration::days(i64::from(lookback_days));
        for symbol in symbols {
            self.conn.execute(
                "INSERT INTO ops.daily_sync_attempt VALUES (?,?,?,?,?,0,'NOT_PUBLISHED',0,0,NULL,NULL,NULL,'publication_condition',?,?,?)",
                params![Uuid::new_v4().to_string(),job_id.to_string(),symbol,window_start.to_string(),end.to_string(),reason,now.to_rfc3339(),now.to_rfc3339()],
            )?;
        }
        Ok(())
    }

    /// Audits and publishes one explicit request as an immutable Parquet snapshot.
    /// A failed audit or interrupted rename never changes `snapshots/current.json`.
    pub fn publish_daily_sync(&mut self, job_id: Uuid) -> Result<DailySnapshotResult> {
        self.publish_daily_sync_with_hook(job_id, || Ok(()))
    }

    fn publish_daily_sync_with_hook<F>(
        &mut self,
        job_id: Uuid,
        mut after_directory_publish: F,
    ) -> Result<DailySnapshotResult>
    where
        F: FnMut() -> Result<()>,
    {
        let (status, symbols_json, start, end, lookback_days): (String, String, String, String, i64) = self.conn.query_row(
            "SELECT status,symbols_json,range_start::VARCHAR,range_end::VARCHAR,lookback_days FROM ops.daily_sync_job WHERE job_id=?",
            params![job_id.to_string()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        )?;
        if status != "SUCCESS" && status != "AUDIT_FAILED" {
            bail!("only a successfully ingested sync job can be published");
        }
        let symbols: Vec<String> = serde_json::from_str(&symbols_json)?;
        let start = NaiveDate::parse_from_str(&start, "%Y-%m-%d")?;
        let end = NaiveDate::parse_from_str(&end, "%Y-%m-%d")?;
        let natural_days = (end - start).num_days() + 1;
        let calendar_days: i64 = self.conn.query_row(
            "SELECT count(*) FROM core.trading_calendar_latest WHERE market='CN' AND source='szse' AND trade_date BETWEEN ? AND ?",
            params![start.to_string(),end.to_string()], |row| row.get(0),
        )?;
        let open_days: Vec<String> = {
            let mut statement = self.conn.prepare("SELECT trade_date::VARCHAR FROM core.trading_calendar_latest WHERE market='CN' AND source='szse' AND is_open AND trade_date BETWEEN ? AND ? ORDER BY trade_date")?;
            statement
                .query_map(params![start.to_string(), end.to_string()], |row| {
                    row.get(0)
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut missing = Vec::new();
        for symbol in &symbols {
            for day in &open_days {
                let exists: bool = self.conn.query_row("SELECT count(*)>0 FROM staging.daily_bar_latest WHERE symbol=? AND trade_date=? AND adjustment='none'", params![symbol,day], |row| row.get(0))?;
                if !exists {
                    missing.push(format!("{symbol}@{day}"));
                }
            }
        }
        let mut observed_outside_calendar = 0_i64;
        let mut unit_anomalies = 0_i64;
        let mut duplicate_source_keys = 0_i64;
        let empty_source_attempts: i64 = self.conn.query_row(
            "SELECT count(*) FROM ops.daily_sync_attempt WHERE job_id=? AND status='EMPTY'",
            params![job_id.to_string()],
            |row| row.get(0),
        )?;
        for symbol in &symbols {
            observed_outside_calendar += self.conn.query_row(
                "SELECT count(*) FROM staging.daily_bar_latest b LEFT JOIN core.trading_calendar_latest c ON c.market='CN' AND c.source='szse' AND c.trade_date=b.trade_date WHERE b.symbol=? AND b.trade_date BETWEEN ? AND ? AND (c.is_open IS NULL OR c.is_open=false)",
                params![symbol,start.to_string(),end.to_string()], |row| row.get::<_,i64>(0),
            )?;
            unit_anomalies += self.conn.query_row(
                "SELECT count(*) FROM staging.daily_bar_latest b WHERE b.symbol=? AND b.trade_date BETWEEN ? AND ? AND (b.open<=0 OR b.high<=0 OR b.low<=0 OR b.close<=0 OR b.low>least(b.open,b.close) OR b.high<greatest(b.open,b.close) OR b.volume_shares<0 OR b.volume_shares<>floor(b.volume_shares) OR (b.amount_cny IS NOT NULL AND b.amount_cny<0))",
                params![symbol,start.to_string(),end.to_string()], |row| row.get::<_,i64>(0),
            )?;
            duplicate_source_keys += self.conn.query_row(
                "SELECT count(*) FROM (SELECT trade_date,source FROM staging.daily_bar_latest WHERE symbol=? AND adjustment='none' AND trade_date BETWEEN ? AND ? GROUP BY trade_date,source HAVING count(*)>1)",
                params![symbol,start.to_string(),end.to_string()], |row| row.get::<_,i64>(0),
            )?;
        }
        let coverage_status = if calendar_days != natural_days
            || !missing.is_empty()
            || observed_outside_calendar > 0
            || unit_anomalies > 0
            || duplicate_source_keys > 0
            || empty_source_attempts > 0
        {
            "gaps"
        } else {
            "complete"
        };
        let (mut unknown_state_days, mut conflicting_state_days) = (0_i64, 0_i64);
        for symbol in &symbols {
            for day in &open_days {
                let (facts,unknown,distinct):(i64,i64,i64)=self.conn.query_row(
                    "SELECT count(*),count(*) FILTER(WHERE trade_status='UNKNOWN'),count(DISTINCT trade_status) FROM core.security_status_latest WHERE symbol=? AND effective_date=?",
                    params![symbol,day],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
                )?;
                if facts == 0 || unknown > 0 {
                    unknown_state_days += 1;
                }
                if distinct > 1 {
                    conflicting_state_days += 1;
                }
            }
        }
        let mut status_coverage_gaps = 0_i64;
        for symbol in &symbols {
            status_coverage_gaps+=self.conn.query_row("SELECT count(*) FROM ops.security_status_coverage WHERE symbol=? AND coverage_start<=? AND coverage_end>=? AND (declared_coverage<>'complete' OR verification_status<>'verified')",params![symbol,end.to_string(),start.to_string()],|row|row.get::<_,i64>(0))?;
        }
        let audit = serde_json::json!({"status":coverage_status,"coverage_basis":"explicit_symbols_x_confirmed_SZSE_open_days_only","requested_start":start.to_string(),"requested_end":end.to_string(),"symbols":symbols,"calendar_days":calendar_days,"natural_days":natural_days,"confirmed_open_days":open_days.len(),"missing_symbol_days":missing,"empty_source_attempts":empty_source_attempts,"bars_on_closed_or_uncovered_dates":observed_outside_calendar,"duplicate_symbol_date_source_keys":duplicate_source_keys,"ohlc_or_volume_unit_anomalies":unit_anomalies,"unknown_state_security_days":unknown_state_days,"conflicting_state_security_days":conflicting_state_days,"unverified_or_gapped_state_coverage_records":status_coverage_gaps,"state_semantics":"status/source preserved; absent status remains UNKNOWN"});
        self.conn.execute(
            "UPDATE ops.daily_sync_job SET audit_json=? WHERE job_id=?",
            params![audit.to_string(), job_id.to_string()],
        )?;
        if coverage_status != "complete" {
            self.conn.execute("UPDATE ops.daily_sync_job SET status='AUDIT_FAILED',error_class='coverage_validation',error=?,updated_at=? WHERE job_id=?",params![audit.to_string(),Utc::now().to_rfc3339(),job_id.to_string()])?;
            bail!(
                "daily snapshot audit did not prove complete coverage: {}",
                audit
            );
        }
        self.conn.execute("UPDATE ops.daily_sync_job SET status='SUCCESS',error=NULL,error_class=NULL,updated_at=? WHERE job_id=?",params![Utc::now().to_rfc3339(),job_id.to_string()])?;
        let snapshot_id = Uuid::new_v4();
        let snapshots_dir = self.data_dir.join("snapshots");
        fs::create_dir_all(&snapshots_dir)?;
        let temp_dir = snapshots_dir.join(format!(".tmp-{}", snapshot_id));
        fs::create_dir(&temp_dir)?;
        let parquet = temp_dir.join("daily.parquet");
        let symbols_sql = symbols
            .iter()
            .map(|s| format!("'{}'", s.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let query = format!("(SELECT b.*,
            coalesce((SELECT CASE WHEN count(DISTINCT s.trade_status)>1 THEN 'CONFLICT' ELSE max(s.trade_status) END FROM core.security_status_latest s WHERE s.symbol=b.symbol AND s.effective_date=b.trade_date), 'UNKNOWN') AS trade_status,
            (SELECT string_agg(DISTINCT s.source, ',') FROM core.security_status_latest s WHERE s.symbol=b.symbol AND s.effective_date=b.trade_date) AS status_source,
            (SELECT string_agg(DISTINCT s.revision_id, ',') FROM core.security_status_latest s WHERE s.symbol=b.symbol AND s.effective_date=b.trade_date) AS status_revision_ids,
            (SELECT string_agg(DISTINCT s.row_hash, ',') FROM core.security_status_latest s WHERE s.symbol=b.symbol AND s.effective_date=b.trade_date) AS status_source_hashes,
            (SELECT string_agg(DISTINCT s.source_ref, '|') FROM core.security_status_latest s WHERE s.symbol=b.symbol AND s.effective_date=b.trade_date) AS status_source_refs,
            (SELECT string_agg(DISTINCT s.raw_sha256, ',') FROM core.security_status_latest s WHERE s.symbol=b.symbol AND s.effective_date=b.trade_date) AS status_raw_sha256,
            (SELECT string_agg(DISTINCT cast(s.published_at AS VARCHAR), '|') FROM core.security_status_latest s WHERE s.symbol=b.symbol AND s.effective_date=b.trade_date) AS status_published_at,
            (SELECT string_agg(DISTINCT cast(s.available_at AS VARCHAR), '|') FROM core.security_status_latest s WHERE s.symbol=b.symbol AND s.effective_date=b.trade_date) AS status_available_at,
            (SELECT string_agg(DISTINCT cast(s.observed_at AS VARCHAR), '|') FROM core.security_status_latest s WHERE s.symbol=b.symbol AND s.effective_date=b.trade_date) AS status_observed_at,
            (SELECT string_agg(DISTINCT s.verification_status, '|') FROM core.security_status_latest s WHERE s.symbol=b.symbol AND s.effective_date=b.trade_date) AS status_verification_status,
            (SELECT string_agg(DISTINCT s.coverage_ref, '|') FROM core.security_status_latest s WHERE s.symbol=b.symbol AND s.effective_date=b.trade_date) AS status_coverage_refs,
            coalesce((SELECT string_agg(DISTINCT c.source || ':' || c.declared_coverage || ':' || c.verification_status || ':' || c.source_ref, '|') FROM ops.security_status_coverage c WHERE c.symbol=b.symbol AND b.trade_date BETWEEN c.coverage_start AND c.coverage_end), 'unverified') AS status_coverage,
            coalesce((SELECT count(*)>0 FROM core.security_status_latest s WHERE s.symbol=b.symbol AND s.effective_date=b.trade_date), false) AS status_covered
            FROM staging.daily_bar_latest b WHERE b.symbol IN ({symbols_sql}) AND b.trade_date BETWEEN DATE '{start}' AND DATE '{end}' AND b.adjustment='none'
            QUALIFY row_number() OVER(PARTITION BY b.symbol,b.trade_date ORDER BY CASE b.source WHEN 'tencent' THEN 1 WHEN 'tdx' THEN 2 ELSE 3 END,b.observed_at DESC,b.revision_id DESC)=1 ORDER BY b.symbol,b.trade_date)");
        self.conn.execute_batch(&format!(
            "COPY {query} TO '{}' (FORMAT PARQUET, COMPRESSION ZSTD)",
            parquet.display().to_string().replace('\'', "''")
        ))?;
        let data = fs::read(&parquet)?;
        let data_hash = sha256(&data);
        let mut directory = Vec::with_capacity(symbols.len());
        for symbol in &symbols {
            let identity: Option<SecurityDirectoryDbRow> = self.conn.query_row(
                "SELECT i.instrument_id,i.market,i.code,i.asset_class,i.first_observed_date::VARCHAR,i.last_observed_date::VARCHAR,coalesce(n.name,''),coalesce(n.source,'') FROM core.instrument i LEFT JOIN LATERAL (SELECT name,source FROM core.instrument_symbol_latest n WHERE n.instrument_id=i.instrument_id ORDER BY n.valid_from DESC,n.observed_at DESC LIMIT 1) n ON true WHERE (CASE i.market WHEN 'SH' THEN 'sh' WHEN 'SZ' THEN 'sz' ELSE 'bj' END || i.code)=?",
                params![symbol],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
            ).optional()?;
            match identity {
                Some((instrument_id,market,code,asset_type,first,last,name,name_source))=>directory.push(serde_json::json!({"instrument_id":instrument_id,"symbol":symbol,"code":code,"market":market,"asset_type":asset_type,"name":if name.is_empty(){None}else{Some(name)},"name_source":if name_source.is_empty(){None}else{Some(name_source)},"first_observed_date":first,"last_observed_date":last,"listed_date":null,"delisted_date":null})),
                None=>directory.push(serde_json::json!({"instrument_id":null,"symbol":symbol,"code":&symbol[2..],"market":&symbol[..2],"asset_type":"UNKNOWN","name":null,"name_source":null,"first_observed_date":null,"last_observed_date":null,"listed_date":null,"delisted_date":null})),
            }
        }
        let directory_bytes = serde_json::to_vec_pretty(&directory)?;
        let directory_hash = sha256(&directory_bytes);
        fs::write(temp_dir.join("securities.json"), &directory_bytes)?;
        let calendar_json: String = self.conn.query_row("SELECT coalesce(string_agg(trade_date::VARCHAR || ':' || is_open::VARCHAR, ',' ORDER BY trade_date),'') FROM core.trading_calendar_latest WHERE market='CN' AND source='szse' AND trade_date BETWEEN ? AND ?",params![start.to_string(),end.to_string()],|row|row.get(0))?;
        let calendar_hash = sha256(calendar_json.as_bytes());
        fs::write(
            temp_dir.join("trading_calendar.txt"),
            calendar_json.as_bytes(),
        )?;
        let attempt_provenance: Vec<serde_json::Value> = {
            let mut statement=self.conn.prepare("SELECT a.symbol,a.window_start::VARCHAR,a.window_end::VARCHAR,a.attempt_no,a.status,a.source_rows,a.inserted_revisions,a.raw_path,a.raw_sha256,a.request_url,a.error_class,a.error,r.run_id FROM ops.daily_sync_attempt a LEFT JOIN ops.ingest_run r ON r.raw_path=a.raw_path WHERE a.job_id=? ORDER BY a.symbol,a.attempt_no")?;
            statement.query_map(params![job_id.to_string()],|row|Ok(serde_json::json!({"symbol":row.get::<_,String>(0)?,"start":row.get::<_,String>(1)?,"end":row.get::<_,String>(2)?,"attempt":row.get::<_,i64>(3)?,"status":row.get::<_,String>(4)?,"source_rows":row.get::<_,i64>(5)?,"inserted_revisions":row.get::<_,i64>(6)?,"raw_path":row.get::<_,Option<String>>(7)?,"raw_sha256":row.get::<_,Option<String>>(8)?,"request_url":row.get::<_,Option<String>>(9)?,"error_class":row.get::<_,Option<String>>(10)?,"error":row.get::<_,Option<String>>(11)?,"source_run_id":row.get::<_,Option<String>>(12)?})))?.collect::<std::result::Result<Vec<_>,_>>()?
        };
        let manifest = serde_json::json!({"schema_version":SCHEMA_VERSION,"snapshot_id":snapshot_id,"job_id":job_id,"created_at":Utc::now().to_rfc3339(),"source":"tencent (Tencent preferred, then TDX for overlapping rows)","source_attempts":attempt_provenance,"symbols":symbols,"revision_lookback_days":lookback_days,"revision_lookback_start":(start-chrono::Duration::days(lookback_days)).to_string(),"requested_start":start.to_string(),"as_of_date":end.to_string(),"data_cutoff_date":end.to_string(),"coverage_status":coverage_status,"price_adjustment":"none","calendar_source":"SZSE CN calendar","calendar_file":"trading_calendar.txt","calendar_sha256":calendar_hash,"catalog_file":"securities.json","catalog_sha256":directory_hash,"data_file":"daily.parquet","data_sha256":data_hash,"rows":self.conn.query_row::<i64,_,_>(&format!("SELECT count(*) FROM read_parquet('{}')",parquet.display().to_string().replace('\'',"''")),[],|row|row.get(0))?,"audit":audit,"status_unknowns_preserved":true});
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        let manifest_hash = sha256(&manifest_bytes);
        fs::write(temp_dir.join("manifest.json"), &manifest_bytes)?;
        // Re-open and hash every payload before publishing its immutable directory.
        if sha256(&fs::read(&parquet)?) != data_hash
            || sha256(&fs::read(temp_dir.join("securities.json"))?) != directory_hash
            || sha256(&fs::read(temp_dir.join("trading_calendar.txt"))?) != calendar_hash
            || sha256(&fs::read(temp_dir.join("manifest.json"))?) != manifest_hash
        {
            bail!("snapshot validation failed before publish");
        }
        let final_dir = snapshots_dir.join(snapshot_id.to_string());
        fs::rename(&temp_dir, &final_dir)?;
        after_directory_publish()?;
        let current_tmp = snapshots_dir.join(format!(".current-{}.json", snapshot_id));
        fs::write(
            &current_tmp,
            serde_json::to_vec_pretty(
                &serde_json::json!({"snapshot_id":snapshot_id,"manifest":format!("{snapshot_id}/manifest.json"),"manifest_sha256":manifest_hash}),
            )?,
        )?;
        fs::rename(&current_tmp, snapshots_dir.join("current.json"))?;
        let manifest_path = final_dir.join("manifest.json");
        self.conn.execute(
            "INSERT INTO ops.daily_snapshot VALUES (?,?,?,?,?,?,?,?)",
            params![
                snapshot_id.to_string(),
                job_id.to_string(),
                end.to_string(),
                end.to_string(),
                coverage_status,
                manifest_path.display().to_string(),
                manifest_hash,
                Utc::now().to_rfc3339()
            ],
        )?;
        self.conn.execute("INSERT INTO ops.daily_snapshot_current VALUES ('daily_bar',?,?,?) ON CONFLICT(dataset) DO UPDATE SET snapshot_id=excluded.snapshot_id,manifest_path=excluded.manifest_path,updated_at=excluded.updated_at",params![snapshot_id.to_string(),manifest_path.display().to_string(),Utc::now().to_rfc3339()])?;
        self.conn.execute("UPDATE ops.daily_sync_job SET status='PUBLISHED',snapshot_id=?,updated_at=? WHERE job_id=?",params![snapshot_id.to_string(),Utc::now().to_rfc3339(),job_id.to_string()])?;
        Ok(DailySnapshotResult {
            snapshot_id,
            manifest_path,
            data_path: final_dir.join("daily.parquet"),
            manifest_sha256: manifest_hash,
            data_sha256: data_hash,
            coverage_status: coverage_status.into(),
            data_cutoff_date: end.to_string(),
        })
    }

    pub fn current_daily_snapshot(&self) -> Result<Option<DailySnapshotResult>> {
        let pointer = self.data_dir.join("snapshots/current.json");
        if !pointer.exists() {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&pointer)?)?;
        let id = value["snapshot_id"]
            .as_str()
            .context("invalid current snapshot pointer")?;
        let manifest_path = self
            .data_dir
            .join("snapshots")
            .join(id)
            .join("manifest.json");
        let bytes = fs::read(&manifest_path)?;
        let expected = value["manifest_sha256"]
            .as_str()
            .context("pointer lacks manifest hash")?;
        if sha256(&bytes) != expected {
            bail!("current snapshot manifest hash mismatch")
        }
        let manifest: serde_json::Value = serde_json::from_slice(&bytes)?;
        let data_path = manifest_path
            .parent()
            .context("manifest has no parent")?
            .join("daily.parquet");
        let data_hash = manifest["data_sha256"]
            .as_str()
            .context("manifest lacks data hash")?
            .to_owned();
        if sha256(&fs::read(&data_path)?) != data_hash {
            bail!("current snapshot data hash mismatch")
        }
        Ok(Some(DailySnapshotResult {
            snapshot_id: Uuid::parse_str(id)?,
            manifest_path,
            data_path,
            manifest_sha256: expected.to_owned(),
            data_sha256: data_hash,
            coverage_status: manifest["coverage_status"]
                .as_str()
                .unwrap_or("unverified")
                .into(),
            data_cutoff_date: manifest["data_cutoff_date"].as_str().unwrap_or("").into(),
        }))
    }

    /// Read the recorded daily-sync and historical backfill state while the
    /// caller holds the warehouse's exclusive writer lock. No network or data
    /// mutation is performed; immutable snapshot files remain the reader path.
    pub fn daily_sync_status_report(&self) -> Result<serde_json::Value> {
        let backfills: Vec<serde_json::Value> = {
            let mut statement = self.conn.prepare(
                "SELECT dataset,source,status,count(*) AS windows,
                        min(window_start)::VARCHAR AS first_window_start,
                        max(window_end)::VARCHAR AS last_window_end,
                        max(updated_at)::VARCHAR AS latest_update
                 FROM ops.history_backfill_window
                 GROUP BY dataset,source,status ORDER BY dataset,source,status",
            )?;
            statement
                .query_map([], |row| {
                    Ok(serde_json::json!({
                        "dataset": row.get::<_, String>(0)?,
                        "source": row.get::<_, String>(1)?,
                        "status": row.get::<_, String>(2)?,
                        "windows": row.get::<_, i64>(3)?,
                        "first_window_start": row.get::<_, Option<String>>(4)?,
                        "last_window_end": row.get::<_, Option<String>>(5)?,
                        "latest_update": row.get::<_, Option<String>>(6)?,
                    }))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let jobs: Vec<serde_json::Value> = {
            let mut statement = self.conn.prepare(
                "SELECT job_id,dataset,source,symbols_json,range_start::VARCHAR,
                        range_end::VARCHAR,lookback_days,status,cursor_symbol,
                        cursor_start::VARCHAR,cursor_end::VARCHAR,attempted_count,
                        source_rows,inserted_revisions,duplicate_rows,empty_responses,
                        snapshot_id,error_class,error,started_at::VARCHAR,
                        finished_at::VARCHAR,updated_at::VARCHAR
                 FROM ops.daily_sync_job ORDER BY started_at DESC LIMIT 50",
            )?;
            statement
                .query_map([], |row| {
                    Ok(serde_json::json!({
                        "job_id": row.get::<_, String>(0)?,
                        "dataset": row.get::<_, String>(1)?,
                        "source": row.get::<_, String>(2)?,
                        "symbols": serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(3)?).unwrap_or(serde_json::Value::Null),
                        "range_start": row.get::<_, String>(4)?,
                        "range_end": row.get::<_, String>(5)?,
                        "lookback_days": row.get::<_, i64>(6)?,
                        "status": row.get::<_, String>(7)?,
                        "cursor_symbol": row.get::<_, Option<String>>(8)?,
                        "cursor_start": row.get::<_, Option<String>>(9)?,
                        "cursor_end": row.get::<_, Option<String>>(10)?,
                        "attempted_count": row.get::<_, i64>(11)?,
                        "source_rows": row.get::<_, i64>(12)?,
                        "inserted_revisions": row.get::<_, i64>(13)?,
                        "duplicate_rows": row.get::<_, i64>(14)?,
                        "empty_responses": row.get::<_, i64>(15)?,
                        "snapshot_id": row.get::<_, Option<String>>(16)?,
                        "error_class": row.get::<_, Option<String>>(17)?,
                        "error": row.get::<_, Option<String>>(18)?,
                        "started_at": row.get::<_, String>(19)?,
                        "finished_at": row.get::<_, Option<String>>(20)?,
                        "updated_at": row.get::<_, String>(21)?,
                    }))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let attempts: Vec<serde_json::Value> = {
            let mut statement = self.conn.prepare(
                "SELECT a.status,count(*) AS attempts,max(a.finished_at)::VARCHAR AS latest_finish
                 FROM ops.daily_sync_attempt a GROUP BY a.status ORDER BY a.status",
            )?;
            statement
                .query_map([], |row| {
                    Ok(serde_json::json!({
                        "status": row.get::<_, String>(0)?,
                        "attempts": row.get::<_, i64>(1)?,
                        "latest_finish": row.get::<_, Option<String>>(2)?,
                    }))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let failed_windows: Vec<serde_json::Value> = {
            let mut statement = self.conn.prepare(
                "SELECT dataset,symbol,window_start::VARCHAR,window_end::VARCHAR,
                        row_count,error,updated_at::VARCHAR
                 FROM ops.history_backfill_window WHERE status='FAILED'
                 ORDER BY updated_at DESC LIMIT 100",
            )?;
            statement
                .query_map([], |row| {
                    Ok(serde_json::json!({
                        "dataset": row.get::<_, String>(0)?,
                        "symbol": row.get::<_, String>(1)?,
                        "window_start": row.get::<_, String>(2)?,
                        "window_end": row.get::<_, String>(3)?,
                        "row_count": row.get::<_, i64>(4)?,
                        "error": row.get::<_, Option<String>>(5)?,
                        "updated_at": row.get::<_, String>(6)?,
                    }))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let sample_identities: Vec<serde_json::Value> = {
            let mut statement = self.conn.prepare(
                "SELECT CASE i.market WHEN 'SH' THEN 'sh' WHEN 'SZ' THEN 'sz' WHEN 'BJ' THEN 'bj' END || i.code AS symbol,
                        i.instrument_id,i.market,i.code,i.asset_class,
                        n.name,n.source,n.valid_from::VARCHAR AS name_valid_from,
                        c.source AS classification_source,c.method AS classification_method,
                        c.effective_date::VARCHAR AS classification_effective_date
                 FROM core.instrument i
                 LEFT JOIN LATERAL (SELECT name,source,valid_from FROM core.instrument_symbol_latest n
                                    WHERE n.instrument_id=i.instrument_id ORDER BY n.valid_from DESC,n.observed_at DESC LIMIT 1) n ON true
                 LEFT JOIN LATERAL (SELECT source,method,effective_date FROM core.instrument_classification_latest c
                                    WHERE c.instrument_id=i.instrument_id ORDER BY c.effective_date DESC LIMIT 1) c ON true
                 WHERE (i.market='SH' AND i.code IN ('600519','510300'))
                 ORDER BY i.code",
            )?;
            statement
                .query_map([], |row| {
                    Ok(serde_json::json!({
                        "symbol": row.get::<_, String>(0)?,
                        "instrument_id": row.get::<_, String>(1)?,
                        "market": row.get::<_, String>(2)?,
                        "code": row.get::<_, String>(3)?,
                        "asset_class": row.get::<_, String>(4)?,
                        "name": row.get::<_, Option<String>>(5)?,
                        "name_source": row.get::<_, Option<String>>(6)?,
                        "name_valid_from": row.get::<_, Option<String>>(7)?,
                        "classification_source": row.get::<_, Option<String>>(8)?,
                        "classification_method": row.get::<_, Option<String>>(9)?,
                        "classification_effective_date": row.get::<_, Option<String>>(10)?,
                    }))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let calendar: Vec<serde_json::Value> = {
            let mut statement = self.conn.prepare(
                "SELECT source,count(*) AS dates,min(trade_date)::VARCHAR AS first_date,
                        max(trade_date)::VARCHAR AS last_date
                 FROM core.trading_calendar_latest WHERE market='CN' GROUP BY source ORDER BY source",
            )?;
            statement
                .query_map([], |row| {
                    Ok(serde_json::json!({
                        "source": row.get::<_, String>(0)?,
                        "dates": row.get::<_, i64>(1)?,
                        "first_date": row.get::<_, Option<String>>(2)?,
                        "last_date": row.get::<_, Option<String>>(3)?,
                    }))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let calendar_tail: Vec<serde_json::Value> = {
            let mut statement = self.conn.prepare(
                "SELECT trade_date::VARCHAR,is_open,observed_at::VARCHAR,run_id
                 FROM core.trading_calendar_latest WHERE market='CN' AND source='szse'
                 ORDER BY trade_date DESC LIMIT 10",
            )?;
            statement
                .query_map([], |row| {
                    Ok(serde_json::json!({
                        "trade_date": row.get::<_, String>(0)?,
                        "is_open": row.get::<_, bool>(1)?,
                        "observed_at": row.get::<_, String>(2)?,
                        "run_id": row.get::<_, String>(3)?,
                    }))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let current = self.current_daily_snapshot()?.map(|snapshot| {
            serde_json::json!({
                "snapshot_id": snapshot.snapshot_id,
                "manifest_path": snapshot.manifest_path,
                "manifest_sha256": snapshot.manifest_sha256,
                "data_sha256": snapshot.data_sha256,
                "coverage_status": snapshot.coverage_status,
                "data_cutoff_date": snapshot.data_cutoff_date,
            })
        });
        Ok(serde_json::json!({
            "backfill_windows": backfills,
            "failed_backfill_windows_recent": failed_windows,
            "calendar_coverage": calendar,
            "calendar_tail": calendar_tail,
            "sample_security_identities": sample_identities,
            "daily_sync_jobs_recent": jobs,
            "daily_sync_attempts_by_status": attempts,
            "current_daily_snapshot": current,
        }))
    }

    pub fn latest_confirmed_daily_sync(
        &mut self,
        symbols: &[String],
        lookback_days: u32,
        retries: u32,
    ) -> Result<DailySyncResult> {
        let today = (Utc::now() + chrono::Duration::hours(8)).date_naive();
        let today_calendar: Option<bool> = self
            .conn
            .query_row(
                "SELECT is_open FROM core.trading_calendar_latest WHERE market='CN' AND source='szse' AND trade_date=?",
                params![today.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if today_calendar.is_none() {
            return self.sync_daily_tencent(symbols, today, today, lookback_days, retries);
        }
        let latest: Option<String>=self.conn.query_row("SELECT max(trade_date)::VARCHAR FROM core.trading_calendar_latest WHERE market='CN' AND source='szse' AND is_open AND trade_date<=?",params![today.to_string()],|row|row.get(0))?;
        let day = latest
            .context("no previously confirmed open day in SZSE calendar; import calendar first")?;
        let day = NaiveDate::parse_from_str(&day, "%Y-%m-%d")?;
        if (today - day).num_days() > 7 {
            bail!(
                "latest confirmed open day {day} is stale; refresh the SZSE calendar before scheduled sync"
            );
        }
        self.sync_daily_tencent(symbols, day, day, lookback_days, retries)
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

    /// Symbols in the currently observed SH/SZ equity and ETF pool. This is not
    /// a point-in-time universe and cannot include securities absent from the
    /// local instrument table (for example, delisted names never observed here).
    pub fn observed_market_symbols(&self) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare(
            "SELECT CASE market WHEN 'SH' THEN 'sh' ELSE 'sz' END || code
             FROM core.instrument
             WHERE market IN ('SH','SZ')
               AND asset_class IN ('EQUITY_CANDIDATE','EQUITY','ETF')
             ORDER BY market, code",
        )?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?)
    }

    pub fn market_backfill_window_done(
        &self,
        symbol: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT count(*)>0 FROM ops.history_backfill_window
             WHERE dataset='observed_market_daily_bar' AND symbol=? AND window_start=? AND window_end=?
               AND source='tencent' AND status IN ('SUCCESS','EMPTY')",
            params![symbol, start.to_string(), end.to_string()],
            |row| row.get(0),
        )?)
    }

    pub fn record_market_backfill_window(
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
        self.conn.execute(
            "INSERT INTO ops.history_backfill_window VALUES ('observed_market_daily_bar', ?, ?, ?, 'tencent', ?, ?, ?, ?)
             ON CONFLICT(dataset,symbol,window_start,window_end,source) DO UPDATE SET
             status=excluded.status,row_count=excluded.row_count,error=excluded.error,updated_at=excluded.updated_at",
            params![symbol, start.to_string(), end.to_string(), status, i64::try_from(rows)?, error, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn audit_market_history_length(
        &self,
        symbol: &str,
        start: NaiveDate,
        end: NaiveDate,
        expected_windows: usize,
    ) -> Result<HistoryLengthAudit> {
        let mut recorded_windows = 0_usize;
        let mut failed_windows = 0_usize;
        let mut source_rows = 0_i64;
        let mut window_start = start;
        while window_start <= end {
            let window_end = std::cmp::min(window_start + chrono::Duration::days(799), end);
            let marker: Option<(String, i64)> = self
                .conn
                .query_row(
                    "SELECT status, row_count FROM ops.history_backfill_window
                     WHERE dataset='observed_market_daily_bar' AND symbol=? AND source='tencent'
                       AND window_start=? AND window_end=?",
                    params![symbol, window_start.to_string(), window_end.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            match marker {
                Some((status, rows)) if status == "SUCCESS" || status == "EMPTY" => {
                    recorded_windows += 1;
                    source_rows += rows;
                }
                Some((status, _)) if status == "FAILED" => failed_windows += 1,
                _ => {}
            }
            window_start = window_end + chrono::Duration::days(1);
        }
        let (stored_days, first_date, last_date) = self.conn.query_row(
            "SELECT count(DISTINCT trade_date), min(trade_date)::VARCHAR, max(trade_date)::VARCHAR
             FROM staging.daily_bar_revision
             WHERE symbol=? AND source='tencent' AND adjustment='none' AND trade_date BETWEEN ? AND ?",
            params![symbol, start.to_string(), end.to_string()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?)),
        )?;
        Ok(HistoryLengthAudit {
            symbol: symbol.to_owned(),
            expected_windows,
            recorded_windows,
            failed_windows,
            source_rows,
            stored_days,
            first_date,
            last_date,
            row_count_matches: source_rows == stored_days,
            coverage_complete: recorded_windows == expected_windows && failed_windows == 0,
        })
    }

    /// Compares source-recorded daily bars with the official SZSE calendar.
    /// Missing calendar dates remain unknown; they are never inferred as closed.
    pub fn audit_trading_calendar(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        symbols: &[String],
    ) -> Result<TradingCalendarAudit> {
        if start > end {
            bail!("start cannot be later than end");
        }
        let mut calendar = BTreeMap::<NaiveDate, bool>::new();
        let mut statement = self.conn.prepare(
            "SELECT trade_date::VARCHAR, is_open FROM core.trading_calendar_latest
             WHERE market='CN' AND source='szse' AND trade_date BETWEEN ? AND ?
             ORDER BY trade_date",
        )?;
        for row in statement.query_map(params![start.to_string(), end.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
        })? {
            let (date, is_open) = row?;
            calendar.insert(NaiveDate::parse_from_str(&date, "%Y-%m-%d")?, is_open);
        }
        let mut missing_calendar_dates = Vec::new();
        let mut date = start;
        loop {
            if !calendar.contains_key(&date) {
                missing_calendar_dates.push(date.to_string());
            }
            if date == end {
                break;
            }
            date = date
                .succ_opt()
                .context("date range exceeds supported calendar")?;
        }
        let calendar_open_dates = calendar
            .iter()
            .filter_map(|(date, is_open)| is_open.then_some(*date))
            .collect::<Vec<_>>();
        let mut audits = Vec::with_capacity(symbols.len());
        for symbol in symbols {
            let mut observed = BTreeMap::<NaiveDate, ()>::new();
            let mut statement = self.conn.prepare(
                "SELECT DISTINCT trade_date::VARCHAR FROM staging.daily_bar_latest
                 WHERE symbol=? AND source='tencent' AND adjustment='none'
                   AND trade_date BETWEEN ? AND ? ORDER BY trade_date",
            )?;
            for row in statement
                .query_map(params![symbol, start.to_string(), end.to_string()], |row| {
                    row.get::<_, String>(0)
                })?
            {
                observed.insert(NaiveDate::parse_from_str(&row?, "%Y-%m-%d")?, ());
            }
            let expected_open_dates = calendar_open_dates
                .iter()
                .filter(|date| calendar.contains_key(date))
                .copied()
                .collect::<Vec<_>>();
            let missing_open_dates = expected_open_dates
                .iter()
                .filter(|date| !observed.contains_key(date))
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            let bars_on_closed_dates = observed
                .keys()
                .filter(|date| calendar.get(date) == Some(&false))
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            let bars_on_uncovered_calendar_dates = observed
                .keys()
                .filter(|date| !calendar.contains_key(date))
                .count();
            let observed_bar_days = observed.len();
            let matches_calendar_on_covered_dates =
                missing_open_dates.is_empty() && bars_on_closed_dates.is_empty();
            audits.push(SymbolCalendarAudit {
                symbol: symbol.clone(),
                observed_bar_days,
                expected_open_days: expected_open_dates.len(),
                bars_on_uncovered_calendar_dates,
                missing_open_dates,
                bars_on_closed_dates,
                matches_calendar_on_covered_dates,
            });
        }
        let natural_days = (end - start).num_days() as usize + 1;
        let calendar_days = calendar.len();
        let open_days = calendar.values().filter(|is_open| **is_open).count();
        let calendar_complete = calendar_days == natural_days;
        Ok(TradingCalendarAudit {
            start: start.to_string(),
            end: end.to_string(),
            natural_days,
            calendar_days,
            open_days,
            missing_calendar_dates,
            calendar_complete,
            symbols: audits,
        })
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
    ) -> Result<PathBuf> {
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
        self.finish_run(run_id, 0, 0)?;
        Ok(path)
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
    /// Stores raw historical index membership source material and parsed facts.
    /// This generic path deliberately cannot certify `verified_pit` or complete
    /// coverage: no trusted historical constituent adapter is currently wired.
    pub fn ingest_index_membership_source(
        &mut self,
        source_name: &str,
        request_url: &str,
        response: &[u8],
        facts: &[IndexMembershipFact],
        coverage: &[IndexMembershipCoverage],
    ) -> Result<IngestResult> {
        if source_name.is_empty()
            || !source_name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
            })
            || request_url.trim().is_empty()
            || response.is_empty()
        {
            bail!("index membership import requires source, reference, and non-empty raw response");
        }
        let snapshot_hash = sha256(response);
        let mut fact_keys = std::collections::BTreeSet::new();
        for fact in facts {
            validate_index_code(&fact.index_code)?;
            if fact.instrument_id.trim().is_empty() || fact.source_ref.trim().is_empty() {
                bail!("membership fact requires instrument_id and source_ref");
            }
            if !matches!(
                fact.publication_time_method.as_str(),
                "source_reported" | "simulated_minus_14_calendar_days" | "unknown"
            ) {
                bail!("invalid membership publication time method");
            }
            if fact
                .effective_to
                .is_some_and(|end| end <= fact.effective_from)
            {
                bail!("membership effective interval must be non-empty and half-open");
            }
            validate_sha256(&fact.source_revision_hash)?;
            let key = (
                fact.index_code.as_str(),
                fact.instrument_id.as_str(),
                fact.effective_from,
                fact.effective_to,
            );
            if !fact_keys.insert(key) {
                bail!("duplicate index membership fact in import");
            }
        }
        let mut coverage_keys = std::collections::BTreeSet::new();
        for item in coverage {
            validate_index_code(&item.index_code)?;
            if item.coverage_end < item.coverage_start
                || item.source_ref.trim().is_empty()
                || !matches!(
                    item.declared_status.as_str(),
                    "complete" | "gaps" | "unverified"
                )
            {
                bail!("invalid index membership coverage record");
            }
            validate_sha256(&item.source_revision_hash)?;
            if !coverage_keys.insert((
                item.index_code.as_str(),
                item.coverage_start,
                item.coverage_end,
                item.source_ref.as_str(),
            )) {
                bail!("duplicate index membership coverage record in import");
            }
        }

        let run_id = self.create_run(source_name, request_url)?;
        let outcome = (|| -> Result<IngestResult> {
            let path = self.archive_raw_source(
                run_id,
                RawArchiveRequest {
                    source: source_name,
                    extension: "bin",
                    response,
                    metadata: serde_json::json!({
                        "adapter_version": "index-membership-generic-v1",
                        "url": request_url,
                        "fact_count": facts.len(),
                        "coverage_count": coverage.len(),
                        "verification": "unknown",
                        "publication_time_methods": facts.iter().map(|fact| fact.publication_time_method.as_str()).collect::<std::collections::BTreeSet<_>>()
                    }),
                },
            )?;
            self.conn.execute(
                "UPDATE ops.ingest_run SET raw_path=?, raw_sha256=? WHERE run_id=?",
                params![
                    path.display().to_string(),
                    snapshot_hash,
                    run_id.to_string()
                ],
            )?;
            let observed_at = Utc::now();
            let transaction = self.conn.transaction()?;
            let mut inserted = 0;
            for fact in facts {
                let hash = sha256(
                    format!(
                        "{}|{}|{}|{}|{}|{}|{}|{}|{}|unknown",
                        fact.index_code,
                        fact.instrument_id,
                        fact.effective_from,
                        fact.effective_to
                            .map_or_else(String::new, |date| date.to_string()),
                        fact.published_at
                            .map_or_else(String::new, |date| date.to_rfc3339()),
                        fact.effective_to_published_at
                            .map_or_else(String::new, |date| date.to_rfc3339()),
                        fact.publication_time_method,
                        fact.source_ref,
                        fact.source_revision_hash
                    )
                    .as_bytes(),
                );
                let existing: Option<String> = transaction
                    .query_row(
                        "SELECT row_hash FROM core.index_membership_latest
                     WHERE index_code=? AND instrument_id=? AND effective_from=? AND source_ref=?",
                        params![
                            fact.index_code,
                            fact.instrument_id,
                            fact.effective_from.to_string(),
                            fact.source_ref
                        ],
                        |row| row.get(0),
                    )
                    .optional()?;
                if existing.as_deref() == Some(hash.as_str()) {
                    continue;
                }
                transaction.execute(
                    "INSERT INTO core.index_membership_revision
                     (revision_id,index_code,instrument_id,effective_from,effective_to,published_at,
                      source_ref,source_snapshot_sha256,source_revision_hash,verification_status,
                      observed_at,run_id,row_hash,effective_to_published_at,publication_time_method)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'unknown', ?, ?, ?, ?, ?)",
                    params![
                        Uuid::new_v4().to_string(),
                        fact.index_code,
                        fact.instrument_id,
                        fact.effective_from.to_string(),
                        fact.effective_to.map(|date| date.to_string()),
                        fact.published_at.map(|date| date.to_rfc3339()),
                        fact.source_ref,
                        snapshot_hash,
                        fact.source_revision_hash,
                        observed_at.to_rfc3339(),
                        run_id.to_string(),
                        hash,
                        fact.effective_to_published_at.map(|date| date.to_rfc3339()),
                        fact.publication_time_method
                    ],
                )?;
                inserted += 1;
            }
            for item in coverage {
                let verified_state = if item.declared_status == "gaps" {
                    "gaps"
                } else {
                    "unverified"
                };
                let hash = sha256(
                    format!(
                        "{}|{}|{}|{}|{}|{}|{}",
                        item.index_code,
                        item.coverage_start,
                        item.coverage_end,
                        item.declared_status,
                        verified_state,
                        item.source_ref,
                        item.source_revision_hash
                    )
                    .as_bytes(),
                );
                let existing: Option<String> = transaction
                    .query_row(
                        "SELECT row_hash FROM ops.index_membership_coverage_latest
                     WHERE index_code=? AND coverage_start=? AND coverage_end=? AND source_ref=?",
                        params![
                            item.index_code,
                            item.coverage_start.to_string(),
                            item.coverage_end.to_string(),
                            item.source_ref
                        ],
                        |row| row.get(0),
                    )
                    .optional()?;
                if existing.as_deref() == Some(hash.as_str()) {
                    continue;
                }
                transaction.execute(
                    "INSERT INTO ops.index_membership_coverage_revision VALUES
                     (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        Uuid::new_v4().to_string(),
                        item.index_code,
                        item.coverage_start.to_string(),
                        item.coverage_end.to_string(),
                        item.declared_status,
                        verified_state,
                        item.gap_detail,
                        item.source_ref,
                        snapshot_hash,
                        item.source_revision_hash,
                        observed_at.to_rfc3339(),
                        run_id.to_string(),
                        hash
                    ],
                )?;
                inserted += 1;
            }
            transaction.commit()?;
            Ok(IngestResult {
                run_id,
                source_rows: facts.len() + coverage.len(),
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

    /// Imports the third-party CSI 300 interval CSV with synthetic announcement
    /// times. Synthetic times are stored for analysis but never certify PIT.
    pub fn import_index_constitution_csv(
        &mut self,
        csv_bytes: &[u8],
        request_url: &str,
    ) -> Result<IngestResult> {
        #[derive(Debug)]
        struct CsvRow {
            symbol: String,
            name: String,
            opt_in: NaiveDate,
            opt_out: Option<NaiveDate>,
        }

        let mut reader = csv::Reader::from_reader(csv_bytes);
        let headers = reader.headers()?.clone();
        let column = |name: &str| {
            headers
                .iter()
                .position(|header| header == name)
                .with_context(|| format!("index constitution CSV lacks {name} column"))
        };
        let (symbol_col, name_col, opt_in_col, opt_out_col) = (
            column("symbol")?,
            column("name")?,
            column("opt-in")?,
            column("opt-out")?,
        );
        let mut rows = Vec::new();
        let mut unresolved_opt_in = Vec::new();
        for (row_index, record) in reader.records().enumerate() {
            let record = record.with_context(|| format!("invalid CSV row {}", row_index + 2))?;
            let symbol = record.get(symbol_col).unwrap_or_default().trim().to_owned();
            let name = record.get(name_col).unwrap_or_default().trim().to_owned();
            let opt_in_raw = record.get(opt_in_col).unwrap_or_default().trim();
            if opt_in_raw.is_empty() {
                if !matches!(symbol.get(..2), Some("SH" | "SZ"))
                    || symbol.len() != 8
                    || !symbol[2..].bytes().all(|byte| byte.is_ascii_digit())
                    || name.is_empty()
                {
                    bail!("invalid CSI 300 history row {}", row_index + 2);
                }
                unresolved_opt_in.push(symbol);
                continue;
            }
            let opt_in = NaiveDate::parse_from_str(opt_in_raw, "%Y-%m-%d")
                .with_context(|| format!("invalid opt-in date at CSV row {}", row_index + 2))?;
            let opt_out_raw = record.get(opt_out_col).unwrap_or_default().trim();
            let opt_out = if opt_out_raw.is_empty() {
                None
            } else {
                Some(
                    NaiveDate::parse_from_str(opt_out_raw, "%Y-%m-%d").with_context(|| {
                        format!("invalid opt-out date at CSV row {}", row_index + 2)
                    })?,
                )
            };
            if !matches!(symbol.get(..2), Some("SH" | "SZ"))
                || symbol.len() != 8
                || !symbol[2..].bytes().all(|byte| byte.is_ascii_digit())
                || name.is_empty()
                || opt_out.is_some_and(|end| end <= opt_in)
            {
                bail!("invalid CSI 300 history row {}", row_index + 2);
            }
            rows.push(CsvRow {
                symbol,
                name,
                opt_in,
                opt_out,
            });
        }
        if rows.is_empty() {
            bail!("index constitution CSV contains no membership rows");
        }

        let source_ref = request_url.to_owned();
        let mut facts = Vec::with_capacity(rows.len());
        let mut coverage_end = NaiveDate::from_ymd_opt(2016, 1, 1).unwrap();
        let import_date = Utc::now().date_naive();
        for row in rows {
            let market = &row.symbol[..2];
            let code = &row.symbol[2..];
            let instrument_id: String = match self
                .conn
                .query_row(
                    "SELECT instrument_id FROM core.instrument WHERE market=? AND code=?",
                    params![market, code],
                    |query| query.get(0),
                )
                .optional()?
            {
                Some(id) => id,
                None => {
                    // The CSV supplies identity but not verified asset classification or
                    // local market-observation history. Register it as unknown, observed now.
                    let id = Uuid::new_v4().to_string();
                    let observed = Utc::now().to_rfc3339();
                    self.conn.execute(
                        "INSERT INTO core.instrument VALUES (?, ?, ?, 'UNKNOWN', ?, ?, ?, ?)",
                        params![
                            id,
                            market,
                            code,
                            import_date.to_string(),
                            import_date.to_string(),
                            observed,
                            observed
                        ],
                    )?;
                    id
                }
            };
            let start_known = (row.opt_in - chrono::Duration::days(14))
                .and_hms_opt(0, 0, 0)
                .context("invalid simulated opt-in publication time")?
                .and_utc();
            let end_known = row.opt_out.map(|date| {
                (date - chrono::Duration::days(14))
                    .and_hms_opt(0, 0, 0)
                    .expect("valid midnight")
                    .and_utc()
            });
            coverage_end = coverage_end.max(row.opt_out.unwrap_or(row.opt_in));
            let row_hash = sha256(
                format!(
                    "{}|{}|{}|{}",
                    row.symbol,
                    row.name,
                    row.opt_in,
                    row.opt_out
                        .map_or_else(String::new, |date| date.to_string())
                )
                .as_bytes(),
            );
            facts.push(IndexMembershipFact {
                index_code: "sh000300".into(),
                instrument_id,
                effective_from: row.opt_in,
                effective_to: row.opt_out,
                published_at: Some(start_known),
                effective_to_published_at: end_known,
                publication_time_method: "simulated_minus_14_calendar_days".into(),
                source_ref: source_ref.clone(),
                source_revision_hash: row_hash,
            });
        }
        let coverage = [IndexMembershipCoverage {
            index_code: "sh000300".into(),
            coverage_start: NaiveDate::from_ymd_opt(2016, 1, 1).unwrap(),
            coverage_end,
            declared_status: "unverified".into(),
            gap_detail: Some(format!(
                "Third-party normalized CSV; historical codes/names are canonicalized and source notice publication timestamps and completeness have not been independently verified. All announcement times are synthetic: effective date minus 14 calendar days at 00:00 UTC. Omitted {} rows with missing opt-in dates: {}.",
                unresolved_opt_in.len(),
                unresolved_opt_in.join(", ")
            )),
            source_ref: source_ref.clone(),
            source_revision_hash: sha256(csv_bytes),
        }];
        self.ingest_index_membership_source(
            "index-constitution",
            request_url,
            csv_bytes,
            &facts,
            &coverage,
        )
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
                for timestamp in [&row.published_at, &row.available_at].into_iter().flatten() {
                    DateTime::parse_from_rfc3339(timestamp)
                        .context("status publication timestamps must be RFC3339 with an offset")?;
                }
                let verification_status = row.verification_status.as_deref().unwrap_or("unknown");
                if !matches!(verification_status, "verified" | "unverified" | "unknown") {
                    bail!("invalid status verification_status");
                }
                let hash = sha256(
                    format!(
                        "{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{}|{:?}|{:?}",
                        row.symbol,
                        date,
                        row.trade_status,
                        row.is_st,
                        row.limit_rule_id,
                        row.source_ref,
                        row.published_at,
                        verification_status,
                        row.available_at,
                        row.coverage_ref
                    )
                    .as_bytes(),
                );
                let existing: Option<String> = transaction.query_row("SELECT row_hash FROM core.security_status_latest WHERE symbol=? AND effective_date=? AND source=?", params![row.symbol,date.to_string(),source_name], |query| query.get(0)).optional()?;
                rows += 1;
                if existing.as_deref() == Some(&hash) {
                    continue;
                }
                transaction.execute("INSERT INTO core.security_status_revision (revision_id,symbol,effective_date,trade_status,is_st,limit_rule_id,source,observed_at,run_id,row_hash,source_ref,raw_path,raw_sha256,published_at,available_at,verification_status,coverage_ref) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)", params![Uuid::new_v4().to_string(),row.symbol,date.to_string(),row.trade_status,row.is_st,row.limit_rule_id,source_name,observed.to_rfc3339(),run_id.to_string(),hash,row.source_ref,path.display().to_string(),sha256(csv_bytes),row.published_at,row.available_at,verification_status,row.coverage_ref])?;
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

    /// Imports a small, explicitly sourced current security directory through
    /// the same exclusive warehouse writer. This is not historical PIT master
    /// data; callers must provide primary-source evidence per row.
    pub fn import_security_directory_csv(
        &mut self,
        csv_bytes: &[u8],
        source_name: &str,
        source_url: &str,
    ) -> Result<IngestResult> {
        if source_name.is_empty()
            || !source_name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
            })
            || source_url.trim().is_empty()
        {
            bail!("source name and source URL are required");
        }
        let run_id = self.create_run(source_name, source_url)?;
        let outcome = (|| -> Result<IngestResult> {
            let path = self.archive_raw_source(
                run_id,
                RawArchiveRequest {
                    source: source_name,
                    extension: "csv",
                    response: csv_bytes,
                    metadata: serde_json::json!({
                        "adapter_version": "security-directory-evidence-csv-v1",
                        "url": source_url,
                        "scope": "explicit_current_security_directory_only",
                    }),
                },
            )?;
            self.conn.execute(
                "UPDATE ops.ingest_run SET raw_path=?,raw_sha256=? WHERE run_id=?",
                params![
                    path.display().to_string(),
                    sha256(csv_bytes),
                    run_id.to_string()
                ],
            )?;
            let mut reader = csv::Reader::from_reader(csv_bytes);
            let headers = reader.headers()?.clone();
            for required in [
                "market",
                "code",
                "asset_class",
                "name",
                "effective_date",
                "source_ref",
            ] {
                if !headers.iter().any(|header| header == required) {
                    bail!("security directory CSV is missing column {required}");
                }
            }
            let mut records = Vec::new();
            for row in reader.deserialize::<SecurityDirectoryCsvRow>() {
                let record = row?;
                if !matches!(record.market.as_str(), "SH" | "SZ" | "BJ")
                    || !matches!(record.asset_class.as_str(), "EQUITY" | "ETF")
                    || !record.code.bytes().all(|byte| byte.is_ascii_digit())
                    || record.code.len() != 6
                    || record.name.trim().is_empty()
                    || record.name.contains('\0')
                    || record.source_ref.trim().is_empty()
                {
                    bail!("security directory row has invalid market/code/class/name/source_ref");
                }
                NaiveDate::parse_from_str(&record.effective_date, "%Y-%m-%d")?;
                let prefix = match record.market.as_str() {
                    "SH" => "sh",
                    "SZ" => "sz",
                    "BJ" => "bj",
                    _ => unreachable!(),
                };
                sources::validate_explicit_symbol(&format!("{prefix}{}", record.code), true)?;
                records.push(record);
            }
            if records.is_empty() {
                bail!("security directory CSV has no evidence rows");
            }
            let transaction = self.conn.transaction()?;
            let observed = Utc::now();
            let mut inserted = 0_usize;
            for record in &records {
                let effective_date = NaiveDate::parse_from_str(&record.effective_date, "%Y-%m-%d")?;
                let instrument_id: Option<String> = transaction
                    .query_row(
                        "SELECT instrument_id FROM core.instrument WHERE market=? AND code=?",
                        params![record.market, record.code],
                        |row| row.get(0),
                    )
                    .optional()?;
                let instrument_id =
                    instrument_id.unwrap_or_else(|| format!("{}:{}", record.market, record.code));
                let date = effective_date.to_string();
                let class_hash = sha256(
                    format!(
                        "{}|{}|{}|{}|{}|{}",
                        instrument_id,
                        record.asset_class,
                        date,
                        record.source_ref,
                        source_name,
                        sha256(csv_bytes)
                    )
                    .as_bytes(),
                );
                let symbol = format!(
                    "{}{}",
                    match record.market.as_str() {
                        "SH" => "sh",
                        "SZ" => "sz",
                        "BJ" => "bj",
                        _ => unreachable!(),
                    },
                    record.code
                );
                let name = record.name.trim();
                let name_hash = sha256(
                    format!(
                        "{}|{}|{}|{}|{}|{}",
                        instrument_id,
                        symbol,
                        name,
                        date,
                        record.source_ref,
                        sha256(csv_bytes)
                    )
                    .as_bytes(),
                );
                transaction.execute(
                    "INSERT INTO core.instrument(instrument_id,market,code,asset_class,first_observed_date,last_observed_date,created_at,updated_at)
                     VALUES (?,?,?,?,?,?,?,?)
                     ON CONFLICT(market,code) DO UPDATE SET asset_class=excluded.asset_class,
                       last_observed_date=greatest(core.instrument.last_observed_date,excluded.last_observed_date),
                       updated_at=excluded.updated_at",
                    params![instrument_id, record.market, record.code, record.asset_class, date, date, observed.to_rfc3339(), observed.to_rfc3339()],
                )?;
                let prior_class: Option<String> = transaction
                    .query_row(
                        "SELECT row_hash FROM core.instrument_classification_latest WHERE instrument_id=? AND source=?",
                        params![instrument_id, source_name],
                        |row| row.get(0),
                    )
                    .optional()?;
                if prior_class.as_deref() != Some(&class_hash) {
                    transaction.execute(
                        "INSERT INTO core.instrument_classification_revision VALUES (?,?,?,?,?,?,?,?,?)",
                        params![Uuid::new_v4().to_string(), instrument_id, record.asset_class, date, source_name, "official-source-directory-evidence-v1", observed.to_rfc3339(), run_id.to_string(), class_hash],
                    )?;
                    inserted += 1;
                }
                let prior_name: Option<String> = transaction
                    .query_row(
                        "SELECT row_hash FROM core.instrument_symbol_latest WHERE instrument_id=? AND symbol=? AND source=? AND valid_from=?",
                        params![instrument_id, symbol, source_name, date],
                        |row| row.get(0),
                    )
                    .optional()?;
                if prior_name.as_deref() != Some(&name_hash) {
                    transaction.execute(
                        "INSERT INTO core.instrument_symbol_revision VALUES (?,?,?,?,?,NULL,?,?,?,?)",
                        params![Uuid::new_v4().to_string(), instrument_id, symbol, name, date, source_name, observed.to_rfc3339(), run_id.to_string(), name_hash],
                    )?;
                    inserted += 1;
                }
            }
            transaction.commit()?;
            Ok(IngestResult {
                run_id,
                source_rows: records.len(),
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

    /// Imports A's source facts and their explicit coverage claims through the
    /// warehouse's single writer. Missing rows remain UNKNOWN; declared coverage
    /// and independent verification are stored separately.
    pub fn import_security_status_evidence(
        &mut self,
        csv_bytes: &[u8],
        source_name: &str,
        source_url: &str,
        coverage: &[StatusCoverageEvidence],
    ) -> Result<IngestResult> {
        if coverage.is_empty() {
            bail!("status evidence import requires explicit coverage records");
        }
        for item in coverage {
            sources::validate_explicit_symbol(&item.symbol, true)?;
            if item.coverage_start > item.coverage_end
                || !matches!(
                    item.declared_coverage.as_str(),
                    "complete" | "gaps" | "unverified"
                )
                || !matches!(item.verification_status.as_str(), "verified" | "unverified")
                || item.source_ref.trim().is_empty()
            {
                bail!("invalid status coverage evidence");
            }
        }
        let result = self.import_security_status_csv(csv_bytes, source_name, source_url)?;
        let raw_path: Option<String> = self
            .conn
            .query_row(
                "SELECT raw_path FROM ops.ingest_run WHERE run_id=?",
                params![result.run_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let raw_hash: Option<String> = self
            .conn
            .query_row(
                "SELECT raw_sha256 FROM ops.ingest_run WHERE run_id=?",
                params![result.run_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let tx = self.conn.transaction()?;
        for item in coverage {
            tx.execute(
                "INSERT INTO ops.security_status_coverage VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    Uuid::new_v4().to_string(),
                    source_name,
                    item.symbol,
                    item.coverage_start.to_string(),
                    item.coverage_end.to_string(),
                    item.declared_coverage,
                    item.verification_status,
                    item.source_ref,
                    raw_path,
                    raw_hash,
                    item.detail,
                    Utc::now().to_rfc3339()
                ],
            )?;
        }
        tx.commit()?;
        Ok(result)
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
    #[serde(default)]
    source_ref: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    available_at: Option<String>,
    #[serde(default)]
    verification_status: Option<String>,
    #[serde(default)]
    coverage_ref: Option<String>,
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

fn validate_index_code(index_code: &str) -> Result<()> {
    if index_code.len() != 8
        || !matches!(index_code.get(..2), Some("sh" | "sz"))
        || !index_code.as_bytes()[2..].iter().all(u8::is_ascii_digit)
    {
        bail!("index code must be an explicit sh/sz prefix plus six digits");
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        bail!("source revision hash must be lowercase SHA-256 hex");
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

    fn symbol_payload(symbol: &str, rows: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"code":0,"data":{symbol:{"day":rows}}})).unwrap()
    }

    fn fixture_bar(symbol: &str, date: NaiveDate, close: &str) -> Vec<u8> {
        symbol_payload(
            symbol,
            serde_json::json!([[date.to_string(), "10", "11", "12", "9", close, "100"]]),
        )
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
    fn index_membership_import_preserves_provenance_and_downgrades_unverified_claims() {
        let temporary = TempDir::new().unwrap();
        let mut warehouse = Warehouse::open(temporary.path()).unwrap();
        warehouse
            .conn
            .execute_batch(include_str!("../sql/migrations/005_index_membership.sql"))
            .unwrap();
        let raw = b"official fixture attachment bytes";
        let day = NaiveDate::from_ymd_opt(2025, 5, 30).unwrap();
        let fact = IndexMembershipFact {
            index_code: "sh000300".into(),
            instrument_id: "SH:600000".into(),
            effective_from: day,
            effective_to: None,
            published_at: None,
            effective_to_published_at: None,
            publication_time_method: "unknown".into(),
            source_ref: "https://example.invalid/adjustment.pdf".into(),
            source_revision_hash: "a".repeat(64),
        };
        let coverage = IndexMembershipCoverage {
            index_code: "sh000300".into(),
            coverage_start: day,
            coverage_end: day,
            declared_status: "complete".into(),
            gap_detail: None,
            source_ref: fact.source_ref.clone(),
            source_revision_hash: fact.source_revision_hash.clone(),
        };
        let imported = warehouse
            .ingest_index_membership_source(
                "csindex",
                "https://example.invalid/adjustment.pdf",
                raw,
                std::slice::from_ref(&fact),
                std::slice::from_ref(&coverage),
            )
            .unwrap();
        assert_eq!(imported.new_revisions, 2);
        let snapshot_hash = sha256(raw);
        let stored: (String, String, String) = warehouse
            .conn
            .query_row(
                "SELECT source_snapshot_sha256, verification_status, published_at
             FROM core.index_membership_latest WHERE index_code='sh000300'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2).unwrap_or_default())),
            )
            .unwrap();
        assert_eq!(stored.0, snapshot_hash);
        assert_eq!(stored.1, "unknown");
        assert!(stored.2.is_empty());
        let coverage_status: (String, String) = warehouse
            .conn
            .query_row(
                "SELECT declared_status, coverage_status FROM ops.index_membership_coverage_latest",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(coverage_status, ("complete".into(), "unverified".into()));
        let repeated = warehouse
            .ingest_index_membership_source(
                "csindex",
                "https://example.invalid/adjustment.pdf",
                raw,
                &[fact],
                &[coverage],
            )
            .unwrap();
        assert_eq!(repeated.new_revisions, 0);
        assert_eq!(
            warehouse
                .conn
                .query_row::<i64, _, _>(
                    "SELECT count(*) FROM core.index_membership_revision",
                    [],
                    |row| row.get(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            warehouse
                .conn
                .query_row::<i64, _, _>("SELECT max(version) FROM ops.schema_version", [], |row| {
                    row.get(0)
                })
                .unwrap(),
            7
        );
    }

    #[test]
    fn daily_sync_is_idempotent_tracks_revisions_and_publishes_immutable_snapshot() {
        let temporary = TempDir::new().unwrap();
        let mut warehouse = Warehouse::open(temporary.path()).unwrap();
        let symbol = "sh600519".to_owned();
        let day = NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
        let first = fixture_bar(&symbol, day, "10.5");
        let mut fetched = first.clone();
        let synced = warehouse
            .sync_daily_tencent_with_fetch(
                std::slice::from_ref(&symbol),
                day,
                day,
                0,
                1,
                |_, _, _| Ok(("fixture://tencent".into(), fetched.clone())),
            )
            .unwrap();
        assert_eq!(synced.inserted_revisions, 1);
        assert_eq!(synced.attempts, 1);
        let calendar = [sources::CalendarDay {
            trade_date: day,
            is_open: true,
        }];
        warehouse
            .ingest_calendar_response(2025, 1, "fixture://calendar", b"calendar", &calendar)
            .unwrap();
        let published = warehouse.publish_daily_sync(synced.job_id).unwrap();
        assert_eq!(published.coverage_status, "complete");
        let old_id = published.snapshot_id;
        let empty = warehouse
            .sync_daily_tencent_with_fetch(
                std::slice::from_ref(&symbol),
                day,
                day,
                0,
                0,
                |symbol, _, _| {
                    Ok((
                        "fixture://empty-after-prior-bar".into(),
                        serde_json::to_vec(
                            &serde_json::json!({"code":0,"data":{symbol:{"day":[]}}}),
                        )
                        .unwrap(),
                    ))
                },
            )
            .unwrap();
        assert_eq!(empty.empty_responses, 1);
        assert!(warehouse.publish_daily_sync(empty.job_id).is_err());
        assert_eq!(
            warehouse
                .current_daily_snapshot()
                .unwrap()
                .unwrap()
                .snapshot_id,
            old_id
        );
        let empty_audit: String = warehouse
            .conn
            .query_row(
                "SELECT audit_json FROM ops.daily_sync_job WHERE job_id=?",
                params![empty.job_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&empty_audit).unwrap()["empty_source_attempts"],
            1
        );
        let snapshot_rows: i64 = warehouse
            .conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM read_parquet('{}')",
                    published.data_path.display()
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(snapshot_rows, 1);
        let unknown_status: String = warehouse
            .conn
            .query_row(
                &format!(
                    "SELECT trade_status FROM read_parquet('{}')",
                    published.data_path.display()
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unknown_status, "UNKNOWN");

        let repeat = warehouse
            .sync_daily_tencent_with_fetch(
                std::slice::from_ref(&symbol),
                day,
                day,
                0,
                1,
                |_, _, _| Ok(("fixture://tencent".into(), first.clone())),
            )
            .unwrap();
        assert_eq!(repeat.inserted_revisions, 0);
        assert_eq!(repeat.duplicate_rows, 1);
        let repeat_snapshot = warehouse.publish_daily_sync(repeat.job_id).unwrap();
        assert_eq!(repeat_snapshot.data_sha256, published.data_sha256);
        fetched = fixture_bar(&symbol, day, "10.75");
        let revised = warehouse
            .sync_daily_tencent_with_fetch(
                std::slice::from_ref(&symbol),
                day,
                day,
                0,
                1,
                |_, _, _| Ok(("fixture://tencent".into(), fetched.clone())),
            )
            .unwrap();
        assert_eq!(revised.inserted_revisions, 1);
        assert_eq!(
            warehouse
                .conn
                .query_row::<i64, _, _>(
                    "SELECT count(*) FROM staging.daily_bar_revision WHERE symbol=?",
                    params![symbol],
                    |row| row.get(0)
                )
                .unwrap(),
            2
        );
        let new_snapshot = warehouse.publish_daily_sync(revised.job_id).unwrap();
        assert_ne!(old_id, new_snapshot.snapshot_id);
        assert!(published.data_path.exists());
        assert_eq!(
            warehouse
                .current_daily_snapshot()
                .unwrap()
                .unwrap()
                .snapshot_id,
            new_snapshot.snapshot_id
        );
        assert_eq!(
            resolve_published_daily_snapshot(temporary.path(), Some(old_id))
                .unwrap()
                .snapshot_id,
            old_id
        );
        assert_eq!(
            resolve_published_daily_snapshot(temporary.path(), None)
                .unwrap()
                .snapshot_id,
            new_snapshot.snapshot_id
        );
        let next_day = day.succ_opt().unwrap();
        let interrupted = warehouse
            .sync_daily_tencent_with_fetch(
                std::slice::from_ref(&symbol),
                next_day,
                next_day,
                0,
                0,
                |_, _, _| {
                    Ok((
                        "fixture://next-day".into(),
                        fixture_bar(&symbol, next_day, "10.8"),
                    ))
                },
            )
            .unwrap();
        warehouse
            .ingest_calendar_response(
                2025,
                1,
                "fixture://calendar-next-day",
                b"calendar-next",
                &[sources::CalendarDay {
                    trade_date: next_day,
                    is_open: true,
                }],
            )
            .unwrap();
        assert!(
            warehouse
                .publish_daily_sync_with_hook(interrupted.job_id, || {
                    bail!("simulated interruption after immutable directory rename")
                })
                .is_err()
        );
        assert_eq!(
            resolve_published_daily_snapshot(temporary.path(), None)
                .unwrap()
                .snapshot_id,
            new_snapshot.snapshot_id
        );

        let missing_day = next_day.succ_opt().unwrap();
        let incomplete = warehouse
            .sync_daily_tencent_with_fetch(
                std::slice::from_ref(&symbol),
                missing_day,
                missing_day,
                0,
                0,
                |_, _, _| {
                    Ok((
                        "fixture://missing-calendar".into(),
                        fixture_bar(&symbol, missing_day, "10.9"),
                    ))
                },
            )
            .unwrap();
        assert!(warehouse.publish_daily_sync(incomplete.job_id).is_err());
        assert_eq!(
            warehouse
                .conn
                .query_row::<String, _, _>(
                    "SELECT status FROM ops.daily_sync_job WHERE job_id=?",
                    params![incomplete.job_id.to_string()],
                    |row| row.get(0)
                )
                .unwrap(),
            "AUDIT_FAILED"
        );
        assert_eq!(
            warehouse
                .current_daily_snapshot()
                .unwrap()
                .unwrap()
                .snapshot_id,
            new_snapshot.snapshot_id
        );
        warehouse
            .ingest_calendar_response(
                2025,
                1,
                "fixture://calendar-missing-day",
                b"calendar-repair",
                &[sources::CalendarDay {
                    trade_date: missing_day,
                    is_open: true,
                }],
            )
            .unwrap();
        let retried_publish = warehouse.publish_daily_sync(incomplete.job_id).unwrap();
        assert_eq!(retried_publish.coverage_status, "complete");
    }

    #[test]
    fn daily_sync_retries_errors_resumes_interrupted_jobs_and_keeps_empty_unknown() {
        let temporary = TempDir::new().unwrap();
        let mut warehouse = Warehouse::open(temporary.path()).unwrap();
        let symbol = "sh600519".to_owned();
        let day = NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
        let body = fixture_bar(&symbol, day, "10.5");
        let mut calls = 0;
        let result = warehouse
            .sync_daily_tencent_with_fetch(
                std::slice::from_ref(&symbol),
                day,
                day,
                0,
                2,
                |_, _, _| {
                    calls += 1;
                    if calls == 1 {
                        bail!("simulated timeout")
                    }
                    Ok(("fixture://retry".into(), body.clone()))
                },
            )
            .unwrap();
        assert_eq!(result.attempts, 2);
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = warehouse.sync_daily_tencent_with_fetch(
                &["sh600001".into()],
                day,
                day,
                0,
                0,
                |_, _, _| panic!("simulated interruption"),
            );
        }));
        assert!(interrupted.is_err());
        let running_job: String = warehouse
            .conn
            .query_row(
                "SELECT job_id FROM ops.daily_sync_job WHERE status='RUNNING'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let resumed = warehouse
            .sync_daily_tencent_with_fetch(&["sh600001".into()], day, day, 0, 0, |_, _, _| {
                Ok((
                    "fixture://resume".into(),
                    symbol_payload(
                        "sh600001",
                        serde_json::json!([[day.to_string(), "10", "11", "12", "9", "10", "100"]]),
                    ),
                ))
            })
            .unwrap();
        assert_eq!(resumed.job_id.to_string(), running_job);
        assert_eq!(
            warehouse
                .conn
                .query_row::<String, _, _>(
                    "SELECT status FROM ops.daily_sync_job WHERE job_id=?",
                    params![resumed.job_id.to_string()],
                    |row| row.get(0)
                )
                .unwrap(),
            "SUCCESS"
        );

        let empty_symbol = "sz159919".to_owned();
        let empty = warehouse
            .sync_daily_tencent_with_fetch(&[empty_symbol], day, day, 0, 0, |symbol, _, _| {
                Ok((
                    "fixture://empty".into(),
                    serde_json::to_vec(&serde_json::json!({"code":0,"data":{symbol:{"day":[]}}}))
                        .unwrap(),
                ))
            })
            .unwrap();
        assert_eq!(empty.empty_responses, 1);
        assert!(warehouse.publish_daily_sync(empty.job_id).is_err());
        assert_eq!(
            warehouse
                .conn
                .query_row::<String, _, _>(
                    "SELECT status FROM ops.daily_sync_attempt WHERE job_id=?",
                    params![empty.job_id.to_string()],
                    |row| row.get(0)
                )
                .unwrap(),
            "EMPTY"
        );
    }

    #[test]
    fn daily_sync_excludes_current_shanghai_day_and_writer_lock_is_exclusive() {
        let temporary = TempDir::new().unwrap();
        let mut first = Warehouse::open(temporary.path()).unwrap();
        assert!(Warehouse::open(temporary.path()).is_err());
        let today = (Utc::now() + chrono::Duration::hours(8)).date_naive();
        assert!(
            first
                .sync_daily_tencent_with_fetch(
                    &["sh600519".into()],
                    today,
                    today,
                    0,
                    0,
                    |_, _, _| unreachable!()
                )
                .is_err()
        );
        assert_eq!(
            first
                .conn
                .query_row::<String, _, _>("SELECT status FROM ops.daily_sync_job", [], |row| row
                    .get(0))
                .unwrap(),
            "NOT_PUBLISHED"
        );
        assert!(
            first
                .latest_confirmed_daily_sync(&["sh600001".into()], 0, 0)
                .is_err()
        );
        assert_eq!(
            first
                .conn
                .query_row::<i64, _, _>(
                    "SELECT count(*) FROM ops.daily_sync_job WHERE status='NOT_PUBLISHED'",
                    [],
                    |row| row.get(0)
                )
                .unwrap(),
            2
        );
        assert_eq!(
            first
                .conn
                .query_row::<String, _, _>("SELECT status FROM ops.daily_sync_attempt", [], |row| {
                    row.get(0)
                })
                .unwrap(),
            "NOT_PUBLISHED"
        );
    }

    #[test]
    fn status_coverage_claim_and_unknown_semantics_are_frozen_without_promotion() {
        let temporary = TempDir::new().unwrap();
        let mut warehouse = Warehouse::open(temporary.path()).unwrap();
        let symbol = "sh600519".to_owned();
        let day = NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
        let body = fixture_bar(&symbol, day, "10.5");
        let sync = warehouse
            .sync_daily_tencent_with_fetch(
                std::slice::from_ref(&symbol),
                day,
                day,
                0,
                0,
                |_, _, _| Ok(("fixture://bar".into(), body.clone())),
            )
            .unwrap();
        let status_csv=b"symbol,effective_date,trade_status,is_st,limit_rule_id,source_ref,published_at,available_at,verification_status,coverage_ref\nsh600519,2025-01-02,TRADABLE,false,,fixture://status-fact,2025-01-01T09:00:00+08:00,2025-01-01T09:05:00+08:00,unverified,fixture://coverage\n";
        warehouse
            .import_security_status_evidence(
                status_csv,
                "fixture-status",
                "fixture://status",
                &[StatusCoverageEvidence {
                    symbol: symbol.clone(),
                    coverage_start: day,
                    coverage_end: day,
                    declared_coverage: "gaps".into(),
                    verification_status: "unverified".into(),
                    source_ref: "fixture://coverage".into(),
                    detail: Some("fixture has only one confirmed row".into()),
                }],
            )
            .unwrap();
        warehouse
            .ingest_calendar_response(
                2025,
                1,
                "fixture://calendar",
                b"calendar",
                &[sources::CalendarDay {
                    trade_date: day,
                    is_open: true,
                }],
            )
            .unwrap();
        let published = warehouse.publish_daily_sync(sync.job_id).unwrap();
        let row:(String,String,bool,String,String,String,String,String,String,String) = warehouse.conn.query_row(&format!("SELECT trade_status,status_source,status_covered,status_coverage,status_source_hashes,status_source_refs,status_available_at,status_published_at,status_verification_status,status_coverage_refs FROM read_parquet('{}')",published.data_path.display()),[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?))).unwrap();
        assert_eq!(row.0, "TRADABLE");
        assert_eq!(row.1, "fixture-status");
        assert!(row.2);
        assert!(row.3.contains("gaps:unverified"));
        assert_eq!(row.4.len(), 64);
        assert_eq!(row.5, "fixture://status-fact");
        assert!(row.6.contains("2025-01-01 01:05:00"));
        assert!(row.7.contains("2025-01-01 01:00:00"));
        assert_eq!(row.8, "unverified");
        assert_eq!(row.9, "fixture://coverage");
    }

    #[test]
    fn index_constitution_csv_marks_both_simulated_announcement_times() {
        let temporary = TempDir::new().unwrap();
        let mut warehouse = Warehouse::open(temporary.path()).unwrap();
        let csv = b"symbol,name,opt-in,opt-out\nSH600000,\xe6\xb5\xa6\xe5\x8f\x91\xe9\x93\xb6\xe8\xa1\x8c,2020-01-20,2020-06-15\nSH600001,missing,,2020-06-15\n";
        let imported = warehouse
            .import_index_constitution_csv(
                csv,
                "https://raw.githubusercontent.com/unliftedq/index-constitution/main/history/csi300.csv",
            )
            .unwrap();
        assert_eq!(imported.source_rows, 2);
        let stored: (String, String, String, String, String) = warehouse
            .conn
            .query_row(
                "SELECT CAST(published_at AS VARCHAR), CAST(effective_to_published_at AS VARCHAR),
                        publication_time_method, verification_status, CAST(effective_to AS VARCHAR)
                 FROM core.index_membership_latest WHERE index_code='sh000300'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert!(stored.0.starts_with("2020-01-06 00:00:00"));
        assert!(stored.1.starts_with("2020-06-01 00:00:00"));
        assert_eq!(stored.2, "simulated_minus_14_calendar_days");
        assert_eq!(stored.3, "unknown");
        assert_eq!(stored.4, "2020-06-15");
        let (coverage_status, gap_detail): (String, String) = warehouse
            .conn
            .query_row(
                "SELECT coverage_status, gap_detail FROM ops.index_membership_coverage_latest",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(coverage_status, "unverified");
        assert!(gap_detail.contains("SH600001"));
        let repeated = warehouse
            .import_index_constitution_csv(
                csv,
                "https://raw.githubusercontent.com/unliftedq/index-constitution/main/history/csi300.csv",
            )
            .unwrap();
        assert_eq!(repeated.new_revisions, 0);
    }

    #[test]
    fn rejects_malformed_index_provenance_and_membership_intervals() {
        let temporary = TempDir::new().unwrap();
        let mut warehouse = Warehouse::open(temporary.path()).unwrap();
        let fact = IndexMembershipFact {
            index_code: "sh000300".into(),
            instrument_id: "SH:600000".into(),
            effective_from: NaiveDate::from_ymd_opt(2025, 5, 30).unwrap(),
            effective_to: Some(NaiveDate::from_ymd_opt(2025, 5, 30).unwrap()),
            published_at: None,
            effective_to_published_at: None,
            publication_time_method: "unknown".into(),
            source_ref: "fixture://source".into(),
            source_revision_hash: "not-a-sha".into(),
        };
        assert!(
            warehouse
                .ingest_index_membership_source("csindex", "fixture://source", b"x", &[fact], &[])
                .is_err()
        );
        assert!(
            warehouse
                .ingest_index_membership_source("../escape", "fixture://source", b"x", &[], &[])
                .is_err()
        );
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
    fn status_uses_shared_lock_and_read_only_database_access() {
        let temporary = TempDir::new().unwrap();
        {
            let _writer = Warehouse::open(temporary.path()).unwrap();
            assert!(Warehouse::open_readonly_status(temporary.path()).is_err());
        }

        let status_reader = Warehouse::open_readonly_status(temporary.path()).unwrap();
        assert!(Warehouse::open(temporary.path()).is_err());
        let report = status_reader.daily_sync_status_report().unwrap();
        assert!(report["current_daily_snapshot"].is_null());
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

        // Synthetic status history exercises the execution view's explicit states and
        // its conservative behavior when no status observation exists. This validates
        // the warehouse view only; the ETF backtest currently consumes plain Bar rows
        // and does not use daily_bar_execution as an execution gate.
        let halted_date = NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        let unknown_date = NaiveDate::from_ymd_opt(2026, 9, 22).unwrap();
        let missing_status_date = NaiveDate::from_ymd_opt(2026, 9, 23).unwrap();
        for bar_date in [halted_date, unknown_date, missing_status_date] {
            let raw = payload(serde_json::json!([[
                bar_date.to_string(),
                "10",
                "11",
                "12",
                "9",
                "123.45"
            ]]));
            warehouse
                .ingest_tencent_day_response(
                    "sh600519",
                    bar_date,
                    bar_date,
                    &raw,
                    "fixture://tencent-status",
                )
                .unwrap();
        }
        let status_history = b"symbol,effective_date,trade_status,is_st,limit_rule_id\nsh600519,2026-09-18,TRADABLE,false,mainboard\nsh600519,2026-09-21,HALTED,false,mainboard\nsh600519,2026-09-22,UNKNOWN,,mainboard\n";
        warehouse
            .import_security_status_csv(status_history, "fixture", "fixture://status-history")
            .unwrap();

        let execution_row = |effective_date: &str| {
            warehouse
                .conn
                .query_row(
                    "SELECT trade_status, is_tradable FROM research.daily_bar_execution WHERE symbol='sh600519' AND trade_date=?",
                    [effective_date],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, bool>(1)?)),
                )
                .unwrap()
        };
        assert_eq!(execution_row("2026-09-18"), (Some("TRADABLE".into()), true));
        assert_eq!(execution_row("2026-09-21"), (Some("HALTED".into()), false));
        assert_eq!(execution_row("2026-09-22"), (Some("UNKNOWN".into()), false));
        assert_eq!(execution_row("2026-09-23"), (None, false));
    }

    #[test]
    fn trading_calendar_audit_keeps_uncovered_dates_unknown_and_finds_bar_gaps() {
        let temporary = TempDir::new().unwrap();
        let mut warehouse = Warehouse::open(temporary.path()).unwrap();
        let dates = [
            ("2026-01-05", false),
            ("2026-01-06", true),
            ("2026-01-07", true),
            ("2026-01-08", false),
        ];
        let calendar = dates
            .iter()
            .map(|(date, is_open)| sources::CalendarDay {
                trade_date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
                is_open: *is_open,
            })
            .collect::<Vec<_>>();
        warehouse
            .ingest_calendar_response(2026, 1, "fixture://szse", b"{}", &calendar)
            .unwrap();
        let rows = serde_json::json!([
            ["2026-01-06", "10", "11", "12", "9", "100"],
            ["2026-01-08", "10", "11", "12", "9", "100"]
        ]);
        warehouse
            .ingest_tencent_day_response(
                "sh600519",
                NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                NaiveDate::from_ymd_opt(2026, 1, 9).unwrap(),
                &payload(rows),
                "fixture://tencent",
            )
            .unwrap();
        let audit = warehouse
            .audit_trading_calendar(
                NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                NaiveDate::from_ymd_opt(2026, 1, 9).unwrap(),
                &["sh600519".into()],
            )
            .unwrap();
        assert!(!audit.calendar_complete);
        assert_eq!(audit.missing_calendar_dates, vec!["2026-01-09"]);
        assert_eq!(audit.open_days, 2);
        assert_eq!(audit.symbols[0].bars_on_uncovered_calendar_dates, 0);
        assert_eq!(audit.symbols[0].missing_open_dates, vec!["2026-01-07"]);
        assert_eq!(audit.symbols[0].bars_on_closed_dates, vec!["2026-01-08"]);
        assert!(!audit.symbols[0].matches_calendar_on_covered_dates);
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

    #[test]
    fn official_security_directory_import_is_idempotent_and_source_bearing() {
        let temporary = TempDir::new().unwrap();
        let mut warehouse = Warehouse::open(temporary.path()).unwrap();
        let bytes = "market,code,asset_class,name,effective_date,source_ref\nSH,600519,EQUITY,贵州茅台,2026-09-26,https://example.test/stock\nSH,510300,ETF,沪深300ETF华泰柏瑞,2026-09-26,https://example.test/etf\n";
        let first = warehouse
            .import_security_directory_csv(
                bytes.as_bytes(),
                "sse-official",
                "https://example.test/combined-directory",
            )
            .unwrap();
        let repeated = warehouse
            .import_security_directory_csv(
                bytes.as_bytes(),
                "sse-official",
                "https://example.test/combined-directory",
            )
            .unwrap();
        assert_eq!(first.source_rows, 2);
        assert_eq!(first.new_revisions, 4);
        assert_eq!(repeated.new_revisions, 0);
        let report = warehouse.daily_sync_status_report().unwrap();
        let identities = report["sample_security_identities"].as_array().unwrap();
        assert_eq!(identities.len(), 2);
        assert_eq!(identities[0]["name_source"], "sse-official");
        assert_eq!(identities[0]["asset_class"], "ETF");
        assert_eq!(identities[1]["name_source"], "sse-official");
        assert_eq!(identities[1]["asset_class"], "EQUITY");
    }
}
