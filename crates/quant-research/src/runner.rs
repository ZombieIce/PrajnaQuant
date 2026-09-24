use crate::{
    backtest,
    core::{Bar, ExperimentConfig},
    data::SnapshotManifest,
    experiment::{ExperimentResult, UniverseReportIdentity},
    factor,
    universe::{
        AssetScope, Capability, CapabilityKind, CoverageStatus, MembershipSnapshot, PitStatus,
        UniverseDefinition, market_bar_symbol, resolve_members,
    },
};
use anyhow::{Result, ensure};
use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub fn run_experiment(
    config: ExperimentConfig,
    snapshot: SnapshotManifest,
    bars: &[Bar],
    benchmark_bars: &[Bar],
) -> Result<ExperimentResult> {
    config.validate()?;
    ensure!(
        config.universe.is_none(),
        "universe_definition_required: use run_experiment_with_universe with the immutable published definition"
    );
    let calendar = crate::data::load_snapshot_trading_calendar(&snapshot)?
        .unwrap_or_else(|| factor::observed_market_calendar(bars));
    let observations = if config.strategy.uses_rotation_score() {
        crate::strategy::rotation_factor_observations_with_calendar(
            bars,
            &config.strategy,
            config.research.forward_days,
            config.research.label_method,
            &calendar,
        )
    } else {
        factor::momentum_observations_with_calendar(
            bars,
            config.strategy.score_lookback(),
            config.research.forward_days,
            config.research.label_method,
            &calendar,
        )
    };
    let mut expected_by_date = BTreeMap::<NaiveDate, usize>::new();
    for bar in bars {
        if config.start.is_none_or(|start| bar.trade_date >= start)
            && config.end.is_none_or(|end| bar.trade_date <= end)
        {
            *expected_by_date.entry(bar.trade_date).or_default() += 1;
        }
    }
    let observations = observations
        .into_iter()
        .filter(|observation| {
            config.start.is_none_or(|start| observation.date >= start)
                && config.end.is_none_or(|end| observation.date <= end)
        })
        .collect();
    let labelable_expected = factor::expected_with_forward_window(
        &expected_by_date,
        config.research.forward_days,
        config.research.label_method,
        &calendar,
    );
    let factor = factor::evaluate_observations_with_context(
        if config.strategy.uses_rotation_score() {
            "etf_rotation_score"
        } else {
            "momentum_close_to_close"
        },
        config.strategy.score_lookback(),
        config.research.forward_days,
        config.research.quantiles,
        config.research.label_method,
        observations,
        &labelable_expected,
    );
    let scores = if config.strategy.uses_rotation_score() {
        crate::strategy::rotation_scores(bars, &config.strategy)
    } else {
        factor::observation_map(&factor::momentum_observations(
            bars,
            config.strategy.lookback_days,
            0,
        ))
    };
    run_experiment_with_inputs(config, snapshot, factor, &scores, bars, benchmark_bars)
}

