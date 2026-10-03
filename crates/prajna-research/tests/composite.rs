mod common;

use polars::prelude::*;
use prajna_domain::VenueId;
use prajna_research::{compute, compute_composite, factor::*, load_panel};

fn rotation(trend: Option<u32>) -> Factor {
    Factor::new(
        FactorKind::RotationScore,
        RotationScoreParams {
            short: 1,
            long: 1,
            vol: 2,
            w_s: 1.0,
            w_l: 1.0,
            w_v: 0.0,
            trend,
        },
    )
    .unwrap()
}

fn values(statuses: &[FactorStatus]) -> DataFrame {
    DataFrame::new(
        statuses.len(),
        vec![
            Series::new("instrument_id".into(), vec!["A.SYNTH"; statuses.len()]).into(),
            Series::new(
                "session_date".into(),
                (0..statuses.len() as i32).collect::<Vec<_>>(),
            )
            .cast(&DataType::Date)
            .unwrap()
            .into(),
            Series::new(
                "value".into(),
                statuses
                    .iter()
                    .map(|s| (*s == FactorStatus::Ok).then_some(0.25))
                    .collect::<Vec<_>>(),
            )
            .into(),
            Series::new(
                "available_at".into(),
                statuses
                    .iter()
                    .map(|s| (*s == FactorStatus::Ok).then_some(100_i64))
                    .collect::<Vec<_>>(),
            )
            .i64()
            .unwrap()
            .clone()
            .into_datetime(TimeUnit::Nanoseconds, Some(TimeZone::UTC))
            .into_series()
            .into(),
            Series::new(
                "status".into(),
                statuses.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            )
            .into(),
        ],
    )
    .unwrap()
}

#[test]
fn every_pair_of_dependency_statuses_obeys_priority_even_for_zero_weight() {
    use FactorStatus::*;
    let statuses = [Ok, UnknownAvailability, MissingInput, InsufficientWindow];
    let expected = [
        [Ok, UnknownAvailability, MissingInput, InsufficientWindow],
        [
            UnknownAvailability,
            UnknownAvailability,
            MissingInput,
            InsufficientWindow,
        ],
        [MissingInput, MissingInput, MissingInput, InsufficientWindow],
        [
            InsufficientWindow,
            InsufficientWindow,
            InsufficientWindow,
            InsufficientWindow,
        ],
    ];
    for (left, left_status) in statuses.iter().enumerate() {
        for (right, right_status) in statuses.iter().enumerate() {
            let frame = compute_composite(
                &rotation(None),
                &[
                    ("momentum_short", values(&[*left_status])),
                    ("momentum_long", values(&[Ok])),
                    ("volatility", values(&[*right_status])),
                ],
            )
            .unwrap();
            assert_eq!(frame.schema().as_ref(), &values_schema().polars);
            assert_eq!(
                frame.column("status").unwrap().str().unwrap().get(0),
                Some(expected[left][right].as_str())
            );
            assert_eq!(
                frame.column("value").unwrap().f64().unwrap().get(0),
                (expected[left][right] == Ok).then_some(0.5)
            );
            assert_eq!(
                frame
                    .column("available_at")
                    .unwrap()
                    .datetime()
                    .unwrap()
                    .physical()
                    .get(0),
                Some(100)
            );
        }
    }
}

#[test]
fn published_three_by_ten_scores_match_hand_calculation() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let frame = compute(
        &panel,
        &panel.instruments,
        &rotation(None),
        AvailabilityAssumption::None,
    )
    .unwrap();
    assert_eq!(frame.height(), 30);
    assert_eq!(frame.schema().as_ref(), &values_schema().polars);
    let value = frame.column("value").unwrap().f64().unwrap();
    let status = frame.column("status").unwrap().str().unwrap();
    // The zero-weight volatility still needs three closes, delaying eligibility.
    assert_eq!(status.get(1), Some("insufficient_window"));
    assert_eq!(value.get(1), None);
    for (row, expected) in [
        (2, 0.019801980198019802),
        (4, -0.07766990291262135),
        (12, -0.020202020202020204),
        (22, 0.02),
        (23, -0.019801980198019802),
        (29, 0.038834951456310676),
    ] {
        assert!((value.get(row).unwrap() - expected).abs() < 1e-12);
        assert_eq!(status.get(row), Some("ok"));
    }
    assert_eq!(status.get(19), Some("missing_input"));
    assert_eq!(value.get(19), None);
}

