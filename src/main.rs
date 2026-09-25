use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ashare_warehouse::{StatusCoverageEvidence, Warehouse, sources};
use chrono::{Duration as ChronoDuration, NaiveDate};
use clap::{Parser, Subcommand};

const MARKET_HISTORY_WINDOW_DAYS: i64 = 800;

#[derive(Parser)]
#[command(about = "A versioned local A-share research warehouse")]
struct Cli {
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init,
    Status,
    Export,
    /// Exports ETF bars for one date to an immutable ZSTD Parquet snapshot.
    ExportEtfDay {
        #[arg(long)]
        trade_date: NaiveDate,
    },
    /// Backfills raw daily bars for the ETF universe observed in the instrument table.
    BackfillEtfHistory {
        #[arg(long)]
        start: NaiveDate,
        #[arg(long)]
        end: NaiveDate,
        /// Restrict the run to one explicit-market ETF symbol.
        #[arg(long)]
        symbol: Option<String>,
        /// Limit the number of ETF symbols for a controlled batch; zero means all.
        #[arg(long, default_value_t = 0)]
        limit: usize,
        #[arg(long, default_value_t = 250)]
        sleep_ms: u64,
        /// Suppress human-readable progress on stderr; final JSON is still printed.
        #[arg(long, default_value_t = false)]
        quiet: bool,
    },
    /// Backfills raw daily bars for the SH/SZ equity and ETF instruments currently observed locally.
    BackfillMarketHistory {
        #[arg(long)]
        start: NaiveDate,
        #[arg(long)]
        end: NaiveDate,
        /// Limit the number of symbols for a controlled batch; zero means all.
        #[arg(long, default_value_t = 0)]
        limit: usize,
        #[arg(long, default_value_t = 250)]
        sleep_ms: u64,
        /// Suppress human-readable progress on stderr; final JSON is still printed.
        #[arg(long, default_value_t = false)]
        quiet: bool,
    },
    /// Compares source-recorded window lengths with distinct Tencent dates in storage.
    AuditMarketHistory {
        #[arg(long, default_value = "2016-01-01")]
        start: NaiveDate,
        #[arg(long, default_value = "2026-09-24")]
        end: NaiveDate,
        /// Repeat for each symbol; defaults to a small SH/SZ stock and ETF sample.
        #[arg(long)]
        symbol: Vec<String>,
    },
    /// Downloads and imports an official SZSE monthly trading calendar.
    FetchCalendar {
        #[arg(long)]
        year: i32,
        #[arg(long)]
        month: u32,
    },
    /// Compares Tencent daily bars with the saved official SZSE calendar.
    AuditTradingCalendar {
        #[arg(long, default_value = "2016-01-01")]
        start: NaiveDate,
        #[arg(long, default_value = "2026-09-24")]
        end: NaiveDate,
        /// Repeat for each symbol; defaults to a small SH/SZ stock and ETF sample.
        #[arg(long)]
        symbol: Vec<String>,
    },
    /// Downloads one TDX official post-close ZIP package and stages all priced records.
    FetchTdxDay {
        #[arg(long)]
        trade_date: NaiveDate,
    },
    /// Parses a previously downloaded official TDX ZIP after a transient source outage.
    ImportTdxZip {
        #[arg(long)]
        trade_date: NaiveDate,
        #[arg(long)]
        input: PathBuf,
    },
    /// Downloads a full Sina adjustment-factor series for one explicit-market symbol.
    FetchAdjustments {
        #[arg(long)]
        symbol: String,
        #[arg(long, default_value = "qfq")]
        kind: String,
    },
    /// Imports source-declared status history. Required columns are documented in docs.
    ImportStatusCsv {
        #[arg(long)]
        source: String,
        #[arg(long)]
        source_url: String,
        #[arg(long)]
        input: PathBuf,
    },
    /// Imports A's status facts plus explicit per-security coverage evidence.
    ImportStatusEvidence {
        #[arg(long)]
        source: String,
        #[arg(long)]
        source_url: String,
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        coverage: PathBuf,
    },
    /// Imports the third-party CSI 300 history with explicitly simulated notice times.
    ImportIndexConstitutionCsv {
        #[arg(long)]
        input: PathBuf,
        #[arg(
            long,
            default_value = "https://raw.githubusercontent.com/unliftedq/index-constitution/main/history/csi300.csv"
        )]
        source_url: String,
    },
    /// Publishes a daily Parquet snapshot only after calendar and market-coverage checks pass.
    PublishDaily {
        #[arg(long)]
        trade_date: NaiveDate,
    },
    /// Incrementally sync an explicit security list, rechecking a bounded lookback,
    /// then audit and atomically publish a frozen snapshot.
    SyncDaily {
        #[arg(long, required = true)]
        symbol: Vec<String>,
        #[arg(long)]
        start: NaiveDate,
        #[arg(long)]
        end: NaiveDate,
        #[arg(long, default_value_t = 10)]
        lookback_days: u32,
        #[arg(long, default_value_t = 2)]
        retries: u32,
    },
    /// Sync and publish the latest prior open day confirmed by the imported calendar.
    SyncDailyLatest {
        #[arg(long, required = true)]
        symbol: Vec<String>,
        #[arg(long, default_value_t = 10)]
        lookback_days: u32,
        #[arg(long, default_value_t = 2)]
        retries: u32,
    },
    /// Retry audit/publication for a completed daily-sync job without refetching.
    PublishSync {
        #[arg(long)]
        job_id: uuid::Uuid,
    },
    /// Imports a saved Tencent day-K JSON response. Network fetching stays in a separate adapter.
    ImportTencentFixture {
        #[arg(long)]
        symbol: String,
        #[arg(long)]
        start: NaiveDate,
        #[arg(long)]
        end: NaiveDate,
        #[arg(long)]
        response: PathBuf,
        #[arg(long, default_value = "fixture://local")]
        request_url: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut warehouse = Warehouse::open(&cli.data_dir)?;
    let output = match cli.command {
        Command::Init | Command::Status => serde_json::to_value(warehouse.status()?)?,
        Command::Export => serde_json::to_value(warehouse.export_snapshot()?)?,
        Command::ExportEtfDay { trade_date } => {
            serde_json::to_value(warehouse.export_etf_daily_snapshot(trade_date)?)?
        }
        Command::BackfillEtfHistory {
            start,
            end,
            symbol,
            limit,
            sleep_ms,
            quiet,
        } => {
            if start > end {
                anyhow::bail!("start cannot be later than end");
            }
            let mut symbols = match symbol {
                Some(symbol) => vec![symbol],
                None => warehouse.etf_symbols()?,
            };
            if limit > 0 {
                symbols.truncate(limit);
            }
            let mut success_windows = 0_usize;
            let mut empty_windows = 0_usize;
            let mut skipped_windows = 0_usize;
            let mut failed_windows = 0_usize;
            let mut imported_rows = 0_usize;
            let mut errors = Vec::new();
            let started = Instant::now();
            if !quiet {
                eprintln!(
                    "ETF历史回填开始: {}只, {} 至 {}, 每窗口最长700天",
                    symbols.len(),
                    start,
                    end
                );
            }
            for (symbol_index, symbol) in symbols.iter().enumerate() {
                let before_success = success_windows;
                let before_empty = empty_windows;
                let before_skipped = skipped_windows;
                let before_failed = failed_windows;
                let before_rows = imported_rows;
                let mut window_start = start;
                while window_start <= end {
                    let window_end = std::cmp::min(window_start + ChronoDuration::days(699), end);
                    if warehouse.backfill_window_done(symbol, window_start, window_end)? {
                        skipped_windows += 1;
                        window_start = window_end + ChronoDuration::days(1);
                        continue;
                    }
                    match sources::fetch_tencent_raw_daily(symbol, window_start, window_end) {
                        Ok((url, body)) if sources::tencent_raw_daily_is_empty(&body, symbol)? => {
                            warehouse.archive_empty_tencent_window(
                                symbol,
                                window_start,
                                window_end,
                                &url,
                                &body,
                            )?;
                            warehouse.record_backfill_window(
                                symbol,
                                window_start,
                                window_end,
                                "EMPTY",
                                0,
                                None,
                            )?;
                            empty_windows += 1;
                        }
                        Ok((url, body)) => match warehouse.ingest_tencent_day_response(
                            symbol,
                            window_start,
                            window_end,
                            &body,
                            &url,
                        ) {
                            Ok(result) => {
                                warehouse.record_backfill_window(
                                    symbol,
                                    window_start,
                                    window_end,
                                    "SUCCESS",
                                    result.source_rows,
                                    None,
                                )?;
                                success_windows += 1;
                                imported_rows += result.source_rows;
                            }
                            Err(error) => {
                                if !quiet {
                                    eprintln!(
                                        "  失败: {symbol} {window_start}..{window_end}: {error}"
                                    );
                                }
                                if errors.len() < 10 {
                                    errors.push(format!(
                                        "{symbol} {window_start}..{window_end}: {error:#}"
                                    ));
                                }
                                warehouse.record_backfill_window(
                                    symbol,
                                    window_start,
                                    window_end,
                                    "FAILED",
                                    0,
                                    Some(&error.to_string()),
                                )?;
                                failed_windows += 1;
                            }
                        },
                        Err(error) => {
                            if !quiet {
                                eprintln!("  失败: {symbol} {window_start}..{window_end}: {error}");
                            }
                            if errors.len() < 10 {
                                errors.push(format!(
                                    "{symbol} {window_start}..{window_end}: {error:#}"
                                ));
                            }
                            warehouse.record_backfill_window(
                                symbol,
                                window_start,
                                window_end,
                                "FAILED",
                                0,
                                Some(&error.to_string()),
                            )?;
                            failed_windows += 1;
                        }
                    }
                    thread::sleep(Duration::from_millis(sleep_ms));
                    window_start = window_end + ChronoDuration::days(1);
                }
                if !quiet {
                    let completed = symbol_index + 1;
                    let elapsed = started.elapsed();
                    let eta = if completed < symbols.len() {
                        Duration::from_secs_f64(
                            elapsed.as_secs_f64() / completed as f64
                                * (symbols.len() - completed) as f64,
                        )
                    } else {
                        Duration::ZERO
                    };
                    eprintln!(
                        "[{completed}/{} {:>5.1}%] {symbol}: 新增{}行, 成功窗{}, 空窗{}, 跳过{}, 失败{} | 已用{} | 预计剩余{}",
                        symbols.len(),
                        completed as f64 * 100.0 / symbols.len() as f64,
                        imported_rows - before_rows,
                        success_windows - before_success,
                        empty_windows - before_empty,
                        skipped_windows - before_skipped,
                        failed_windows - before_failed,
                        format_duration(elapsed),
                        format_duration(eta),
                    );
                }
            }
            serde_json::json!({
                "symbols": symbols.len(),
                "success_windows": success_windows,
                "empty_windows": empty_windows,
                "skipped_windows": skipped_windows,
                "failed_windows": failed_windows,
                "imported_rows": imported_rows,
                "errors": errors,
            })
        }
        Command::BackfillMarketHistory {
            start,
            end,
            limit,
            sleep_ms,
            quiet,
        } => {
            if start > end {
                anyhow::bail!("start cannot be later than end");
            }
            let mut symbols = warehouse.observed_market_symbols()?;
            if limit > 0 {
                symbols.truncate(limit);
            }
            let total_calendar_days = (end - start).num_days() as usize + 1;
            let total_windows_per_symbol =
                total_calendar_days.div_ceil(MARKET_HISTORY_WINDOW_DAYS as usize);
            let total_windows = symbols.len().saturating_mul(total_windows_per_symbol);
            let mut completed_windows = 0_usize;
            let mut success_windows = 0_usize;
            let mut empty_windows = 0_usize;
            let mut skipped_windows = 0_usize;
            let mut failed_windows = 0_usize;
            let mut imported_rows = 0_usize;
            let mut errors = Vec::new();
            let started = Instant::now();
            if !quiet {
                eprintln!(
                    "沪深市场历史日线回填开始: {}只本地已观察证券, {} 至 {}, 每窗口最长{}天, 约{}个窗口",
                    symbols.len(),
                    start,
                    end,
                    MARKET_HISTORY_WINDOW_DAYS,
                    total_windows
                );
                eprintln!(
                    "证券池不含本机从未观察到的退市证券；腾讯历史接口不支持北交所。原始价、不含成交额。已成功窗口可断点跳过。2026-09-23直连测试：2016日线返回HTTP 200。时间模型采用采集时可见现时，非历史公告发布时间。"
                );
            }
            for (symbol_index, symbol) in symbols.iter().enumerate() {
                let mut window_start = start;
                while window_start <= end {
                    let window_end = std::cmp::min(
                        window_start + ChronoDuration::days(MARKET_HISTORY_WINDOW_DAYS - 1),
                        end,
                    );
                    if warehouse.market_backfill_window_done(symbol, window_start, window_end)? {
                        skipped_windows += 1;
                        completed_windows += 1;
                        window_start = window_end + ChronoDuration::days(1);
                        continue;
                    }
                    let mut window_rows = 0_usize;
                    let status = match sources::fetch_tencent_raw_daily(
                        symbol,
                        window_start,
                        window_end,
                    ) {
                        Ok((url, body)) if sources::tencent_raw_daily_is_empty(&body, symbol)? => {
                            warehouse.archive_empty_tencent_window(
                                symbol,
                                window_start,
                                window_end,
                                &url,
                                &body,
                            )?;
                            empty_windows += 1;
                            "EMPTY"
                        }
                        Ok((url, body)) => match warehouse.ingest_tencent_day_response(
                            symbol,
                            window_start,
                            window_end,
                            &body,
                            &url,
                        ) {
                            Ok(result) => {
                                window_rows = result.source_rows;
                                imported_rows += result.source_rows;
                                success_windows += 1;
                                "SUCCESS"
                            }
                            Err(error) => {
                                failed_windows += 1;
                                if errors.len() < 20 {
                                    errors.push(format!(
                                        "{symbol} {window_start}..{window_end}: {error:#}"
                                    ));
                                }
                                if !quiet {
                                    eprintln!(
                                        "  入库失败 {symbol} {window_start}..{window_end}: {error}"
                                    );
                                }
                                warehouse.record_market_backfill_window(
                                    symbol,
                                    window_start,
                                    window_end,
                                    "FAILED",
                                    0,
                                    Some(&error.to_string()),
                                )?;
                                completed_windows += 1;
                                thread::sleep(Duration::from_millis(sleep_ms));
                                window_start = window_end + ChronoDuration::days(1);
                                continue;
                            }
                        },
                        Err(error) => {
                            failed_windows += 1;
                            if errors.len() < 20 {
                                errors.push(format!(
                                    "{symbol} {window_start}..{window_end}: {error:#}"
                                ));
                            }
                            if !quiet {
                                eprintln!(
                                    "  下载失败 {symbol} {window_start}..{window_end}: {error}"
                                );
                            }
                            warehouse.record_market_backfill_window(
                                symbol,
                                window_start,
                                window_end,
                                "FAILED",
                                0,
                                Some(&error.to_string()),
                            )?;
                            completed_windows += 1;
                            thread::sleep(Duration::from_millis(sleep_ms));
                            window_start = window_end + ChronoDuration::days(1);
                            continue;
                        }
                    };
                    warehouse.record_market_backfill_window(
                        symbol,
                        window_start,
                        window_end,
                        status,
                        window_rows,
                        None,
                    )?;
                    completed_windows += 1;
                    if !quiet && (completed_windows % 25 == 0 || completed_windows == total_windows)
                    {
                        let elapsed = started.elapsed();
                        let eta = if completed_windows > 0 && completed_windows < total_windows {
                            Duration::from_secs_f64(
                                elapsed.as_secs_f64() / completed_windows as f64
                                    * (total_windows - completed_windows) as f64,
                            )
                        } else {
                            Duration::ZERO
                        };
                        eprintln!(
                            "[{}/{} {:>5.1}%] 当前{} {}..{}: {}行 | 成功{} 空窗{} 跳过{} 失败{} | 已用{} | 预计剩余{}",
                            completed_windows,
                            total_windows,
                            if total_windows == 0 {
                                100.0
                            } else {
                                completed_windows as f64 * 100.0 / total_windows as f64
                            },
                            symbol,
                            window_start,
                            window_end,
                            window_rows,
                            success_windows,
                            empty_windows,
                            skipped_windows,
                            failed_windows,
                            format_duration(elapsed),
                            format_duration(eta)
                        );
                    }
                    thread::sleep(Duration::from_millis(sleep_ms));
                    window_start = window_end + ChronoDuration::days(1);
                }
                if !quiet && symbol_index + 1 == symbols.len() {
                    eprintln!("全部证券窗口已处理。");
                }
            }
            serde_json::json!({
                "scope": "currently_observed_SH_SZ_equities_and_ETFs",
                "start": start.to_string(),
                "end": end.to_string(),
                "symbols": symbols.len(),
                "total_windows": total_windows,
                "completed_windows": completed_windows,
                "success_windows": success_windows,
                "empty_windows": empty_windows,
                "skipped_windows": skipped_windows,
                "failed_windows": failed_windows,
                "imported_rows": imported_rows,
                "unsupported": "Beijing market; historical symbols absent from local instrument table",
                "errors": errors,
            })
        }
        Command::AuditMarketHistory { start, end, symbol } => {
            if start > end {
                anyhow::bail!("start cannot be later than end");
            }
            let symbols = if symbol.is_empty() {
                vec![
                    "sh600000".to_owned(),
                    "sh600519".to_owned(),
                    "sh510050".to_owned(),
                    "sz000528".to_owned(),
                    "sz159919".to_owned(),
                ]
            } else {
                symbol
            };
            let total_days = (end - start).num_days() as usize + 1;
            let expected_windows = total_days.div_ceil(MARKET_HISTORY_WINDOW_DAYS as usize);
            let mut audits = Vec::with_capacity(symbols.len());
            for symbol in symbols {
                sources::validate_explicit_symbol(&symbol, false)?;
                audits.push(warehouse.audit_market_history_length(
                    &symbol,
                    start,
                    end,
                    expected_windows,
                )?);
            }
            serde_json::to_value(audits)?
        }
        Command::FetchCalendar { year, month } => {
            let (url, body, days) = sources::fetch_szse_calendar(year, month)?;
            serde_json::to_value(
                warehouse.ingest_calendar_response(year, month, &url, &body, &days)?,
            )?
        }
        Command::AuditTradingCalendar { start, end, symbol } => {
            let symbols = if symbol.is_empty() {
                vec![
                    "sh600000".to_owned(),
                    "sh600519".to_owned(),
                    "sh510050".to_owned(),
                    "sz000528".to_owned(),
                    "sz159919".to_owned(),
                ]
            } else {
                symbol
            };
            for symbol in &symbols {
                sources::validate_explicit_symbol(symbol, false)?;
            }
            serde_json::to_value(warehouse.audit_trading_calendar(start, end, &symbols)?)?
        }
        Command::FetchTdxDay { trade_date } => {
            let expected_url = format!(
                "https://www.tdx.com.cn/products/data/data/g4day/{}.zip",
                trade_date.format("%Y%m%d")
            );
            let (url, body, bars) = match sources::fetch_tdx_daily_package(trade_date) {
                Ok(result) => result,
                Err(error) => {
                    warehouse.record_fetch_failure("tdx", &expected_url, &error)?;
                    return Err(error);
                }
            };
            serde_json::to_value(
                warehouse.ingest_tdx_package_response(trade_date, &url, &body, &bars)?,
            )?
        }
        Command::ImportTdxZip { trade_date, input } => {
            let body =
                fs::read(&input).with_context(|| format!("read TDX ZIP {}", input.display()))?;
            let bars = sources::parse_tdx_daily_package(&body, trade_date)?;
            serde_json::to_value(warehouse.ingest_tdx_package_response(
                trade_date,
                "file://local-tdx-zip",
                &body,
                &bars,
            )?)?
        }
        Command::FetchAdjustments { symbol, kind } => {
            let (url, body, factors) = sources::fetch_sina_adjustments(&symbol, &kind)?;
            serde_json::to_value(
                warehouse.ingest_adjustment_response(&symbol, &kind, &url, &body, &factors)?,
            )?
        }
        Command::ImportStatusCsv {
            source,
            source_url,
            input,
        } => {
            let bytes =
                fs::read(&input).with_context(|| format!("read status CSV {}", input.display()))?;
            serde_json::to_value(warehouse.import_security_status_csv(
                &bytes,
                &source,
                &source_url,
            )?)?
        }
        Command::ImportStatusEvidence {
            source,
            source_url,
            input,
            coverage,
        } => {
            let bytes =
                fs::read(&input).with_context(|| format!("read status CSV {}", input.display()))?;
            let coverage_bytes = fs::read(&coverage)
                .with_context(|| format!("read status coverage CSV {}", coverage.display()))?;
            let mut reader = csv::Reader::from_reader(coverage_bytes.as_slice());
            let mut items = Vec::new();
            for record in reader.deserialize::<StatusCoverageCsv>() {
                let record = record?;
                items.push(StatusCoverageEvidence {
                    symbol: record.symbol,
                    coverage_start: NaiveDate::parse_from_str(&record.coverage_start, "%Y-%m-%d")?,
                    coverage_end: NaiveDate::parse_from_str(&record.coverage_end, "%Y-%m-%d")?,
                    declared_coverage: record.declared_coverage,
                    verification_status: record.verification_status,
                    source_ref: record.source_ref,
                    detail: record.detail,
                });
            }
            serde_json::to_value(warehouse.import_security_status_evidence(
                &bytes,
                &source,
                &source_url,
                &items,
            )?)?
        }
        Command::ImportIndexConstitutionCsv { input, source_url } => {
            let bytes = fs::read(&input)
                .with_context(|| format!("read index constitution CSV {}", input.display()))?;
            eprintln!("沪深300历史成分导入开始: {} bytes", bytes.len());
            let result = warehouse.import_index_constitution_csv(&bytes, &source_url)?;
            eprintln!(
                "沪深300历史成分导入完成: {} 条成员区间, 新增修订 {} 条",
                result.source_rows - 1,
                result.new_revisions
            );
            serde_json::to_value(result)?
        }
        Command::PublishDaily { trade_date } => {
            serde_json::to_value(warehouse.publish_daily(trade_date)?)?
        }
        Command::SyncDaily {
            symbol,
            start,
            end,
            lookback_days,
            retries,
        } => {
            let mut result =
                warehouse.sync_daily_tencent(&symbol, start, end, lookback_days, retries)?;
            match warehouse.publish_daily_sync(result.job_id) {
                Ok(snapshot) => { result.snapshot=Some(snapshot); serde_json::to_value(result)? }
                Err(error) => return Err(error).context(format!("sync job {} ingested but audit/publication failed; retry with `publish-sync --job-id {}`",result.job_id,result.job_id)),
            }
        }
        Command::SyncDailyLatest {
            symbol,
            lookback_days,
            retries,
        } => {
            let mut result =
                warehouse.latest_confirmed_daily_sync(&symbol, lookback_days, retries)?;
            match warehouse.publish_daily_sync(result.job_id) {
                Ok(snapshot) => { result.snapshot=Some(snapshot); serde_json::to_value(result)? }
                Err(error) => return Err(error).context(format!("sync job {} ingested but audit/publication failed; retry with `publish-sync --job-id {}`",result.job_id,result.job_id)),
            }
        }
        Command::PublishSync { job_id } => {
            serde_json::to_value(warehouse.publish_daily_sync(job_id)?)?
        }
        Command::ImportTencentFixture {
            symbol,
            start,
            end,
            response,
            request_url,
        } => {
            let bytes = fs::read(&response)
                .with_context(|| format!("read fixture {}", response.display()))?;
            serde_json::to_value(warehouse.ingest_tencent_day_response(
                &symbol,
                start,
                end,
                &bytes,
                &request_url,
            )?)?
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

#[derive(serde::Deserialize)]
struct StatusCoverageCsv {
    symbol: String,
    coverage_start: String,
    coverage_end: String,
    declared_coverage: String,
    verification_status: String,
    source_ref: String,
    detail: Option<String>,
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}
