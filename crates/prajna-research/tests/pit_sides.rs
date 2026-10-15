#[path = "common/pit.rs"]
mod fixture;

use prajna_account::VirtualPortfolioId;
use prajna_domain::{InstrumentId, Quantity};
use prajna_research::{
    etf_market::{DerivedMarket, MarketEvidence},
    fast_event::{
        FastEventConfig, FastEventCosts, PortfolioRun, Rebalance, Sizing, run_fast_event_pit,
    },
    pit_strategy::{self, RiskSignals},
    strategy::{Decision, UnfilledEntry},
    vector::{VectorCosts, run_vector_pit},
};
use std::collections::BTreeMap;

fn case(
    initial: f64,
    target: BTreeMap<InstrumentId, f64>,
    lower: bool,
    commission: &str,
    expected: [(&str, &str, &str); 2],
    deferred: bool,
) {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    assert_eq!(f.market.sessions().len(), 10);
    assert!(!f.tri.version().is_empty());
    assert!(!f.risk.signals.is_empty());
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let mut evidence: MarketEvidence = serde_json::from_value(f.input["market"].clone()).unwrap();
    for cell in &mut evidence.openings {
        if cell.instrument_id == f.ids[0] && cell.session_date == fixture::date(4) {
            cell.reference.as_mut().unwrap().adjusted_previous_close =
                if lower { "1.111" } else { "0.909" }.parse().unwrap();
        }
    }
    let market = DerivedMarket::from_dataset(lake.path(), &f.dsv, &f.catalog, evidence).unwrap();
    let targets = target
        .into_iter()
        .map(|(id, w)| {
            let index = if id.symbol() == "A" { 0 } else { 1 };
            (f.ids[index].clone(), w)
        })
        .collect();
    let decisions = vec![
        Decision {
            decision_session: panel.sessions[0].clone(),
            ranked: vec![],
            targets: BTreeMap::from([(f.ids[0].clone(), initial)]),
        },
        Decision {
            decision_session: panel.sessions[2].clone(),
            ranked: vec![],
            targets,
        },
    ];
    let mut parity_nav = Vec::new();
    for (i, sizing) in [Sizing::Lot, Sizing::VectorParity].into_iter().enumerate() {
        let config = FastEventConfig {
            currency: "CNY".parse().unwrap(),
            venue: "SZSE".parse().unwrap(),
            initial_cash: "10000".parse().unwrap(),
            sizing,
            costs: FastEventCosts {
                commission_rate: commission.parse().unwrap(),
                ..FastEventCosts::ZERO
            },
            lot_sizes: f
                .ids
                .iter()
                .map(|id| (id.clone(), "100".parse().unwrap()))
                .collect(),
        };
        let result = run_fast_event_pit(
            &panel,
            &market,
            &RiskSignals::default(),
            &config,
            &[PortfolioRun {
                id: VirtualPortfolioId(1),
                capital: config.initial_cash,
                decisions: decisions.clone(),
                unfilled_entry: UnfilledEntry::Skip,
                rebalance: Rebalance::FullTarget,
            }],
        )
        .unwrap();
        let positions = &result.sessions[3].open_after.portfolios[&VirtualPortfolioId(1)].positions;
        assert_eq!(
            positions.get(&f.ids[0]).map(|p| p.quantity),
            Some(expected[i].0.parse::<Quantity>().unwrap()),
            "{sizing:?}"
        );
        assert_eq!(
            positions
                .get(&f.ids[1])
                .map_or(Quantity::ZERO, |p| p.quantity),
            expected[i].1.parse::<Quantity>().unwrap(),
            "{sizing:?}"
        );
        assert_eq!(
            result.sessions[3].open_after.account.cash,
            expected[i].2.parse().unwrap(),
            "{sizing:?}"
        );
        assert_eq!(
            result.attempts.iter().any(|a| matches!(&a.event,
            prajna_research::strategy::ExecutionEvent::Deferred {attempt_session,..}
                if attempt_session.session_date == fixture::date(4))),
            deferred,
            "{sizing:?}"
        );
        if deferred {
            assert!(
                result
                    .fills
                    .iter()
                    .all(|f| f.fill.executed_at() != panel.sessions[3].ts_open)
            );
        }
        for session in &result.sessions {
            session.open_after.validate().unwrap();
            session.close.validate().unwrap();
            if sizing == Sizing::VectorParity {
                parity_nav.push(
                    session
                        .open_after
                        .account
                        .equity
                        .to_string()
                        .parse::<f64>()
                        .unwrap(),
                );
            }
        }
    }
    let vector = run_vector_pit(
        &panel,
        &decisions,
        &market,
        &RiskSignals::default(),
        &VectorCosts {
            commission_rate: commission.parse().unwrap(),
            buy_slippage_bps: 0.0,
            sell_slippage_bps: 0.0,
            buy_tax_rate: 0.0,
            sell_tax_rate: 0.0,
        },
    )
    .unwrap();
    let (a, b, cash) = expected[1];
    let a = a.parse::<f64>().unwrap();
    let b = b.parse::<f64>().unwrap();
    let equity = a + b + cash.parse::<f64>().unwrap();
    assert!(
        (vector.sessions[3]
            .weights_after_execution
            .get(&f.ids[0])
            .copied()
            .unwrap_or(0.0)
            - a / equity)
            .abs()
            < 1e-12
    );
    assert!(
        (vector.sessions[3]
            .weights_after_execution
            .get(&f.ids[1])
            .copied()
            .unwrap_or(0.0)
            - b / equity)
            .abs()
            < 1e-12
    );
    assert!(
        (vector.sessions[3].nav / (1.0 + vector.sessions[3].gross_return) * 10000.0 - equity).abs()
            < 1e-8
    );
    assert_eq!(
        vector
            .executions
            .iter()
            .any(|e| e.session_date == fixture::date(4).to_string()
                && e.kind == prajna_research::vector::VectorExecutionKind::Deferred),
        deferred
    );
    for (session, fast_equity) in vector.sessions.iter().zip(parity_nav) {
        assert!((session.nav / (1.0 + session.gross_return) * 10000.0 - fast_equity).abs() < 1e-8);
    }
}

