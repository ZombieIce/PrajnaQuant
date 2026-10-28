mod common;

use prajna_account::Side;
use std::collections::{BTreeMap, BTreeSet};

use polars::prelude::{NamedFrom, Series};
use prajna_account::VirtualPortfolioId;
use prajna_domain::{Currency, Notional, VenueId};
use prajna_research::{
    fast_event::{
        EntryBudget, FastEventConfig, FastEventCosts, PortfolioRun, Rebalance, Sizing,
        run_fast_event,
    },
    load_execution_status, load_panel,
    strategy::{Decision, UnfilledEntry},
};

#[test]
fn parity_open_equity_is_exactly_pre_open_equity_less_f8_cost() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let venue = VenueId::new("SYNTH").unwrap();
    let panel = load_panel(lake.path(), &dsv, &venue).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let decision = Decision {
        decision_session: panel.sessions[0].clone(),
        ranked: vec![],
        targets: BTreeMap::from([("A.SYNTH".parse().unwrap(), 1.0)]),
    };
    let result = run_fast_event(
        &panel,
        &statuses,
        &FastEventConfig {
            currency: Currency::new("CNY").unwrap(),
            venues: BTreeSet::from([venue]),
            initial_cash: "1000".parse().unwrap(),
            sizing: Sizing::VectorParity,
            costs: FastEventCosts {
                commission_rate: "0.01".parse().unwrap(),
                buy_slippage_rate: "0.01".parse().unwrap(),
                buy_tax_rate: "0.02".parse().unwrap(),
                ..FastEventCosts::ZERO
            },
            lot_sizes: BTreeMap::new(),
        },
        &[PortfolioRun {
            id: VirtualPortfolioId(1),
            capital: "1000".parse().unwrap(),
            decisions: vec![decision],
            unfilled_entry: UnfilledEntry::Skip,
            rebalance: Rebalance::FullTarget,
        }],
    )
    .unwrap();
    assert_eq!(
        result.sessions[0].open_after.account.equity,
        "1000".parse().unwrap()
    );
    assert_eq!(
        result.sessions[1].open_after.account.equity,
        "960".parse().unwrap()
    );
    assert_eq!(result.sessions[1].open_after.account.cash, Notional::ZERO);
    assert_eq!(result.fills.len(), 1);
    assert_eq!(
        result.fills[0].fill.executed_at(),
        panel.sessions[1].ts_open
    );
    assert_eq!(result.fills[0].fee_basis, "1000".parse().unwrap());
    assert_eq!(result.fills[0].fill.total_fees(), "40".parse().unwrap());
}

#[test]
fn retry_cash_cut_is_bounded_even_when_one_quantity_quantum_cannot_change_principal() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let venue = VenueId::new("SYNTH").unwrap();
    let mut panel = load_panel(lake.path(), &dsv, &venue).unwrap();
    let mut statuses = load_execution_status(lake.path(), &dsv).unwrap();
    statuses
        .get_mut(&("B.SYNTH".parse().unwrap(), panel.sessions[1].session_date))
        .unwrap()
        .is_tradable = false;
    let opens = panel.grid.column("open_decimal").unwrap().str().unwrap();
    let tiny = (0..opens.len())
        .map(|i| opens.get(i).map(|_| "0.000000000000000001"))
        .collect::<Vec<_>>();
    panel
        .grid
        .with_column(Series::new("open_decimal".into(), tiny).into())
        .unwrap();
    let result = run_fast_event(
        &panel,
        &statuses,
        &FastEventConfig {
            currency: Currency::new("CNY").unwrap(),
            venues: BTreeSet::from([venue]),
            initial_cash: "0.000000000000000018".parse().unwrap(),
            sizing: Sizing::VectorParity,
            lot_sizes: BTreeMap::new(),
            costs: FastEventCosts {
                commission_rate: "0.1".parse().unwrap(),
                buy_slippage_rate: "0.1".parse().unwrap(),
                ..FastEventCosts::ZERO
            },
        },
        &[PortfolioRun {
            id: VirtualPortfolioId(1),
            capital: "0.000000000000000018".parse().unwrap(),
            decisions: vec![Decision {
                decision_session: panel.sessions[0].clone(),
                ranked: vec![],
                targets: BTreeMap::from([
                    ("A.SYNTH".parse().unwrap(), 0.5),
                    ("B.SYNTH".parse().unwrap(), 0.5),
                ]),
            }],
            unfilled_entry: UnfilledEntry::Retry,
            rebalance: Rebalance::FullTarget,
        }],
    )
    .unwrap();
    assert_eq!(
        result.fills[1].fill.order().quantity(),
        "6.5".parse().unwrap()
    );
    assert_eq!(result.sessions[2].open_after.account.cash, Notional::ZERO);
    assert_eq!(
        result.sessions[2].open_after.account.equity,
        "0.000000000000000014".parse().unwrap()
    );
}

