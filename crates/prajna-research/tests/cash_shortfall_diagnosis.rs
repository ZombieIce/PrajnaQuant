//! Entirely synthetic #271 scenario retained as the #274 investable-equity
//! regression: normal parity full-target plans size from cash plus securities,
//! keep receivables in total NAV, and report fractions against total NAV. The
//! original rejection policy stays reproducible from the pre-#274 revision and
//! its evidence in #271 / PR #272; these tests cover the replacement policy.
#[allow(dead_code)]
#[path = "common/pit.rs"]
mod fixture;

use std::collections::{BTreeMap, BTreeSet};

use prajna_account::{PortfolioValuation, Side, VirtualPortfolioId};
use prajna_data::{CorporateActionKind, CorporateActionRow};
use prajna_domain::{InstrumentId, Notional, Quantity};
use prajna_research::{
    corporate_actions::{ActionCoverageStatus, CorporateActionCoverage, CorporateActionSchedule},
    etf_market::{DerivedMarket, MarketEvidence},
    fast_event::{
        FastEventConfig, FastEventCosts, FastEventError, FastEventResult, PortfolioRun, Rebalance,
        Sizing, SizingAdjustmentReason, run_fast_event_pit_with_actions,
    },
    pit_strategy::{self, RiskSignals},
    strategy::{Decision, ExecutionEvent, UnfilledEntry},
    vector::{
        VectorCosts, VectorError, VectorExecution, VectorExecutionKind, VectorPitSizing,
        VectorResult, run_vector_pit_with_actions,
    },
};

const VP: VirtualPortfolioId = VirtualPortfolioId(1);

fn q(value: &str) -> Quantity {
    value.parse().unwrap()
}

fn n(value: &str) -> Notional {
    value.parse().unwrap()
}

