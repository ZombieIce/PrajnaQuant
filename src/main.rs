use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ashare_warehouse::{Warehouse, sources};
use chrono::{Duration as ChronoDuration, NaiveDate};
use clap::{Parser, Subcommand};

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
    /// Downloads and imports an official SZSE monthly trading calendar.
    FetchCalendar {
        #[arg(long)]
        year: i32,
        #[arg(long)]
        month: u32,
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
    /// Publishes a daily Parquet snapshot only after calendar and market-coverage checks pass.
    PublishDaily {
        #[arg(long)]
        trade_date: NaiveDate,
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
        Command::FetchCalendar { year, month } => {
            let (url, body, days) = sources::fetch_szse_calendar(year, month)?;
            serde_json::to_value(
                warehouse.ingest_calendar_response(year, month, &url, &body, &days)?,
            )?
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
        Command::PublishDaily { trade_date } => {
            serde_json::to_value(warehouse.publish_daily(trade_date)?)?
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