#[test]
fn two_vps_have_independent_fills_without_double_counting_and_all_sells_precede_buys() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let venue = VenueId::new("SYNTH").unwrap();
    let panel = load_panel(lake.path(), &dsv, &venue).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let result = run_fast_event(
        &panel,
        &statuses,
        &FastEventConfig {
            currency: Currency::new("CNY").unwrap(),
            venues: BTreeSet::from([venue]),
            initial_cash: "120000".parse().unwrap(),
            sizing: Sizing::VectorParity,
            costs: FastEventCosts::ZERO,
            lot_sizes: BTreeMap::new(),
        },
        &[1, 2].map(|id| PortfolioRun {
            id: VirtualPortfolioId(id),
            capital: "50000".parse().unwrap(),
            decisions: [(0, "A.SYNTH"), (1, "B.SYNTH")]
                .map(|(index, instrument)| Decision {
                    decision_session: panel.sessions[index].clone(),
                    ranked: vec![],
                    targets: BTreeMap::from([(instrument.parse().unwrap(), 1.0)]),
                })
                .to_vec(),
            unfilled_entry: UnfilledEntry::Skip,
            rebalance: Rebalance::FullTarget,
        }),
    )
    .unwrap();
    assert_eq!(
        result.fills[0].fill.order().instrument_id(),
        result.fills[1].fill.order().instrument_id()
    );
    assert_ne!(
        result.fills[0].fill.order().portfolio_id(),
        result.fills[1].fill.order().portfolio_id()
    );
    let a = "A.SYNTH".parse().unwrap();
    assert_eq!(
        result.sessions[1].open_after.account.positions[&a].quantity,
        "1000".parse().unwrap()
    );
    assert_eq!(
        result.sessions[1].open_after.unallocated_capital,
        "20000".parse().unwrap()
    );
    assert_eq!(
        result.sessions[1].close.account.equity,
        "121000".parse().unwrap()
    );
    assert_eq!(
        result.fills[2..]
            .iter()
            .map(|f| f.fill.order().side())
            .collect::<Vec<_>>(),
        [Side::Sell, Side::Sell, Side::Buy, Side::Buy]
    );
    for session in &result.sessions {
        session.open_after.validate().unwrap();
        session.close.validate().unwrap();
    }
}

#[test]
fn lot_buys_reduce_one_lot_until_slippage_minimum_commission_and_tax_fit_cash() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let venue = VenueId::new("SYNTH").unwrap();
    let mut panel = load_panel(lake.path(), &dsv, &venue).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let opens = panel.grid.column("open").unwrap().f64().unwrap();
    let decimals = (0..opens.len())
        .map(|row| opens.get(row).map(|_| "10"))
        .collect::<Vec<_>>();
    panel
        .grid
        .with_column(Series::new("open_decimal".into(), decimals).into())
        .unwrap();
    let a: prajna_domain::InstrumentId = "A.SYNTH".parse().unwrap();
    for (minimum, cash, equity, top_up) in [
        ("2", "63.73", "963.73", "0"),
        ("20", "52.82", "952.82", "10.91"),
    ] {
        let result = run_fast_event(
            &panel,
            &statuses,
            &FastEventConfig {
                currency: Currency::new("CNY").unwrap(),
                venues: BTreeSet::from([venue.clone()]),
                initial_cash: "1000".parse().unwrap(),
                sizing: Sizing::Lot,
                lot_sizes: panel
                    .instruments
                    .iter()
                    .cloned()
                    .map(|id| (id, "10".parse().unwrap()))
                    .collect(),
                costs: FastEventCosts {
                    commission_rate: "0.01".parse().unwrap(),
                    buy_slippage_rate: "0.01".parse().unwrap(),
                    buy_tax_rate: "0.02".parse().unwrap(),
                    minimum_commission: minimum.parse().unwrap(),
                    ..FastEventCosts::ZERO
                },
            },
            &[PortfolioRun {
                id: VirtualPortfolioId(1),
                capital: "1000".parse().unwrap(),
                decisions: vec![Decision {
                    decision_session: panel.sessions[0].clone(),
                    ranked: vec![],
                    targets: BTreeMap::from([(a.clone(), 1.0)]),
                }],
                unfilled_entry: UnfilledEntry::Skip,
                rebalance: Rebalance::FullTarget,
            }],
        )
        .unwrap();
        let fill = &result.fills[0].fill;
        assert_eq!(fill.order().quantity(), "90".parse().unwrap());
        assert_eq!(fill.price(), "10.1".parse().unwrap());
        assert_eq!(
            fill.costs().minimum_commission_top_up,
            top_up.parse().unwrap()
        );
        assert_eq!(
            result.sessions[1].open_after.account.cash,
            cash.parse().unwrap()
        );
        assert_eq!(
            result.sessions[1].open_after.account.equity,
            equity.parse().unwrap()
        );
    }
}

#[test]
fn d7_decimal_price_text_is_preserved_without_binary_float_roundtrip() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    assert_eq!(
        panel
            .grid
            .column("open_decimal")
            .unwrap()
            .str()
            .unwrap()
            .get(0),
        Some("100")
    );
}