#[derive(Clone, Copy)]
enum Action {
    None,
    Dividend(&'static str),
    Liquidation(&'static str),
}

struct Scenario {
    capital: &'static str,
    action: Action,
    pay_day: u32,
    commission: &'static str,
    /// `(decision session index, [(fixture instrument index, target weight)])`.
    decisions: Vec<(usize, Vec<(usize, f64)>)>,
    /// `(instrument index, session day, price)`; applied to open and close.
    price_overrides: Vec<(usize, u32, f64)>,
    /// `(instrument index, session day, reference price)`; used to derive the
    /// opening limit gates on an otherwise synthetically tradable bar.
    reference_overrides: Vec<(usize, u32, &'static str)>,
}

fn action_row(
    action: Action,
    instrument_id: InstrumentId,
    pay_day: u32,
) -> Option<CorporateActionRow> {
    let (kind, per_share) = match action {
        Action::None => return None,
        Action::Dividend(per_share) => (CorporateActionKind::Dividend, per_share),
        Action::Liquidation(per_share) => (CorporateActionKind::Liquidation, per_share),
    };
    let dividend = kind == CorporateActionKind::Dividend;
    Some(CorporateActionRow {
        announcement_id: format!("synthetic-{kind:?}"),
        action_index: 0,
        instrument_id,
        kind,
        announced_date: fixture::date(1),
        announced_at: Some(fixture::time(1, 8)),
        available_at: fixture::time(1, 15),
        record_date: dividend.then(|| fixture::date(2)),
        ex_date: dividend.then(|| fixture::date(3)),
        pay_date: Some(fixture::date(pay_day)),
        effective_date: (!dividend).then(|| fixture::date(3)),
        cash_per_share: dividend.then(|| per_share.into()),
        share_ratio: None,
        liquidation_cash_per_share: (!dividend).then(|| per_share.into()),
        source_raw_sha256: "synthetic".into(),
        normalization_version: "synthetic-actions@1".into(),
    })
}

fn run(
    scenario: Scenario,
) -> (
    Result<FastEventResult, FastEventError>,
    Result<VectorResult, VectorError>,
) {
    run_sizing(scenario, Sizing::VectorParity, "1", "0")
}

fn run_sizing(
    scenario: Scenario,
    sizing: Sizing,
    lot: &str,
    minimum: &str,
) -> (
    Result<FastEventResult, FastEventError>,
    Result<VectorResult, VectorError>,
) {
    run_allocated(scenario, sizing, lot, minimum, None)
}

fn run_allocated(
    scenario: Scenario,
    sizing: Sizing,
    lot: &str,
    minimum: &str,
    allocations: Option<Vec<(VirtualPortfolioId, Notional)>>,
) -> (
    Result<FastEventResult, FastEventError>,
    Result<VectorResult, VectorError>,
) {
    let lake = fixture::lake();
    let mut overrides: BTreeMap<(usize, u32), (f64, f64)> =
        (3..=10).map(|day| ((1, day), (0.5, 0.5))).collect();
    overrides.extend(
        scenario
            .price_overrides
            .into_iter()
            .map(|(instrument, day, price)| ((instrument, day), (price, price))),
    );
    let f = fixture::fixture_with_action_prices(lake.path(), overrides);
    let market = if scenario.reference_overrides.is_empty() {
        f.market.clone()
    } else {
        let mut evidence: MarketEvidence =
            serde_json::from_value(f.input["market"].clone()).unwrap();
        for (instrument, day, reference) in &scenario.reference_overrides {
            let opening = evidence
                .openings
                .iter_mut()
                .find(|opening| {
                    opening.instrument_id == f.ids[*instrument]
                        && opening.session_date == fixture::date(*day)
                })
                .expect("reference override opening");
            opening
                .reference
                .as_mut()
                .expect("synthetic opening reference")
                .adjusted_previous_close = reference.parse().unwrap();
        }
        DerivedMarket::from_dataset(lake.path(), &f.dsv, &f.catalog, evidence).unwrap()
    };
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let instrument = f.ids[1].clone();
    let action = action_row(scenario.action, instrument.clone(), scenario.pay_day);
    let actions = CorporateActionSchedule::new(
        "synthetic-actions@1",
        action.iter().cloned().collect(),
        f.ids
            .iter()
            .map(|id| CorporateActionCoverage {
                instrument_id: id.clone(),
                start: fixture::date(1),
                end: fixture::date(10),
                observed_actions: usize::from(action.is_some() && id == &instrument),
                unapplied_action_dates: vec![],
                unexplained_price_jumps: vec![],
                status: ActionCoverageStatus::Complete,
            })
            .collect(),
    )
    .unwrap();
    let decisions = scenario
        .decisions
        .iter()
        .map(|(index, targets)| Decision {
            decision_session: panel.sessions[*index].clone(),
            ranked: vec![],
            targets: targets
                .iter()
                .map(|(instrument, weight)| (f.ids[*instrument].clone(), *weight))
                .collect(),
        })
        .collect::<Vec<_>>();
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: scenario.capital.parse().unwrap(),
        sizing,
        costs: FastEventCosts {
            commission_rate: scenario.commission.parse().unwrap(),
            minimum_commission: minimum.parse().unwrap(),
            ..FastEventCosts::ZERO
        },
        lot_sizes: f.ids.iter().map(|id| (id.clone(), q(lot))).collect(),
    };
    let portfolios = allocations
        .unwrap_or_else(|| vec![(VP, config.initial_cash)])
        .into_iter()
        .map(|(id, capital)| PortfolioRun {
            id,
            capital,
            decisions: decisions.clone(),
            unfilled_entry: UnfilledEntry::Skip,
            rebalance: Rebalance::FullTarget,
        })
        .collect::<Vec<_>>();
    let costs = VectorCosts {
        commission_rate: config.costs.commission_rate.to_f64(),
        buy_slippage_bps: 0.0,
        sell_slippage_bps: 0.0,
        buy_tax_rate: 0.0,
        sell_tax_rate: 0.0,
    };
    (
        run_fast_event_pit_with_actions(
            &panel,
            &market,
            &RiskSignals::default(),
            &config,
            &portfolios,
            &actions,
        ),
        run_vector_pit_with_actions(
            &panel,
            &decisions,
            &market,
            &RiskSignals::default(),
            &costs,
            config.initial_cash,
            &actions,
        ),
    )
}

fn portfolio(result: &FastEventResult, session: usize, close: bool) -> &PortfolioValuation {
    let snapshot = if close {
        &result.sessions[session].close
    } else {
        &result.sessions[session].open_after
    };
    &snapshot.portfolios[&VP]
}

fn positions(result: &FastEventResult, session: usize) -> BTreeMap<InstrumentId, Quantity> {
    portfolio(result, session, false)
        .positions
        .iter()
        .map(|(id, marked)| (id.clone(), marked.quantity))
        .collect()
}

fn trace<'a>(vector: &'a VectorResult, session: &str) -> &'a VectorExecution {
    vector
        .executions
        .iter()
        .find(|execution| execution.session_date == session)
        .expect("PIT execution trace")
}

fn sizing<'a>(vector: &'a VectorResult, session: &str) -> &'a VectorPitSizing {
    vector
        .executions
        .iter()
        .find(|execution| {
            execution.session_date == session && execution.kind == VectorExecutionKind::Executed
        })
        .and_then(|execution| execution.pit_sizing.as_ref())
        .expect("executed PIT sizing trace")
}

fn assert_identities(result: &FastEventResult) {
    for session in &result.sessions {
        for snapshot in [&session.open_after, &session.close] {
            let p = &snapshot.portfolios[&VP];
            assert_eq!(&snapshot.account, p);
            assert_eq!(
                p.equity.mantissa(),
                p.cash.mantissa()
                    + p.positions
                        .values()
                        .map(|position| position.market_value.mantissa())
                        .sum::<i128>()
                    + p.dividend_receivable.mantissa()
                    + p.liquidation_receivable.mantissa()
            );
            assert!(p.cash >= Notional::ZERO);
            assert!(
                p.positions
                    .values()
                    .all(|position| position.quantity >= Quantity::ZERO)
            );
            assert!(p.dividend_receivable >= Notional::ZERO);
            assert!(p.liquidation_receivable >= Notional::ZERO);
        }
    }
}

