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