#[test]
fn parity_retry_only_buys_the_missing_leg_and_cash_cuts_do_not_create_pending() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let venue = VenueId::new("SYNTH").unwrap();
    let panel = load_panel(lake.path(), &dsv, &venue).unwrap();
    let mut statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let b = "B.SYNTH".parse().unwrap();
    statuses
        .get_mut(&(b, panel.sessions[1].session_date))
        .unwrap()
        .is_tradable = false;
    for target in [0.25, 0.5] {
        let result = run_fast_event(
            &panel,
            &statuses,
            &FastEventConfig {
                currency: Currency::new("CNY").unwrap(),
                venues: BTreeSet::from([venue.clone()]),
                initial_cash: "1000".parse().unwrap(),
                sizing: Sizing::VectorParity,
                costs: FastEventCosts {
                    commission_rate: "0.01".parse().unwrap(),
                    buy_slippage_rate: "0.01".parse().unwrap(),
                    buy_tax_rate: "0.02".parse().unwrap(),
                    ..FastEventCosts::ZERO
                },
                lot_sizes: BTreeMap::new(),
            },
            &[PortfolioRun {
                id: VirtualPortfolioId(1),
                capital: "1000".parse().unwrap(),
                decisions: vec![Decision {
                    decision_session: panel.sessions[0].clone(),
                    ranked: vec![],
                    targets: BTreeMap::from([
                        ("A.SYNTH".parse().unwrap(), target),
                        ("B.SYNTH".parse().unwrap(), target),
                    ]),
                }],
                unfilled_entry: UnfilledEntry::Retry,
                rebalance: Rebalance::FullTarget,
            }],
        )
        .unwrap();
        assert_eq!(result.fills.len(), 2);
        assert_eq!(
            result.fills[1].fill.order().instrument_id().to_string(),
            "B.SYNTH"
        );
        let a = "A.SYNTH".parse().unwrap();
        assert_eq!(
            result.sessions[1].open_after.account.positions[&a].quantity,
            result.sessions[2].open_after.account.positions[&a].quantity
        );
        assert!(!result.attempts.iter().any(|a| matches!(
            a.event,
            prajna_research::strategy::ExecutionEvent::PendingAtEnd { .. }
        )));
        if target == 0.25 {
            assert_eq!(result.fills[1].fee_basis, "247.5".parse().unwrap());
            assert_eq!(
                result.sessions[2].open_after.account.cash,
                "485.1".parse().unwrap()
            );
            assert_eq!(
                result.sessions[2].open_after.account.equity,
                "980.1".parse().unwrap()
            );
        } else {
            assert!(result.sessions[2].open_after.account.cash.to_f64() < 1e-12);
            assert!(
                (result.fills[1].fill.order().quantity().to_f64() - 4.711538461538462).abs()
                    < 1e-14
            );
        }
    }
}

#[test]
fn poc_entry_only_keeps_continuing_quantities_and_retries_unentered_targets() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let venue = VenueId::new("SYNTH").unwrap();
    let mut panel = load_panel(lake.path(), &dsv, &venue).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let opens = panel.grid.column("open_decimal").unwrap().str().unwrap();
    let mut decimals = (0..opens.len())
        .map(|i| opens.get(i).map(str::to_owned))
        .collect::<Vec<_>>();
    decimals[2] = Some("200".into());
    panel
        .grid
        .with_column(Series::new("open_decimal".into(), decimals).into())
        .unwrap();
    let targets = BTreeMap::from([
        ("A.SYNTH".parse().unwrap(), 0.5),
        ("B.SYNTH".parse().unwrap(), 0.5),
    ]);
    for budget in [EntryBudget::FixedInitial, EntryBudget::RemainingCash] {
        for capital in ["1000", "10"] {
            let result = run_fast_event(
                &panel,
                &statuses,
                &FastEventConfig {
                    currency: Currency::new("CNY").unwrap(),
                    venues: BTreeSet::from([venue.clone()]),
                    initial_cash: capital.parse().unwrap(),
                    sizing: Sizing::Lot,
                    costs: FastEventCosts::ZERO,
                    lot_sizes: panel
                        .instruments
                        .iter()
                        .cloned()
                        .map(|id| (id, "1".parse().unwrap()))
                        .collect(),
                },
                &[PortfolioRun {
                    id: VirtualPortfolioId(1),
                    capital: capital.parse().unwrap(),
                    decisions: [0, 1]
                        .into_iter()
                        .map(|i| Decision {
                            decision_session: panel.sessions[i].clone(),
                            ranked: vec![],
                            targets: targets.clone(),
                        })
                        .collect(),
                    unfilled_entry: UnfilledEntry::Skip,
                    rebalance: Rebalance::EntryOnly(budget),
                }],
            )
            .unwrap();
            if capital == "1000" {
                assert_eq!(result.fills.len(), 2);
                assert!(
                    result
                        .fills
                        .iter()
                        .all(|f| f.fill.order().quantity() == "5".parse().unwrap())
                );
                assert_eq!(
                    result.sessions[2].open_after.account.equity,
                    "1500".parse().unwrap()
                );
            } else {
                assert!(result.fills.is_empty());
                assert!(matches!(
                    result.attempts.last().unwrap().event,
                    prajna_research::strategy::ExecutionEvent::PendingAtEnd { .. }
                ));
            }
        }
    }
}