#[test]
fn ex_date_and_pay_date_open_size_from_investable_equity_then_invest_paid_cash() {
    let (fast, vector) = run(Scenario {
        capital: "1000",
        action: Action::Dividend("0.5"),
        pay_day: 4,
        commission: "0",
        decisions: vec![
            (0, vec![(1, 1.0)]),
            (1, vec![(1, 1.0)]),
            (2, vec![(1, 1.0)]),
            (3, vec![(1, 1.0)]),
        ],
        price_overrides: vec![],
        reference_overrides: vec![],
    });
    let (fast, vector) = (fast.unwrap(), vector.unwrap());
    assert_identities(&fast);
    assert_eq!(
        fast.fills.len(),
        2,
        "ex-date and pay-date-open full targets need no trade"
    );

    // D2 open buys 1000 shares at 1; D2 close locks 500 receivable. At the D3
    // open total NAV is 1000 but investable equity is 500, so 100% holds.
    let ex = portfolio(&fast, 2, false);
    assert_eq!(
        (ex.cash, ex.dividend_receivable, ex.equity),
        (n("0"), n("500"), n("1000"))
    );
    assert_eq!(positions(&fast, 2).into_values().next().unwrap(), q("1000"));
    let ex_sizing = sizing(&vector, &fixture::date(3).to_string());
    assert_eq!(ex_sizing.equity, n("1000"));
    assert_eq!(ex_sizing.investable_equity, n("500"));
    assert_eq!(ex_sizing.fees, Notional::ZERO);
    assert_eq!(
        ex_sizing.desired_quantities.values().next().unwrap(),
        &q("1000")
    );

    // The D3-close Decision is a full target at the pay-date D4 open: the
    // receivable is still unpaid, so it cannot fund a new buy or create a fill.
    assert!(
        fast.fills
            .iter()
            .all(|fill| fill.fill.executed_at() != fast.sessions[3].session.ts_open)
    );
    let pay_date_open = trace(&vector, &fixture::date(4).to_string());
    assert_eq!(pay_date_open.kind, VectorExecutionKind::Executed);
    let pay_date_sizing = pay_date_open.pit_sizing.as_ref().unwrap();
    assert_eq!(
        (
            pay_date_sizing.equity,
            pay_date_sizing.investable_equity,
            pay_date_sizing.fees
        ),
        (n("1000"), n("500"), n("0"))
    );
    assert_eq!(
        pay_date_sizing.desired_quantities.values().next().unwrap(),
        &q("1000")
    );
    assert_eq!(
        (vector.sessions[3].turnover, vector.sessions[3].cost),
        (0.0, 0.0)
    );
    // Pay-date open still excludes the receivable; close only moves buckets.
    let paid_open = portfolio(&fast, 3, false);
    assert_eq!(
        (paid_open.cash, paid_open.dividend_receivable),
        (n("0"), n("500"))
    );
    let paid_close = portfolio(&fast, 3, true);
    assert_eq!(
        (
            paid_close.cash,
            paid_close.dividend_receivable,
            paid_close.equity
        ),
        (n("500"), n("0"), n("1000"))
    );

    // A later Decision at the pay-date close invests only at the next open.
    assert_eq!(portfolio(&fast, 4, false).cash, Notional::ZERO);
    assert_eq!(fast.fills[1].fill.order().quantity(), q("1000"));
    assert_eq!(
        (fast.fills[1].fee_basis, fast.fills[1].turnover_basis),
        (n("500"), n("500"))
    );
    assert_eq!(
        fast.fills[1].fill.executed_at(),
        fast.sessions[4].session.ts_open
    );
    assert_eq!(
        (vector.sessions[2].turnover, vector.sessions[3].turnover),
        (0.0, 0.0)
    );
    assert_eq!(
        (vector.sessions[4].turnover, vector.sessions[4].cost),
        (0.5, 0.0)
    );
}

#[test]
fn zero_fee_targets_keep_intended_cash_without_renormalizing() {
    let (fast, vector) = run(Scenario {
        capital: "1000",
        action: Action::Dividend("0.5"),
        pay_day: 4,
        commission: "0",
        decisions: vec![(0, vec![(1, 1.0)]), (1, vec![(1, 0.4), (0, 0.2)])],
        price_overrides: vec![],
        reference_overrides: vec![],
    });
    let (fast, vector) = (fast.unwrap(), vector.unwrap());
    assert_identities(&fast);
    // Investable equity 500: 40% = 200 (400 shares at 0.5), 20% = 100
    // (100 shares at 1). The remaining 200 stays intentional cash.
    let sizing = sizing(&vector, &fixture::date(3).to_string());
    assert_eq!(
        (sizing.equity, sizing.investable_equity),
        (n("1000"), n("500"))
    );
    let mut held = positions(&fast, 2).into_values().collect::<Vec<_>>();
    held.sort();
    assert_eq!(held, vec![q("100"), q("400")]);
    let mut rebalanced = fast
        .fills
        .iter()
        .filter(|fill| fill.fill.executed_at() == fast.sessions[2].session.ts_open)
        .map(|fill| (fill.fill.order().quantity(), fill.fee_basis))
        .collect::<Vec<_>>();
    rebalanced.sort();
    assert_eq!(rebalanced, vec![(q("100"), n("100")), (q("600"), n("300"))]);
    let after = portfolio(&fast, 2, false);
    assert_eq!((after.cash, after.equity), (n("200"), n("1000")));
    assert_eq!(
        (vector.sessions[2].turnover, vector.sessions[2].cost),
        (0.4, 0.0)
    );
    let components = vector.sessions[2].portfolio_components.as_ref().unwrap();
    assert_eq!(
        (
            components.available_cash_weight,
            components.dividend_receivable_weight
        ),
        (0.2, 0.5)
    );
}

