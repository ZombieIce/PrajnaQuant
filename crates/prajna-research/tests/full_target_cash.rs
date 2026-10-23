#[allow(dead_code)]
#[path = "common/mod.rs"]
mod common;
#[allow(dead_code)]
#[path = "common/pit.rs"]
mod fixture;

use std::collections::BTreeMap;

use polars::prelude::{NamedFrom, Series};
use prajna_account::VirtualPortfolioId;
use prajna_domain::{Currency, InstrumentId, Notional, Quantity, VenueId};
use prajna_research::{
    fast_event::{
        FastEventConfig, FastEventCosts, PortfolioRun, Rebalance, Sizing, SizingAdjustmentReason,
        run_fast_event, run_fast_event_pit,
    },
    load_execution_status, load_panel,
    pit_strategy::{self, RiskSignals},
    strategy::{Decision, UnfilledEntry},
    vector::{VectorCosts, run_vector_pit},
};

fn set_open_decimals(panel: &mut prajna_research::Panel, replacements: &[(usize, usize, &str)]) {
    let sessions = panel.sessions.len();
    let open = panel.grid.column("open_decimal").unwrap().str().unwrap();
    let mut values = (0..panel.instruments.len() * sessions)
        .map(|row| open.get(row).map(str::to_owned))
        .collect::<Vec<_>>();
    for (instrument, session, value) in replacements {
        values[instrument * sessions + session] = Some((*value).to_owned());
    }
    panel
        .grid
        .with_column(Series::new("open_decimal".into(), values).into())
        .unwrap();
}

fn config(venue: VenueId, costs: FastEventCosts) -> FastEventConfig {
    FastEventConfig {
        currency: Currency::new("CNY").unwrap(),
        venue,
        initial_cash: "1000".parse().unwrap(),
        sizing: Sizing::VectorParity,
        costs,
        lot_sizes: BTreeMap::new(),
    }
}

fn portfolio(decision: Decision) -> PortfolioRun {
    PortfolioRun {
        id: VirtualPortfolioId(1),
        capital: "1000".parse().unwrap(),
        decisions: vec![decision],
        unfilled_entry: UnfilledEntry::Skip,
        rebalance: Rebalance::FullTarget,
    }
}

#[test]
fn non_pit_two_leg_quantization_regression_matches_account_and_cash_residue() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let venue = VenueId::new("SYNTH").unwrap();
    let mut panel = load_panel(lake.path(), &dsv, &venue).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    set_open_decimals(&mut panel, &[(0, 1, "1"), (1, 1, "3")]);
    let a: InstrumentId = "A.SYNTH".parse().unwrap();
    let b: InstrumentId = "B.SYNTH".parse().unwrap();
    let decision = Decision {
        decision_session: panel.sessions[0].clone(),
        ranked: vec![],
        targets: BTreeMap::from([(a.clone(), 0.5), (b.clone(), 0.5)]),
    };
    let result = run_fast_event(
        &panel,
        &statuses,
        &config(
            venue,
            FastEventCosts {
                commission_rate: "0.0003".parse().unwrap(),
                ..FastEventCosts::ZERO
            },
        ),
        &[portfolio(decision)],
    )
    .unwrap();

    assert_eq!(result.semantic_version, "fast_event@7");
    assert_eq!(result.fills.len(), 2);
    assert_eq!(
        result.fills[0].fill.order().instrument_id(),
        &a,
        "buy legs settle in InstrumentId order"
    );
    assert_eq!(
        result.fills[0].fill.order().quantity(),
        "499.85".parse::<Quantity>().unwrap()
    );
    assert_eq!(
        result.fills[1].fill.order().quantity(),
        "166.616666666666666666".parse::<Quantity>().unwrap()
    );
    assert_eq!(
        result.fills[1].fee_basis,
        "500".parse::<Notional>().unwrap()
    );
    let total = result
        .fills
        .iter()
        .map(|fill| fill.fill.total_fees().mantissa())
        .sum::<i128>();
    assert_eq!(
        Notional::from_mantissa(total).unwrap(),
        "0.3".parse::<Notional>().unwrap()
    );
    let held = &result.sessions[1].open_after.portfolios[&VirtualPortfolioId(1)];
    assert_eq!(held.cash, "0.000000000000000002".parse().unwrap());
    assert_eq!(
        held.positions[&a].quantity,
        "499.85".parse::<Quantity>().unwrap()
    );
    assert_eq!(
        held.positions[&b].quantity,
        "166.616666666666666666".parse::<Quantity>().unwrap()
    );
    let adjustment = &result.sizing_adjustments[0];
    assert_eq!(
        adjustment.reason,
        SizingAdjustmentReason::QuantizationCashBudget
    );
    assert_eq!(
        adjustment.requested_quantity,
        "166.616666666666666667".parse::<Quantity>().unwrap()
    );
    assert_eq!(
        adjustment.actual_quantity,
        "166.616666666666666666".parse::<Quantity>().unwrap()
    );
    let cash = adjustment.cash.unwrap();
    assert_eq!(
        cash.candidate_principal,
        "499.850000000000000001".parse().unwrap()
    );
    assert_eq!(
        cash.actual_principal,
        "499.849999999999999998".parse().unwrap()
    );
    assert_eq!(cash.cash_residue, "0.000000000000000002".parse().unwrap());
}

