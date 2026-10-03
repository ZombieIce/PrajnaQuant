mod common;

use polars::prelude::*;
use prajna_domain::VenueId;
use prajna_research::{Panel, compute_base, factor::*, load_panel};

fn fixture() -> Panel {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap()
}

fn factors() -> [Factor; 3] {
    [
        Factor::new(FactorKind::Momentum, MomentumParams { n: 1 }).unwrap(),
        Factor::new(FactorKind::Volatility, VolatilityParams { n: 2 }).unwrap(),
        Factor::new(FactorKind::TrendFilter, TrendFilterParams { n: 3 }).unwrap(),
    ]
}

fn check(frame: &DataFrame, row: usize, value: Option<f64>, status: &str) {
    assert_eq!(
        frame.column("status").unwrap().str().unwrap().get(row),
        Some(status)
    );
    let actual = frame.column("value").unwrap().f64().unwrap().get(row);
    match (actual, value) {
        (Some(a), Some(b)) => assert!((a - b).abs() <= 1e-12, "{a} != {b}"),
        pair => assert_eq!(pair.0, pair.1),
    }
    if status != "ok" {
        assert!(
            frame
                .column("available_at")
                .unwrap()
                .is_null()
                .get(row)
                .unwrap()
        );
    }
}

#[test]
fn published_v1_values_match_hand_calculation_and_schema() {
    let panel = fixture();
    let mut members = panel.instruments.clone();
    members.reverse();
    for (kind, factor) in factors().iter().enumerate() {
        let frame = compute_base(&panel, &members, factor, AvailabilityAssumption::None).unwrap();
        assert_eq!(frame.height(), 30);
        assert_eq!(frame.schema().as_ref(), &values_schema().polars);
        let ids = frame.column("instrument_id").unwrap().str().unwrap();
        assert_eq!(ids.get(0), Some("A.SYNTH"));
        assert_eq!(ids.get(10), Some("B.SYNTH"));
        assert_eq!(ids.get(20), Some("C.SYNTH"));
        check(&frame, 0, None, "insufficient_window");
        check(&frame, 19, None, "missing_input");
        match kind {
            0 => {
                check(&frame, 1, Some(0.01), "ok");
                check(&frame, 4, Some(99.0 / 103.0 - 1.0), "ok");
                check(&frame, 11, Some(-0.01), "ok");
            }
            1 => {
                check(&frame, 1, None, "insufficient_window");
                // Two returns: 0.01 and 1/101; sample sigma = abs(r1-r2)/sqrt(2).
                check(
                    &frame,
                    2,
                    Some((0.01_f64 - 1.0 / 101.0).abs() / 2.0_f64.sqrt()),
                    "ok",
                );
                check(&frame, 22, Some(0.01 / 2.0_f64.sqrt()), "ok");
            }
            _ => {
                check(&frame, 1, None, "insufficient_window");
                check(&frame, 2, Some(1.0), "ok");
                check(&frame, 4, Some(0.0), "ok");
                check(&frame, 12, Some(0.0), "ok");
            }
        }
        assert_eq!(
            frame
                .column("available_at")
                .unwrap()
                .datetime()
                .unwrap()
                .physical()
                .get(2),
            Some(panel.sessions[2].ts_close.as_unix_nanos())
        );
    }
}

#[test]
fn missing_sessions_propagate_through_required_windows_without_compression() {
    let panel = fixture();
    // Move the missing B bar into the interior so subsequent windows cross it.
    let mut has_bar = vec![true; 30];
    has_bar[13] = false;
    has_bar[19] = false;
    let mut grid = panel.grid.clone();
    grid.with_column(Series::new("has_bar".into(), has_bar).into())
        .unwrap();
    let panel = Panel { grid, ..panel };
    for (kind, factor) in factors().iter().enumerate() {
        let frame = compute_base(
            &panel,
            &panel.instruments,
            factor,
            AvailabilityAssumption::None,
        )
        .unwrap();
        for row in 13..=14 {
            check(&frame, row, None, "missing_input");
        }
        if kind == 0 {
            check(&frame, 15, Some(103.0 / 102.0 - 1.0), "ok");
        } else {
            check(&frame, 15, None, "missing_input");
            assert_eq!(
                frame.column("status").unwrap().str().unwrap().get(16),
                Some("ok")
            );
        }
    }
    // Momentum only needs the endpoints, not interior bars.
    let factor = Factor::new(FactorKind::Momentum, MomentumParams { n: 3 }).unwrap();
    let frame = compute_base(
        &panel,
        &panel.instruments,
        &factor,
        AvailabilityAssumption::None,
    )
    .unwrap();
    check(&frame, 15, Some(103.0 / 98.0 - 1.0), "ok");
    check(&frame, 16, None, "missing_input");
}