#[test]
fn nonzero_cost_rotation_uses_pre_trade_total_nav_for_cost_and_turnover() {
    // Seed exactly 1000 shares at price 1 while paying the run-wide 1% entry
    // fee, so the pre-trade total NAV of the rotation is exactly 1000.
    let (fast, vector) = run(Scenario {
        capital: "1010.101010101010101010",
        action: Action::Dividend("0.5"),
        pay_day: 4,
        commission: "0.01",
        decisions: vec![(0, vec![(1, 1.0)]), (1, vec![(0, 1.0)])],
        price_overrides: vec![],
        reference_overrides: vec![],
    });
    let (fast, vector) = (fast.unwrap(), vector.unwrap());
    assert_identities(&fast);
    let before = portfolio(&fast, 2, false);
    // open_after is post-rotation: 1000 pre-trade total NAV - 10 fees = 990.
    assert_eq!(
        (before.equity, before.cash, before.dividend_receivable),
        (n("990"), n("0"), n("500"))
    );
    let sizing = sizing(&vector, &fixture::date(3).to_string());
    assert_eq!(
        (sizing.equity, sizing.investable_equity, sizing.fees),
        (n("1000"), n("500"), n("10"))
    );
    assert_eq!(
        sizing.investable_equity.mantissa() - sizing.fees.mantissa(),
        n("490").mantissa()
    );
    assert_eq!(
        sizing.desired_quantities.values().next().unwrap(),
        &q("490")
    );
    let rotation = fast
        .fills
        .iter()
        .filter(|fill| fill.fill.executed_at() == fast.sessions[2].session.ts_open)
        .collect::<Vec<_>>();
    assert_eq!(rotation.len(), 2);
    assert_eq!(
        rotation
            .iter()
            .map(|fill| fill.fee_basis.mantissa())
            .sum::<i128>(),
        n("1000").mantissa()
    );
    assert_eq!(
        rotation
            .iter()
            .map(|fill| fill.fill.total_fees().mantissa())
            .sum::<i128>(),
        n("10").mantissa()
    );
    let buy = rotation
        .iter()
        .find(|fill| fill.fill.order().side() == Side::Buy)
        .unwrap();
    assert_eq!(
        (buy.fill.order().quantity(), buy.turnover_basis),
        (q("490"), n("500"))
    );
    let after = portfolio(&fast, 2, true);
    assert_eq!(
        (after.cash, after.dividend_receivable, after.equity),
        (n("0"), n("500"), n("990"))
    );
    assert_eq!(
        (vector.sessions[2].cost, vector.sessions[2].turnover),
        (0.01, 1.0)
    );
    let components = vector.sessions[2].portfolio_components.as_ref().unwrap();
    assert_eq!(components.available_cash_weight, 0.0);
    assert_eq!(components.dividend_receivable_weight, 500.0 / 990.0);
}

#[test]
fn receivable_is_not_deducted_twice_in_the_rounding_proof() {
    // Investable equity 1000 and receivable 500. The 50/50 target leaves no
    // cash, and the scale-18 quantity for the price-3 leg rounds one quantum
    // above its budget. The old proof also subtracted the receivable and
    // rejected this as a non-quantization shortfall.
    let (fast, vector) = run(Scenario {
        capital: "2000",
        action: Action::Dividend("0.5"),
        pay_day: 4,
        commission: "0",
        decisions: vec![(0, vec![(1, 1.0)]), (1, vec![(1, 0.5), (0, 0.5)])],
        price_overrides: (3..=10).map(|day| (0, day, 3.0)).collect(),
        reference_overrides: vec![],
    });
    let (fast, vector) = (fast.unwrap(), vector.unwrap());
    assert_identities(&fast);
    let sizing = sizing(&vector, &fixture::date(3).to_string());
    assert_eq!(
        (sizing.equity, sizing.investable_equity),
        (n("2000"), n("1000"))
    );
    assert_eq!(sizing.cash_adjustments.len(), 1);
    let adjustment = &sizing.cash_adjustments[0];
    assert_eq!(
        adjustment.reason,
        SizingAdjustmentReason::QuantizationCashBudget
    );
    assert_eq!(
        (adjustment.candidate_quantity, adjustment.actual_quantity),
        (q("166.666666666666666667"), q("166.666666666666666666"))
    );
    assert_eq!(
        (adjustment.candidate_principal, adjustment.actual_principal),
        (n("500.000000000000000001"), n("499.999999999999999998"))
    );
    assert_eq!(sizing.cash_residue, Some(n("0.000000000000000002")));
    assert_eq!(
        fast.sizing_adjustments[0].actual_quantity,
        q("166.666666666666666666")
    );
    assert_eq!(
        fast.sizing_adjustments[0].reason,
        SizingAdjustmentReason::QuantizationCashBudget
    );
    let after = portfolio(&fast, 2, false);
    assert_eq!(
        (after.cash, after.dividend_receivable, after.equity),
        (n("0.000000000000000002"), n("1000"), n("2000"))
    );
    assert_eq!(
        (vector.sessions[2].turnover, vector.sessions[2].cost),
        (0.5, 0.0)
    );
}

