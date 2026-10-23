//! Entirely synthetic #271 scenario retained as the #274 investable-equity
//! regression: normal parity full-target plans size from cash plus securities,
//! keep receivables in total NAV, and report fractions against total NAV. The
//! original rejection policy stays reproducible from the pre-#274 revision and
//! its evidence in #271 / PR #272; these tests cover the replacement policy.
#[allow(dead_code)]
#[path = "common/pit.rs"]
mod fixture;

use std::collections::BTreeMap;

use prajna_account::{PortfolioValuation, Side, VirtualPortfolioId};
use prajna_data::{CorporateActionKind, CorporateActionRow};
use prajna_domain::{InstrumentId, Notional, Quantity};
use prajna_research::{
    corporate_actions::{ActionCoverageStatus, CorporateActionCoverage, CorporateActionSchedule},
    fast_event::{
        FastEventConfig, FastEventCosts, FastEventError, FastEventResult, PortfolioRun, Rebalance,
        Sizing, SizingAdjustmentReason, run_fast_event_pit_with_actions,
    },
    pit_strategy::{self, RiskSignals},
    strategy::{Decision, UnfilledEntry},
    vector::{
        VectorCosts, VectorError, VectorExecutionKind, VectorPitSizing, VectorResult,
        run_vector_pit_with_actions,
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
    commission: &'static str,
    /// `(decision session index, [(fixture instrument index, target weight)])`.
    decisions: Vec<(usize, Vec<(usize, f64)>)>,
    /// `(instrument index, session day, price)`; applied to open and close.
    price_overrides: Vec<(usize, u32, f64)>,
}

fn action_row(action: Action, instrument_id: InstrumentId) -> Option<CorporateActionRow> {
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
        pay_date: Some(fixture::date(4)),
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
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let instrument = f.ids[1].clone();
    let action = action_row(scenario.action, instrument.clone());
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
        venue: "SZSE".parse().unwrap(),
        initial_cash: scenario.capital.parse().unwrap(),
        sizing: Sizing::VectorParity,
        costs: FastEventCosts {
            commission_rate: scenario.commission.parse().unwrap(),
            ..FastEventCosts::ZERO
        },
        lot_sizes: BTreeMap::new(),
    };
    let portfolios = [PortfolioRun {
        id: VP,
        capital: config.initial_cash,
        decisions: decisions.clone(),
        unfilled_entry: UnfilledEntry::Skip,
        rebalance: Rebalance::FullTarget,
    }];
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
            &f.market,
            &RiskSignals::default(),
            &config,
            &portfolios,
            &actions,
        ),
        run_vector_pit_with_actions(
            &panel,
            &decisions,
            &f.market,
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
        commission: "0",
        decisions: vec![
            (0, vec![(1, 1.0)]),
            (1, vec![(1, 1.0)]),
            (3, vec![(1, 1.0)]),
        ],
        price_overrides: vec![],
    });
    let (fast, vector) = (fast.unwrap(), vector.unwrap());
    assert_identities(&fast);
    assert_eq!(fast.fills.len(), 2, "ex-date full target needs no trade");

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
        commission: "0",
        decisions: vec![(0, vec![(1, 1.0)]), (1, vec![(1, 0.4), (0, 0.2)])],
        price_overrides: vec![],
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
        commission: "0.01",
        decisions: vec![(0, vec![(1, 1.0)]), (1, vec![(0, 1.0)])],
        price_overrides: vec![],
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
        commission: "0",
        decisions: vec![(0, vec![(1, 1.0)]), (1, vec![(1, 0.5), (0, 0.5)])],
        price_overrides: (3..=10).map(|day| (0, day, 3.0)).collect(),
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
        commission: "0",
        decisions: vec![
            (0, vec![(1, 1.0)]),
            (1, vec![(2, 1.0)]),
            (2, vec![(2, 1.0)]),
            (3, vec![(2, 1.0)]),
        ],
        price_overrides: vec![],
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
        commission: "0",
        decisions: vec![(0, vec![(1, 1.0)]), (1, vec![(1, 0.5), (2, 0.5)])],
        price_overrides: vec![],
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
fn exhausted_fees_and_invalid_targets_still_fail() {
    let (fast, vector) = run(Scenario {
        capital: "1000",
        action: Action::Dividend("0.5"),
        commission: "0.5",
        decisions: vec![(0, vec![(1, 1.0)]), (1, vec![(0, 1.0)])],
        price_overrides: vec![],
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
        commission: "0",
        decisions: vec![(0, vec![(1, 1.5)])],
        price_overrides: vec![],
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
