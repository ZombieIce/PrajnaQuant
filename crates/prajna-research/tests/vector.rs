mod common;

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use polars::prelude::{DataFrame, DataType, NamedFrom, Series};
use prajna_domain::{InstrumentId, TimestampNs, VenueId};
use prajna_research::{
    Panel, PanelSession, executable,
    factor::AvailabilityAssumption,
    load_execution_status, load_panel,
    strategy::{Decision, ExecutionEvent, execute},
    vector::{VectorCosts, run_vector},
};
use serde_json::Value;

fn instrument(id: &str) -> InstrumentId {
    id.parse().unwrap()
}

fn session(panel: &Panel, date: &str) -> PanelSession {
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
    panel
        .sessions
        .iter()
        .find(|session| session.session_date == date)
        .unwrap()
        .clone()
}

fn decision(panel: &Panel, date: &str, targets: &[(&str, f64)]) -> Decision {
    Decision {
        decision_session: session(panel, date),
        ranked: ["A.SYNTH", "B.SYNTH", "C.SYNTH"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| (instrument(id), 3.0 - index as f64, index as u32 + 1))
            .collect(),
        targets: targets
            .iter()
            .map(|(id, weight)| (instrument(id), *weight))
            .collect(),
    }
}

fn weights(targets: &[(&str, f64)]) -> BTreeMap<InstrumentId, f64> {
    targets
        .iter()
        .map(|(id, weight)| (instrument(id), *weight))
        .collect()
}

fn vector_costs() -> VectorCosts {
    VectorCosts {
        commission_rate: 0.001,
        buy_slippage_bps: 10.0,
        sell_slippage_bps: 10.0,
        buy_tax_rate: 0.0,
        sell_tax_rate: 0.0,
    }
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-12,
        "expected {expected}, got {actual}"
    );
}

fn assert_json_shape(actual: &Value, expected: &Value, field: Option<&str>) {
    if matches!(
        field,
        Some("targets" | "target_weights" | "weights_after_execution")
    ) {
        let actual_weights = actual.as_object().unwrap();
        let expected_weights = expected.as_object().unwrap();
        assert!(actual_weights.values().all(Value::is_number));
        assert!(expected_weights.values().all(Value::is_number));
        return;
    }

    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => {
            let mut actual_keys = actual.keys().collect::<Vec<_>>();
            let mut expected_keys = expected.keys().collect::<Vec<_>>();
            actual_keys.sort();
            expected_keys.sort();
            assert_eq!(actual_keys, expected_keys);
            for (key, actual_value) in actual {
                assert_json_shape(actual_value, &expected[key], Some(key));
            }
        }
        (Value::Array(actual), Value::Array(expected)) => {
            if actual.iter().any(Value::is_object) || expected.iter().any(Value::is_object) {
                if matches!(field, Some("sessions" | "decisions" | "executions")) {
                    assert_eq!(actual.len(), expected.len(), "{field:?} record count");
                }
                for (actual_value, expected_value) in actual.iter().zip(expected) {
                    assert_json_shape(actual_value, expected_value, None);
                }
            } else {
                assert!(actual.iter().all(Value::is_string));
                assert!(expected.iter().all(Value::is_string));
            }
        }
        (Value::String(_), Value::String(_))
        | (Value::Number(_), Value::Number(_))
        | (Value::Bool(_), Value::Bool(_))
        | (Value::Null, Value::Null) => {}
        _ => panic!("JSON value shapes differ: {actual} vs {expected}"),
    }
}

#[test]
fn three_by_ten_result_has_golden_json_shape_and_zero_gross_returns() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let decisions = [
        decision(&panel, "2026-01-07", &[("C.SYNTH", 1.0)]),
        decision(&panel, "2026-01-08", &[("B.SYNTH", 1.0)]),
        decision(&panel, "2026-01-09", &[("B.SYNTH", 1.0)]),
        decision(&panel, "2026-01-12", &[("C.SYNTH", 1.0)]),
        decision(&panel, "2026-01-13", &[("A.SYNTH", 1.0)]),
        decision(&panel, "2026-01-14", &[("A.SYNTH", 1.0)]),
        decision(&panel, "2026-01-15", &[("A.SYNTH", 0.5), ("B.SYNTH", 0.5)]),
        decision(&panel, "2026-01-16", &[("C.SYNTH", 1.0)]),
    ];
    let events = execute(&decisions, &panel.sessions, |instrument, session| {
        executable(&panel, &statuses, instrument, session).unwrap()
    });
    let result = run_vector(
        &panel,
        &decisions,
        &events,
        &vector_costs(),
        AvailabilityAssumption::None,
    )
    .unwrap();

    assert_eq!(result.sessions.len(), 10);
    assert!(
        result
            .sessions
            .iter()
            .all(|session| session.gross_return == 0.0)
    );
    for pair in result.sessions.windows(2) {
        if pair[1].cost == 0.0 {
            assert_eq!(pair[1].nav, pair[0].nav);
        } else {
            assert!(pair[1].nav < pair[0].nav);
        }
    }
    for session in &result.sessions {
        if !result
            .executions
            .iter()
            .any(|event| event.session_date == session.session_date)
        {
            assert_eq!(session.cost, 0.0);
        }
        assert!(
            session
                .weights_after_execution
                .values()
                .all(|weight| *weight >= 0.0)
        );
        assert!(session.weights_after_execution.values().sum::<f64>() <= 1.0);
        assert!(
            session.target_weights.values().all(|weight| *weight >= 0.0)
                && session.target_weights.values().sum::<f64>() <= 1.0
        );
    }

    let actual = serde_json::to_value(result).unwrap();
    let expected: Value = serde_json::from_str(include_str!(
        "../../../poc/mvp1-golden/expected/dataset-v1.json"
    ))
    .unwrap();
    assert_json_shape(&actual, &expected["vector"], None);
}

