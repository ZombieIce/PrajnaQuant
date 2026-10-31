use std::collections::BTreeMap;

use polars::prelude::*;
use prajna_data::{NormalizerRegistry, RawStore, SourceKind, SourceRecordInput, publish_dataset};
use prajna_domain::{InstrumentId, VenueId};
use prajna_research::{
    Panel, executable,
    factor::{AvailabilityAssumption, FactorStatus},
    load_execution_status, load_panel, ma_crossover,
    strategy::{BuyAndHoldParams, MaCrossoverParams, VectorEngine, VectorStrategy},
    vector::{VectorCosts, run_vector_with_policy},
};
use serde_json::Value;

const FIXTURE: &[u8] = include_bytes!("../../../poc/mvp1-golden/fixtures/s1-s3-v1.json");

fn publish(lake: &std::path::Path) -> String {
    let store = RawStore::open(lake).unwrap();
    let hash = store
        .put(
            FIXTURE,
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::Fixture,
                source_id: "s1-s3".into(),
                request: Default::default(),
                observed_at: "synthetic".into(),
                ingested_by: "test".into(),
            },
        )
        .unwrap();
    publish_dataset(
        lake,
        &store,
        &NormalizerRegistry::with_builtins(),
        "synthetic-etf-daily",
        "3",
        &[hash],
        "2026-10-10T00:00:00Z",
    )
    .unwrap()
    .dsv
}

fn panel() -> Panel {
    let lake = tempfile::tempdir().unwrap();
    let dsv = publish(lake.path());
    load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap()
}

fn compare(actual: &Value, expected: &Value, field: &str) {
    match (actual, expected) {
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(
                a.keys().collect::<Vec<_>>(),
                b.keys().collect::<Vec<_>>(),
                "{field}"
            );
            for (key, value) in a {
                compare(value, &b[key], key);
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{field}");
            for (a, b) in a.iter().zip(b) {
                compare(a, b, field);
            }
        }
        (Value::Number(a), Value::Number(b)) => {
            let tolerance = if field == "nav" { 1e-10 } else { 1e-12 };
            assert!(
                (a.as_f64().unwrap() - b.as_f64().unwrap()).abs() <= tolerance,
                "{field}: {a} vs {b}"
            );
        }
        _ => assert_eq!(actual, expected, "{field}"),
    }
}

#[test]
fn s1_s3_match_independent_vector_goldens_on_every_session() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = publish(lake.path());
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let costs = VectorCosts {
        commission_rate: 0.001,
        buy_slippage_bps: 10.0,
        sell_slippage_bps: 10.0,
        buy_tax_rate: 0.0,
        sell_tax_rate: 0.0,
    };
    let cases = [
        (
            VectorStrategy::BuyAndHold(BuyAndHoldParams::default()),
            include_str!("../../../poc/mvp1-golden/expected/s1-s3-v1.buy_and_hold.json"),
        ),
        (
            VectorStrategy::MaCrossover(MaCrossoverParams::default()),
            include_str!("../../../poc/mvp1-golden/expected/s1-s3-v1.ma_crossover.json"),
        ),
    ];
    for (strategy, golden) in cases {
        let expected: Value = serde_json::from_str(golden).unwrap();
        let decisions = strategy
            .decide_panel(&panel, &panel.instruments, AvailabilityAssumption::None)
            .unwrap();
        let result = run_vector_with_policy(
            &panel,
            &decisions,
            strategy.unfilled_entry(),
            &costs,
            AvailabilityAssumption::None,
            |id, session| executable(&panel, &statuses, id, session).unwrap(),
        )
        .unwrap();
        compare(
            &serde_json::to_value(&result).unwrap(),
            &expected["vector"],
            "vector",
        );
        if matches!(strategy, VectorStrategy::BuyAndHold(_)) {
            assert_eq!(decisions.len(), 1);
            assert_eq!(decisions[0].decision_session, panel.sessions[0]);
            assert_eq!(
                result.executions[0].session_date,
                panel.sessions[1].session_date.to_string()
            );
        } else {
            let gaps = ma_crossover::gaps(
                &panel,
                &panel.instruments,
                &MaCrossoverParams::default(),
                AvailabilityAssumption::None,
            )
            .unwrap();
            let rows = expected["factors"]["ma_gap(20,60)"].as_array().unwrap();
            assert_eq!(gaps.len(), rows.len());
            for (gap, row) in gaps.iter().zip(rows) {
                assert_eq!(gap.instrument_id.to_string(), row["instrument_id"]);
                assert_eq!(gap.status.as_str(), row["status"]);
                compare(
                    &serde_json::to_value(gap.value).unwrap(),
                    &row["value"],
                    "gap",
                );
            }
        }
    }
}

