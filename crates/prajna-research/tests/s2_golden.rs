use std::{collections::BTreeSet, fs, path::Path};

use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, Utc};
use polars::prelude::DataFrame;
use prajna_data::{NormalizerRegistry, RawStore, SourceKind, SourceRecordInput, publish_dataset};
use prajna_domain::{InstrumentId, VenueId};
use prajna_research::{
    Panel, StaticUniverse,
    cache::store::FactorCache,
    executable,
    factor::{
        AvailabilityAssumption, Factor, FactorGraph, FactorKind, FactorParams, RotationScoreParams,
    },
    load_execution_status, load_panel,
    strategy::{RankDirection, StrategyCapability, VectorStrategy, Weighting, execute},
    vector::{VectorCosts, run_vector},
};
use serde_json::{Map, Number, Value, json};

struct GoldenCase {
    name: &'static str,
    fixture: &'static str,
    expected: &'static str,
    trend: Option<u32>,
    assert_three_by_ten_edges: bool,
}

const CASES: [GoldenCase; 3] = [
    GoldenCase {
        name: "dataset-v1 3x10",
        fixture: "dataset-v1.json",
        expected: "dataset-v1.json",
        trend: None,
        assert_three_by_ten_edges: true,
    },
    GoldenCase {
        name: "B2 S2 scale 64x252 v2",
        fixture: "b2-s2-scale-64x252-v2.json",
        expected: "b2-s2-scale-64x252-v2.json",
        trend: None,
        assert_three_by_ten_edges: false,
    },
    GoldenCase {
        name: "B2 S2 scale 64x252 v2 trend 20",
        fixture: "b2-s2-scale-64x252-v2.json",
        expected: "b2-s2-scale-64x252-v2.trend20.json",
        trend: Some(20),
        assert_three_by_ten_edges: false,
    },
];

#[derive(Clone, Default)]
struct RecordContext {
    instrument_id: Option<(Option<String>, Option<String>)>,
    session: Option<(Option<String>, Option<String>)>,
}

struct Tolerances {
    factor_abs: f64,
    nav_abs: f64,
}

#[test]
fn s2_cases_match_the_independent_golden_end_to_end() {
    for case in CASES {
        run_case(&case);
    }
}

fn publish_v3_fixture(lake_root: &Path, fixture: &[u8], source_id: &str, case: &str) -> String {
    let store =
        RawStore::open(lake_root).unwrap_or_else(|error| panic!("{case}: open Raw store: {error}"));
    let raw_hash = store
        .put(
            fixture,
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::Fixture,
                source_id: source_id.into(),
                request: Default::default(),
                observed_at: "synthetic".into(),
                ingested_by: "prajna-research-test".into(),
            },
        )
        .unwrap_or_else(|error| panic!("{case}: store Raw fixture: {error}"));
    publish_dataset(
        lake_root,
        &store,
        &NormalizerRegistry::with_builtins(),
        "synthetic-etf-daily",
        "3",
        &[raw_hash],
        "2026-09-30T00:00:00Z",
    )
    .unwrap_or_else(|error| panic!("{case}: publish DSV: {error}"))
    .dsv
}

