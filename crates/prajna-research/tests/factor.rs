use arrow_schema::{DataType, TimeUnit};
use prajna_data::restricted_jcs;
use prajna_research::factor::*;
use serde_json::json;

fn rotation(p: RotationScoreParams) -> Factor {
    Factor::new(FactorParams::RotationScore(p)).unwrap()
}
fn params() -> RotationScoreParams {
    RotationScoreParams {
        short: 5,
        long: 20,
        vol: 10,
        w_s: 1.0,
        w_l: 0.5,
        w_v: 0.25,
        trend: None,
    }
}
fn base(params: FactorParams) -> Factor {
    Factor::new(params).unwrap()
}

#[test]
fn canonical_identity_covers_every_parameter_and_kind() {
    let p = params();
    let original = rotation(p).canonical_json();
    let bytes = restricted_jcs(&original).unwrap();
    assert_eq!(
        bytes,
        restricted_jcs(&rotation(p).canonical_json()).unwrap()
    );
    assert_eq!(
        original,
        json!({"kind":"rotation_score", "version":"1", "params":{"short":5,"long":20,"vol":10,"w_s":"3ff0000000000000","w_l":"3fe0000000000000","w_v":"3fd0000000000000","trend":null}})
    );
    for changed in [
        RotationScoreParams { short: 6, ..p },
        RotationScoreParams { long: 21, ..p },
        RotationScoreParams { vol: 11, ..p },
        RotationScoreParams {
            w_s: 1.0000000000000002,
            ..p
        },
        RotationScoreParams {
            w_l: 0.5000000000000001,
            ..p
        },
        RotationScoreParams {
            w_v: 0.25000000000000006,
            ..p
        },
        RotationScoreParams {
            trend: Some(1),
            ..p
        },
        RotationScoreParams {
            trend: Some(2),
            ..p
        },
    ] {
        assert_ne!(
            bytes,
            restricted_jcs(&rotation(changed).canonical_json()).unwrap()
        );
    }
    assert_ne!(
        rotation(RotationScoreParams {
            trend: Some(1),
            ..p
        })
        .canonical_json(),
        rotation(RotationScoreParams {
            trend: Some(2),
            ..p
        })
        .canonical_json()
    );
    for weight in [
        0.0,
        -0.0,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        f64::MAX,
        -f64::MAX,
    ] {
        for slot in 0..3 {
            let mut changed = p;
            match slot {
                0 => changed.w_s = weight,
                1 => changed.w_l = weight,
                _ => changed.w_v = weight,
            }
            restricted_jcs(&rotation(changed).canonical_json()).unwrap();
        }
    }
    for slot in 0..3 {
        let mut positive = p;
        match slot {
            0 => positive.w_s = 0.0,
            1 => positive.w_l = 0.0,
            _ => positive.w_v = 0.0,
        }
        let mut negative = positive;
        match slot {
            0 => negative.w_s = -0.0,
            1 => negative.w_l = -0.0,
            _ => negative.w_v = -0.0,
        }
        assert_ne!(
            rotation(positive).canonical_json(),
            rotation(negative).canonical_json()
        );
    }
    let definitions = [
        FactorParams::Momentum(WindowParams { n: 2 }),
        FactorParams::Volatility(WindowParams { n: 2 }),
        FactorParams::TrendFilter(WindowParams { n: 2 }),
    ];
    let mut identities = std::collections::BTreeSet::new();
    for definition in definitions {
        let factor = base(definition);
        assert!(identities.insert(restricted_jcs(&factor.canonical_json()).unwrap()));
        assert_eq!(factor.kind().version(), "1");
        assert!(factor.dependencies().is_empty());
        let changed = match definition {
            FactorParams::Momentum(_) => FactorParams::Momentum(WindowParams { n: 3 }),
            FactorParams::Volatility(_) => FactorParams::Volatility(WindowParams { n: 3 }),
            _ => FactorParams::TrendFilter(WindowParams { n: 3 }),
        };
        assert_ne!(factor.canonical_json(), base(changed).canonical_json());
    }
    restricted_jcs(&base(FactorParams::Momentum(WindowParams { n: u32::MAX })).canonical_json())
        .unwrap();
}

#[test]
fn invalid_windows_and_all_nonfinite_weights_are_rejected() {
    for invalid in [
        FactorParams::Momentum(WindowParams { n: 0 }),
        FactorParams::Volatility(WindowParams { n: 0 }),
        FactorParams::Volatility(WindowParams { n: 1 }),
        FactorParams::TrendFilter(WindowParams { n: 0 }),
    ] {
        assert!(matches!(
            Factor::new(invalid),
            Err(FactorError::InvalidWindow { .. })
        ));
    }
    let p = params();
    for invalid in [
        RotationScoreParams { short: 0, ..p },
        RotationScoreParams { long: 0, ..p },
        RotationScoreParams { vol: 0, ..p },
        RotationScoreParams { vol: 1, ..p },
        RotationScoreParams {
            trend: Some(0),
            ..p
        },
    ] {
        assert!(matches!(
            Factor::new(FactorParams::RotationScore(invalid)),
            Err(FactorError::InvalidWindow { .. })
        ));
    }
    for weight in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        for invalid in [
            RotationScoreParams { w_s: weight, ..p },
            RotationScoreParams { w_l: weight, ..p },
            RotationScoreParams { w_v: weight, ..p },
        ] {
            assert!(matches!(
                Factor::new(FactorParams::RotationScore(invalid)),
                Err(FactorError::NonFiniteWeight(_))
            ));
        }
    }
    rotation(RotationScoreParams {
        short: 1,
        long: 1,
        vol: 2,
        trend: Some(1),
        w_s: -1.0,
        ..p
    });
}

