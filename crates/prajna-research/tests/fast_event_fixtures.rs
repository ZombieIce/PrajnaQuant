mod common;

use std::collections::BTreeMap;

use chrono::{Datelike, Duration, NaiveDate};
use prajna_account::{Side, VirtualPortfolioId};
use prajna_domain::{Currency, InstrumentId, VenueId};
use prajna_research::{
    Panel,
    fast_event::{
        FastEventConfig, FastEventCosts, PortfolioRun, PriceField, Rebalance, Sizing,
        run_fast_event,
    },
    load_execution_status, load_panel,
    strategy::{Decision, ExecutionEvent, UnfilledEntry},
};
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("../../../poc/poc0-benchmark/fixtures/dataset-v1.json");
const MA: &str = include_str!("../../../poc/poc0-benchmark/fixtures/b2-ma20-60-v1.json");

fn config(panel: &Panel, sizing: Sizing) -> FastEventConfig {
    FastEventConfig {
        currency: Currency::new("CNY").unwrap(),
        venue: VenueId::new("SYNTH").unwrap(),
        initial_cash: "100000".parse().unwrap(),
        sizing,
        costs: FastEventCosts {
            commission_rate: "0.001".parse().unwrap(),
            buy_slippage_rate: "0.001".parse().unwrap(),
            sell_slippage_rate: "0.001".parse().unwrap(),
            minimum_commission: "100".parse().unwrap(),
            ..FastEventCosts::ZERO
        },
        lot_sizes: panel
            .instruments
            .iter()
            .cloned()
            .map(|id| (id, "100".parse().unwrap()))
            .collect(),
    }
}

fn decision(panel: &Panel, index: usize, targets: BTreeMap<InstrumentId, f64>) -> Decision {
    Decision {
        decision_session: panel.sessions[index].clone(),
        ranked: vec![],
        targets,
    }
}

fn golden_decisions(panel: &Panel, expected: &str) -> Vec<Decision> {
    let golden: Value = serde_json::from_str(expected).unwrap();
    golden["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let date = NaiveDate::parse_from_str(row["session_date"].as_str().unwrap(), "%Y-%m-%d")
                .unwrap();
            let index = panel
                .sessions
                .iter()
                .position(|s| s.session_date == date)
                .unwrap();
            decision(
                panel,
                index,
                row["targets"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(id, w)| (id.parse().unwrap(), w.as_f64().unwrap()))
                    .collect(),
            )
        })
        .collect()
}

fn ma_fixture() -> Vec<u8> {
    let source: Value = serde_json::from_str(MA).unwrap();
    let mut fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let flat = source["flat_sessions"].as_u64().unwrap() as usize;
    let high = source["high_sessions"].as_u64().unwrap() as usize;
    let count = flat + high + source["low_sessions"].as_u64().unwrap() as usize;
    let mut date =
        NaiveDate::parse_from_str(source["start_date"].as_str().unwrap(), "%Y-%m-%d").unwrap();
    let mut calendar = Vec::new();
    while calendar.len() < count {
        if date.weekday().num_days_from_monday() < 5 {
            calendar.push(date.to_string());
        }
        date += Duration::days(1);
    }
    fixture["calendar"] = json!(calendar);
    for instrument in fixture["instruments"].as_array_mut().unwrap() {
        let rising = instrument["symbol"] == source["rising_symbol"];
        instrument["closes"] = Value::Array(
            (0..count)
                .map(|i| {
                    source[if !rising || i < flat {
                        "flat_close"
                    } else if i < flat + high {
                        "high_close"
                    } else {
                        "low_close"
                    }]
                    .clone()
                })
                .collect(),
        );
    }
    fixture["missing_bars"] = json!([]);
    fixture["execution_status_overrides"] = json!([]);
    serde_json::to_vec(&fixture).unwrap()
}