fn run_case(case: &GoldenCase) {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture_path = repo_root
        .join("poc/poc0-benchmark/fixtures")
        .join(case.fixture);
    let expected_path = repo_root
        .join("poc/mvp1-golden/expected")
        .join(case.expected);
    let fixture_bytes = fs::read(&fixture_path).unwrap_or_else(|error| {
        panic!(
            "{}: could not read {}: {error}",
            case.name,
            fixture_path.display()
        )
    });
    let fixture: Value = serde_json::from_slice(&fixture_bytes)
        .unwrap_or_else(|error| panic!("{}: invalid fixture JSON: {error}", case.name));
    let expected_bytes = fs::read(&expected_path).unwrap_or_else(|error| {
        panic!(
            "{}: could not read {}: {error}",
            case.name,
            expected_path.display()
        )
    });
    let expected: Value = serde_json::from_slice(&expected_bytes)
        .unwrap_or_else(|error| panic!("{}: invalid expected JSON: {error}", case.name));
    let tolerances = tolerances(&expected, case.name);

    let lake = tempfile::tempdir().expect("create temporary lake");
    let dsv = publish_v3_fixture(lake.path(), &fixture_bytes, case.fixture, case.name);
    let venue = VenueId::new("SYNTH").expect("valid synthetic venue");
    let panel = load_panel(lake.path(), &dsv, &venue)
        .unwrap_or_else(|error| panic!("{}: load panel: {error}", case.name));
    let statuses = load_execution_status(lake.path(), &dsv)
        .unwrap_or_else(|error| panic!("{}: load execution statuses: {error}", case.name));

    if case.assert_three_by_ten_edges {
        assert_three_by_ten_edge_cases(&panel, &statuses);
    }

    let strategy_config = fixture
        .get("strategy")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{}: fixture strategy must be an object", case.name));
    let score = Factor::new(
        FactorKind::RotationScore,
        RotationScoreParams {
            short: u32_field(strategy_config, "momentum_short_days", case.name),
            long: u32_field(strategy_config, "momentum_long_days", case.name),
            vol: u32_field(strategy_config, "volatility_window", case.name),
            w_s: f64_field(strategy_config, "short_momentum_weight", case.name),
            w_l: f64_field(strategy_config, "long_momentum_weight", case.name),
            w_v: f64_field(strategy_config, "volatility_weight", case.name),
            trend: case.trend,
        },
    )
    .unwrap_or_else(|error| panic!("{}: invalid rotation score factor: {error}", case.name));
    let strategy = VectorStrategy {
        score: score.clone(),
        direction: RankDirection::Descending,
        top_k: u32_field(strategy_config, "top_n", case.name),
        rebalance_every: u32_field(strategy_config, "rebalance_every", case.name),
        weighting: Weighting::EqualWeight,
        capabilities: BTreeSet::from([StrategyCapability::Vectorizable]),
    };
    let costs_config = fixture
        .get("costs")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{}: fixture costs must be an object", case.name));
    let costs = VectorCosts {
        commission_rate: f64_field(costs_config, "commission_rate", case.name),
        buy_slippage_bps: f64_field(costs_config, "buy_slippage_bps", case.name),
        sell_slippage_bps: f64_field(costs_config, "sell_slippage_bps", case.name),
        buy_tax_rate: f64_field(costs_config, "buy_tax_rate", case.name),
        sell_tax_rate: f64_field(costs_config, "sell_tax_rate", case.name),
    };

    let universe = StaticUniverse::new("all", panel.instruments.clone())
        .unwrap_or_else(|error| panic!("{}: create static universe: {error}", case.name));
    universe
        .store(lake.path())
        .unwrap_or_else(|error| panic!("{}: store static universe: {error}", case.name));
    let universe_id = universe
        .id()
        .unwrap_or_else(|error| panic!("{}: identify static universe: {error}", case.name));
    let cache = FactorCache::open(lake.path())
        .unwrap_or_else(|error| panic!("{}: open factor cache: {error}", case.name));
    let root_values = cache
        .get_or_compute(
            &panel,
            universe.members(),
            &universe_id,
            &dsv,
            &score,
            AvailabilityAssumption::None,
        )
        .unwrap_or_else(|error| panic!("{}: compute rotation score tree: {error}", case.name));

    let mut actual_factors = Map::new();
    actual_factors.insert(factor_key(&score), factor_rows(&root_values));
    for factor in FactorGraph::resolve(score.clone())
        .unwrap_or_else(|error| panic!("{}: resolve factor tree: {error}", case.name))
        .into_iter()
        .filter(|factor| factor != &score)
    {
        let values = cache
            .get_or_compute(
                &panel,
                universe.members(),
                &universe_id,
                &dsv,
                &factor,
                AvailabilityAssumption::None,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "{}: load cached {} values: {error}",
                    case.name,
                    factor_key(&factor)
                )
            });
        actual_factors.insert(factor_key(&factor), factor_rows(&values));
    }

    let decisions = strategy
        .decide(&root_values, &panel.sessions)
        .unwrap_or_else(|error| panic!("{}: decide: {error}", case.name));
    let events = execute(&decisions, &panel.sessions, |instrument, session| {
        executable(&panel, &statuses, instrument, session)
            .unwrap_or_else(|error| panic!("{}: execution check: {error}", case.name))
    });
    let result = run_vector(
        &panel,
        &decisions,
        &events,
        &costs,
        AvailabilityAssumption::None,
    )
    .unwrap_or_else(|error| panic!("{}: run vector: {error}", case.name));
    let actual_vector = serde_json::to_value(result)
        .unwrap_or_else(|error| panic!("{}: serialize vector result: {error}", case.name));

    assert_json_matches(
        &Value::Object(actual_factors),
        expected
            .get("factors")
            .unwrap_or_else(|| panic!("{}: expected factors are missing", case.name)),
        "factors",
        &RecordContext::default(),
        &tolerances,
        case.name,
    );
    assert_json_matches(
        &actual_vector,
        expected
            .get("vector")
            .unwrap_or_else(|| panic!("{}: expected vector result is missing", case.name)),
        "vector",
        &RecordContext::default(),
        &tolerances,
        case.name,
    );
}

