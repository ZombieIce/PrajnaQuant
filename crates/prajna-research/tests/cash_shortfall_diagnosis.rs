//! Entirely synthetic #271 diagnosis: NAV can include unspendable dividends.
//! These tests preserve the current rejection policy; they do not authorize a
//! new full-target allocator or claim frozen-data acceptance.
#[allow(dead_code)]
#[path = "common/pit.rs"]
mod fixture;

use std::collections::BTreeMap;

use prajna_account::VirtualPortfolioId;
use prajna_data::{CorporateActionKind, CorporateActionRow};
use prajna_domain::{Notional, Quantity};
use prajna_research::{
    corporate_actions::{ActionCoverageStatus, CorporateActionCoverage, CorporateActionSchedule},
    fast_event::{
        FastEventConfig, FastEventCosts, FastEventError, FastEventResult, PortfolioRun, Rebalance,
        Sizing, run_fast_event_pit_with_actions,
    },
    pit_strategy::{self, RiskSignals},
    strategy::{Decision, UnfilledEntry},
    vector::{VectorCosts, VectorError, VectorResult, run_vector_pit_with_actions},
};

const SHORTFALL: &str = "full-target cash shortfall is not attributable to scale-18 rounding";

fn run(
    rebalance: Option<(usize, f64)>,
    dividend: bool,
) -> (
    Result<FastEventResult, FastEventError>,
    Result<VectorResult, VectorError>,
) {
    let lake = fixture::lake();
    let overrides = (3..=10).map(|day| ((1, day), (0.5, 0.5))).collect();
    let f = fixture::fixture_with_action_prices(lake.path(), overrides);
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    // The second synthetic instrument has no fixture-specific halt/unknown days.
    let instrument = f.ids[1].clone();
    let action = CorporateActionRow {
        announcement_id: "synthetic-dividend".into(),
        action_index: 0,
        instrument_id: instrument.clone(),
        kind: CorporateActionKind::Dividend,
        announced_date: fixture::date(1),
        announced_at: Some(fixture::time(1, 8)),
        available_at: fixture::time(1, 15),
        record_date: Some(fixture::date(2)),
        ex_date: Some(fixture::date(3)),
        pay_date: Some(fixture::date(4)),
        effective_date: None,
        cash_per_share: Some("0.5".into()),
        share_ratio: None,
        liquidation_cash_per_share: None,
        source_raw_sha256: "synthetic".into(),
        normalization_version: "synthetic-actions@1".into(),
    };
    let actions = CorporateActionSchedule::new(
        "synthetic-actions@1",
        if dividend { vec![action] } else { vec![] },
        f.ids
            .iter()
            .map(|id| CorporateActionCoverage {
                instrument_id: id.clone(),
                start: fixture::date(1),
                end: fixture::date(10),
                observed_actions: usize::from(dividend && id == &instrument),
                unapplied_action_dates: vec![],
                unexplained_price_jumps: vec![],
                status: ActionCoverageStatus::Complete,
            })
            .collect(),
    )
    .unwrap();
    let decision = |index: usize, target| Decision {
        decision_session: panel.sessions[index].clone(),
        ranked: vec![],
        targets: BTreeMap::from([(instrument.clone(), target)]),
    };
    let mut decisions = vec![decision(0, 1.0)];
    if let Some((index, target)) = rebalance {
        decisions.push(decision(index, target));
    }
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venue: "SZSE".parse().unwrap(),
        initial_cash: "1000".parse().unwrap(),
        sizing: Sizing::VectorParity,
        costs: FastEventCosts::ZERO,
        lot_sizes: BTreeMap::new(),
    };
    let portfolios = [PortfolioRun {
        id: VirtualPortfolioId(1),
        capital: config.initial_cash,
        decisions: decisions.clone(),
        unfilled_entry: UnfilledEntry::Skip,
        rebalance: Rebalance::FullTarget,
    }];
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
            &VectorCosts {
                commission_rate: 0.0,
                buy_slippage_bps: 0.0,
                sell_slippage_bps: 0.0,
                buy_tax_rate: 0.0,
                sell_tax_rate: 0.0,
            },
            config.initial_cash,
            &actions,
        ),
    )
}