#[test]
fn s1_s2_s3_decision_tapes_have_exact_two_level_identities_at_both_daily_marks() {
    let goldens = [
        include_str!("../../../poc/mvp3-golden/expected/dataset-v1.s1.vector_parity.json"),
        include_str!("../../../poc/mvp3-golden/expected/dataset-v1.s2.vector_parity.json"),
        include_str!("../../../poc/mvp3-golden/expected/dataset-v1.s3.vector_parity.json"),
    ];
    for count in [10, 130] {
        let lake = tempfile::tempdir().unwrap();
        let dsv = if count == 10 {
            common::publish_version(lake.path(), "3")
        } else {
            common::publish_fixture(lake.path(), &ma_fixture(), "3")
        };
        let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
        let statuses = load_execution_status(lake.path(), &dsv).unwrap();
        let equal = || {
            panel
                .instruments
                .iter()
                .cloned()
                .map(|id| (id, 1.0 / 3.0))
                .collect()
        };
        for (strategy, golden) in goldens.iter().enumerate() {
            let decisions = if count == 10 {
                golden_decisions(&panel, golden)
            } else {
                match strategy {
                    0 => vec![decision(&panel, 0, equal())],
                    // This fixture's S2 scores tie at zero except the high/low transition.
                    // Descending rank / ascending ID selects A, except the drop on index 80.
                    1 => (2..count)
                        .map(|i| {
                            decision(
                                &panel,
                                i,
                                BTreeMap::from([(
                                    if i == 80 { "B.SYNTH" } else { "A.SYNTH" }.parse().unwrap(),
                                    1.0,
                                )]),
                            )
                        })
                        .collect(),
                    _ => golden_decisions(
                        &panel,
                        include_str!(
                            "../../../poc/mvp3-golden/expected/b2-ma20-60-v1.s3.vector_parity.json"
                        ),
                    ),
                }
            };
            for sizing in [Sizing::VectorParity, Sizing::Lot] {
                let result = run_fast_event(
                    &panel,
                    &statuses,
                    &config(&panel, sizing),
                    &[PortfolioRun {
                        id: VirtualPortfolioId(1),
                        capital: "100000".parse().unwrap(),
                        decisions: decisions.clone(),
                        unfilled_entry: if strategy == 0 {
                            UnfilledEntry::Retry
                        } else {
                            UnfilledEntry::Skip
                        },
                        rebalance: Rebalance::FullTarget,
                    }],
                )
                .unwrap_or_else(|error| {
                    panic!("{count} Sessions / S{} / {sizing:?}: {error}", strategy + 1)
                });
                assert_eq!(result.sessions.len(), count);
                for session in &result.sessions {
                    session.open_after.validate().unwrap();
                    session.close.validate().unwrap();
                }
                assert!(
                    result
                        .fills
                        .iter()
                        .all(|f| f.fill.executed_at() == f.fill.order().submitted_at())
                );
                if count == 130 && strategy == 2 {
                    assert_eq!(result.fills.len(), 2);
                    assert_eq!(
                        result.fills[0].fill.executed_at(),
                        panel.sessions[61].ts_open
                    );
                    assert_eq!(
                        result.fills[1].fill.executed_at(),
                        panel.sessions[86].ts_open
                    );
                    assert_eq!(result.fills[1].fill.order().side(), Side::Sell);
                }
            }
        }
    }
}

#[test]
fn missing_marks_carry_same_field_and_halted_holdings_defer_the_whole_attempt() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let b: InstrumentId = "B.SYNTH".parse().unwrap();
    let result = run_fast_event(
        &panel,
        &statuses,
        &config(&panel, Sizing::Lot),
        &[PortfolioRun {
            id: VirtualPortfolioId(1),
            capital: "100000".parse().unwrap(),
            decisions: vec![
                decision(&panel, 0, BTreeMap::from([(b.clone(), 1.0)])),
                decision(
                    &panel,
                    5,
                    BTreeMap::from([("A.SYNTH".parse().unwrap(), 1.0)]),
                ),
                decision(&panel, 7, BTreeMap::from([(b.clone(), 1.0)])),
                decision(&panel, 8, BTreeMap::new()),
            ],
            unfilled_entry: UnfilledEntry::Skip,
            rebalance: Rebalance::FullTarget,
        }],
    )
    .unwrap();
    assert!(result.attempts.iter().any(|a| matches!(&a.event,
        ExecutionEvent::Deferred { attempt_session, blocked, .. }
        if attempt_session == &panel.sessions[6] && blocked.contains(&b))));
    assert!(
        !result
            .fills
            .iter()
            .any(|f| f.fill.executed_at() == panel.sessions[6].ts_open
                || f.fill.executed_at() == panel.sessions[9].ts_open)
    );
    let last = &result.sessions[9];
    assert_eq!(
        last.open_sources[&b].session_date,
        panel.sessions[8].session_date
    );
    assert_eq!(last.open_sources[&b].field, PriceField::Open);
    assert_eq!(
        last.close_sources[&b].session_date,
        panel.sessions[8].session_date
    );
    assert_eq!(last.close_sources[&b].field, PriceField::Close);
    assert_eq!(
        last.open_after.account.positions[&b].price,
        "100".parse().unwrap()
    );
    assert_eq!(
        last.close.account.positions[&b].price,
        "99".parse().unwrap()
    );
    assert!(last.close.account.equity > "0".parse().unwrap());
}

