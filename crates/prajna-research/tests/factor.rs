use arrow_schema::{DataType as ArrowDataType, TimeUnit as ArrowTimeUnit};
use polars::prelude::{DataType, TimeUnit, TimeZone};
use prajna_research::factor::{
    AvailabilityAssumption, Factor, FactorGraph, FactorKind, FactorStatus, MomentumParams,
    RotationScoreParams, TrendFilterParams, VALUES_SCHEMA_VERSION, VolatilityParams, values_schema,
};
use serde_json::json;

fn momentum(n: u32) -> Factor {
    Factor::new(FactorKind::Momentum, MomentumParams { n }).unwrap()
}

fn volatility(n: u32) -> Factor {
    Factor::new(FactorKind::Volatility, VolatilityParams { n }).unwrap()
}

fn trend_filter(n: u32) -> Factor {
    Factor::new(FactorKind::TrendFilter, TrendFilterParams { n }).unwrap()
}

fn rotation_score(w_s: f64, w_l: f64) -> Factor {
    Factor::new(
        FactorKind::RotationScore,
        RotationScoreParams {
            short: 2,
            long: 5,
            vol: 3,
            w_s,
            w_l,
            w_v: 0.0,
            trend: None,
        },
    )
    .unwrap()
}

fn score(short: u32, long: u32, vol: u32, trend: Option<u32>) -> Factor {
    Factor::new(
        FactorKind::RotationScore,
        RotationScoreParams {
            short,
            long,
            vol,
            w_s: 0.0,
            w_l: 0.0,
            w_v: 0.0,
            trend,
        },
    )
    .unwrap()
}

#[test]
fn canonical_json_is_stable_and_preserves_exact_weight_bits() {
    assert_eq!(momentum(5).canonical_json(), momentum(5).canonical_json());
    assert_eq!(
        momentum(5).canonical_json(),
        json!({"kind": "momentum", "version": "1", "params": {"n": 5}})
    );

    assert_eq!(
        rotation_score(1.0, -0.0).canonical_json(),
        json!({
            "kind": "rotation_score",
            "version": "1",
            "params": {
                "short": 2,
                "long": 5,
                "vol": 3,
                "w_s": "3ff0000000000000",
                "w_l": "8000000000000000",
                "w_v": "0000000000000000",
                "trend": null
            }
        })
    );
    assert_ne!(
        rotation_score(1.0, 0.0).canonical_json(),
        rotation_score(1.0000000000000002, 0.0).canonical_json()
    );
    assert_ne!(
        rotation_score(0.0, 0.0).canonical_json(),
        rotation_score(-0.0, 0.0).canonical_json()
    );
}

#[test]
fn canonical_json_changes_when_any_factor_parameter_changes() {
    assert_eq!(
        volatility(3).canonical_json(),
        json!({"kind": "volatility", "version": "1", "params": {"n": 3}})
    );
    assert_eq!(
        trend_filter(4).canonical_json(),
        json!({"kind": "trend_filter", "version": "1", "params": {"n": 4}})
    );

    let pairs = [
        (momentum(2), momentum(3)),
        (volatility(2), volatility(3)),
        (trend_filter(2), trend_filter(3)),
        (
            rotation_score(1.0, 2.0),
            Factor::new(
                FactorKind::RotationScore,
                RotationScoreParams {
                    short: 3,
                    long: 5,
                    vol: 3,
                    w_s: 1.0,
                    w_l: 2.0,
                    w_v: 0.0,
                    trend: None,
                },
            )
            .unwrap(),
        ),
        (
            rotation_score(1.0, 2.0),
            Factor::new(
                FactorKind::RotationScore,
                RotationScoreParams {
                    short: 2,
                    long: 6,
                    vol: 3,
                    w_s: 1.0,
                    w_l: 2.0,
                    w_v: 0.0,
                    trend: None,
                },
            )
            .unwrap(),
        ),
        (
            rotation_score(1.0, 2.0),
            Factor::new(
                FactorKind::RotationScore,
                RotationScoreParams {
                    short: 2,
                    long: 5,
                    vol: 4,
                    w_s: 1.0,
                    w_l: 2.0,
                    w_v: 0.0,
                    trend: None,
                },
            )
            .unwrap(),
        ),
        (rotation_score(1.0, 2.0), rotation_score(3.0, 2.0)),
        (rotation_score(1.0, 2.0), rotation_score(1.0, 3.0)),
        (
            rotation_score(1.0, 2.0),
            Factor::new(
                FactorKind::RotationScore,
                RotationScoreParams {
                    short: 2,
                    long: 5,
                    vol: 3,
                    w_s: 1.0,
                    w_l: 2.0,
                    w_v: 1.0,
                    trend: None,
                },
            )
            .unwrap(),
        ),
        (
            rotation_score(1.0, 2.0),
            Factor::new(
                FactorKind::RotationScore,
                RotationScoreParams {
                    short: 2,
                    long: 5,
                    vol: 3,
                    w_s: 1.0,
                    w_l: 2.0,
                    w_v: 0.0,
                    trend: Some(7),
                },
            )
            .unwrap(),
        ),
    ];
    for (left, right) in pairs {
        assert_ne!(left.canonical_json(), right.canonical_json());
    }
}

