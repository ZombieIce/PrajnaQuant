#[allow(dead_code)]
mod common;

#[path = "common/cn_daily.rs"]
mod cn_daily;

use std::collections::{BTreeMap, BTreeSet};

use prajna_account::{Fill, Order, Side, VirtualPortfolioId};
use prajna_data::ExecutionStatusRow;
use prajna_domain::{Currency, InstrumentId, Notional, Price, VenueId};
use prajna_research::{
    ExecutionStatusMap, Panel, PanelSession,
    factor::AvailabilityAssumption,
    fast_event::{
        EntryBudget, FastEventConfig, FastEventCosts, FastEventError,
        MULTI_VENUE_LOT_ENGINE_VERSION, MULTI_VENUE_PARITY_ENGINE_VERSION, MarketExecution,
        PortfolioRun, Rebalance, Sizing, run_fast_event, run_lot_with_execution,
    },
    load_panel_for_venues,
    strategy::{Decision, UnfilledEntry},
    vector::{MULTI_VENUE_SEMANTIC_VERSION, VectorCosts, run_vector_with_policy},
};

const DATES: [&str; 3] = ["2026-01-05", "2026-01-06", "2026-01-07"];

fn venue(name: &str) -> VenueId {
    VenueId::new(name).unwrap()
}

fn n(value: &str) -> Notional {
    value.parse().unwrap()
}

fn fixture() -> (Panel, ExecutionStatusMap) {
    let dataset = cn_daily::SyntheticDailyDataset::publish(
        &cn_daily::matched_calendar(&DATES),
        cn_daily::shanghai_shenzhen_records(),
        &[],
    );
    let panel = load_panel_for_venues(
        dataset.root(),
        &dataset.dsv,
        &[venue("XSHG"), venue("XSHE")],
    )
    .unwrap();
    let mut statuses = ExecutionStatusMap::new();
    for instrument in &panel.instruments {
        for session in &panel.sessions {
            statuses.insert(
                (instrument.clone(), session.session_date),
                ExecutionStatusRow {
                    instrument_id: instrument.clone(),
                    session_date: session.session_date,
                    trade_status: "TRADABLE".into(),
                    is_tradable: true,
                    available_at: Some(session.ts_open),
                    source: "synthetic".into(),
                },
            );
        }
    }
    (panel, statuses)
}

fn venues() -> BTreeSet<VenueId> {
    BTreeSet::from([venue("XSHG"), venue("XSHE")])
}

fn pick(panel: &Panel, venue: &str) -> InstrumentId {
    panel
        .instruments
        .iter()
        .find(|instrument| instrument.venue().as_str() == venue)
        .unwrap()
        .clone()
}

fn decisions(panel: &Panel, xshg: &InstrumentId, xshe: &InstrumentId) -> Vec<Decision> {
    vec![
        Decision {
            decision_session: panel.sessions[0].clone(),
            ranked: vec![],
            targets: BTreeMap::from([(xshg.clone(), 0.5), (xshe.clone(), 0.5)]),
        },
        Decision {
            decision_session: panel.sessions[1].clone(),
            ranked: vec![],
            targets: BTreeMap::from([(xshg.clone(), 0.0), (xshe.clone(), 1.0)]),
        },
    ]
}

fn config(venues: BTreeSet<VenueId>, sizing: Sizing, costs: FastEventCosts) -> FastEventConfig {
    FastEventConfig {
        currency: Currency::new("CNY").unwrap(),
        venues,
        initial_cash: n("10000"),
        sizing,
        costs,
        lot_sizes: BTreeMap::new(),
    }
}

fn portfolio(decisions: Vec<Decision>) -> PortfolioRun {
    PortfolioRun {
        id: VirtualPortfolioId(1),
        capital: n("10000"),
        decisions,
        unfilled_entry: UnfilledEntry::Skip,
        rebalance: Rebalance::FullTarget,
    }
}

fn assert_ledger_identity(valuation: &prajna_account::AccountValuation) {
    let positions = valuation
        .account
        .positions
        .values()
        .map(|position| position.market_value.mantissa())
        .sum::<i128>();
    let total = valuation
        .account
        .cash
        .mantissa()
        .checked_add(valuation.account.dividend_receivable.mantissa())
        .and_then(|value| value.checked_add(valuation.account.liquidation_receivable.mantissa()))
        .and_then(|value| value.checked_add(positions))
        .unwrap();
    assert_eq!(
        Notional::from_mantissa(total).unwrap(),
        valuation.account.equity
    );
}