fn with_numbers(mut frame: DataFrame, numbers: &[f64], times: &[i64]) -> DataFrame {
    frame
        .with_column(Series::new("value".into(), numbers).into())
        .unwrap();
    frame
        .with_column(
            Series::new("available_at".into(), times)
                .i64()
                .unwrap()
                .clone()
                .into_datetime(TimeUnit::Nanoseconds, Some(TimeZone::UTC))
                .into_series()
                .into(),
        )
        .unwrap();
    frame
}

#[test]
fn shuffled_keys_and_roles_preserve_weighted_scores_filter_and_latest_availability() {
    let factor = Factor::new(
        FactorKind::RotationScore,
        RotationScoreParams {
            short: 1,
            long: 2,
            vol: 2,
            w_s: 2.0,
            w_l: -1.0,
            w_v: 3.0,
            trend: Some(2),
        },
    )
    .unwrap();
    let mut frame = values(&[FactorStatus::Ok; 2]);
    // Same session on two instruments: alignment must use both key columns.
    frame
        .with_column(Series::new("instrument_id".into(), ["B.SYNTH", "A.SYNTH"]).into())
        .unwrap();
    frame
        .with_column(
            Series::new("session_date".into(), [0_i32, 0])
                .cast(&DataType::Date)
                .unwrap()
                .into(),
        )
        .unwrap();
    let result = compute_composite(
        &factor,
        &[
            (
                "volatility",
                with_numbers(frame.clone(), &[0.125, 0.125], &[200, 200]).reverse(),
            ),
            (
                "trend",
                with_numbers(frame.clone(), &[0.0, 1.0], &[500, 50]),
            ),
            (
                "momentum_long",
                with_numbers(frame.clone(), &[0.5, 0.5], &[300, 400]),
            ),
            (
                "momentum_short",
                with_numbers(frame, &[0.25, 0.25], &[100, 100]).reverse(),
            ),
        ],
    )
    .unwrap();
    assert_eq!(
        result
            .column("instrument_id")
            .unwrap()
            .str()
            .unwrap()
            .get(0),
        Some("A.SYNTH")
    );
    assert_eq!(
        result.column("value").unwrap().f64().unwrap().get(0),
        Some(-0.375)
    );
    assert_eq!(
        result.column("status").unwrap().str().unwrap().get(1),
        Some("filtered")
    );
    assert_eq!(result.column("value").unwrap().f64().unwrap().get(1), None);
    let times = result.column("available_at").unwrap().datetime().unwrap();
    assert_eq!(times.physical().get(0), Some(400));
    assert_eq!(times.physical().get(1), Some(500));
    assert_eq!(result.schema().as_ref(), &values_schema().polars);
}

#[test]
fn non_ok_dependencies_take_precedence_over_a_zero_trend() {
    for status in [
        FactorStatus::UnknownAvailability,
        FactorStatus::MissingInput,
        FactorStatus::InsufficientWindow,
    ] {
        let frame = compute_composite(
            &rotation(Some(2)),
            &[
                ("momentum_short", values(&[FactorStatus::Ok])),
                ("momentum_long", values(&[status])),
                ("volatility", values(&[FactorStatus::Ok])),
                (
                    "trend",
                    with_numbers(values(&[FactorStatus::Ok]), &[0.0], &[200]),
                ),
            ],
        )
        .unwrap();
        assert_eq!(
            frame.column("status").unwrap().str().unwrap().get(0),
            Some(status.as_str())
        );
        assert_eq!(frame.column("value").unwrap().f64().unwrap().get(0), None);
    }
}