#[test]
fn zero_investable_equity_waits_then_invests_only_paid_cash() {
    let (fast, vector) = run(Scenario {
        capital: "1000",
        action: Action::Liquidation("0.5"),
        pay_day: 4,
        commission: "0",
        decisions: vec![
            (0, vec![(1, 1.0)]),
            (1, vec![(2, 1.0)]),
            (2, vec![(2, 1.0)]),
            (3, vec![(2, 1.0)]),
        ],
        price_overrides: vec![],
        reference_overrides: vec![],
    });
    let (fast, vector) = (fast.unwrap(), vector.unwrap());
    assert_identities(&fast);
    // The effective liquidation converts the holding into a 500 receivable,
    // leaving investable equity at zero with positive total NAV.
    let converted = portfolio(&fast, 2, false);
    assert_eq!(
        (
            converted.cash,
            converted.liquidation_receivable,
            converted.equity
        ),
        (n("0"), n("500"), n("500"))
    );
    assert!(converted.positions.is_empty());
    assert_eq!(
        fast.fills.len(),
        2,
        "zero investable equity creates no trade"
    );
    assert!(fast.account.fee_settlements().is_empty());
    let sizing = sizing(&vector, &fixture::date(3).to_string());
    assert_eq!(
        (sizing.equity, sizing.investable_equity),
        (n("500"), n("0"))
    );
    assert_eq!(sizing.fees, Notional::ZERO);
    assert!(sizing.desired_quantities.is_empty());
    assert_eq!(
        (vector.sessions[2].turnover, vector.sessions[2].cost),
        (0.0, 0.0)
    );
    // Pay-date open still cannot spend; the close transfer funds only the next
    // normal Decision at the following open.
    let paid_open = portfolio(&fast, 3, false);
    assert_eq!(
        (paid_open.cash, paid_open.liquidation_receivable),
        (n("0"), n("500"))
    );
    let paid_close = portfolio(&fast, 3, true);
    assert_eq!(
        (paid_close.cash, paid_close.liquidation_receivable),
        (n("500"), n("0"))
    );
    assert_eq!(portfolio(&fast, 4, false).cash, Notional::ZERO);
    assert_eq!(positions(&fast, 4).into_values().next().unwrap(), q("500"));
    assert_eq!(
        (vector.sessions[4].turnover, vector.sessions[4].cost),
        (1.0, 0.0)
    );
}

#[test]
fn skipped_buy_keeps_intended_cash_with_a_positive_receivable() {
    // ids[2] has no raw open on the ex-date, so its 50% buy leg is skipped.
    // The remaining held-instrument target still executes against investable
    // equity while the 500 receivable stays outside both the plan and the cash.
    let (fast, vector) = run(Scenario {
        capital: "1000",
        action: Action::Dividend("0.5"),
        pay_day: 4,
        commission: "0",
        decisions: vec![(0, vec![(1, 1.0)]), (1, vec![(1, 0.5), (2, 0.5)])],
        price_overrides: vec![],
        reference_overrides: vec![],
    });
    let (fast, vector) = (fast.unwrap(), vector.unwrap());
    assert_identities(&fast);
    let event = fast
        .attempts
        .iter()
        .find(|attempt| {
            matches!(
                &attempt.event,
                prajna_research::strategy::ExecutionEvent::Executed { attempt_session, .. }
                    if attempt_session.session_date == fixture::date(3)
            )
        })
        .expect("ex-date execution attempt");
    let prajna_research::strategy::ExecutionEvent::Executed { skipped_buys, .. } = &event.event
    else {
        unreachable!()
    };
    assert_eq!(skipped_buys.len(), 1);
    let after = portfolio(&fast, 2, false);
    assert_eq!(
        (after.cash, after.dividend_receivable, after.equity),
        (n("250"), n("500"), n("1000"))
    );
    let sizing = sizing(&vector, &fixture::date(3).to_string());
    assert_eq!(
        (sizing.equity, sizing.investable_equity, sizing.fees),
        (n("1000"), n("500"), n("0"))
    );
    assert_eq!(
        (vector.sessions[2].turnover, vector.sessions[2].cost),
        (0.25, 0.0)
    );
}