#[test]
fn multi_venue_fast_event_ledger_is_hand_calculated_across_venues() {
    let (panel, statuses) = fixture();
    let xshg = pick(&panel, "XSHG");
    let xshe = pick(&panel, "XSHE");
    let result = run_fast_event(
        &panel,
        &statuses,
        &config(
            venues(),
            Sizing::VectorParity,
            FastEventCosts {
                commission_rate: n("0.01"),
                ..FastEventCosts::ZERO
            },
        ),
        &[portfolio(decisions(&panel, &xshg, &xshe))],
    )
    .unwrap();

    assert_eq!(result.semantic_version, MULTI_VENUE_PARITY_ENGINE_VERSION);
    assert_eq!(result.account.venues(), &venues());
    assert_eq!(result.fills.len(), 4);
    for session in &result.sessions {
        assert_ledger_identity(&session.open_after);
        assert_ledger_identity(&session.close);
    }

    // 1% commission on each 5,000 target basis: after-fee equity is 9,900,
    // each venue holds 495 shares at 10.00 and cash is exactly zero.
    let first = &result.sessions[1].open_after.account;
    assert_eq!(first.cash, Notional::ZERO);
    assert_eq!(first.positions[&xshg].quantity, "495".parse().unwrap());
    assert_eq!(first.positions[&xshe].quantity, "495".parse().unwrap());
    assert_eq!(first.equity, n("9900"));
    let first_fills = result
        .fills
        .iter()
        .filter(|fill| fill.fill.executed_at() == panel.sessions[1].ts_open)
        .collect::<Vec<_>>();
    assert_eq!(first_fills.len(), 2);
    assert!(first_fills.iter().all(|fill| {
        fill.fill.order().quantity() == "495".parse().unwrap()
            && fill.fill.costs().commission == n("50")
            && fill.fill.order().side() == Side::Buy
    }));

    // Selling 495 XSHG and buying 485.1 XSHE at 10.00 leaves XSHE at 980.1,
    // cash 0 and after-fee equity 9,801.
    let second = &result.sessions[2].open_after.account;
    assert_eq!(second.cash, Notional::ZERO);
    assert_eq!(second.positions.get(&xshg), None);
    assert_eq!(second.positions[&xshe].quantity, "980.1".parse().unwrap());
    assert_eq!(second.equity, n("9801"));
    let second_fills = result
        .fills
        .iter()
        .filter(|fill| fill.fill.executed_at() == panel.sessions[2].ts_open)
        .collect::<Vec<_>>();
    assert_eq!(second_fills.len(), 2);
    let sell = second_fills
        .iter()
        .find(|fill| fill.fill.order().side() == Side::Sell)
        .unwrap();
    assert_eq!(sell.fill.order().instrument_id(), &xshg);
    assert_eq!(sell.fill.order().quantity(), "495".parse().unwrap());
    assert_eq!(sell.fill.costs().commission, n("49.5"));
    let buy = second_fills
        .iter()
        .find(|fill| fill.fill.order().side() == Side::Buy)
        .unwrap();
    assert_eq!(buy.fill.order().instrument_id(), &xshe);
    assert_eq!(buy.fill.order().quantity(), "485.1".parse().unwrap());
    assert_eq!(buy.fill.costs().commission, n("49.5"));

    // The close mark is 10.10, so the final 980.1 XSHE shares value 9,899.01.
    assert_eq!(
        result.sessions[2].close.account.equity,
        "9899.01".parse().unwrap()
    );
}