#[test]
fn invalid_dependency_contracts_are_errors_not_dropped_rows_or_default_values() {
    let ok = values(&[FactorStatus::Ok]);
    let deps = vec![
        ("momentum_short", ok.clone()),
        ("momentum_long", ok.clone()),
        ("volatility", ok.clone()),
    ];
    assert!(compute_composite(&rotation(None), &deps[..2]).is_err());
    let mut invalid = deps.clone();
    invalid[2].0 = "momentum_long";
    assert!(compute_composite(&rotation(None), &invalid).is_err());
    invalid = deps.clone();
    invalid.push(("trend", ok.clone()));
    assert!(compute_composite(&rotation(None), &invalid).is_err());
    assert!(compute_composite(&rotation(Some(2)), &deps).is_err());
    let base = Factor::new(FactorKind::Momentum, MomentumParams { n: 1 }).unwrap();
    assert!(compute_composite(&base, &deps).is_err());

    let mut duplicate = ok.clone();
    duplicate.vstack_mut(&ok).unwrap();
    let mut mismatched = ok.clone();
    mismatched
        .with_column(Series::new("instrument_id".into(), ["B.SYNTH"]).into())
        .unwrap();
    let mut invalid_status = ok.clone();
    invalid_status
        .with_column(Series::new("status".into(), ["typo"]).into())
        .unwrap();
    let mut null_key = ok.clone();
    null_key
        .with_column(Series::new("instrument_id".into(), [None::<&str>]).into())
        .unwrap();
    let mut no_time = ok.clone();
    no_time
        .with_column(
            Series::new("available_at".into(), [None::<i64>])
                .i64()
                .unwrap()
                .clone()
                .into_datetime(TimeUnit::Nanoseconds, Some(TimeZone::UTC))
                .into_series()
                .into(),
        )
        .unwrap();
    let mut wrong_schema = ok.clone();
    wrong_schema.drop_in_place("value").unwrap();
    for bad in [
        duplicate,
        mismatched,
        invalid_status,
        null_key,
        no_time,
        wrong_schema,
        with_numbers(ok.clone(), &[f64::NAN], &[100]),
        values(&[FactorStatus::Filtered]),
    ] {
        let mut invalid = deps.clone();
        invalid[2].1 = bad;
        assert!(compute_composite(&rotation(None), &invalid).is_err());
    }
    let mut with_trend = deps;
    with_trend.push(("trend", with_numbers(ok, &[0.5], &[100])));
    assert!(compute_composite(&rotation(Some(2)), &with_trend).is_err());
}

#[test]
fn compute_supports_base_roots_empty_universes_and_explicit_availability_assumptions() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    let mut panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let mut times: Vec<_> = panel
        .grid
        .column("close_available_at")
        .unwrap()
        .datetime()
        .unwrap()
        .physical()
        .iter()
        .collect();
    times[0] = None;
    let delayed = panel.sessions[4].ts_close.as_unix_nanos();
    times[1] = Some(delayed);
    panel
        .grid
        .with_column(
            Series::new("close_available_at".into(), times)
                .i64()
                .unwrap()
                .clone()
                .into_datetime(TimeUnit::Nanoseconds, Some(TimeZone::UTC))
                .into_series()
                .into(),
        )
        .unwrap();
    let unknown = compute(
        &panel,
        &panel.instruments,
        &rotation(None),
        AvailabilityAssumption::None,
    )
    .unwrap();
    assert_eq!(
        unknown.column("status").unwrap().str().unwrap().get(2),
        Some("unknown_availability")
    );
    assert_eq!(unknown.column("value").unwrap().f64().unwrap().get(2), None);
    let assumed = compute(
        &panel,
        &panel.instruments,
        &rotation(None),
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
    let filtered = compute(
        &panel,
        &panel.instruments,
        &rotation(Some(3)),
        AvailabilityAssumption::TreatUnknownAsTsClose,
    )
    .unwrap();
    assert_eq!(
        filtered.column("status").unwrap().str().unwrap().get(4),
        Some("filtered")
    );
    assert_eq!(
        filtered.column("value").unwrap().f64().unwrap().get(4),
        None
    );
    let base = Factor::new(FactorKind::Momentum, MomentumParams { n: 1 }).unwrap();
    for factor in [base, rotation(None), rotation(Some(3))] {
        let empty = compute(&panel, &[], &factor, AvailabilityAssumption::None).unwrap();
        assert_eq!(empty.height(), 0);
        assert_eq!(empty.schema().as_ref(), &values_schema().polars);
    }
}
