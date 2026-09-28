#![cfg(feature = "app")]

use anyhow::{Context, Result, ensure};
use chrono::NaiveDate;
use quant_research::{
    core::{ExperimentConfig, StrategyConfig},
    data::{self, SnapshotManifest},
    experiment::ExperimentResult,
    runner, server,
    universe::{AssetScope, PitStatus, UniverseSelection},
};
use std::{collections::BTreeMap, fs, path::PathBuf};
use uuid::Uuid;

const SNAPSHOT_ID: &str = "fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34";
const SNAPSHOT_SHA256: &str = "8c8debb7e1fdd7b3b592cc0991e807b26f1e94a59fb7bbe7e66e00f3030af872";

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

/// Live-data integration check. Run explicitly with --ignored after the real
/// Universe has been published into PRAJNA_ETF_MVP_OUTPUT.
#[test]
#[ignore = "requires the audited local snapshot and a published five-ETF Universe"]
fn published_version_and_locked_snapshot_round_trip_with_account_ledger() -> Result<()> {
    let output = PathBuf::from(std::env::var("PRAJNA_ETF_MVP_OUTPUT")?);
    let universe_id = Uuid::parse_str(&std::env::var("PRAJNA_ETF_MVP_UNIVERSE_ID")?)?;
    let version_id = Uuid::parse_str(&std::env::var("PRAJNA_ETF_MVP_VERSION_ID")?)?;
    let definition = server::load_universe_version(&output, universe_id, version_id)?;
    ensure!(definition.asset_scope == AssetScope::Etf);
    ensure!(definition.manual_members.len() == 5);
    ensure!(definition.content_hash == definition.calculate_content_hash()?);
    ensure!(
        definition
            .manual_members
            .iter()
            .map(|member| member.instrument.code.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            == ["513300", "518880", "159612", "510320", "159952"]
                .into_iter()
                .collect()
    );

    let snapshot_dir = output.join("snapshots").join(SNAPSHOT_ID);
    let manifest_path = snapshot_dir.join("manifest.json");
    let mut manifest: SnapshotManifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    ensure!(manifest.snapshot_id.to_string() == SNAPSHOT_ID);
    ensure!(manifest.sha256 == SNAPSHOT_SHA256);
    manifest.file = snapshot_dir.join("etf_daily.parquet");
    manifest.trading_calendar_file = Some(snapshot_dir.join("trading_calendar.parquet"));

    let symbols = ["sh513300", "sh518880", "sz159612", "sh510320", "sz159952"];
    let start = date(2025, 9, 23);
    let end = date(2026, 9, 21);
    let audit = data::audit_etf_market_coverage(
        &manifest.file,
        manifest.trading_calendar_file.as_deref(),
        &symbols.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
        start,
        end,
    )?;
    ensure!(audit.status != "gaps" && audit.calendar_status == "complete");
    ensure!(audit.expected_dates.len() == 241);
    ensure!(
        audit
            .symbols
            .iter()
            .all(|s| s.rows == 241 && s.missing_dates.is_empty())
    );

    let warmup_start = date(2025, 6, 1);
    let selected: std::collections::BTreeSet<_> = symbols.into_iter().collect();
    let mut bars = data::load_bars(&manifest.file, Some(warmup_start), Some(end))?;
    bars.retain(|bar| selected.contains(bar.symbol.as_str()));
    let mut config = ExperimentConfig {
        name: "ETF MVP five-instrument rotation, one year (integration check)".into(),
        universe: Some(UniverseSelection {
            universe_id,
            version_id,
            asset_scope: AssetScope::Etf,
        }),
        start: Some(start),
        end: Some(end),
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
    let result =
        runner::run_experiment_with_universe(config, manifest, &bars, &[], &definition, false)?;
    let identity = result
        .universe
        .as_ref()
        .context("run omitted Universe identity")?;
    ensure!(identity.universe_id == universe_id && identity.version_id == version_id);
    ensure!(identity.content_hash == definition.content_hash);
    ensure!(identity.snapshot_hash == SNAPSHOT_SHA256);
    ensure!(identity.pit_status == PitStatus::RetrospectiveStatic);
    ensure!(identity.membership_hashes.len() > 200);
    ensure!(result.backtest.equity_curve.len() == 241);
    ensure!(result.backtest.position_curve.len() == result.backtest.equity_curve.len());

    let mut cash = result.config.initial_cash;
    let mut holdings = BTreeMap::<String, i64>::new();
    let mut marks = BTreeMap::<String, f64>::new();
    let mut trades = result.backtest.trades.iter().peekable();
    for (equity, positions) in result
        .backtest
        .equity_curve
        .iter()
        .zip(&result.backtest.position_curve)
    {
        for bar in bars.iter().filter(|bar| bar.trade_date == equity.date) {
            marks.insert(bar.symbol.clone(), bar.close);
        }
        while trades.peek().is_some_and(|trade| trade.date <= equity.date) {
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
                side => anyhow::bail!("unknown side {side}"),
            }
        }
        let marked_value = holdings
            .iter()
            .map(|(symbol, qty)| *qty as f64 * marks.get(symbol).copied().unwrap_or_default())
            .sum::<f64>();
        ensure!(
            (cash - equity.cash).abs() < 1e-6,
            "cash mismatch on {}",
            equity.date
        );
        ensure!(
            (cash + marked_value - equity.equity).abs() < 1e-6,
            "NAV mismatch on {}",
            equity.date
        );
        ensure!((positions.cash + positions.invested_value - equity.equity).abs() < 1e-6);
    }
    ensure!(
        (result.backtest.metrics.commission
            + result.backtest.metrics.tax
            + result.backtest.metrics.slippage_cost
            - result.backtest.metrics.total_cost)
            .abs()
            < 1e-6
    );
    ensure!(
        result
            .backtest
            .instrument_performance
            .iter()
            .all(|p| p.final_quantity >= 0)
    );
    let attributed_pnl = result
        .backtest
        .instrument_performance
        .iter()
        .map(|instrument| instrument.total_pnl)
        .sum::<f64>();
    ensure!(
        (attributed_pnl - (result.backtest.metrics.final_equity - result.config.initial_cash))
            .abs()
            < 1e-5,
        "instrument P&L does not reconcile to final NAV change"
    );

    let temp = tempfile::tempdir()?;
    let path = result.save(temp.path())?;
    let reread: ExperimentResult = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        reread
            .universe
            .as_ref()
            .context("saved result lost identity")?
            .version_id
            == version_id
    );
    ensure!(reread.snapshot.snapshot_id.to_string() == SNAPSHOT_ID);
    ensure!(reread.backtest.trades.len() == result.backtest.trades.len());
    Ok(())
}