#[test]
fn construction_rejects_invalid_windows_weights_and_kind_mismatches() {
    assert!(Factor::new(FactorKind::Momentum, MomentumParams { n: 0 }).is_err());
    assert!(Factor::new(FactorKind::Volatility, VolatilityParams { n: 1 }).is_err());
    assert!(Factor::new(FactorKind::TrendFilter, TrendFilterParams { n: 0 }).is_err());

    for (w_s, w_l, w_v) in [
        (f64::NAN, 0.0, 0.0),
        (0.0, f64::INFINITY, 0.0),
        (0.0, 0.0, f64::NEG_INFINITY),
    ] {
        assert!(
            Factor::new(
                FactorKind::RotationScore,
                RotationScoreParams {
                    short: 1,
                    long: 1,
                    vol: 2,
                    w_s,
                    w_l,
                    w_v,
                    trend: Some(1),
                },
            )
            .is_err()
        );
    }
    assert!(
        Factor::new(
            FactorKind::RotationScore,
            RotationScoreParams {
                short: 1,
                long: 1,
                vol: 1,
                w_s: 0.0,
                w_l: 0.0,
                w_v: 0.0,
                trend: None,
            },
        )
        .is_err()
    );
    assert!(
        Factor::new(
            FactorKind::RotationScore,
            RotationScoreParams {
                short: 1,
                long: 1,
                vol: 2,
                w_s: 0.0,
                w_l: 0.0,
                w_v: 0.0,
                trend: Some(0),
            },
        )
        .is_err()
    );
    assert!(Factor::new(FactorKind::Volatility, MomentumParams { n: 2 }).is_err());
    assert!("unregistered".parse::<FactorKind>().is_err());
}

#[test]
fn rotation_score_dependencies_reflect_roles_and_optional_trend() {
    assert!(momentum(3).dependencies().is_empty());
    assert!(volatility(5).dependencies().is_empty());
    assert!(trend_filter(10).dependencies().is_empty());

    let without_trend = score(3, 8, 5, None);
    let dependencies = without_trend.dependencies();
    assert_eq!(
        dependencies
            .iter()
            .map(|(role, _)| *role)
            .collect::<Vec<_>>(),
        ["momentum_short", "momentum_long", "volatility"]
    );
    assert_eq!(
        dependencies[0].1.canonical_json(),
        momentum(3).canonical_json()
    );
    assert_eq!(
        dependencies[1].1.canonical_json(),
        momentum(8).canonical_json()
    );
    assert_eq!(
        dependencies[2].1.canonical_json(),
        volatility(5).canonical_json()
    );

    let with_trend = score(3, 8, 5, Some(10));
    assert_eq!(
        with_trend
            .dependencies()
            .iter()
            .map(|(role, _)| *role)
            .collect::<Vec<_>>(),
        ["momentum_short", "momentum_long", "volatility", "trend"]
    );
    assert_eq!(
        with_trend.dependencies()[3].1.canonical_json(),
        trend_filter(10).canonical_json()
    );
}

#[test]
fn factor_graph_orders_dependencies_first_and_deduplicates_shared_factors() {
    let factors = FactorGraph::resolve(score(3, 3, 5, Some(10))).unwrap();
    let kinds = factors
        .iter()
        .map(|factor| factor.kind().as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        ["momentum", "volatility", "trend_filter", "rotation_score"]
    );
    assert_eq!(
        factors
            .iter()
            .filter(|factor| factor.kind() == FactorKind::Momentum)
            .count(),
        1
    );
}

#[test]
fn status_and_availability_assumption_use_stable_strings() {
    for (status, value) in [
        (FactorStatus::Ok, "ok"),
        (FactorStatus::InsufficientWindow, "insufficient_window"),
        (FactorStatus::MissingInput, "missing_input"),
        (FactorStatus::UnknownAvailability, "unknown_availability"),
        (FactorStatus::Filtered, "filtered"),
    ] {
        assert_eq!(status.to_string(), value);
        assert_eq!(value.parse::<FactorStatus>().unwrap(), status);
    }
    assert!("not_a_status".parse::<FactorStatus>().is_err());

    assert_eq!(AvailabilityAssumption::None.canonical_str(), "none");
    assert_eq!(
        AvailabilityAssumption::TreatUnknownAsTsClose.canonical_str(),
        "treat_unknown_as_ts_close"
    );
    for assumption in [
        AvailabilityAssumption::None,
        AvailabilityAssumption::TreatUnknownAsTsClose,
    ] {
        assert_eq!(
            assumption
                .to_string()
                .parse::<AvailabilityAssumption>()
                .unwrap(),
            assumption
        );
    }
    assert!("unknown".parse::<AvailabilityAssumption>().is_err());
}