#[test]
fn upper_limit_held_reduction_retains_target_quantity() {
    case(
        0.8,
        BTreeMap::from([
            ("A.SZSE".parse().unwrap(), 0.5),
            ("B.SZSE".parse().unwrap(), 0.5),
        ]),
        false,
        "0",
        [("5000", "5000", "0"); 2],
        false,
    );
}

#[test]
fn lower_limit_buy_only_does_not_require_sell_permission() {
    case(
        0.5,
        BTreeMap::from([("A.SZSE".parse().unwrap(), 1.0)]),
        true,
        "0",
        [("10000", "0", "0"); 2],
        false,
    );
}

#[test]
fn blocked_buy_increase_keeps_existing_quantity_instead_of_liquidating() {
    case(
        0.5,
        BTreeMap::from([("A.SZSE".parse().unwrap(), 1.0)]),
        false,
        "0",
        [("5000", "0", "5000"); 2],
        false,
    );
}

#[test]
fn upper_limit_reduction_sizes_after_fees_and_lot_rounding() {
    case(
        0.8,
        BTreeMap::from([
            ("A.SZSE".parse().unwrap(), 0.5),
            ("B.SZSE".parse().unwrap(), 0.5),
        ]),
        false,
        "0.01",
        [("4900", "4900", "40"), ("4920.32", "4920.32", "0")],
        false,
    );
}

#[test]
fn lower_limit_increase_remains_buy_only_after_fees() {
    case(
        0.5,
        BTreeMap::from([("A.SZSE".parse().unwrap(), 1.0)]),
        true,
        "0.01",
        [("9900", "0", "1"), ("9900.25", "0", "0")],
        false,
    );
}

#[test]
fn blocked_increment_freezes_quantity_without_charging_skipped_fees() {
    case(
        0.5,
        BTreeMap::from([("A.SZSE".parse().unwrap(), 1.0)]),
        false,
        "0.01",
        [("5000", "0", "4950"), ("4975", "0", "4975")],
        false,
    );
}

#[test]
fn blocked_increment_removes_its_fee_and_resizes_the_other_buy() {
    case(
        0.5,
        BTreeMap::from([
            ("A.SZSE".parse().unwrap(), 0.8),
            ("B.SZSE".parse().unwrap(), 0.2),
        ]),
        false,
        "0.01",
        [("5000", "1900", "3031"), ("4975", "1986.02", "2969.08")],
        false,
    );
}