#[test]
fn zero_gap_enters_and_exits_without_normalizing_the_universe() {
    let mut panel = panel();
    let ids = panel.grid.column("instrument_id").unwrap().str().unwrap();
    let indices = panel.grid.column("session_index").unwrap().u32().unwrap();
    let closes = (0..panel.grid.height())
        .map(|row| {
            let id = ids.get(row);
            let index = indices.get(row);
            Some(if id == Some("A.SYNTH") {
                match index.unwrap() {
                    3 => 110.0,
                    4 => 90.0,
                    5 => 80.0,
                    _ => 100.0,
                }
            } else {
                100.0
            })
        })
        .collect::<Vec<_>>();
    panel
        .grid
        .with_column(Series::new("close".into(), closes).into())
        .unwrap();
    let params = MaCrossoverParams {
        params_version: 1,
        short_window: 2,
        long_window: 3,
    };
    let strategy = VectorStrategy::MaCrossover(params.clone());
    let decisions = strategy
        .decide_panel(&panel, &panel.instruments, AvailabilityAssumption::None)
        .unwrap();
    assert_eq!(decisions[0].decision_session, panel.sessions[3]);
    assert_eq!(
        decisions[0].targets,
        BTreeMap::from([("A.SYNTH".parse::<InstrumentId>().unwrap(), 1.0 / 3.0)])
    );
    // [100,110,90] has gap exactly zero; [110,90,80] then exits.
    assert!(
        decisions
            .iter()
            .any(|d| d.decision_session == panel.sessions[5] && d.targets.is_empty())
    );
    let gaps = ma_crossover::gaps(
        &panel,
        &panel.instruments,
        &params,
        AvailabilityAssumption::None,
    )
    .unwrap();
    assert_eq!(gaps[2].value, Some(0.0));
    assert_eq!(gaps[4].value, Some(0.0));
}

#[test]
fn missing_window_resets_comparison_and_never_bridges_a_gap() {
    let panel = panel();
    let params = MaCrossoverParams::default();
    let gaps = ma_crossover::gaps(
        &panel,
        &panel.instruments,
        &params,
        AvailabilityAssumption::None,
    )
    .unwrap();
    let c = gaps
        .iter()
        .filter(|r| r.instrument_id.to_string() == "C.SYNTH")
        .collect::<Vec<_>>();
    assert!(
        c[61..=120]
            .iter()
            .all(|r| r.status == FactorStatus::MissingInput && r.value.is_none())
    );
    assert_eq!(c[121].status, FactorStatus::Ok);
    let decisions = VectorStrategy::MaCrossover(params)
        .decide_panel(&panel, &panel.instruments, AvailabilityAssumption::None)
        .unwrap();
    assert!(
        decisions
            .iter()
            .all(|d| !d.targets.contains_key(&"C.SYNTH".parse().unwrap()))
    );
}