#[test]
fn multi_venue_fast_event_and_vector_match_in_their_common_subset() {
    let (panel, statuses) = fixture();
    let xshg = pick(&panel, "XSHG");
    let xshe = pick(&panel, "XSHE");
    let tape = decisions(&panel, &xshg, &xshe);
    let fast_costs = FastEventCosts {
        commission_rate: n("0.01"),
        ..FastEventCosts::ZERO
    };
    let fast = run_fast_event(
        &panel,
        &statuses,
        &config(venues(), Sizing::VectorParity, fast_costs),
        &[portfolio(tape.clone())],
    )
    .unwrap();
    let vector = run_vector_with_policy(
        &panel,
        &tape,
        UnfilledEntry::Skip,
        &VectorCosts {
            commission_rate: 0.01,
            buy_slippage_bps: 0.0,
            sell_slippage_bps: 0.0,
            buy_tax_rate: 0.0,
            sell_tax_rate: 0.0,
        },
        AvailabilityAssumption::None,
        |_, _| true,
    )
    .unwrap();

    assert_eq!(
        vector.semantic_version.as_deref(),
        Some(MULTI_VENUE_SEMANTIC_VERSION)
    );
    assert_eq!(vector.sessions.len(), fast.sessions.len());
    for (vector_session, fast_session) in vector.sessions.iter().zip(&fast.sessions) {
        let vector_open = vector_session.nav / (1.0 + vector_session.gross_return);
        let fast_open = fast_session.open_after.account.equity.to_f64() / 10_000.0;
        assert!(
            (vector_open - fast_open).abs() / fast_open <= 1e-9,
            "{}: Vector={vector_open} Fast={fast_open}",
            vector_session.session_date
        );
    }
}

#[test]
fn multi_venue_lot_uses_its_own_engine_semantic_version() {
    let (panel, statuses) = fixture();
    let xshg = pick(&panel, "XSHG");
    let xshe = pick(&panel, "XSHE");
    let mut config = config(venues(), Sizing::Lot, FastEventCosts::ZERO);
    config.lot_sizes = panel
        .instruments
        .iter()
        .cloned()
        .map(|instrument| (instrument, "100".parse().unwrap()))
        .collect();
    let decision = Decision {
        decision_session: panel.sessions[0].clone(),
        ranked: vec![],
        targets: BTreeMap::from([(xshg, 0.1), (xshe, 0.1)]),
    };
    let result = run_fast_event(&panel, &statuses, &config, &[portfolio(vec![decision])]).unwrap();
    assert_eq!(result.semantic_version, MULTI_VENUE_LOT_ENGINE_VERSION);
}

fn error(config: FastEventConfig, portfolios: &[PortfolioRun]) -> String {
    let (panel, statuses) = fixture();
    run_fast_event(&panel, &statuses, &config, portfolios)
        .unwrap_err()
        .to_string()
}

struct NoExecution;

impl MarketExecution for NoExecution {
    fn open(
        &mut self,
        _: &PanelSession,
        _: &BTreeMap<InstrumentId, Price>,
        _: &BTreeMap<InstrumentId, bool>,
        _: FastEventCosts,
    ) -> Result<(), FastEventError> {
        Ok(())
    }

    fn execute(
        &mut self,
        _: &Order,
        _: Price,
        _: FastEventCosts,
    ) -> Result<Option<Fill>, FastEventError> {
        Ok(None)
    }

    fn close(&mut self, _: &PanelSession) -> Result<(), FastEventError> {
        Ok(())
    }
}

#[test]
fn external_accurate_execution_rejects_multi_venue_panels() {
    let (panel, statuses) = fixture();
    let mut config = config(venues(), Sizing::Lot, FastEventCosts::ZERO);
    config.lot_sizes = panel
        .instruments
        .iter()
        .cloned()
        .map(|instrument| (instrument, "100".parse().unwrap()))
        .collect();
    let error =
        run_lot_with_execution(&panel, &statuses, &config, &[], &mut NoExecution).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("external execution (Accurate) requires one Venue")
    );
}

#[test]
fn adr_0027_boundaries_are_rejected() {
    let scope = config(
        BTreeSet::from([venue("XSHG")]),
        Sizing::VectorParity,
        FastEventCosts::ZERO,
    );
    assert!(error(scope, &[]).contains("invalid panel venue scope"));
    let mut p = portfolio(vec![]);
    p.rebalance = Rebalance::EntryOnly(EntryBudget::FixedInitial);
    assert!(
        error(config(venues(), Sizing::Lot, FastEventCosts::ZERO), &[p])
            .contains("multi-venue execution requires full_target")
    );
}
