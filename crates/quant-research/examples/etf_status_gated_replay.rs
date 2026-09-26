use anyhow::{Context, Result, ensure};
use chrono::{NaiveDate, Utc};
use duckdb::Connection;
use quant_research::{data, experiment::ExperimentResult, runner, server};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::PathBuf};
use uuid::Uuid;

const BASELINE_ID: &str = "4e076758-8069-4e0e-9269-1d3f9d574159";
const UNIVERSE_ID: &str = "d8811237-6c37-4189-86b8-9b05fbccc405";
const VERSION_ID: &str = "3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da";

fn sql_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut output = PathBuf::from("research-output");
    let mut evidence_dir = PathBuf::from("research-output/batch3-a-status-evidence");
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = args.next().context("--output requires a directory")?.into(),
            "--evidence-dir" => {
                evidence_dir = args
                    .next()
                    .context("--evidence-dir requires a directory")?
                    .into()
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    let baseline_path = output
        .join("experiments")
        .join(BASELINE_ID)
        .join("experiment.json");
    let baseline: ExperimentResult = serde_json::from_slice(
        &fs::read(&baseline_path)
            .with_context(|| format!("read baseline {}", baseline_path.display()))?,
    )?;
    ensure!(
        baseline
            .universe
            .as_ref()
            .is_some_and(|u| u.universe_id.to_string() == UNIVERSE_ID
                && u.version_id.to_string() == VERSION_ID),
        "baseline is not the locked published Universe/version"
    );
    ensure!(
        baseline.snapshot.snapshot_id.to_string() == "fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34",
        "baseline does not use the locked price snapshot"
    );
    ensure!(
        baseline.snapshot.sha256
            == "8c8debb7e1fdd7b3b592cc0991e807b26f1e94a59fb7bbe7e66e00f3030af872",
        "baseline price snapshot hash changed"
    );

    let calendar_file = baseline
        .snapshot
        .trading_calendar_file
        .as_deref()
        .context("locked baseline is missing its hashed exchange calendar")?;
    let calendar_path = sql_path(calendar_file);
    let calendar_conn = Connection::open_in_memory()?;
    let mut calendar_statement = calendar_conn.prepare(&format!(
        "SELECT CAST(trade_date AS VARCHAR) FROM read_parquet('{calendar_path}')\
         WHERE is_open AND trade_date BETWEEN DATE '2025-09-23' AND DATE '2026-09-21' ORDER BY trade_date"
    ))?;
    let calendar_rows = calendar_statement.query_map([], |row| row.get::<_, String>(0))?;
    let calendar_dates = calendar_rows.collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        calendar_dates.len() == 241,
        "locked official calendar no longer has 241 expected dates"
    );
    fs::create_dir_all(&evidence_dir)?;
    let audit_manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(evidence_dir.join("manifest.json")).with_context(|| {
            format!(
                "read status audit manifest under {}",
                evidence_dir.display()
            )
        })?,
    )?;
    ensure!(
        audit_manifest["coverage_status"] == "gaps"
            && audit_manifest["expected_status_rows"] == 1205,
        "real-status audit must remain an explicit 1205-cell gap before replay"
    );
    fs::write(
        evidence_dir.join("expected-trading-dates.json"),
        serde_json::to_vec_pretty(&calendar_dates)?,
    )?;

    let id = Uuid::new_v4();
    let dir = output.join("snapshots").join(id.to_string());
    fs::create_dir_all(&dir)?;
    let file = dir.join("etf_daily_status_unknown.parquet");
    let source = sql_path(&baseline.snapshot.file);
    let target = sql_path(&file);
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(&format!(
        "COPY (SELECT *,
                CASE WHEN symbol IN ('sh513300','sh518880','sh510320','sz159612','sz159952')
                          AND trade_date BETWEEN DATE '2025-09-23' AND DATE '2026-09-21'
                     THEN 'UNKNOWN'::VARCHAR ELSE NULL::VARCHAR END AS trade_status,
                false AS is_tradable,
                CASE WHEN symbol IN ('sh513300','sh518880','sh510320','sz159612','sz159952')
                          AND trade_date BETWEEN DATE '2025-09-23' AND DATE '2026-09-21'
                     THEN 'batch3_a_no_qualifying_historical_evidence'::VARCHAR ELSE NULL::VARCHAR END AS status_sources,
                NULL::VARCHAR AS status_observed_at, false AS execution_status_covered
         FROM read_parquet('{source}')) TO '{target}' (FORMAT PARQUET, COMPRESSION ZSTD)"
    ))?;
    let bytes = fs::read(&file)?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let mut snapshot = baseline.snapshot.clone();
    snapshot.snapshot_id = id;
    snapshot.created_at = Utc::now().to_rfc3339();
    snapshot.file = file.clone();
    snapshot.sha256 = sha256.clone();
    snapshot.limitations.push("status cells are deliberately UNKNOWN placeholders because complete historical exchange status evidence was not obtained; this snapshot is an audit/blocking input, not real status data".into());
    snapshot.limitations.push(format!("source audit manifest, frozen status fact snapshot, and coverage report are stored under {}; this is an UNKNOWN blocking overlay, not a real status input", evidence_dir.display()));
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&snapshot)?,
    )?;

    let definition = server::load_universe_version(
        &output,
        Uuid::parse_str(UNIVERSE_ID)?,
        Uuid::parse_str(VERSION_ID)?,
    )?;
    let bars = data::load_bars(
        &file,
        Some(NaiveDate::from_ymd_opt(2025, 6, 1).unwrap()),
        Some(NaiveDate::from_ymd_opt(2026, 9, 21).unwrap()),
    )?;
    let result = runner::run_experiment_with_universe(
        baseline.config.clone(),
        snapshot.clone(),
        &bars,
        &[],
        &definition,
        false,
    )?;
    ensure!(
        result.backtest.execution_status_mode.as_deref() == Some("status_gated"),
        "unknown placeholder input did not activate the status gate"
    );
    ensure!(
        result.backtest.trades.is_empty(),
        "unknown state unexpectedly allowed a fill"
    );
    ensure!(
        result.backtest.unexecuted_orders.len() == 24
            && result
                .backtest
                .unexecuted_orders
                .iter()
                .all(|order| order.reason == "unknown_status"),
        "unknown status rejection audit no longer matches the locked baseline signal dates"
    );
    let status_map = data::load_snapshot_execution_statuses(&file)?
        .context("status columns were not read from the frozen overlay")?;
    let sample_symbols = ["sh513300", "sh518880", "sh510320", "sz159612", "sz159952"];
    let covered_window_rows = status_map
        .iter()
        .filter(|((date, symbol), _)| {
            *date >= NaiveDate::from_ymd_opt(2025, 9, 23).unwrap()
                && *date <= NaiveDate::from_ymd_opt(2026, 9, 21).unwrap()
                && sample_symbols.contains(&symbol.as_str())
        })
        .collect::<Vec<_>>();
    ensure!(
        covered_window_rows.len() == 1205
            && covered_window_rows.iter().all(|(_, status)| {
                status.trade_status.as_deref() == Some("UNKNOWN")
                    && !status.is_tradable
                    && status.sources.as_deref()
                        == Some("batch3_a_no_qualifying_historical_evidence")
            }),
        "status snapshot contains a non-UNKNOWN or covered row"
    );

    let baseline_nav_error = verify_ledger(&baseline, &bars)?;
    let status_nav_error = verify_ledger(&result, &bars)?;
    let max_nav_error = baseline_nav_error.max(status_nav_error);
    ensure!(
        max_nav_error < 1e-6,
        "independent cash/position ledger differs from NAV: {max_nav_error}"
    );

    let path = result.save(&output)?;

    let comparison = serde_json::json!({
        "baseline_experiment_id": BASELINE_ID,
        "baseline_execution_status_mode": baseline.backtest.execution_status_mode,
        "status_experiment_id": result.experiment_id,
        "status_execution_status_mode": result.backtest.execution_status_mode,
        "price_snapshot_id": baseline.snapshot.snapshot_id,
        "price_snapshot_sha256": baseline.snapshot.sha256,
        "status_snapshot_id": id,
        "status_snapshot_sha256": sha256,
        "source_audit_manifest": evidence_dir.join("manifest.json"),
        "source_audit_path": "docs/handoffs/evidence/batch3-a/source-audit.json",
        "source_audit_sha256": "e6d56634d4f8da0f6a68dcc52074bc56cffef7c4ea39df7b874de1b7121cb5e8",
        "source_audit": serde_json::from_slice::<serde_json::Value>(&fs::read(evidence_dir.join("manifest.json"))?)?,
        "status_rows": covered_window_rows.len(),
        "status_coverage": "unverified_unknown_placeholder",
        "baseline": summarize(&baseline),
        "status_gated": summarize(&result),
        "baseline_max_independent_nav_error": baseline_nav_error,
        "status_max_independent_nav_error": status_nav_error,
        "max_independent_nav_error": max_nav_error,
        "status_experiment_path": path,
        "accounting_note": "price bars are identical; the gated run uses all-UNKNOWN placeholders because no qualifying historical source was acquired, and is intentionally blocking, not a real-market performance estimate"
    });
    let comparison_path = output
        .join("experiments")
        .join(result.experiment_id.to_string())
        .join("batch3-a-comparison.json");
    fs::write(&comparison_path, serde_json::to_vec_pretty(&comparison)?)?;
    println!("{}", serde_json::to_string_pretty(&comparison)?);
    Ok(())
}