#[test]
fn dependencies_keep_roles_even_for_equal_windows_and_zero_weights() {
    for trend in [None, Some(20)] {
        let root = rotation(RotationScoreParams {
            short: 5,
            long: 5,
            w_s: 0.0,
            w_l: 0.0,
            w_v: 0.0,
            trend,
            ..params()
        });
        let deps = root.dependencies();
        assert_eq!(
            deps.iter().map(|(role, _)| *role).collect::<Vec<_>>(),
            if trend.is_some() {
                vec!["momentum_short", "momentum_long", "volatility", "trend"]
            } else {
                vec!["momentum_short", "momentum_long", "volatility"]
            }
        );
        assert_eq!(deps[0].1.canonical_json(), deps[1].1.canonical_json());
        assert_eq!(
            deps[0].1.params(),
            &FactorParams::Momentum(WindowParams { n: 5 })
        );
        assert_eq!(
            deps[2].1.params(),
            &FactorParams::Volatility(WindowParams { n: 10 })
        );
        if trend.is_some() {
            assert_eq!(
                deps[3].1.params(),
                &FactorParams::TrendFilter(WindowParams { n: 20 })
            );
        }
        let ordered = FactorGraph::resolve(&root).unwrap();
        assert_eq!(ordered.len(), if trend.is_some() { 4 } else { 3 });
        assert_eq!(
            ordered.last().unwrap().canonical_json(),
            root.canonical_json()
        );
        for (_, dependency) in deps {
            assert_eq!(
                ordered
                    .iter()
                    .filter(|f| f.canonical_json() == dependency.canonical_json())
                    .count(),
                1
            );
            assert!(
                ordered
                    .iter()
                    .position(|f| f.canonical_json() == dependency.canonical_json())
                    .unwrap()
                    < ordered.len() - 1
            );
        }
        assert_eq!(ordered[0].kind(), FactorKind::Momentum);
        assert_eq!(ordered[1].kind(), FactorKind::Volatility);
    }
    let leaf = base(FactorParams::Momentum(WindowParams { n: 1 }));
    assert_eq!(
        FactorGraph::resolve(&leaf).unwrap()[0].canonical_json(),
        leaf.canonical_json()
    );
}

#[test]
fn canonical_strings_round_trip_and_reject_unknowns() {
    for (value, text) in [
        (FactorStatus::Ok, "ok"),
        (FactorStatus::InsufficientWindow, "insufficient_window"),
        (FactorStatus::MissingInput, "missing_input"),
        (FactorStatus::UnknownAvailability, "unknown_availability"),
        (FactorStatus::Filtered, "filtered"),
    ] {
        assert_eq!(value.to_string(), text);
        assert_eq!(text.parse::<FactorStatus>().unwrap(), value);
    }
    for (value, text) in [
        (AvailabilityAssumption::None, "none"),
        (
            AvailabilityAssumption::TreatUnknownAsTsClose,
            "treat_unknown_as_ts_close",
        ),
    ] {
        assert_eq!(value.to_string(), text);
        assert_eq!(text.parse::<AvailabilityAssumption>().unwrap(), value);
    }
    for text in ["", "OK", "unknown", "None"] {
        assert!(text.parse::<FactorStatus>().is_err());
        assert!(text.parse::<AvailabilityAssumption>().is_err());
    }
}

#[test]
fn values_schema_has_exact_types_nullability_and_metadata() {
    let schema = values_schema();
    assert_eq!(VALUES_SCHEMA_VERSION, 1);
    let expected = [
        ("instrument_id", DataType::Utf8, false),
        ("session_date", DataType::Date32, false),
        ("value", DataType::Float64, true),
        (
            "available_at",
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            true,
        ),
        ("status", DataType::Utf8, false),
    ];
    assert_eq!(schema.fields().len(), expected.len());
    for (field, (name, dtype, nullable)) in schema.fields().iter().zip(expected) {
        assert_eq!(field.name(), name);
        assert_eq!(field.data_type(), &dtype);
        assert_eq!(field.is_nullable(), nullable);
    }
    assert_eq!(
        schema.metadata().get("prajna.table").unwrap(),
        "factor_values"
    );
    assert_eq!(schema.metadata().get("prajna.schema_version").unwrap(), "1");
    assert_eq!(
        schema.field(3).metadata().get("prajna.time_role").unwrap(),
        "available_at"
    );
}