#[test]
fn blocked_sell_with_a_positive_receivable_defers_without_fills_or_fees() {
    // Lower-limit sell block on the ex-date: the required sell is on the held
    // instrument, so the whole target defers while the receivable stays in NAV.
    let (fast, vector) = run(Scenario {
        capital: "1000",
        action: Action::Dividend("0.5"),
        pay_day: 4,
        commission: "0",
        decisions: vec![(0, vec![(1, 1.0)]), (1, vec![(1, 0.5)])],
        price_overrides: vec![],
        reference_overrides: (3..=10)
            .map(|day| (1, day, "0.555555555555555556"))
            .collect(),
    });
    let (fast, vector) = (fast.unwrap(), vector.unwrap());
    assert_identities(&fast);
    assert_eq!(fast.fills.len(), 1, "blocked sell must not fill");
    let held = fast.fills[0].fill.order().instrument_id().clone();
    let attempt = fast
        .attempts
        .iter()
        .find(|attempt| {
            matches!(
                &attempt.event,
                ExecutionEvent::Deferred { attempt_session, .. }
                    if attempt_session.session_date == fixture::date(3)
            )
        })
        .expect("D3 deferral");
    let ExecutionEvent::Deferred { blocked, .. } = &attempt.event else {
        unreachable!()
    };
    assert_eq!(
        blocked.iter().cloned().collect::<Vec<_>>(),
        vec![held.clone()]
    );
    let after = portfolio(&fast, 2, false);
    assert_eq!(
        (after.cash, after.dividend_receivable, after.equity),
        (n("0"), n("500"), n("1000"))
    );
    assert_eq!(positions(&fast, 2)[&held], q("1000"));
    let event = trace(&vector, &fixture::date(3).to_string());
    assert_eq!(event.kind, VectorExecutionKind::Deferred);
    assert!(event.blocked.contains(&held));
    let sizing = event.pit_sizing.as_ref().unwrap();
    assert_eq!(
        (sizing.equity, sizing.investable_equity, sizing.fees),
        (n("1000"), n("500"), n("0"))
    );
    assert_eq!(sizing.required_sides[&held], "sell");
    assert_eq!(
        (vector.sessions[2].turnover, vector.sessions[2].cost),
        (0.0, 0.0)
    );
}

fn fee_only_scenario(reference: Option<&'static str>) -> (FastEventResult, VectorResult) {
    // Price 1e9 keeps 3.75e-7 shares materially valued; the ex-date target
    // differs from drift by 5e-13, so the quantity delta rounds to zero while a
    // sell-side candidate fee remains. The dividend creates a 75 receivable and
    // is paid only after the last retry.
    let (fast, vector) = run(Scenario {
        capital: "1000",
        action: Action::Dividend("200000000"),
        pay_day: 10,
        commission: "0.5",
        decisions: vec![(0, vec![(1, 0.5)]), (1, vec![(1, 0.4999999999995)])],
        price_overrides: (1..=10).map(|day| (1, day, 1.0e9)).collect(),
        reference_overrides: reference
            .into_iter()
            .flat_map(|price| (3..=10).map(move |day| (1, day, price)))
            .collect(),
    });
    (fast.unwrap(), vector.unwrap())
}

#[test]
fn fee_only_sell_with_a_positive_receivable_settles_once() {
    let (fast, vector) = fee_only_scenario(None);
    assert_identities(&fast);
    assert_eq!(fast.fills.len(), 1);
    let settlements = fast.account.fee_settlements();
    assert_eq!(settlements.len(), 1);
    let fee = &settlements[0];
    assert_eq!(fee.fee_side(), Side::Sell);
    assert_eq!(fee.fee_basis(), n("0.000000000375"));
    assert_eq!(fee.total_fees(), n("0.0000000001875"));
    assert_eq!(fee.executed_at(), fast.sessions[2].session.ts_open);
    let after = portfolio(&fast, 2, false);
    assert_eq!(
        (after.cash, after.dividend_receivable, after.equity),
        (n("374.9999999998125"), n("75"), n("824.9999999998125"))
    );
    let sizing = sizing(&vector, &fixture::date(3).to_string());
    assert_eq!(
        (sizing.equity, sizing.investable_equity, sizing.fees),
        (n("825"), n("750"), n("0.0000000001875"))
    );
    assert!(sizing.cash_adjustments.is_empty());
    assert_eq!(
        sizing.desired_quantities.values().next().unwrap(),
        &q("0.000000375")
    );
    let total_nav = "825".parse::<f64>().unwrap();
    assert_eq!(
        vector.sessions[2].turnover,
        "0.000000000375".parse::<f64>().unwrap() / total_nav
    );
    assert_eq!(
        vector.sessions[2].cost,
        "0.0000000001875".parse::<f64>().unwrap() / total_nav
    );
}