fn assert_three_by_ten_edge_cases(panel: &Panel, statuses: &prajna_research::ExecutionStatusMap) {
    let missing_id: InstrumentId = "B.SYNTH".parse().expect("valid instrument ID");
    let unknown_id: InstrumentId = "C.SYNTH".parse().expect("valid instrument ID");
    let halted_id: InstrumentId = "B.SYNTH".parse().expect("valid instrument ID");
    let missing_session = panel_session(panel, "2026-01-16");
    let unknown_session = panel_session(panel, "2026-01-08");
    let halted_session = panel_session(panel, "2026-01-13");

    let instrument_index = panel
        .instruments
        .binary_search(&missing_id)
        .expect("missing-bar instrument belongs to universe");
    let session_index = panel
        .sessions
        .binary_search_by_key(&missing_session.session_date, |session| {
            session.session_date
        })
        .expect("missing-bar session belongs to panel");
    let grid_row = instrument_index * panel.sessions.len() + session_index;
    assert_eq!(
        panel
            .grid
            .column("has_bar")
            .expect("panel has bar flag")
            .bool()
            .expect("bar flag is boolean")
            .get(grid_row),
        Some(false),
        "named missing-bar assertion: B.SYNTH has no bar on 2026-01-16"
    );
    assert!(
        !executable(panel, statuses, &missing_id, &missing_session)
            .expect("missing-bar execution check"),
        "named missing-bar assertion: B.SYNTH must not execute on 2026-01-16"
    );

    let unknown = statuses
        .get(&(unknown_id.clone(), unknown_session.session_date))
        .expect("named UNKNOWN status row exists");
    assert_eq!(
        unknown.trade_status, "UNKNOWN",
        "named UNKNOWN assertion: C.SYNTH on 2026-01-08"
    );
    assert!(
        !executable(panel, statuses, &unknown_id, &unknown_session)
            .expect("UNKNOWN execution check"),
        "named UNKNOWN assertion: C.SYNTH must not execute on 2026-01-08"
    );

    let halted = statuses
        .get(&(halted_id.clone(), halted_session.session_date))
        .expect("named HALTED status row exists");
    assert_eq!(
        halted.trade_status, "HALTED",
        "named HALTED assertion: B.SYNTH on 2026-01-13"
    );
    assert!(
        !executable(panel, statuses, &halted_id, &halted_session).expect("HALTED execution check"),
        "named HALTED assertion: B.SYNTH must not execute on 2026-01-13"
    );
}

fn panel_session(panel: &Panel, date: &str) -> prajna_research::PanelSession {
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").expect("valid ISO date");
    panel
        .sessions
        .iter()
        .find(|session| session.session_date == date)
        .expect("fixture session exists")
        .clone()
}

fn factor_key(factor: &Factor) -> String {
    match factor.params() {
        FactorParams::Momentum(params) => format!("momentum({})", params.n),
        FactorParams::Volatility(params) => format!("volatility({})", params.n),
        FactorParams::TrendFilter(params) => format!("trend_filter({})", params.n),
        FactorParams::RotationScore(_) => factor.kind().as_str().to_owned(),
    }
}

fn factor_rows(values: &DataFrame) -> Value {
    let ids = values
        .column("instrument_id")
        .expect("factor values have instrument IDs")
        .str()
        .expect("instrument IDs are strings");
    let dates = values
        .column("session_date")
        .expect("factor values have session dates")
        .date()
        .expect("session dates are dates")
        .physical();
    let scores = values
        .column("value")
        .expect("factor values have scores")
        .f64()
        .expect("factor scores are floats");
    let available_at = values
        .column("available_at")
        .expect("factor values have availability timestamps")
        .datetime()
        .expect("availability values are datetimes")
        .physical();
    let statuses = values
        .column("status")
        .expect("factor values have statuses")
        .str()
        .expect("factor statuses are strings");

    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("Unix epoch");
    Value::Array(
        (0..values.height())
            .map(|row| {
                let date = epoch
                    .checked_add_signed(Duration::days(i64::from(
                        dates.get(row).expect("session date is non-null"),
                    )))
                    .expect("session date fits chrono range");
                json!({
                    "available_at": available_at
                        .get(row)
                        .map(format_timestamp),
                    "instrument_id": ids.get(row).expect("instrument ID is non-null"),
                    "session_date": date.to_string(),
                    "status": statuses.get(row).expect("factor status is non-null"),
                    "value": scores.get(row),
                })
            })
            .collect(),
    )
}

fn format_timestamp(nanoseconds: i64) -> String {
    DateTime::<Utc>::from_timestamp(
        nanoseconds.div_euclid(1_000_000_000),
        nanoseconds.rem_euclid(1_000_000_000) as u32,
    )
    .expect("factor availability timestamp fits chrono range")
    .to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

fn u32_field(object: &Map<String, Value>, key: &str, case: &str) -> u32 {
    object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_else(|| panic!("{case}: fixture field {key} must be a u32"))
}

fn f64_field(object: &Map<String, Value>, key: &str, case: &str) -> f64 {
    object
        .get(key)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("{case}: fixture field {key} must be numeric"))
}