#[test]
fn fees_can_turn_nominal_weight_increase_into_a_blocked_required_sell() {
    case(
        0.5,
        BTreeMap::from([
            ("A.SZSE".parse().unwrap(), 0.5001),
            ("B.SZSE".parse().unwrap(), 0.4999),
        ]),
        true,
        "0.01",
        [("5000", "0", "4950"), ("4975", "0", "4975")],
        true,
    );
}

#[test]
fn lower_limit_reduction_defers_all_legs_without_fees() {
    case(
        0.8,
        BTreeMap::from([
            ("A.SZSE".parse().unwrap(), 0.5),
            ("B.SZSE".parse().unwrap(), 0.5),
        ]),
        true,
        "0",
        [("8000", "0", "2000"); 2],
        true,
    );
}

#[test]
fn unchanged_lower_limit_holding_needs_no_sell_permission() {
    case(
        0.5,
        BTreeMap::from([("A.SZSE".parse().unwrap(), 0.5)]),
        true,
        "0",
        [("5000", "0", "5000"); 2],
        false,
    );
}

#[test]
fn retry_only_buys_the_missing_absolute_increment_and_keeps_other_holdings() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let mut evidence: MarketEvidence = serde_json::from_value(f.input["market"].clone()).unwrap();
    for cell in &mut evidence.openings {
        if cell.instrument_id == f.ids[0] {
            if cell.session_date == fixture::date(4) {
                cell.reference.as_mut().unwrap().adjusted_previous_close = "0.909".parse().unwrap();
            }
            if cell.session_date == fixture::date(5) {
                cell.halt = None;
            }
        }
    }
    let market = DerivedMarket::from_dataset(lake.path(), &f.dsv, &f.catalog, evidence).unwrap();
    let decisions = vec![
        Decision {
            decision_session: panel.sessions[0].clone(),
            ranked: vec![],
            targets: BTreeMap::from([(f.ids[0].clone(), 0.5), (f.ids[1].clone(), 0.2)]),
        },
        Decision {
            decision_session: panel.sessions[2].clone(),
            ranked: vec![],
            targets: BTreeMap::from([(f.ids[0].clone(), 0.7), (f.ids[1].clone(), 0.3)]),
        },
    ];
    for sizing in [Sizing::Lot, Sizing::VectorParity] {
        let config = FastEventConfig {
            currency: "CNY".parse().unwrap(),
            venue: "SZSE".parse().unwrap(),
            initial_cash: "10000".parse().unwrap(),
            sizing,
            costs: FastEventCosts::ZERO,
            lot_sizes: f
                .ids
                .iter()
                .map(|id| (id.clone(), "100".parse().unwrap()))
                .collect(),
        };
        let result = run_fast_event_pit(
            &panel,
            &market,
            &RiskSignals::default(),
            &config,
            &[PortfolioRun {
                id: VirtualPortfolioId(1),
                capital: config.initial_cash,
                decisions: decisions.clone(),
                unfilled_entry: UnfilledEntry::Retry,
                rebalance: Rebalance::FullTarget,
            }],
        )
        .unwrap();
        for (session, a, cash) in [(3, "5000", "2000"), (4, "7000", "0")] {
            let value = &result.sessions[session].open_after;
            let positions = &value.portfolios[&VirtualPortfolioId(1)].positions;
            assert_eq!(
                positions[&f.ids[0]].quantity,
                a.parse().unwrap(),
                "{sizing:?}"
            );
            assert_eq!(
                positions[&f.ids[1]].quantity,
                "3000".parse().unwrap(),
                "{sizing:?}"
            );
            assert_eq!(value.account.cash, cash.parse().unwrap(), "{sizing:?}");
            value.validate().unwrap();
        }
        let retried = result
            .fills
            .iter()
            .filter(|f| f.fill.executed_at() == panel.sessions[4].ts_open)
            .collect::<Vec<_>>();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].fill.order().instrument_id(), &f.ids[0]);
        assert_eq!(retried[0].fill.order().side(), prajna_account::Side::Buy);
        assert_eq!(retried[0].fill.order().quantity(), "2000".parse().unwrap());
    }
}