fn verify_ledger(result: &ExperimentResult, bars: &[quant_research::core::Bar]) -> Result<f64> {
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
                side => anyhow::bail!("unexpected side {side}"),
            }
        }
        ensure!(
            cash >= -1e-8,
            "independent ledger cash is negative on {}",
            point.date
        );
        ensure!(
            holdings.values().all(|quantity| *quantity >= 0),
            "independent ledger has a negative long quantity on {}",
            point.date
        );
        let position_value = holdings
            .iter()
            .map(|(symbol, qty)| {
                let mark = marks.get(symbol).with_context(|| {
                    format!(
                        "missing close mark for held symbol {symbol} on {}",
                        point.date
                    )
                })?;
                Ok(*qty as f64 * mark)
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .sum::<f64>();
        let marked = cash + position_value;
        max_nav_error = max_nav_error.max((marked - point.equity).abs());
    }
    let cost_parts = result.backtest.metrics.commission
        + result.backtest.metrics.tax
        + result.backtest.metrics.slippage_cost;
    ensure!(
        (cost_parts - result.backtest.metrics.total_cost).abs() < 1e-7,
        "reported fee/slippage components do not equal total cost"
    );
    ensure!(
        trades.next().is_none(),
        "trade date falls after the last equity point"
    );
    Ok(max_nav_error)
}

fn summarize(result: &ExperimentResult) -> serde_json::Value {
    serde_json::json!({
        "trades": result.backtest.trades.len(),
        "unexecuted_orders": result.backtest.unexecuted_orders,
        "rebalance_deferrals": result.backtest.rebalance_deferrals,
        "final_positions": result.backtest.final_positions,
        "total_cost": result.backtest.metrics.total_cost,
        "commission": result.backtest.metrics.commission,
        "tax": result.backtest.metrics.tax,
        "slippage_cost": result.backtest.metrics.slippage_cost,
        "final_nav": result.backtest.equity_curve.last().map(|point| point.equity),
        "equity_points": result.backtest.equity_curve.len(),
    })
}