#[test]
fn blocked_fee_only_sell_with_a_positive_receivable_defers_without_settlement() {
    let (fast, vector) = fee_only_scenario(Some("1111111111.111111111111111111"));
    assert_identities(&fast);
    assert_eq!(fast.fills.len(), 1, "fee-only side must not create a fill");
    assert!(fast.account.fee_settlements().is_empty());
    let after = portfolio(&fast, 2, false);
    assert_eq!(
        (after.cash, after.dividend_receivable, after.equity),
        (n("375"), n("75"), n("825"))
    );
    let event = trace(&vector, &fixture::date(3).to_string());
    assert_eq!(event.kind, VectorExecutionKind::Deferred);
    let sizing = event.pit_sizing.as_ref().unwrap();
    assert_eq!(
        (sizing.equity, sizing.investable_equity, sizing.fees),
        (n("825"), n("750"), n("0.0000000001875"))
    );
    assert_eq!(sizing.required_sides.values().next().unwrap(), "sell");
}

#[test]
fn exhausted_fees_and_invalid_targets_still_fail() {
    let (fast, vector) = run(Scenario {
        capital: "1000",
        action: Action::Dividend("0.5"),
        pay_day: 4,
        commission: "0.5",
        decisions: vec![(0, vec![(1, 1.0)]), (1, vec![(0, 1.0)])],
        price_overrides: vec![],
        reference_overrides: vec![],
    });
    for error in [
        fast.unwrap_err().to_string(),
        vector.unwrap_err().to_string(),
    ] {
        assert!(error.contains("costs exhaust pre-open equity"), "{error}");
    }

    let (fast, vector) = run(Scenario {
        capital: "1000",
        action: Action::None,
        pay_day: 4,
        commission: "0",
        decisions: vec![(0, vec![(1, 1.5)])],
        price_overrides: vec![],
        reference_overrides: vec![],
    });
    let error = fast.unwrap_err().to_string();
    assert!(
        error.contains("weights must be finite and in [0, 1]"),
        "{error}"
    );
    let error = vector.unwrap_err().to_string();
    assert!(
        error.contains("portfolio weights must sum to at most 1"),
        "{error}"
    );
}

#[test]
fn lot_unpaid_and_payment_lifecycle_preserves_full_and_partial_targets() {
    for targets in [vec![(1, 1.0)], vec![(1, 0.4), (0, 0.2)], vec![(0, 1.0)]] {
        let (fast, _) = run_sizing(
            Scenario {
                capital: "1000",
                action: Action::Dividend("0.5"),
                pay_day: 4,
                commission: "0",
                decisions: vec![
                    (0, vec![(1, 1.0)]),
                    (1, targets.clone()),
                    (2, targets.clone()),
                    (
                        3,
                        targets
                            .iter()
                            .map(|(id, w)| (if *id == 0 { 2 } else { *id }, *w))
                            .collect(),
                    ),
                ],
                price_overrides: vec![],
                reference_overrides: vec![],
            },
            Sizing::Lot,
            "100",
            "0",
        );
        let fast = fast.unwrap();
        assert_identities(&fast);
        assert_eq!(fast.semantic_version, "fast_event@11");
        let ex = portfolio(&fast, 2, false);
        assert_eq!(ex.equity, n("1000"));
        assert_eq!(ex.dividend_receivable, n("500"));
        // Independently: 500 investment budget, targets 40/20 -> 200/100
        // securities and 200 cash; full hold -> 1000 shares at 0.5;
        // full switch -> 500 shares at 1. No receivable is spendable.
        let expected = if targets.len() == 2 {
            vec![q("100"), q("400")]
        } else if targets[0].0 == 1 {
            vec![q("1000")]
        } else {
            vec![q("500")]
        };
        let mut actual = positions(&fast, 2).into_values().collect::<Vec<_>>();
        actual.sort();
        assert_eq!(actual, expected);
        assert_eq!(ex.cash, if targets.len() == 2 { n("200") } else { n("0") });
        assert!(
            fast.fills
                .iter()
                .all(|fill| fill.fill.executed_at() != fast.sessions[3].session.ts_open)
        );
        let open = portfolio(&fast, 3, false);
        let close = portfolio(&fast, 3, true);
        assert_eq!(close.equity, open.equity);
        assert_eq!(
            close.cash.mantissa(),
            open.cash.mantissa() + n("500").mantissa()
        );
        assert_eq!(close.dividend_receivable, n("0"));
        assert!(
            fast.fills.iter().any(|fill| fill.fill.executed_at()
                == fast.sessions[if targets.len() == 1 && targets[0].0 == 1 {
                    4
                } else {
                    7
                }]
                .session
                .ts_open),
            "{targets:?} {:?}",
            fast.attempts
        );
    }
}