#[test]
fn factor_values_arrow_and_polars_schemas_match_m6() {
    let schema = values_schema();
    assert_eq!(VALUES_SCHEMA_VERSION, 1);
    assert_eq!(
        schema
            .arrow
            .fields()
            .iter()
            .map(|field| field.name().as_str())
            .collect::<Vec<_>>(),
        [
            "instrument_id",
            "session_date",
            "value",
            "available_at",
            "status"
        ]
    );
    let arrow_fields = schema.arrow.fields();
    assert_eq!(arrow_fields[0].data_type(), &ArrowDataType::Utf8);
    assert!(!arrow_fields[0].is_nullable());
    assert_eq!(arrow_fields[1].data_type(), &ArrowDataType::Date32);
    assert!(!arrow_fields[1].is_nullable());
    assert_eq!(arrow_fields[2].data_type(), &ArrowDataType::Float64);
    assert!(arrow_fields[2].is_nullable());
    assert_eq!(
        arrow_fields[3].data_type(),
        &ArrowDataType::Timestamp(ArrowTimeUnit::Nanosecond, Some("UTC".into()))
    );
    assert!(arrow_fields[3].is_nullable());
    assert_eq!(arrow_fields[4].data_type(), &ArrowDataType::Utf8);
    assert!(!arrow_fields[4].is_nullable());

    assert_eq!(
        schema
            .polars
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        [
            "instrument_id",
            "session_date",
            "value",
            "available_at",
            "status"
        ]
    );
    assert_eq!(schema.polars.get("instrument_id"), Some(&DataType::String));
    assert_eq!(schema.polars.get("session_date"), Some(&DataType::Date));
    assert_eq!(schema.polars.get("value"), Some(&DataType::Float64));
    assert_eq!(schema.polars.get("status"), Some(&DataType::String));
    match schema.polars.get("available_at") {
        Some(DataType::Datetime(unit, timezone)) => {
            assert_eq!(*unit, TimeUnit::Nanoseconds);
            assert!(TimeZone::eq_none_as_utc(
                timezone.as_ref(),
                Some(&TimeZone::UTC)
            ));
        }
        dtype => panic!("expected UTC nanosecond datetime, got {dtype:?}"),
    }
}

#[test]
fn restricted_jcs_preserves_all_weight_slots_and_extreme_finite_values() {
    let make = |weights: [f64; 3]| {
        Factor::new(
            FactorKind::RotationScore,
            RotationScoreParams {
                short: 1,
                long: 1,
                vol: 2,
                w_s: weights[0],
                w_l: weights[1],
                w_v: weights[2],
                trend: None,
            },
        )
        .unwrap()
    };
    for slot in 0..3 {
        for (left, right) in [(0.0, -0.0), (1.0, 1.0000000000000002)] {
            let mut a = [1.0; 3];
            let mut b = a;
            a[slot] = left;
            b[slot] = right;
            assert_ne!(
                prajna_data::restricted_jcs(&make(a).canonical_json()).unwrap(),
                prajna_data::restricted_jcs(&make(b).canonical_json()).unwrap()
            );
        }
        for weight in [f64::MIN_POSITIVE, f64::from_bits(1), f64::MAX, -f64::MAX] {
            let mut weights = [1.0; 3];
            weights[slot] = weight;
            let factor = make(weights);
            let canonical = prajna_data::restricted_jcs(&factor.canonical_json()).unwrap();
            assert_eq!(
                canonical,
                prajna_data::restricted_jcs(&make(weights).canonical_json()).unwrap()
            );
        }
    }
}

#[test]
fn rotation_rejects_each_invalid_window_and_nonfinite_weight_slot() {
    for (short, long, vol, trend) in [
        (0, 1, 2, None),
        (1, 0, 2, None),
        (1, 1, 0, None),
        (1, 1, 1, None),
        (1, 1, 2, Some(0)),
    ] {
        assert!(
            Factor::new(
                FactorKind::RotationScore,
                RotationScoreParams {
                    short,
                    long,
                    vol,
                    trend,
                    w_s: 0.0,
                    w_l: 0.0,
                    w_v: 0.0
                }
            )
            .is_err()
        );
    }
    for slot in 0..3 {
        for weight in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut w = [0.0; 3];
            w[slot] = weight;
            assert!(
                Factor::new(
                    FactorKind::RotationScore,
                    RotationScoreParams {
                        short: 1,
                        long: 1,
                        vol: 2,
                        trend: None,
                        w_s: w[0],
                        w_l: w[1],
                        w_v: w[2]
                    }
                )
                .is_err()
            );
        }
    }
}