/// Runs a strategy/factor experiment against one immutable universe version.
/// All bars remain available to calculations for warmup and old-position marks;
/// only each decision date's scores and cross-sectional factor observations are
/// masked to that day's resolved members.
pub fn run_experiment_with_universe(
    config: ExperimentConfig,
    snapshot: SnapshotManifest,
    bars: &[Bar],
    benchmark_bars: &[Bar],
    definition: &UniverseDefinition,
    strict_pit: bool,
) -> Result<ExperimentResult> {
    config.validate()?;
    let selection = config
        .universe
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("universe_selection_required"))?;
    ensure!(
        selection.universe_id == definition.universe_id
            && selection.version_id == definition.version_id
            && selection.asset_scope == definition.asset_scope,
        "universe_version_mismatch"
    );
    ensure!(
        definition.calculate_content_hash()? == definition.content_hash,
        "universe_content_hash_mismatch"
    );
    ensure!(!bars.is_empty(), "universe run requires market bars");
    if definition.asset_scope != AssetScope::Etf {
        anyhow::bail!("capability_not_ready: stock ETF Rotation execution has not been validated");
    }

    let run_bars = bars
        .iter()
        .filter(|bar| {
            config.start.is_none_or(|start| bar.trade_date >= start)
                && config.end.is_none_or(|end| bar.trade_date <= end)
        })
        .cloned()
        .collect::<Vec<_>>();
    ensure!(
        !run_bars.is_empty(),
        "universe run has no bars inside configured date range"
    );

    let mut snapshots = BTreeMap::<NaiveDate, MembershipSnapshot>::new();
    for date in run_bars
        .iter()
        .map(|bar| bar.trade_date)
        .collect::<BTreeSet<_>>()
    {
        let cutoff = decision_cutoff(date)?;
        let resolved = resolve_members(definition, date, cutoff, strict_pit)?;
        snapshots.insert(date, resolved);
    }
    let members_by_date = snapshots
        .iter()
        .map(|(date, members)| {
            let symbols = members
                .members
                .iter()
                .map(|member| {
                    market_bar_symbol(&member.instrument.exchange, &member.instrument.code)
                })
                .collect::<Result<BTreeSet<_>>>()?;
            Ok((*date, symbols))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let empty_membership_days = proven_empty_membership_days(definition, &snapshots);

    let all_scores = if config.strategy.uses_rotation_score() {
        crate::strategy::rotation_scores(bars, &config.strategy)
    } else {
        factor::observation_map(&factor::momentum_observations(
            bars,
            config.strategy.lookback_days,
            0,
        ))
    };
    let scores = all_scores
        .into_iter()
        .filter(|((date, symbol), _)| {
            members_by_date
                .get(date)
                .is_some_and(|symbols| symbols.contains(symbol))
        })
        .collect::<HashMap<_, _>>();

    let calendar = crate::data::load_snapshot_trading_calendar(&snapshot)?
        .unwrap_or_else(|| factor::observed_market_calendar(bars));
    let observations = if config.strategy.uses_rotation_score() {
        crate::strategy::rotation_factor_observations_with_calendar(
            bars,
            &config.strategy,
            config.research.forward_days,
            config.research.label_method,
            &calendar,
        )
    } else {
        factor::momentum_observations_with_calendar(
            bars,
            config.strategy.lookback_days,
            config.research.forward_days,
            config.research.label_method,
            &calendar,
        )
    }
    .into_iter()
    .filter(|item| {
        members_by_date
            .get(&item.date)
            .is_some_and(|symbols| symbols.contains(&item.symbol))
    })
    .collect();
    let factor_name = if config.strategy.uses_rotation_score() {
        "etf_rotation_score"
    } else {
        "momentum_close_to_close"
    };
    let expected_by_date = members_by_date
        .iter()
        .map(|(date, symbols)| (*date, symbols.len()))
        .collect::<BTreeMap<_, _>>();
    let labelable_expected = factor::expected_with_forward_window(
        &expected_by_date,
        config.research.forward_days,
        config.research.label_method,
        &calendar,
    );
    let factor = factor::evaluate_observations_with_context(
        factor_name,
        config.strategy.score_lookback(),
        config.research.forward_days,
        config.research.quantiles,
        config.research.label_method,
        observations,
        &labelable_expected,
    );
    let statuses = if snapshot.file.is_file() {
        crate::data::load_snapshot_execution_statuses(&snapshot.file)?
    } else {
        None
    };
    let backtest = backtest::run_with_scores_and_empty_membership_days_and_statuses(
        &run_bars,
        benchmark_bars,
        &config,
        &scores,
        Some(&empty_membership_days),
        statuses.as_ref(),
    );
    let ordered_snapshots = snapshots.values().cloned().collect::<Vec<_>>();
    let report_identity = UniverseReportIdentity::freeze(
        definition,
        &ordered_snapshots,
        vec![
            Capability {
                kind: CapabilityKind::MembershipReady,
                ready: true,
                blockers: Vec::new(),
            },
            Capability {
                kind: CapabilityKind::FactorResearchReady,
                ready: true,
                blockers: Vec::new(),
            },
            crate::universe::strategy_capability(definition.asset_scope),
        ],
        snapshot.sha256.clone(),
    )?;
    let result = ExperimentResult::new(config, snapshot, factor, backtest)
        .with_universe_identity(report_identity);
    Ok(result)
}

fn proven_empty_membership_days(
    definition: &UniverseDefinition,
    snapshots: &BTreeMap<NaiveDate, MembershipSnapshot>,
) -> BTreeSet<NaiveDate> {
    snapshots
        .iter()
        .filter_map(|(date, snapshot)| {
            let empty_is_proven = definition.source_kind == crate::universe::SourceKind::Manual
                || (snapshot.coverage == CoverageStatus::Complete
                    && snapshot.pit_status == PitStatus::VerifiedPit);
            (snapshot.members.is_empty() && empty_is_proven).then_some(*date)
        })
        .collect()
}

pub fn run_experiment_with_inputs(
    config: ExperimentConfig,
    snapshot: SnapshotManifest,
    factor: factor::FactorReport,
    scores: &HashMap<(NaiveDate, String), f64>,
    bars: &[Bar],
    benchmark_bars: &[Bar],
) -> Result<ExperimentResult> {
    let statuses = if snapshot.file.is_file() {
        crate::data::load_snapshot_execution_statuses(&snapshot.file)?
    } else {
        None
    };
    run_experiment_with_inputs_and_statuses(
        config,
        snapshot,
        factor,
        scores,
        bars,
        benchmark_bars,
        statuses.as_ref(),
    )
}

pub(crate) fn run_experiment_with_inputs_and_statuses(
    config: ExperimentConfig,
    snapshot: SnapshotManifest,
    factor: factor::FactorReport,
    scores: &HashMap<(NaiveDate, String), f64>,
    bars: &[Bar],
    benchmark_bars: &[Bar],
    statuses: Option<&crate::core::ExecutionStatusMap>,
) -> Result<ExperimentResult> {
    config.validate()?;
    ensure!(
        config.universe.is_none(),
        "universe_definition_required: use run_experiment_with_universe with the immutable published definition"
    );
    let run_bars = bars
        .iter()
        .filter(|bar| {
            config.start.is_none_or(|start| bar.trade_date >= start)
                && config.end.is_none_or(|end| bar.trade_date <= end)
        })
        .cloned()
        .collect::<Vec<_>>();
    ensure!(
        !run_bars.is_empty(),
        "backtest has no bars in configured date range"
    );
    let backtest = backtest::run_with_scores_and_statuses(
        &run_bars,
        benchmark_bars,
        &config,
        scores,
        statuses,
    );
    Ok(ExperimentResult::new(config, snapshot, factor, backtest))
}

pub(crate) fn decision_cutoff(date: NaiveDate) -> Result<chrono::DateTime<Utc>> {
    // Asia/Shanghai is UTC+08:00; 15:00 local is 07:00 UTC.
    Utc.with_ymd_and_hms(date.year(), date.month(), date.day(), 7, 0, 0)
        .single()
        .ok_or_else(|| anyhow::anyhow!("invalid decision cutoff date"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{CostConfig, ResearchConfig, StrategyConfig},
        data::SnapshotManifest,
        universe::{
            AssetScope, AssetType, CoverageSegment, CoverageStatus, Instrument, ManualMember,
            SourceKind,
        },
    };
    use duckdb::Connection;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn date(day: u32) -> NaiveDate {
        let index = day - 1;
        NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()
            + chrono::Duration::days((index / 5 * 7 + index % 5) as i64)
    }

    #[test]
    fn runner_loads_execution_status_from_snapshot_before_backtest() {
        let dir = tempdir().unwrap();
        let parquet = dir.path().join("status-aware-etf.parquet");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT * FROM (VALUES
                ('sh510300','ETF',DATE '2026-01-05',10.0,10.0,10.0,10.0,1000.0,10000.0,'TRADABLE',TRUE,'fixture'),
                ('sh510300','ETF',DATE '2026-01-06',10.0,11.0,10.0,11.0,1000.0,11000.0,'TRADABLE',TRUE,'fixture'),
                ('sh510300','ETF',DATE '2026-01-07',10.0,12.0,10.0,12.0,1000.0,12000.0,'HALTED',FALSE,'fixture'),
                ('sh510300','ETF',DATE '2026-01-08',10.0,13.0,10.0,13.0,1000.0,13000.0,'TRADABLE',TRUE,'fixture')
             ) AS bars(symbol,name,trade_date,open,high,low,close,volume,amount,trade_status,is_tradable,status_sources)) TO '{}' (FORMAT PARQUET)",
            parquet.to_string_lossy().replace('\'', "''")
        ))
        .unwrap();
        let bars = crate::data::load_bars(&parquet, None, None).unwrap();
        let snapshot = SnapshotManifest {
            snapshot_id: Uuid::from_u128(404),
            created_at: "2026-01-01T00:00:00Z".into(),
            source_database: "fixture.duckdb".into(),
            file: parquet,
            sha256: "fixture-status-hash".into(),
            rows: 4,
            symbols: 1,
            first_date: date(1).to_string(),
            last_date: date(4).to_string(),
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
            selection_rule: "synthetic fixture".into(),
            limitations: vec![],
        };
        let config = ExperimentConfig {
            initial_cash: 10_000.0,
            lot_size: 100,
            strategy: StrategyConfig {
                lookback_days: 1,
                top_n: 1,
                rebalance_every: 1,
                ..StrategyConfig::default()
            },
            costs: CostConfig {
                commission_rate: 0.0,
                minimum_commission: 0.0,
                buy_tax_rate: 0.0,
                sell_tax_rate: 0.0,
                buy_slippage_bps: 0.0,
                sell_slippage_bps: 0.0,
            },
            ..ExperimentConfig::default()
        };

        let result = run_experiment(config, snapshot, &bars, &[]).unwrap();

        assert_eq!(result.backtest.trades.len(), 1);
        assert_eq!(result.backtest.trades[0].date, date(4));
        assert_eq!(result.backtest.trades[0].symbol, "sh510300");
        assert_eq!(result.backtest.trades[0].quantity, 1_000);
        let halted_order = result
            .backtest
            .unexecuted_orders
            .iter()
            .find(|order| order.reason == "halted")
            .unwrap();
        assert_eq!(halted_order.attempt_date, date(3));
        assert_eq!(halted_order.status_sources.as_deref(), Some("fixture"));
        assert!(result.backtest.unexecuted_orders.iter().any(|order| {
            order.reason == "no_future_execution_session" && order.attempt_date == date(4)
        }));
    }

    #[test]
    fn explicit_manual_version_is_applied_and_frozen_in_experiment() {
        let id = Uuid::from_u128(101);
        let version_id = Uuid::from_u128(202);
        let mut definition = UniverseDefinition {
            universe_id: id,
            version_id,
            name: "one ETF".into(),
            description: "manual retrospective fixture".into(),
            asset_scope: AssetScope::Etf,
            source_kind: SourceKind::Manual,
            source_ref: "user:local-user".into(),
            content_hash: String::new(),
            manual_members: vec![ManualMember {
                instrument: Instrument {
                    instrument_id: "ETF:510300".into(),
                    exchange: "XSHG".into(),
                    code: "510300".into(),
                    name: "沪深300ETF".into(),
                    asset_type: AssetType::Etf,
                },
                effective_from: date(1),
                effective_to: None,
                source_ref: "user:local-user".into(),
            }],
            index_events: vec![],
            coverage: vec![CoverageSegment {
                start: date(1),
                end: date(10),
                status: CoverageStatus::Complete,
                source_revision_hash: "manual-fixture-source".into(),
            }],
        };
        definition.content_hash = definition.calculate_content_hash().unwrap();
        let bars = (1..=10)
            .map(|day| {
                let close = 100.0 + day as f64;
                Bar {
                    symbol: "sh510300".into(),
                    name: "沪深300ETF".into(),
                    trade_date: date(day),
                    open: close,
                    high: close,
                    low: close,
                    close,
                    volume: 1_000.0,
                    amount: Some(100_000.0),
                }
            })
            .collect::<Vec<_>>();
        let snapshot = SnapshotManifest {
            snapshot_id: Uuid::from_u128(303),
            created_at: "2026-01-01T00:00:00Z".into(),
            source_database: "fixture.duckdb".into(),
            file: "fixture.parquet".into(),
            sha256: "fixture-snapshot-hash".into(),
            rows: bars.len() as i64,
            symbols: 1,
            first_date: date(1).to_string(),
            last_date: date(10).to_string(),
            benchmark_file: None,
            benchmark_sha256: None,
            benchmark_rows: 0,
            benchmark_name: Some("沪深300".into()),
            trading_calendar_file: None,
            trading_calendar_sha256: None,
            trading_calendar_rows: 0,
            trading_calendar_first_date: None,
            trading_calendar_last_date: None,
            trading_calendar_source: None,
            selection_rule: "manual fixture".into(),
            limitations: vec![],
        };
        let config = ExperimentConfig {
            name: "explicit universe run".into(),
            start: Some(date(4)),
            end: Some(date(10)),
            universe: Some(crate::universe::UniverseSelection {
                universe_id: id,
                version_id,
                asset_scope: AssetScope::Etf,
            }),
            strategy: StrategyConfig {
                lookback_days: 1,
                top_n: 1,
                rebalance_every: 1,
                ..StrategyConfig::default()
            },
            research: ResearchConfig {
                forward_days: 2,
                quantiles: 2,
                label_method: crate::core::ForwardReturnMethod::NextOpenToForwardOpen,
            },
            costs: CostConfig::default(),
            ..ExperimentConfig::default()
        };

        let result =
            run_experiment_with_universe(config, snapshot, &bars, &[], &definition, false).unwrap();
        let identity = result.universe.unwrap();
        assert_eq!(identity.universe_id, id);
        assert_eq!(identity.version_id, version_id);
        assert_eq!(identity.pit_status, PitStatus::RetrospectiveStatic);
        assert_eq!(identity.membership_hashes.len(), 7);
        assert_eq!(identity.membership_hashes[0].as_of, date(4));
        assert_eq!(identity.empty_member_dates, Vec::<NaiveDate>::new());
        assert_eq!(identity.snapshot_hash, "fixture-snapshot-hash");
        assert_eq!(result.backtest.equity_curve[0].date, date(4));
        assert!(
            result
                .backtest
                .trades
                .iter()
                .all(|trade| trade.date >= date(4))
        );
        assert!(!result.backtest.trades.is_empty());
    }

    #[test]
    fn unverified_empty_index_snapshot_does_not_trigger_liquidation_day() {
        let mut definition = UniverseDefinition {
            universe_id: Uuid::from_u128(401),
            version_id: Uuid::from_u128(402),
            name: "index with coverage gap".into(),
            description: String::new(),
            asset_scope: AssetScope::Stock,
            source_kind: SourceKind::IndexHistory,
            source_ref: "fixture".into(),
            content_hash: "unknown-until-hash".into(),
            manual_members: vec![],
            index_events: vec![],
            coverage: vec![CoverageSegment {
                start: date(1),
                end: date(1),
                status: CoverageStatus::Gaps,
                source_revision_hash: String::new(),
            }],
        };
        definition.content_hash = definition.calculate_content_hash().unwrap();
        let unresolved = resolve_members(
            &definition,
            date(1),
            decision_cutoff(date(1)).unwrap(),
            false,
        )
        .unwrap();
        assert!(unresolved.members.is_empty());
        assert!(
            proven_empty_membership_days(&definition, &BTreeMap::from([(date(1), unresolved)]))
                .is_empty()
        );

        definition.coverage[0].status = CoverageStatus::Complete;
        definition.coverage[0].source_revision_hash = "verified-empty-fixture".into();
        definition.content_hash = definition.calculate_content_hash().unwrap();
        let resolved = resolve_members(
            &definition,
            date(1),
            decision_cutoff(date(1)).unwrap(),
            true,
        )
        .unwrap();
        assert!(resolved.members.is_empty());
        assert_eq!(
            proven_empty_membership_days(&definition, &BTreeMap::from([(date(1), resolved)])),
            BTreeSet::from([date(1)])
        );
    }

    #[test]
    fn future_return_labels_do_not_change_decision_scores() {
        let make_bars = |future_close: f64| {
            [100.0, 110.0, 105.0, future_close]
                .into_iter()
                .enumerate()
                .map(|(index, close)| Bar {
                    symbol: "sh510300".into(),
                    name: "ETF".into(),
                    trade_date: date(index as u32 + 1),
                    open: close,
                    high: close,
                    low: close,
                    close,
                    volume: 1_000.0,
                    amount: Some(100_000.0),
                })
                .collect::<Vec<_>>()
        };
        let original = make_bars(80.0);
        let changed_future = make_bars(140.0);
        let score_a = factor::observation_map(&factor::momentum_observations(&original, 1, 0));
        let score_b =
            factor::observation_map(&factor::momentum_observations(&changed_future, 1, 0));
        let decision_date = date(2);
        assert_eq!(
            score_a.get(&(decision_date, "sh510300".into())),
            score_b.get(&(decision_date, "sh510300".into()))
        );
        let label_a = factor::momentum_observations(&original, 1, 2)
            .into_iter()
            .find(|row| row.date == decision_date)
            .unwrap()
            .forward_return;
        let label_b = factor::momentum_observations(&changed_future, 1, 2)
            .into_iter()
            .find(|row| row.date == decision_date)
            .unwrap()
            .forward_return;
        assert_ne!(label_a, label_b);
    }
}
