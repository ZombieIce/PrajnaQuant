use anyhow::{Context, ensure};
use chrono::NaiveDate;
use quant_research::data::audit_daily_market_coverage;
use std::{env, path::PathBuf};

/// Example: cargo run -p quant-research --example audit_daily_snapshot -- --snapshot bars.parquet
///   --calendar trading_calendar.parquet --symbols sh600000,sh510300 --start 2026-01-01 --end 2026-01-31
fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let mut snapshot = None;
    let mut calendar = None;
    let mut symbols = None;
    let mut start = None;
    let mut end = None;
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("missing value after {flag}"))?;
        match flag.as_str() {
            "--snapshot" => snapshot = Some(PathBuf::from(value)),
            "--calendar" => calendar = Some(PathBuf::from(value)),
            "--symbols" => symbols = Some(value),
            "--start" => start = Some(value.parse::<NaiveDate>()?),
            "--end" => end = Some(value.parse::<NaiveDate>()?),
            _ => anyhow::bail!("unknown option: {flag}"),
        }
    }
    let snapshot = snapshot.context("--snapshot is required")?;
    let symbols = symbols
        .context("--symbols is required")?
        .split(',')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ensure!(
        !symbols.is_empty(),
        "--symbols must list at least one security"
    );
    let report = audit_daily_market_coverage(
        &snapshot,
        calendar.as_deref(),
        &symbols,
        start.context("--start is required")?,
        end.context("--end is required")?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