#[test]
fn runs_an_empty_strategy_without_execution_events() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();

    let result = run_vector(
        &panel,
        &[],
        &[],
        &vector_costs(),
        AvailabilityAssumption::None,
    )
    .unwrap();

    assert!(result.decisions.is_empty());
    assert!(result.executions.is_empty());
    assert!(result.pending_at_end.is_none());
    assert!(
        result
            .sessions
            .iter()
            .all(|session| session.cost == 0.0 && session.nav == 1.0)
    );
}

#[test]
fn rejects_an_executed_leg_without_an_open_price() {
    let panel = manual_panel();
    let target_weights = weights(&[("Y.SYNTH", 1.0)]);
    let decisions = [Decision {
        decision_session: panel.sessions[1].clone(),
        ranked: vec![(instrument("Y.SYNTH"), 1.0, 1)],
        targets: target_weights.clone(),
    }];
    let events = [ExecutionEvent::Executed {
        decision_session: panel.sessions[1].clone(),
        attempt_session: panel.sessions[2].clone(),
        applied: target_weights,
        skipped_buys: BTreeSet::new(),
    }];

    let error = run_vector(
        &panel,
        &decisions,
        &events,
        &vector_costs(),
        AvailabilityAssumption::None,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("executed event applies Y.SYNTH without an open price")
    );
}

#[test]
fn rejects_liquidating_a_holding_without_an_open_price() {
    let panel = manual_panel();
    let decisions = [
        Decision {
            decision_session: panel.sessions[0].clone(),
            ranked: vec![(instrument("Y.SYNTH"), 1.0, 1)],
            targets: weights(&[("Y.SYNTH", 1.0)]),
        },
        Decision {
            decision_session: panel.sessions[1].clone(),
            ranked: vec![(instrument("Y.SYNTH"), 1.0, 1)],
            targets: BTreeMap::new(),
        },
    ];
    let events = [
        ExecutionEvent::Executed {
            decision_session: panel.sessions[0].clone(),
            attempt_session: panel.sessions[1].clone(),
            applied: weights(&[("Y.SYNTH", 1.0)]),
            skipped_buys: BTreeSet::new(),
        },
        ExecutionEvent::Executed {
            decision_session: panel.sessions[1].clone(),
            attempt_session: panel.sessions[2].clone(),
            applied: BTreeMap::new(),
            skipped_buys: BTreeSet::new(),
        },
    ];

    let error = run_vector(
        &panel,
        &decisions,
        &events,
        &vector_costs(),
        AvailabilityAssumption::None,
    )
    .unwrap_err();

    assert!(error.to_string().contains("without an open price"));
}

#[test]
fn result_records_the_upstream_availability_assumption_and_account_simplifications() {
    let panel = manual_panel();
    let assumption = AvailabilityAssumption::TreatUnknownAsTsClose;
    let result = run_vector(&panel, &[], &[], &vector_costs(), assumption).unwrap();

    assert_eq!(result.availability_assumption, "treat_unknown_as_ts_close");
    for expected in [
        "availability assumption: treat_unknown_as_ts_close",
        "long-only unlevered weights; residual cash earns zero return",
    ] {
        assert!(result.assumptions.iter().any(|item| item == expected));
    }
    assert!(
        !result
            .assumptions
            .iter()
            .any(|item| item == "availability assumption: none")
    );
}