fn tolerances(expected: &Value, case: &str) -> Tolerances {
    let values = expected
        .get("tolerance")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{case}: expected tolerance object is missing"));
    Tolerances {
        factor_abs: f64_field(values, "factor_abs", case),
        nav_abs: f64_field(values, "nav_abs", case),
    }
}

fn assert_json_matches(
    actual: &Value,
    expected: &Value,
    path: &str,
    context: &RecordContext,
    tolerances: &Tolerances,
    case: &str,
) {
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => {
            let context = extend_context(context, actual, expected);
            let actual_keys = actual.keys().collect::<Vec<_>>();
            let expected_keys = expected.keys().collect::<Vec<_>>();
            if actual_keys != expected_keys {
                mismatch(
                    case,
                    path,
                    &context,
                    &format!("object keys actual {actual_keys:?}, expected {expected_keys:?}"),
                );
            }
            for (key, expected_value) in expected {
                assert_json_matches(
                    actual.get(key).expect("object keys were checked"),
                    expected_value,
                    &format!("{path}.{key}"),
                    &context,
                    tolerances,
                    case,
                );
            }
        }
        (Value::Array(actual), Value::Array(expected)) => {
            if actual.len() != expected.len() {
                mismatch(
                    case,
                    path,
                    context,
                    &format!(
                        "array length actual {}, expected {}",
                        actual.len(),
                        expected.len()
                    ),
                );
            }
            for (index, (actual_value, expected_value)) in actual.iter().zip(expected).enumerate() {
                assert_json_matches(
                    actual_value,
                    expected_value,
                    &format!("{path}[{index}]"),
                    context,
                    tolerances,
                    case,
                );
            }
        }
        (Value::Number(actual), Value::Number(expected)) => {
            compare_numbers(actual, expected, path, context, tolerances, case);
        }
        _ if actual == expected => {}
        _ => mismatch(
            case,
            path,
            context,
            &format!("actual {actual}, expected {expected}"),
        ),
    }
}

fn extend_context(
    context: &RecordContext,
    actual: &Map<String, Value>,
    expected: &Map<String, Value>,
) -> RecordContext {
    let context_value = |key: &str| {
        let actual_value = actual.get(key).and_then(Value::as_str).map(str::to_owned);
        let expected_value = expected.get(key).and_then(Value::as_str).map(str::to_owned);
        if actual_value.is_some() || expected_value.is_some() {
            Some((actual_value, expected_value))
        } else {
            None
        }
    };
    let session = context_value("session_date")
        .or_else(|| context_value("decision_session"))
        .or_else(|| context.session.clone());
    RecordContext {
        instrument_id: context_value("instrument_id").or_else(|| context.instrument_id.clone()),
        session,
    }
}

fn compare_numbers(
    actual: &Number,
    expected: &Number,
    path: &str,
    context: &RecordContext,
    tolerances: &Tolerances,
    case: &str,
) {
    if !actual.is_f64() && !expected.is_f64() {
        if actual != expected {
            mismatch(
                case,
                path,
                context,
                &format!("actual {actual}, expected {expected}"),
            );
        }
        return;
    }

    let actual_value = actual.as_f64().expect("JSON float converts to f64");
    let expected_value = expected.as_f64().expect("JSON float converts to f64");
    let tolerance = if path.starts_with("factors.")
        || (path.starts_with("vector.decisions[")
            && path.contains(".ranked[")
            && path.ends_with(".score"))
    {
        tolerances.factor_abs
    } else {
        tolerances.nav_abs
    };
    let difference = (actual_value - expected_value).abs();
    if difference > tolerance {
        mismatch(
            case,
            path,
            context,
            &format!(
                "actual {actual_value}, expected {expected_value}, absolute difference {difference} exceeds tolerance {tolerance}"
            ),
        );
    }
}

fn mismatch(case: &str, path: &str, context: &RecordContext, message: &str) -> ! {
    let mut details = Vec::new();
    if let Some((actual, expected)) = &context.instrument_id {
        details.push(format!(
            "instrument_id actual {}, expected {}",
            actual.as_deref().unwrap_or("<missing>"),
            expected.as_deref().unwrap_or("<missing>")
        ));
    }
    if let Some((actual, expected)) = &context.session {
        details.push(format!(
            "session actual {}, expected {}",
            actual.as_deref().unwrap_or("<missing>"),
            expected.as_deref().unwrap_or("<missing>")
        ));
    }
    let context = if details.is_empty() {
        String::new()
    } else {
        format!(" ({})", details.join(", "))
    };
    panic!("{case}: first mismatch at {path}{context}: {message}");
}