#[test]
fn f8_fee_only_leg_preserves_quantity_and_charges_exact_turnover_cost() {
    let lake = tempfile::tempdir().unwrap();
    let mut fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    fixture["bar_defaults"]["open"] = json!(1);
    let dsv = common::publish_fixture(lake.path(), &serde_json::to_vec(&fixture).unwrap(), "3");
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let mut parameters = config(&panel, Sizing::VectorParity);
    parameters.initial_cash = "1000".parse().unwrap();
    parameters.costs = FastEventCosts {
        commission_rate: "0.01".parse().unwrap(),
        ..FastEventCosts::ZERO
    };
    let a: InstrumentId = "A.SYNTH".parse().unwrap();
    let result = run_fast_event(
        &panel,
        &statuses,
        &parameters,
        &[PortfolioRun {
            id: VirtualPortfolioId(1),
            capital: "1000".parse().unwrap(),
            decisions: vec![
                decision(&panel, 0, BTreeMap::from([(a.clone(), 0.4975)])),
                decision(
                    &panel,
                    1,
                    BTreeMap::from([(a.clone(), 0.5), ("B.SYNTH".parse().unwrap(), 0.4975)]),
                ),
            ],
            unfilled_entry: UnfilledEntry::Skip,
            rebalance: Rebalance::FullTarget,
        }],
    )
    .unwrap();
    assert_eq!(
        result.sessions[1].open_after.account.equity,
        "995.025".parse().unwrap()
    );
    assert_eq!(
        result.sessions[2].open_after.account.equity,
        "990.049875".parse().unwrap()
    );
    assert_eq!(
        result.sessions[2].open_after.account.positions[&a].quantity,
        "495.0249375".parse().unwrap()
    );
    assert_eq!(result.fills.len(), 2);
    assert_eq!(result.account.fee_settlements().len(), 1);
    let settlement = &result.account.fee_settlements()[0];
    assert_eq!(settlement.instrument_id(), &a);
    assert_eq!(settlement.total_fees(), "0.024875625".parse().unwrap());
    assert!(
        result
            .account
            .orders()
            .values()
            .all(|o| o.quantity() > prajna_domain::Quantity::ZERO)
    );
}

#[test]
fn representable_tiny_target_is_not_suppressed_by_an_absolute_weight_threshold() {
    let lake = tempfile::tempdir().unwrap();
    let mut fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    fixture["bar_defaults"]["open"] = json!(1);
    let dsv = common::publish_fixture(lake.path(), &serde_json::to_vec(&fixture).unwrap(), "3");
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let mut parameters = config(&panel, Sizing::VectorParity);
    parameters.initial_cash = "1000000000".parse().unwrap();
    parameters.costs = FastEventCosts {
        commission_rate: "0.01".parse().unwrap(),
        ..FastEventCosts::ZERO
    };
    let result = run_fast_event(
        &panel,
        &statuses,
        &parameters,
        &[PortfolioRun {
            id: VirtualPortfolioId(1),
            capital: "1000000000".parse().unwrap(),
            decisions: vec![decision(
                &panel,
                0,
                BTreeMap::from([("A.SYNTH".parse().unwrap(), 1e-12)]),
            )],
            unfilled_entry: UnfilledEntry::Skip,
            rebalance: Rebalance::FullTarget,
        }],
    )
    .unwrap();
    assert_eq!(result.fills.len(), 1);
    assert_eq!(
        result.fills[0].fill.total_fees(),
        "0.00001".parse().unwrap()
    );
    assert_eq!(
        result.fills[0].fill.order().quantity(),
        "0.00099999999999999".parse().unwrap()
    );
}

#[test]
fn panel_preserves_scale_18_source_prices_which_f64_cannot_represent() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    let venue = VenueId::new("SYNTH").unwrap();
    let original = load_panel(lake.path(), &dsv, &venue).unwrap();
    let mut fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    fixture["bar_defaults"]["open"] = serde_json::from_str("100.000000000000000001").unwrap();
    for instrument in fixture["instruments"].as_array_mut().unwrap() {
        instrument["price_precision"] = json!(18);
    }
    let dsv = common::publish_fixture(lake.path(), &serde_json::to_vec(&fixture).unwrap(), "3");
    let panel = load_panel(lake.path(), &dsv, &venue).unwrap();
    assert_eq!(
        panel.grid.column("open").unwrap().f64().unwrap().get(0),
        original.grid.column("open").unwrap().f64().unwrap().get(0)
    );
    assert_eq!(
        panel
            .grid
            .column("open_decimal")
            .unwrap()
            .str()
            .unwrap()
            .get(0),
        Some("100.000000000000000001")
    );
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let result = run_fast_event(
        &panel,
        &statuses,
        &config(&panel, Sizing::Lot),
        &[PortfolioRun {
            id: VirtualPortfolioId(1),
            capital: "100000".parse().unwrap(),
            decisions: vec![decision(
                &panel,
                0,
                BTreeMap::from([("A.SYNTH".parse().unwrap(), 1.0)]),
            )],
            unfilled_entry: UnfilledEntry::Skip,
            rebalance: Rebalance::FullTarget,
        }],
    )
    .unwrap();
    assert_eq!(
        result.fills[0].fill.raw_price(),
        "100.000000000000000001".parse().unwrap()
    );
}