#[test]
fn sell_settlement_funds_the_buy_and_only_quantization_is_reduced() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let venue = VenueId::new("SYNTH").unwrap();
    let mut panel = load_panel(lake.path(), &dsv, &venue).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    set_open_decimals(
        &mut panel,
        &[(0, 1, "1"), (0, 3, "1"), (1, 1, "3"), (1, 3, "3")],
    );
    let a: InstrumentId = "A.SYNTH".parse().unwrap();
    let b: InstrumentId = "B.SYNTH".parse().unwrap();
    let mut run = portfolio(Decision {
        decision_session: panel.sessions[0].clone(),
        ranked: vec![],
        targets: BTreeMap::from([(a.clone(), 1.0)]),
    });
    run.decisions.push(Decision {
        decision_session: panel.sessions[2].clone(),
        ranked: vec![],
        targets: BTreeMap::from([(a.clone(), 0.5), (b.clone(), 0.5)]),
    });
    let result = run_fast_event(
        &panel,
        &statuses,
        &config(venue, FastEventCosts::ZERO),
        &[run],
    )
    .unwrap();

    assert_eq!(result.fills.len(), 3);
    assert_eq!(
        result.fills[2].fill.order().quantity(),
        "166.666666666666666666".parse::<Quantity>().unwrap()
    );
    assert_eq!(
        result.sessions[3].open_after.portfolios[&VirtualPortfolioId(1)].cash,
        "0.000000000000000002".parse().unwrap()
    );
    assert_eq!(result.sizing_adjustments.len(), 1);
    assert_eq!(
        result.sizing_adjustments[0].instrument_id, b,
        "the sell leg is never reduced"
    );
}

#[test]
fn pit_vector_preview_and_fast_account_consume_the_same_corrected_quantities() {
    let lake = fixture::lake();
    let overrides = (1..=10)
        .map(|day| ((1usize, day), (3.0, 3.0)))
        .collect::<BTreeMap<_, _>>();
    let f = fixture::fixture_with_action_prices(lake.path(), overrides);
    let native = load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let decision = Decision {
        decision_session: panel.sessions[0].clone(),
        ranked: vec![],
        targets: BTreeMap::from([(f.ids[0].clone(), 0.5), (f.ids[1].clone(), 0.5)]),
    };
    let fast = run_fast_event_pit(
        &panel,
        &f.market,
        &RiskSignals::default(),
        &config(
            "SZSE".parse().unwrap(),
            FastEventCosts {
                commission_rate: "0.0003".parse().unwrap(),
                ..FastEventCosts::ZERO
            },
        ),
        &[portfolio(decision.clone())],
    )
    .unwrap();
    let vector = run_vector_pit(
        &panel,
        &[decision],
        &f.market,
        &RiskSignals::default(),
        &VectorCosts {
            commission_rate: 0.0003,
            buy_slippage_bps: 0.0,
            sell_slippage_bps: 0.0,
            buy_tax_rate: 0.0,
            sell_tax_rate: 0.0,
        },
        "1000".parse().unwrap(),
    )
    .unwrap();

    assert_eq!(fast.semantic_version, "fast_event@7");
    assert_eq!(fast.fills.len(), 2);
    assert_eq!(
        fast.fills[1].fill.order().quantity(),
        "166.616666666666666666".parse::<Quantity>().unwrap()
    );
    assert_eq!(
        fast.sessions[1].open_after.portfolios[&VirtualPortfolioId(1)].cash,
        "0.000000000000000002".parse().unwrap()
    );
    let execution = vector
        .executions
        .iter()
        .find(|event| event.session_date == fixture::date(2).to_string())
        .unwrap();
    let sizing = execution.pit_sizing.as_ref().unwrap();
    assert_eq!(
        sizing.desired_quantities[&f.ids[0]],
        "499.85".parse::<Quantity>().unwrap()
    );
    assert_eq!(
        sizing.desired_quantities[&f.ids[1]],
        "166.616666666666666666".parse::<Quantity>().unwrap()
    );
    assert_eq!(sizing.cash_adjustments.len(), 1);
    assert_eq!(
        sizing.cash_residue,
        Some("0.000000000000000002".parse().unwrap())
    );
    assert_eq!(
        sizing.cash_adjustments[0].reason,
        SizingAdjustmentReason::QuantizationCashBudget
    );
    assert_eq!(sizing.fees, "0.3".parse::<Notional>().unwrap());
    assert_eq!(vector.semantic_version.as_deref(), Some("vector@7"));
    assert_eq!(
        vector.sessions[1].nav / (1.0 + vector.sessions[1].gross_return) * 1000.0,
        fast.sessions[1]
            .open_after
            .account
            .equity
            .to_string()
            .parse::<f64>()
            .unwrap()
    );
}