#[test]
fn unknown_availability_requires_explicit_assumption_and_uses_each_inputs_close() {
    let panel = fixture();
    let mut times: Vec<_> = panel
        .grid
        .column("close_available_at")
        .unwrap()
        .datetime()
        .unwrap()
        .physical()
        .into_iter()
        .collect();
    times[0] = None;
    // A historical input can be published after the current session close.
    let delayed = panel.sessions[4].ts_close.as_unix_nanos();
    times[1] = Some(delayed);
    let mut grid = panel.grid.clone();
    grid.with_column(
        Series::new("close_available_at".into(), times)
            .cast(&DataType::Datetime(
                TimeUnit::Nanoseconds,
                Some(TimeZone::UTC),
            ))
            .unwrap()
            .into(),
    )
    .unwrap();
    let panel = Panel { grid, ..panel };
    let factor = Factor::new(FactorKind::Momentum, MomentumParams { n: 2 }).unwrap();
    let unknown = compute_base(
        &panel,
        &panel.instruments,
        &factor,
        AvailabilityAssumption::None,
    )
    .unwrap();
    check(&unknown, 2, None, "unknown_availability");
    let assumed = compute_base(
        &panel,
        &panel.instruments,
        &factor,
        AvailabilityAssumption::TreatUnknownAsTsClose,
    )
    .unwrap();
    check(&assumed, 2, Some(0.02), "ok");
    assert_eq!(
        assumed
            .column("available_at")
            .unwrap()
            .datetime()
            .unwrap()
            .physical()
            .get(2),
        Some(panel.sessions[2].ts_close.as_unix_nanos())
    );
    assert_eq!(
        assumed
            .column("available_at")
            .unwrap()
            .datetime()
            .unwrap()
            .physical()
            .get(3),
        Some(delayed)
    );
    for factor in &factors()[1..] {
        let unknown = compute_base(
            &panel,
            &panel.instruments,
            factor,
            AvailabilityAssumption::None,
        )
        .unwrap();
        check(&unknown, 2, None, "unknown_availability");
        let assumed = compute_base(
            &panel,
            &panel.instruments,
            factor,
            AvailabilityAssumption::TreatUnknownAsTsClose,
        )
        .unwrap();
        assert_eq!(
            assumed.column("status").unwrap().str().unwrap().get(2),
            Some("ok")
        );
        assert_eq!(
            assumed
                .column("available_at")
                .unwrap()
                .datetime()
                .unwrap()
                .physical()
                .get(2),
            Some(delayed)
        );
    }
}

#[test]
fn empty_subset_large_windows_and_shuffled_grid_keep_contract() {
    let panel = fixture();
    let empty = compute_base(&panel, &[], &factors()[0], AvailabilityAssumption::None).unwrap();
    assert_eq!(empty.height(), 0);
    assert_eq!(empty.schema().as_ref(), &values_schema().polars);
    let shuffled = Panel {
        grid: panel.grid.reverse(),
        ..panel
    };
    let member = &shuffled.instruments[1..2];
    let frame = compute_base(
        &shuffled,
        member,
        &factors()[0],
        AvailabilityAssumption::None,
    )
    .unwrap();
    assert_eq!(frame.height(), 10);
    check(&frame, 1, Some(-0.01), "ok");
    let huge = Factor::new(FactorKind::Volatility, VolatilityParams { n: u32::MAX }).unwrap();
    let frame = compute_base(&shuffled, member, &huge, AvailabilityAssumption::None).unwrap();
    assert_eq!(frame.schema().as_ref(), &values_schema().polars);
    for row in 0..10 {
        check(&frame, row, None, "insufficient_window");
    }
    let trend = Factor::new(FactorKind::TrendFilter, TrendFilterParams { n: 1 }).unwrap();
    let frame = compute_base(&shuffled, member, &trend, AvailabilityAssumption::None).unwrap();
    check(&frame, 0, Some(1.0), "ok");
    check(&frame, 9, None, "missing_input");
    let duplicate = vec![member[0].clone(), member[0].clone()];
    assert!(compute_base(&shuffled, &duplicate, &trend, AvailabilityAssumption::None).is_err());
}

#[test]
fn status_precedence_and_future_input_isolation() {
    let original = fixture();
    let baseline = compute_base(
        &original,
        &original.instruments,
        &factors()[1],
        AvailabilityAssumption::None,
    )
    .unwrap();
    let mut times = vec![None::<i64>; 30];
    times[9] = Some(original.sessions[9].ts_close.as_unix_nanos());
    let mut bars = vec![true; 30];
    bars[0] = false;
    bars[19] = false;
    let mut grid = original.grid.clone();
    grid.with_column(Series::new("has_bar".into(), bars).into())
        .unwrap();
    grid.with_column(
        Series::new("close_available_at".into(), times)
            .cast(&DataType::Datetime(
                TimeUnit::Nanoseconds,
                Some(TimeZone::UTC),
            ))
            .unwrap()
            .into(),
    )
    .unwrap();
    let panel = Panel { grid, ..original };
    let frame = compute_base(
        &panel,
        &panel.instruments,
        &factors()[1],
        AvailabilityAssumption::None,
    )
    .unwrap();
    check(&frame, 0, None, "insufficient_window");
    check(&frame, 1, None, "insufficient_window");
    check(&frame, 2, None, "missing_input");
    check(&frame, 3, None, "unknown_availability");

    let mut clean = fixture();
    let mut prices: Vec<_> = clean
        .grid
        .column("close")
        .unwrap()
        .f64()
        .unwrap()
        .into_iter()
        .collect();
    prices[9] = Some(9999.0);
    clean
        .grid
        .with_column(Series::new("close".into(), prices).into())
        .unwrap();
    let changed = compute_base(
        &clean,
        &clean.instruments,
        &factors()[1],
        AvailabilityAssumption::None,
    )
    .unwrap();
    assert_eq!(baseline.slice(0, 9), changed.slice(0, 9));
}