#[test]
fn parameters_universe_and_unknown_or_late_availability_are_checked() {
    let mut panel = panel();
    let bad = VectorStrategy::MaCrossover(MaCrossoverParams {
        params_version: 2,
        ..Default::default()
    });
    assert!(VectorEngine::validate(&bad).is_err());
    assert!(
        VectorEngine::validate(&VectorStrategy::MaCrossover(MaCrossoverParams {
            short_window: 60,
            ..Default::default()
        }))
        .is_err()
    );
    let strategy = VectorStrategy::MaCrossover(MaCrossoverParams::default());
    assert!(
        strategy
            .decide_panel(&panel, &[], AvailabilityAssumption::None)
            .is_err()
    );
    let unknown = Series::new(
        "close_available_at".into(),
        vec![None::<i64>; panel.grid.height()],
    )
    .cast(&DataType::Datetime(
        TimeUnit::Nanoseconds,
        Some(TimeZone::UTC),
    ))
    .unwrap();
    panel.grid.with_column(unknown.into()).unwrap();
    let gaps = ma_crossover::gaps(
        &panel,
        &panel.instruments,
        &MaCrossoverParams::default(),
        AvailabilityAssumption::None,
    )
    .unwrap();
    assert_eq!(gaps[60].status, FactorStatus::UnknownAvailability);
    assert!(
        strategy
            .decide_panel(&panel, &panel.instruments, AvailabilityAssumption::None)
            .unwrap()
            .is_empty()
    );
    assert!(
        !strategy
            .decide_panel(
                &panel,
                &panel.instruments,
                AvailabilityAssumption::TreatUnknownAsTsClose
            )
            .unwrap()
            .is_empty()
    );
    let late = Series::new(
        "close_available_at".into(),
        vec![Some(i64::MAX); panel.grid.height()],
    )
    .cast(&DataType::Datetime(
        TimeUnit::Nanoseconds,
        Some(TimeZone::UTC),
    ))
    .unwrap();
    panel.grid.with_column(late.into()).unwrap();
    assert!(
        strategy
            .decide_panel(&panel, &panel.instruments, AvailabilityAssumption::None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn timing_unavailable_signals_do_not_create_cash_targets_or_shift_schedule() {
    use prajna_research::{
        factor::AvailabilityAssumption,
        timing::{self, TimingParams},
    };
    let raw = panel();
    let mut panel = raw
        .window(raw.sessions[0].session_date, raw.sessions[9].session_date)
        .unwrap();
    let member = panel.instruments[0].clone();
    let params = TimingParams {
        params_version: 1,
        ma_window: 2,
        rebalance_every: 2,
    };
    let start = panel.sessions[4].session_date;
    let decisions = timing::decide(
        &panel,
        std::slice::from_ref(&member),
        &params,
        AvailabilityAssumption::TreatUnknownAsTsClose,
        Some(start),
    )
    .unwrap();
    assert_eq!(
        decisions
            .iter()
            .map(|d| d.decision_session.session_date)
            .collect::<Vec<_>>(),
        [4, 6, 8].map(|i| panel.sessions[i].session_date)
    );
    let ids = panel.grid.column("instrument_id").unwrap().str().unwrap();
    let indices = panel.grid.column("session_index").unwrap().u32().unwrap();
    let times = (0..panel.grid.height())
        .map(|i| {
            let index = indices.get(i).unwrap() as usize;
            if ids.get(i) == Some(member.to_string().as_str()) && index == 4 {
                None
            } else {
                Some(panel.sessions[index].ts_close.as_unix_nanos())
            }
        })
        .collect::<Vec<_>>();
    panel
        .grid
        .with_column(
            Series::new("close_available_at".into(), times)
                .cast(&DataType::Datetime(
                    TimeUnit::Nanoseconds,
                    Some(TimeZone::UTC),
                ))
                .unwrap()
                .into(),
        )
        .unwrap();
    let decisions = timing::decide(
        &panel,
        std::slice::from_ref(&member),
        &params,
        AvailabilityAssumption::None,
        Some(start),
    )
    .unwrap();
    assert_eq!(
        decisions
            .iter()
            .map(|d| d.decision_session.session_date)
            .collect::<Vec<_>>(),
        [6, 8].map(|i| panel.sessions[i].session_date)
    );
    let bars = panel
        .grid
        .column("session_index")
        .unwrap()
        .u32()
        .unwrap()
        .iter()
        .map(|i| i != Some(6))
        .collect::<Vec<_>>();
    panel
        .grid
        .with_column(Series::new("has_bar".into(), bars).into())
        .unwrap();
    let decisions = timing::decide(
        &panel,
        &[member],
        &params,
        AvailabilityAssumption::None,
        Some(start),
    )
    .unwrap();
    assert_eq!(
        decisions
            .iter()
            .map(|d| d.decision_session.session_date)
            .collect::<Vec<_>>(),
        [8].map(|i| panel.sessions[i].session_date)
    );
    let indices = panel.grid.column("session_index").unwrap().u32().unwrap();
    let late = indices
        .iter()
        .map(|i| i.map(|i| panel.sessions[i as usize].ts_close.as_unix_nanos() + 1))
        .collect::<Vec<_>>();
    panel
        .grid
        .with_column(
            Series::new("close_available_at".into(), late)
                .cast(&DataType::Datetime(
                    TimeUnit::Nanoseconds,
                    Some(TimeZone::UTC),
                ))
                .unwrap()
                .into(),
        )
        .unwrap();
    assert!(
        timing::decide(
            &panel,
            &panel.instruments[..1],
            &params,
            AvailabilityAssumption::None,
            Some(start)
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        timing::decide(
            &panel,
            &panel.instruments,
            &params,
            AvailabilityAssumption::None,
            None
        )
        .is_err()
    );
}

#[test]
fn pool_momentum_ranks_actual_candidates_with_deterministic_ties_and_cash_decisions() {
    use prajna_research::{
        engine::Engine,
        fast_event::{Rebalance, Sizing},
        pool_momentum::{self, PoolMomentumTopKParams},
    };
    let raw = panel();
    let mut panel = raw
        .window(raw.sessions[0].session_date, raw.sessions[9].session_date)
        .unwrap();
    let ids = panel
        .grid
        .column("instrument_id")
        .unwrap()
        .str()
        .unwrap()
        .iter()
        .map(|value| value.unwrap().to_owned())
        .collect::<Vec<_>>();
    let indices = panel
        .grid
        .column("session_index")
        .unwrap()
        .u32()
        .unwrap()
        .iter()
        .map(|value| value.unwrap())
        .collect::<Vec<_>>();
    let closes = (0..panel.grid.height())
        .map(|row| {
            let index = indices[row] as f64;
            Some(match ids[row].as_str() {
                "A.SYNTH" => 100.0 + index,
                "C.SYNTH" => 100.0 + 2.0 * index,
                _ => 100.0,
            })
        })
        .collect::<Vec<_>>();
    panel
        .grid
        .with_column(Series::new("close".into(), closes).into())
        .unwrap();
    let start = panel.sessions[3].session_date;
    let params = PoolMomentumTopKParams {
        params_version: 1,
        lookback: 2,
        top_k: 2,
        rebalance_every: 2,
    };
    let decisions = pool_momentum::decide(
        &panel,
        &panel.instruments,
        &params,
        AvailabilityAssumption::None,
        Some(start),
    )
    .unwrap();
    assert_eq!(
        decisions
            .iter()
            .map(|decision| decision.decision_session.session_date)
            .collect::<Vec<_>>(),
        [3, 5, 7, 9].map(|index| panel.sessions[index].session_date)
    );
    let top = &decisions[0];
    let a: InstrumentId = "A.SYNTH".parse().unwrap();
    let c: InstrumentId = "C.SYNTH".parse().unwrap();
    assert_eq!(
        top.ranked
            .iter()
            .map(|(id, _, rank)| (id.clone(), *rank))
            .collect::<Vec<_>>(),
        vec![
            (c.clone(), 1),
            (a.clone(), 2),
            ("B.SYNTH".parse().unwrap(), 3)
        ]
    );
    assert_eq!(top.targets, BTreeMap::from([(a.clone(), 0.5), (c, 0.5)]));
    assert_eq!(
        pool_momentum::decide(
            &panel,
            &panel.instruments,
            &params,
            AvailabilityAssumption::None,
            None
        )
        .unwrap()[0]
            .decision_session
            .session_date,
        panel.sessions[2].session_date
    );

    // Identical scores tie by ascending InstrumentId; fewer than K candidates
    // are equal-weighted over the actual count.
    let closes = vec![Some(100.0); panel.grid.height()];
    panel
        .grid
        .with_column(Series::new("close".into(), closes).into())
        .unwrap();
    let tie = pool_momentum::decide(
        &panel,
        &panel.instruments,
        &PoolMomentumTopKParams {
            top_k: 2,
            ..params.clone()
        },
        AvailabilityAssumption::None,
        Some(start),
    )
    .unwrap();
    let b: InstrumentId = "B.SYNTH".parse().unwrap();
    assert_eq!(
        tie[0].targets,
        BTreeMap::from([(a.clone(), 0.5), (b.clone(), 0.5)])
    );
    assert_eq!(
        tie[0]
            .ranked
            .iter()
            .map(|(_, _, rank)| *rank)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    let wider = pool_momentum::decide(
        &panel,
        &panel.instruments,
        &PoolMomentumTopKParams {
            top_k: 5,
            ..params.clone()
        },
        AvailabilityAssumption::None,
        Some(start),
    )
    .unwrap();
    assert!((wider[0].targets[&a] - 1.0 / 3.0).abs() < f64::EPSILON);
    assert_eq!(wider[0].targets.len(), 3);

    // A scheduled close with no available score is an explicit all-cash target.
    let bars = (0..panel.grid.height())
        .map(|row| indices[row] != 4)
        .collect::<Vec<_>>();
    panel
        .grid
        .with_column(Series::new("has_bar".into(), bars).into())
        .unwrap();
    let cash = pool_momentum::decide(
        &panel,
        &panel.instruments,
        &PoolMomentumTopKParams {
            rebalance_every: 1,
            ..params.clone()
        },
        AvailabilityAssumption::None,
        Some(start),
    )
    .unwrap();
    let no_scores = cash
        .iter()
        .find(|decision| decision.decision_session.session_date == panel.sessions[4].session_date)
        .unwrap();
    assert!(no_scores.targets.is_empty());
    assert!(no_scores.ranked.is_empty());

    for params in [
        PoolMomentumTopKParams {
            params_version: 2,
            ..params.clone()
        },
        PoolMomentumTopKParams {
            lookback: 0,
            ..params.clone()
        },
        PoolMomentumTopKParams {
            top_k: 0,
            ..params.clone()
        },
        PoolMomentumTopKParams {
            rebalance_every: 0,
            ..params.clone()
        },
    ] {
        assert!(VectorEngine::validate(&VectorStrategy::PoolMomentumTopK(params)).is_err());
    }
    assert!(
        Engine::AccurateEvent {
            sizing: Sizing::Lot,
            rebalance: Rebalance::FullTarget,
        }
        .validate(&VectorStrategy::PoolMomentumTopK(params.clone()))
        .is_err()
    );
    assert!(
        pool_momentum::decide(
            &panel,
            &panel.instruments,
            &params,
            AvailabilityAssumption::None,
            Some(chrono::NaiveDate::from_ymd_opt(2030, 1, 1).unwrap()),
        )
        .is_err()
    );
}