#[test]
fn lot_unpaid_rotation_keeps_cash_limit_minimum_commission_and_actual_costs() {
    let (fast, _) = run_sizing(
        Scenario {
            // Buy 1000 at 1 plus minimum commission 5, leaving zero cash.
            capital: "1005",
            action: Action::Dividend("0.5"),
            pay_day: 8,
            commission: "0.001",
            decisions: vec![(0, vec![(1, 1.0)]), (1, vec![(0, 1.0)])],
            price_overrides: vec![],
            reference_overrides: vec![],
        },
        Sizing::Lot,
        "100",
        "5",
    );
    let fast = fast.unwrap();
    assert_identities(&fast);
    let rotation = fast
        .fills
        .iter()
        .filter(|f| f.fill.executed_at() == fast.sessions[2].session.ts_open)
        .collect::<Vec<_>>();
    assert_eq!(rotation.len(), 2);
    let buy = rotation
        .iter()
        .find(|f| f.fill.order().side() == Side::Buy)
        .unwrap();
    let sell = rotation
        .iter()
        .find(|f| f.fill.order().side() == Side::Sell)
        .unwrap();
    // Sell 1000 at .5 -> 500 - 5 = 495. Target principal 500 is
    // insufficient with fees: buy 400 at 1 + 5 fee, cash 90.
    assert_eq!(sell.fill.order().quantity(), q("1000"));
    assert_eq!(buy.fill.order().quantity(), q("400"));
    assert_eq!(sell.fill.total_fees(), n("5"));
    assert_eq!(buy.fill.total_fees(), n("5"));
    let after = portfolio(&fast, 2, false);
    assert_eq!(
        (after.cash, after.dividend_receivable, after.equity),
        (n("90"), n("500"), n("990"))
    );
    assert_eq!(buy.turnover_basis, n("400"));
}

#[test]
fn lot_all_receivable_waits_then_reinvests_after_payment() {
    let (fast, _) = run_sizing(
        Scenario {
            capital: "1000",
            action: Action::Liquidation("1"),
            pay_day: 4,
            commission: "0",
            decisions: vec![
                (0, vec![(1, 1.0)]),
                (1, vec![(2, 1.0)]),
                (2, vec![(2, 1.0)]),
                (3, vec![(2, 1.0)]),
            ],
            price_overrides: vec![],
            reference_overrides: vec![],
        },
        Sizing::Lot,
        "100",
        "0",
    );
    let fast = fast.unwrap();
    assert_identities(&fast);
    let ex = portfolio(&fast, 2, false);
    assert_eq!(
        (ex.cash, ex.liquidation_receivable, ex.equity),
        (n("0"), n("1000"), n("1000"))
    );
    assert!(ex.positions.is_empty());
    assert_eq!(fast.fills.len(), 2);
    assert_eq!(
        fast.fills[1].fill.executed_at(),
        fast.sessions[4].session.ts_open
    );
    assert_eq!(fast.fills[1].fill.order().quantity(), q("1000"));
}

#[test]
fn lot_receivable_budgets_are_virtual_portfolio_local_and_exclude_unallocated_cash() {
    let (fast, _) = run_allocated(
        Scenario {
            capital: "3000",
            action: Action::Dividend("0.5"),
            pay_day: 8,
            commission: "0",
            decisions: vec![(0, vec![(1, 1.0)]), (1, vec![(1, 0.4), (0, 0.2)])],
            price_overrides: vec![],
            reference_overrides: vec![],
        },
        Sizing::Lot,
        "100",
        "0",
        Some(vec![(VP, n("1000")), (VirtualPortfolioId(2), n("500"))]),
    );
    let fast = fast.unwrap();
    for session in &fast.sessions {
        for snapshot in [&session.open_after, &session.close] {
            for field in [
                |p: &PortfolioValuation| p.cash,
                |p: &PortfolioValuation| p.equity,
            ] {
                assert_eq!(
                    field(&snapshot.account).mantissa(),
                    snapshot
                        .portfolios
                        .values()
                        .map(|p| field(p).mantissa())
                        .sum::<i128>()
                        + n("1500").mantissa()
                );
            }
            assert_eq!(
                snapshot.account.dividend_receivable.mantissa(),
                snapshot
                    .portfolios
                    .values()
                    .map(|p| p.dividend_receivable.mantissa())
                    .sum::<i128>()
            );
            assert!(snapshot.portfolios.values().all(|p| p.cash >= n("0") && p.positions.values().all(|v| v.quantity >= q("0"))));
        }
    }
    let snapshot = &fast.sessions[2].open_after;
    // VP1 budget 500 -> A 200, B 100, cash 200, receivable 500.
    // VP2 budget 250 -> A 100, B target 50 rounds to zero, cash 150,
    // receivable 250. The unallocated 1500 never funds either target.
    assert_eq!(
        (
            snapshot.portfolios[&VP].cash,
            snapshot.portfolios[&VP].dividend_receivable
        ),
        (n("200"), n("500"))
    );
    let second = &snapshot.portfolios[&VirtualPortfolioId(2)];
    assert_eq!(
        (second.cash, second.dividend_receivable, second.equity),
        (n("150"), n("250"), n("500"))
    );
    assert_eq!(
        second
            .positions
            .values()
            .map(|p| p.market_value.mantissa())
            .sum::<i128>(),
        n("100").mantissa()
    );
}
