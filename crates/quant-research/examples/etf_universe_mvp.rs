use anyhow::{Context, Result};
use chrono::NaiveDate;
use quant_research::{
    core::{ExperimentConfig, StrategyConfig},
    data::{self, SnapshotManifest},
    runner, server,
    universe::{AssetScope, UniverseSelection},
};
use sha2::Digest;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};
use uuid::Uuid;

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut output = PathBuf::from("research-output");
    let mut universe_id = None;
    let mut version_id = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = args.next().context("--output requires a path")?.into(),
            "--universe-id" => {
                universe_id = Some(Uuid::parse_str(
                    &args.next().context("--universe-id requires a UUID")?,
                )?)
            }
            "--version-id" => {
                version_id = Some(Uuid::parse_str(
                    &args.next().context("--version-id requires a UUID")?,
                )?)
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    let universe_id =
        universe_id.context("--universe-id is required; select the published Universe")?;
    let version_id =
        version_id.context("--version-id is required; select the published version")?;
    let definition = server::load_universe_version(&output, universe_id, version_id)?;
    anyhow::ensure!(
        definition.asset_scope == AssetScope::Etf,
        "the selected published Universe must be ETF-only"
    );
    anyhow::ensure!(
        definition.manual_members.len() == 5,
        "the selected version must contain exactly five ETF members"
    );
    let expected_codes = ["513300", "518880", "159612", "510320", "159952"]
        .into_iter()
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        definition
            .manual_members
            .iter()
            .map(|m| m.instrument.code.as_str())
            .collect::<BTreeSet<_>>()
            == expected_codes,
        "the selected published version does not contain the expected five ETFs"
    );

    // Lock the audited immutable snapshot. Never select the newest snapshot at run time.
    const SNAPSHOT_ID: &str = "fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34";
    const SNAPSHOT_SHA256: &str =
        "8c8debb7e1fdd7b3b592cc0991e807b26f1e94a59fb7bbe7e66e00f3030af872";
    let manifest_path = output
        .join("snapshots")
        .join(SNAPSHOT_ID)
        .join("manifest.json");
    let manifest: SnapshotManifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    anyhow::ensure!(
        manifest.snapshot_id.to_string() == SNAPSHOT_ID,
        "locked snapshot ID mismatch"
    );
    anyhow::ensure!(
        manifest.sha256 == SNAPSHOT_SHA256,
        "locked snapshot manifest hash mismatch"
    );
    anyhow::ensure!(
        manifest.file.is_file(),
        "locked ETF parquet snapshot is missing"
    );
    let etf_bytes = fs::read(&manifest.file)?;
    anyhow::ensure!(
        format!("{:x}", sha2::Sha256::digest(&etf_bytes)) == manifest.sha256,
        "locked ETF parquet hash mismatch"
    );
    if let (Some(calendar_path), Some(calendar_hash)) = (
        manifest.trading_calendar_file.as_ref(),
        manifest.trading_calendar_sha256.as_ref(),
    ) {
        let bytes = fs::read(calendar_path)?;
        anyhow::ensure!(
            format!("{:x}", sha2::Sha256::digest(&bytes)) == *calendar_hash,
            "locked trading calendar hash mismatch"
        );
    } else {
        anyhow::bail!("locked snapshot has no hashed official trading calendar");
    }
    let window_start = date(2025, 9, 23);
    let warmup_start = date(2025, 6, 1);
    let window_end = date(2026, 9, 21);
    let symbols = ["sh513300", "sh518880", "sz159612", "sh510320", "sz159952"];
    let audit = data::audit_etf_market_coverage(
        &manifest.file,
        manifest.trading_calendar_file.as_deref(),
        &symbols
            .iter()
            .map(|symbol| (*symbol).to_string())
            .collect::<Vec<_>>(),
        window_start,
        window_end,
    )?;
    anyhow::ensure!(
        audit.status != "gaps"
            && audit.calendar_status == "complete"
            && audit.expected_dates.len() == 241
            && audit.symbols.iter().all(|symbol| {
                symbol.rows == 241
                    && symbol.missing_dates.is_empty()
                    && symbol.unexpected_dates.is_empty()
                    && symbol.duplicate_dates.is_empty()
                    && symbol.invalid_ohlc_dates.is_empty()
            }),
        "ETF market coverage no longer matches the one-year official-calendar audit"
    );
    let selected = symbols.iter().copied().collect::<BTreeSet<_>>();
    let mut bars = data::load_bars(&manifest.file, Some(warmup_start), Some(window_end))?;
    bars.retain(|bar| selected.contains(bar.symbol.as_str()));
    let investability = symbols
        .iter()
        .map(|symbol| {
            let member_dates = bars
                .iter()
                .filter(|bar| bar.symbol == *symbol && bar.trade_date >= window_start)
                .map(|bar| bar.trade_date)
                .collect::<Vec<_>>();
            anyhow::ensure!(
                !member_dates.is_empty(),
                "no audited-range bars found for {symbol}"
            );
            Ok((
                *symbol,
                (
                    *member_dates.iter().min().unwrap(),
                    *member_dates.iter().max().unwrap(),
                ),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    for member in &definition.manual_members {
        let market = match member.instrument.exchange.as_str() {
            "XSHG" => "sh",
            "XSHE" => "sz",
            other => anyhow::bail!("unsupported exchange {other}"),
        };
        let symbol = format!("{market}{}", member.instrument.code);
        let (first_audited_bar, last_audited_bar) = investability[symbol.as_str()];
        anyhow::ensure!(
            member.effective_from == first_audited_bar
                && member.effective_to
                    == Some(last_audited_bar.succ_opt().expect("valid exclusive end")),
            "published member interval for {} does not match the audited range",
            member.instrument.code
        );
    }
    // The MVP verification is intentionally self-contained and does not use
    // the HS300 index as a universe or performance benchmark.
    let mut manifest = manifest;
    manifest.benchmark_file = None;
    manifest.benchmark_sha256 = None;
    manifest.benchmark_rows = 0;
    manifest.benchmark_name = None;
    let benchmark = Vec::new();

    let mut config = ExperimentConfig {
        name: "ETF MVP five-instrument rotation, one year".into(),
        universe: Some(UniverseSelection {
            universe_id,
            version_id,
            asset_scope: AssetScope::Etf,
        }),
        start: Some(window_start),
        end: Some(window_end),
        ..ExperimentConfig::default()
    };
    config.strategy = StrategyConfig {
        lookback_days: 20,
        top_n: 2,
        rebalance_every: 20,
        momentum_short_days: Some(20),
        momentum_long_days: Some(60),
        volatility_window: Some(20),
        trend_window: Some(60),
        use_trend_filter: true,
        ..StrategyConfig::default()
    };
    let mut result = runner::run_experiment_with_universe(
        config,
        manifest,
        &bars,
        &benchmark,
        &definition,
        false,
    )?;
    result.assumptions.push(
        "MVP scenario assumes zero ETF dividends/distributions; reported performance is raw-price return, not total return".into(),
    );
    let mut cash = result.config.initial_cash;
    let mut holdings = BTreeMap::<String, i64>::new();
    let mut marks = BTreeMap::<String, f64>::new();
    let mut trades = result.backtest.trades.iter().peekable();
    let mut max_nav_error: f64 = 0.0;
    for point in &result.backtest.equity_curve {
        for bar in bars.iter().filter(|bar| bar.trade_date == point.date) {
            marks.insert(bar.symbol.clone(), bar.close);
        }
        while trades.peek().is_some_and(|trade| trade.date <= point.date) {
            let trade = trades.next().expect("peeked trade");
            match trade.side.as_str() {
                "BUY" => {
                    cash -= trade.gross_value + trade.commission + trade.tax;
                    *holdings.entry(trade.symbol.clone()).or_default() += trade.quantity;
                }
                "SELL" => {
                    cash += trade.gross_value - trade.commission - trade.tax;
                    *holdings.entry(trade.symbol.clone()).or_default() -= trade.quantity;
                }
                other => anyhow::bail!("unexpected trade side {other}"),
            }
        }
        let marked_equity = cash
            + holdings
                .iter()
                .map(|(symbol, quantity)| {
                    *quantity as f64 * marks.get(symbol).copied().unwrap_or_default()
                })
                .sum::<f64>();
        max_nav_error = max_nav_error.max((marked_equity - point.equity).abs());
    }
    anyhow::ensure!(
        max_nav_error < 1e-6,
        "cash plus marked holdings does not match NAV: {max_nav_error}"
    );
    anyhow::ensure!(
        result
            .backtest
            .trades
            .iter()
            .all(|trade| trade.date >= window_start),
        "trade occurred before configured start date"
    );
    let result_path = result.save(&output)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "result_path": result_path,
            "universe_id": result.universe.as_ref().map(|u| u.universe_id),
            "version_id": result.universe.as_ref().map(|u| u.version_id),
            "universe_content_hash": result.universe.as_ref().map(|u| &u.content_hash),
            "membership_hash": result.universe.as_ref().and_then(|u| u.membership_hashes.first()).map(|h| &h.membership_hash),
            "snapshot_id": result.snapshot.snapshot_id,
            "snapshot_sha256": result.snapshot.sha256,
            "pit_status": result.universe.as_ref().map(|u| u.pit_status),
            "coverage": result.universe.as_ref().map(|u| u.coverage),
            "warnings": result.universe.as_ref().map(|u| &u.warnings),
            "membership_hash_days": result.universe.as_ref().map(|u| u.membership_hashes.len()),
        "market_bars": bars.len(),
        "run_period_bars": bars.iter().filter(|bar| bar.trade_date >= window_start).count(),
        "investability_dates_by_symbol": investability.iter().map(|(symbol, (first, last))| (*symbol, serde_json::json!({"first_audited_bar":first,"last_audited_bar":last,"assumed_investable_from":first,"assumed_investable_through":last}))).collect::<BTreeMap<_,_>>(),
            "market_coverage_audit_status": audit.status,
            "market_coverage_audit": audit,
        "benchmark_bars": benchmark.len(),
        "benchmark_name": result.backtest.benchmark_name,
        "benchmark_curve_bars": result.backtest.benchmark_curve.len(),
        "max_cash_plus_marked_positions_nav_error": max_nav_error,
            "metrics": result.backtest.metrics,
            "equity_points": result.backtest.equity_curve.len(),
            "factor_summary": {
                "mean_rank_ic": result.factor.mean_rank_ic,
                "rank_ic_ir": result.factor.rank_ic_ir,
                "evaluated_dates": result.factor.evaluated_dates,
            }
        }))?
    );
    Ok(())
}