fn assert_successful_parity(fast: &FastEventResult, vector: &VectorResult) {
    assert_eq!(fast.semantic_version, "fast_event@5");
    assert_eq!(vector.semantic_version.as_deref(), Some("vector@5"));
    assert_eq!(fast.sessions.len(), vector.sessions.len());
    for (f, v) in fast.sessions.iter().zip(&vector.sessions) {
        for snapshot in [&f.open_after, &f.close] {
            let p = &snapshot.portfolios[&VirtualPortfolioId(1)];
            assert_eq!(&snapshot.account, p);
            assert_eq!(
                p.equity.mantissa(),
                p.cash.mantissa()
                    + p.positions
                        .values()
                        .map(|p| p.market_value.mantissa())
                        .sum::<i128>()
                    + p.dividend_receivable.mantissa()
                    + p.liquidation_receivable.mantissa()
            );
            assert!(p.cash >= Notional::ZERO);
            assert!(p.positions.values().all(|p| p.quantity >= Quantity::ZERO));
        }
        let p = &f.open_after.portfolios[&VirtualPortfolioId(1)];
        let equity = p.equity.to_f64();
        assert!((v.nav / (1.0 + v.gross_return) * 1000.0 - equity).abs() < 1e-8);
        assert_eq!(v.cost, 0.0);
    }
}

#[test]
fn unpaid_dividend_causes_non_quantization_shortfall_at_both_pit_entries() {
    // D1 close: target 100%; D2 open: buy 1000 at 1, cash 0.
    // D2 close locks 500 dividend; D3 open: 1000 * .5 + 500 receivable = 1000.
    // Full target wants 2000 shares, needing another 500 cash. Exact arithmetic
    // cannot fund that buy. On pay-date D4 open it is still unspendable: payment
    // occurs at D4 close. No rounding, costs, gates or missing Bars are needed.
    for (index, target) in [(1, 1.0), (2, 1.0), (1, 0.75)] {
        let (fast, vector) = run(Some((index, target)), true);
        for error in [
            fast.unwrap_err().to_string(),
            vector.unwrap_err().to_string(),
        ] {
            assert!(error.contains(SHORTFALL), "{error}");
        }
    }
}

#[test]
fn removing_rebalance_or_leaving_receivable_uninvested_preserves_exact_accounts() {
    for rebalance in [None, Some((1, 0.5))] {
        let (fast, vector) = run(rebalance, true);
        let (fast, vector) = (fast.unwrap(), vector.unwrap());
        assert_successful_parity(&fast, &vector);
        assert_eq!(fast.fills.len(), 1);
        assert_eq!(
            fast.fills[0].fill.executed_at(),
            fast.sessions[1].session.ts_open
        );
        let ex = &fast.sessions[2].open_after.portfolios[&VirtualPortfolioId(1)];
        assert_eq!(ex.cash, Notional::ZERO);
        assert_eq!(
            ex.positions.values().next().unwrap().quantity,
            "1000".parse().unwrap()
        );
        assert_eq!(ex.dividend_receivable, "500".parse().unwrap());
        assert_eq!(ex.equity, "1000".parse().unwrap());
        let components = vector.sessions[2].portfolio_components.as_ref().unwrap();
        assert_eq!(components.dividend_receivable_weight, 0.5);
        assert_eq!(components.available_cash_weight, 0.0);
        let paid_open = &fast.sessions[3].open_after.portfolios[&VirtualPortfolioId(1)];
        assert_eq!(paid_open.cash, Notional::ZERO);
        assert_eq!(paid_open.dividend_receivable, "500".parse().unwrap());
        let paid_close = &fast.sessions[3].close.portfolios[&VirtualPortfolioId(1)];
        assert_eq!(paid_close.cash, "500".parse().unwrap());
        assert_eq!(paid_close.dividend_receivable, Notional::ZERO);
        assert_eq!(paid_close.equity, "1000".parse().unwrap());
    }
}

#[test]
fn full_target_after_payment_or_without_receivable_needs_no_funding_exception() {
    for (rebalance, dividend, expected_quantity, expected_equity, fills) in [
        (Some((3, 1.0)), true, "2000", "1000", 2),
        (Some((1, 1.0)), false, "1000", "500", 1),
    ] {
        let (fast, vector) = run(rebalance, dividend);
        let (fast, vector) = (fast.unwrap(), vector.unwrap());
        assert_successful_parity(&fast, &vector);
        assert_eq!(fast.fills.len(), fills);
        let p = &fast.sessions[4].open_after.portfolios[&VirtualPortfolioId(1)];
        assert_eq!(p.cash, Notional::ZERO);
        assert_eq!(p.dividend_receivable, Notional::ZERO);
        assert_eq!(
            p.positions.values().next().unwrap().quantity,
            expected_quantity.parse().unwrap()
        );
        assert_eq!(p.equity, expected_equity.parse().unwrap());
        if dividend {
            assert_eq!(
                fast.fills[1].fill.executed_at(),
                fast.sessions[4].session.ts_open
            );
        }
    }
}