fn manual_panel() -> Panel {
    let dates = ["2026-01-05", "2026-01-06", "2026-01-07", "2026-01-08"]
        .map(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap());
    let sessions = dates
        .iter()
        .map(|date| {
            let open = Utc
                .with_ymd_and_hms(date.year(), date.month(), date.day(), 1, 30, 0)
                .single()
                .unwrap()
                .timestamp_nanos_opt()
                .unwrap();
            let close = Utc
                .with_ymd_and_hms(date.year(), date.month(), date.day(), 7, 0, 0)
                .single()
                .unwrap()
                .timestamp_nanos_opt()
                .unwrap();
            PanelSession {
                session_date: *date,
                ts_open: TimestampNs::from_unix_nanos(open),
                ts_close: TimestampNs::from_unix_nanos(close),
            }
        })
        .collect::<Vec<_>>();
    let instruments = vec![instrument("X.SYNTH"), instrument("Y.SYNTH")];
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    let mut ids = Vec::new();
    let mut session_dates = Vec::new();
    let mut session_indices = Vec::new();
    let mut opens = Vec::new();
    for (id, prices) in [
        (
            "X.SYNTH",
            [Some(100.0), Some(100.0), Some(110.0), Some(110.0)],
        ),
        ("Y.SYNTH", [Some(100.0), Some(100.0), None, Some(110.0)]),
    ] {
        for (index, (date, price)) in dates.iter().zip(prices).enumerate() {
            ids.push(id);
            session_dates.push(i32::try_from((*date - epoch).num_days()).unwrap());
            session_indices.push(index as u32);
            opens.push(price);
        }
    }
    let session_dates = Series::new("session_date".into(), session_dates)
        .cast(&DataType::Date)
        .unwrap();
    let grid = DataFrame::new(
        ids.len(),
        vec![
            Series::new("instrument_id".into(), ids).into(),
            session_dates.into(),
            Series::new("session_index".into(), session_indices).into(),
            Series::new("open".into(), opens).into(),
        ],
    )
    .unwrap();
    Panel {
        sessions,
        instruments,
        grid,
    }
}

#[test]
fn hand_calculation_covers_execution_cost_drift_and_carried_valuation() {
    let panel = manual_panel();
    let target_weights = weights(&[("X.SYNTH", 0.5), ("Y.SYNTH", 0.5)]);
    let decisions = [Decision {
        decision_session: panel.sessions[0].clone(),
        ranked: vec![
            (instrument("X.SYNTH"), 1.0, 1),
            (instrument("Y.SYNTH"), 0.0, 2),
        ],
        targets: target_weights.clone(),
    }];
    let events = [ExecutionEvent::Executed {
        decision_session: panel.sessions[0].clone(),
        attempt_session: panel.sessions[1].clone(),
        applied: target_weights,
        skipped_buys: BTreeSet::new(),
    }];
    let costs = VectorCosts {
        commission_rate: 0.01,
        buy_slippage_bps: 0.0,
        sell_slippage_bps: 0.0,
        buy_tax_rate: 0.0,
        sell_tax_rate: 0.0,
    };

    // D2 buys 50/50; D2-D3 earns 5% on X while Y carries, then D3-D4
    // recognizes Y's 10% move using the carried D3 valuation.
    let result = run_vector(
        &panel,
        &decisions,
        &events,
        &costs,
        AvailabilityAssumption::None,
    )
    .unwrap();

    assert_eq!(result.sessions[0].cost, 0.0);
    assert_eq!(result.sessions[0].nav, 1.0);
    let execution = &result.sessions[1];
    assert_close(execution.turnover, 1.0);
    assert_eq!(execution.cost, 0.01);
    assert_close(execution.gross_return, 0.05);
    assert_close(execution.net_return, 0.0395);
    assert_close(execution.nav, 1.0395);
    assert_eq!(
        execution.weights_after_execution,
        weights(&[("X.SYNTH", 0.5), ("Y.SYNTH", 0.5)])
    );

    let carried = &result.sessions[2];
    assert_eq!(carried.cost, 0.0);
    assert_eq!(carried.valuation_carried, vec![instrument("Y.SYNTH")]);
    assert_close(carried.gross_return, 0.047619047619047616);
    assert_close(carried.nav, 1.089);
    assert!((carried.weights_after_execution[&instrument("X.SYNTH")] - 11.0 / 21.0).abs() <= 1e-12);
    assert!((carried.weights_after_execution[&instrument("Y.SYNTH")] - 10.0 / 21.0).abs() <= 1e-12);
    let final_session = &result.sessions[3];
    assert_eq!(final_session.cost, 0.0);
    assert_close(final_session.gross_return, 0.0);
    assert_close(final_session.net_return, 0.0);
    assert_close(final_session.nav, carried.nav);
    assert_eq!(
        final_session.weights_after_execution,
        weights(&[("X.SYNTH", 0.5), ("Y.SYNTH", 0.5)])
    );
}
