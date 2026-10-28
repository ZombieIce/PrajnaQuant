#[path = "common/pit.rs"]
mod fixture;

use std::collections::{BTreeMap, BTreeSet};

use polars::prelude::{NamedFrom, Series};
use prajna_account::{Side, VirtualPortfolioId};
use prajna_data::{CorporateActionKind, CorporateActionRow};
use prajna_domain::{Notional, Quantity};
use prajna_research::{
    corporate_actions::{
        ActionCoverageStatus, CorporateActionCoverage, CorporateActionLedgerKind,
        CorporateActionSchedule, ReturnBasis,
    },
    fast_event::{
        FastEventConfig, FastEventCosts, PortfolioRun, Rebalance, Sizing,
        run_fast_event_pit_with_actions, run_fast_event_pit_with_terminal_settlement,
    },
    pit_strategy::{self, RiskSignals},
    strategy::{Decision, UnfilledEntry},
    vector::{VectorCosts, run_vector_pit_with_actions, run_vector_pit_with_terminal_settlement},
};

#[test]
fn dividend_entitlement_becomes_receivable_then_cash_without_early_reinvestment() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let instrument = f.ids[0].clone();
    let action = CorporateActionRow {
        announcement_id: "synthetic-dividend".into(),
        action_index: 0,
        instrument_id: instrument.clone(),
        kind: CorporateActionKind::Dividend,
        announced_date: fixture::date(1),
        announced_at: Some(fixture::time(1, 8)),
        available_at: fixture::time(1, 15),
        record_date: Some(fixture::date(3)),
        ex_date: Some(fixture::date(4)),
        pay_date: Some(fixture::date(5)),
        effective_date: None,
        cash_per_share: Some("0.1".into()),
        share_ratio: None,
        liquidation_cash_per_share: None,
        source_raw_sha256: "synthetic".into(),
        normalization_version: "synthetic-actions@1".into(),
    };
    let actions = CorporateActionSchedule::new(
        "synthetic-actions@1",
        vec![action],
        f.ids
            .iter()
            .map(|id| CorporateActionCoverage {
                instrument_id: id.clone(),
                start: fixture::date(1),
                end: fixture::date(10),
                observed_actions: usize::from(id == &instrument),
                unapplied_action_dates: vec![],
                unexplained_price_jumps: vec![],
                status: ActionCoverageStatus::Complete,
            })
            .collect(),
    )
    .unwrap();
    let actions = actions
        .with_dividend_tax_rate("0.1".parse().unwrap())
        .unwrap();
    let decisions = vec![Decision {
        decision_session: panel.sessions[0].clone(),
        ranked: vec![],
        targets: BTreeMap::from([(instrument, 1.0)]),
    }];
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: "10000".parse().unwrap(),
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
    let fast = run_fast_event_pit_with_actions(
        &panel,
        &f.market,
        &RiskSignals::default(),
        &config,
        &portfolios,
        &actions,
    )
    .unwrap();
    let vector = run_vector_pit_with_actions(
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
    )
    .unwrap();

    let portfolio = &fast.sessions[3].close.portfolios[&VirtualPortfolioId(1)];
    assert_eq!(portfolio.cash, Notional::ZERO);
    assert_eq!(portfolio.dividend_receivable, "900".parse().unwrap());
    assert_eq!(portfolio.equity, "10900".parse().unwrap());
    let paid = &fast.sessions[4].close.portfolios[&VirtualPortfolioId(1)];
    assert_eq!(paid.cash, "900".parse().unwrap());
    assert_eq!(paid.dividend_receivable, Notional::ZERO);
    assert_eq!(paid.equity, "10900".parse().unwrap());
    assert_eq!(
        paid.positions.values().next().unwrap().quantity,
        "10000".parse::<Quantity>().unwrap()
    );
    assert_eq!(fast.return_basis, ReturnBasis::TotalReturn);
    assert_eq!(vector.return_basis, ReturnBasis::TotalReturn);
    assert_eq!(fast.semantic_version, "fast_event@8");
    assert_eq!(
        fast.corporate_action_dividend_tax_rate,
        Some("0.1".parse().unwrap())
    );
    for (fast_session, vector_session) in fast.sessions.iter().zip(&vector.sessions) {
        let open_equity = fast_session
            .open_after
            .account
            .equity
            .to_string()
            .parse::<f64>()
            .unwrap()
            / 10_000.0;
        assert!(
            (open_equity - vector_session.nav / (1.0 + vector_session.gross_return)).abs() < 1e-12
        );
    }
    assert_eq!(
        fast.fills
            .iter()
            .filter(|fill| fill.fill.executed_at() == fast.sessions[4].session.ts_open)
            .count(),
        0,
        "cash payment must not create an out-of-cycle rebalance"
    );
}

#[test]
fn terminal_settlement_ignores_liquidation_action_facts() {
    let lake = fixture::lake();
    let f = fixture::fixture_options(lake.path(), 1.0, true);
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let instrument = f.ids[0].clone();
    let liquidation = CorporateActionRow {
        announcement_id: "synthetic-liquidation".into(),
        action_index: 0,
        instrument_id: instrument.clone(),
        kind: CorporateActionKind::Liquidation,
        announced_date: fixture::date(1),
        announced_at: Some(fixture::time(1, 8)),
        available_at: fixture::time(1, 15),
        record_date: None,
        ex_date: None,
        pay_date: Some(fixture::date(7)),
        effective_date: Some(fixture::date(6)),
        cash_per_share: None,
        share_ratio: None,
        liquidation_cash_per_share: Some("0.8".into()),
        source_raw_sha256: "synthetic".into(),
        normalization_version: "synthetic-actions@1".into(),
    };
    let actions = CorporateActionSchedule::new(
        "synthetic-actions@1",
        vec![liquidation],
        f.ids
            .iter()
            .map(|id| CorporateActionCoverage {
                instrument_id: id.clone(),
                start: fixture::date(1),
                end: fixture::date(10),
                observed_actions: usize::from(id == &instrument),
                unapplied_action_dates: vec![],
                unexplained_price_jumps: vec![],
                status: ActionCoverageStatus::Complete,
            })
            .collect(),
    )
    .unwrap();
    let decisions = vec![Decision {
        // A target formed at the penultimate close must still trade at the
        // final in-lifecycle session's open; any remaining position settles
        // at that session's Catalog-driven close.
        decision_session: panel.sessions[5].clone(),
        ranked: vec![],
        targets: BTreeMap::from([(instrument, 1.0)]),
    }];
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: "10000".parse().unwrap(),
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
    let vector_costs = VectorCosts {
        commission_rate: 0.0,
        buy_slippage_bps: 0.0,
        sell_slippage_bps: 0.0,
        buy_tax_rate: 0.0,
        sell_tax_rate: 0.0,
    };

    let fast = run_fast_event_pit_with_terminal_settlement(
        &panel,
        &f.market,
        &RiskSignals::default(),
        &config,
        &portfolios,
        Some(&actions),
    )
    .unwrap();
    let vector = run_vector_pit_with_terminal_settlement(
        &panel,
        &decisions,
        &f.market,
        &RiskSignals::default(),
        &vector_costs,
        config.initial_cash,
        Some(&actions),
    )
    .unwrap();

    let ignored_kinds = [
        CorporateActionLedgerKind::LiquidationReceivable,
        CorporateActionLedgerKind::LiquidationPaid,
        CorporateActionLedgerKind::LiquidationAmountUnknown,
    ];
    assert!(
        fast.corporate_action_ledger
            .iter()
            .all(|entry| !ignored_kinds.contains(&entry.kind))
    );
    assert!(
        vector
            .corporate_action_ledger
            .iter()
            .all(|entry| !ignored_kinds.contains(&entry.kind))
    );
    assert!(fast.sessions.iter().all(|session| {
        session.close.account.liquidation_receivable == Notional::ZERO
            && session
                .close
                .portfolios
                .values()
                .all(|portfolio| portfolio.liquidation_receivable == Notional::ZERO)
    }));
    assert!(vector.sessions.iter().all(|session| {
        session
            .portfolio_components_close
            .as_ref()
            .is_some_and(|components| components.liquidation_receivable_weight == 0.0)
    }));
    assert_eq!(fast.terminal_settlements.len(), 1);
    assert_eq!(vector.terminal_settlements.len(), 1);
    assert_eq!(fast.terminal_settlements[0].session_date, fixture::date(7));
    assert_eq!(
        vector.terminal_settlements[0].session_date,
        fixture::date(7)
    );
    assert_eq!(fast.terminal_settlements[0].price, "1.0".parse().unwrap());
    assert_eq!(vector.terminal_settlements[0].price, "1.0".parse().unwrap());
    let final_session = &fast.sessions[6];
    assert!(fast.fills.iter().any(|fill| {
        fill.fill.executed_at() == final_session.session.ts_open
            && fill.fill.order().instrument_id() == &f.ids[0]
            && fill.fill.order().quantity() > "0".parse().unwrap()
    }));
    assert!(
        final_session.open_after.portfolios[&VirtualPortfolioId(1)]
            .positions
            .contains_key(&f.ids[0]),
        "position bought at the final Bar open remains held until close settlement"
    );
    assert!(
        !final_session.close.portfolios[&VirtualPortfolioId(1)]
            .positions
            .contains_key(&f.ids[0]),
        "Catalog terminal settlement removes the position at the final Bar close"
    );
    assert!(fast.terminal_settlements[0].quantity > "0".parse().unwrap());
    let final_open_execution = vector
        .executions
        .iter()
        .find(|execution| execution.session_date == fixture::date(7).to_string())
        .expect("final in-lifecycle Bar open has a Vector execution");
    assert_eq!(
        final_open_execution.kind,
        prajna_research::vector::VectorExecutionKind::Executed
    );
    assert!(
        vector.sessions[6].weights_after_execution[&f.ids[0]] > 0.0,
        "Vector permits the final Bar open trade before close settlement"
    );
    assert!(vector.terminal_settlements[0].quantity > "0".parse().unwrap());
    for (fast_session, vector_session) in fast.sessions.iter().zip(&vector.sessions) {
        let fast_open_equity = fast_session.open_after.account.equity.to_f64();
        let vector_open_equity =
            vector_session.nav / (1.0 + vector_session.gross_return) * 10_000.0;
        assert!(
            (fast_open_equity - vector_open_equity).abs() < 1e-8,
            "complete-Bar open parity on {}",
            fast_session.session.session_date
        );
    }
}

#[test]
fn terminal_settlement_carries_same_field_open_across_missing_bars_without_enabling_execution() {
    let lake = fixture::lake();
    let f = fixture::fixture_with_missing_bars_and_action_prices(
        lake.path(),
        1.0,
        true,
        BTreeMap::from([
            ((0, 4), (1.0, 0.9)),
            ((1, 4), (1.0, 0.9)),
            ((0, 7), (1.0, 0.9)),
            ((1, 7), (0.99, 0.9)),
        ]),
        BTreeSet::from([(1, 5), (1, 6)]),
    );
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let a = f.ids[0].clone();
    let b = f.ids[1].clone();
    let actions = CorporateActionSchedule::new(
        "synthetic-actions@269",
        vec![CorporateActionRow {
            announcement_id: "synthetic-dividend-gap".into(),
            action_index: 0,
            instrument_id: b.clone(),
            kind: CorporateActionKind::Dividend,
            announced_date: fixture::date(1),
            announced_at: Some(fixture::time(1, 8)),
            available_at: fixture::time(1, 15),
            record_date: Some(fixture::date(4)),
            ex_date: Some(fixture::date(5)),
            pay_date: Some(fixture::date(6)),
            effective_date: None,
            cash_per_share: Some("0.1".into()),
            share_ratio: None,
            liquidation_cash_per_share: None,
            source_raw_sha256: "synthetic".into(),
            normalization_version: "synthetic-actions@1".into(),
        }],
        f.ids
            .iter()
            .map(|id| CorporateActionCoverage {
                instrument_id: id.clone(),
                start: fixture::date(1),
                end: fixture::date(10),
                observed_actions: usize::from(id == &b),
                unapplied_action_dates: vec![],
                unexplained_price_jumps: vec![],
                status: ActionCoverageStatus::Complete,
            })
            .collect(),
    )
    .unwrap();
    let decisions = vec![
        Decision {
            decision_session: panel.sessions[2].clone(),
            ranked: vec![],
            targets: BTreeMap::from([(a.clone(), 0.5), (b.clone(), 0.5)]),
        },
        Decision {
            decision_session: panel.sessions[3].clone(),
            ranked: vec![],
            targets: BTreeMap::from([(a.clone(), 0.5)]),
        },
    ];
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: "10000".parse().unwrap(),
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
    let vector_costs = VectorCosts {
        commission_rate: 0.0,
        buy_slippage_bps: 0.0,
        sell_slippage_bps: 0.0,
        buy_tax_rate: 0.0,
        sell_tax_rate: 0.0,
    };

    let fast = run_fast_event_pit_with_terminal_settlement(
        &panel,
        &f.market,
        &RiskSignals::default(),
        &config,
        &portfolios,
        Some(&actions),
    )
    .unwrap();
    let vector = run_vector_pit_with_terminal_settlement(
        &panel,
        &decisions,
        &f.market,
        &RiskSignals::default(),
        &vector_costs,
        config.initial_cash,
        Some(&actions),
    )
    .unwrap();

    assert_eq!(fast.semantic_version, "fast_event@9");
    assert_eq!(vector.semantic_version.as_deref(), Some("vector@9"));
    assert_eq!(fast.return_basis, ReturnBasis::TotalReturn);
    assert_eq!(vector.return_basis, ReturnBasis::TotalReturn);
    assert!(fast.corporate_action_completeness.is_complete);
    assert!(vector.corporate_action_completeness.is_complete);
    assert_eq!(
        fast.sessions[3].open_after.portfolios[&VirtualPortfolioId(1)].positions[&b].quantity,
        "5000".parse().unwrap(),
        "the day-4 close decision must not execute at the same day's open"
    );
    assert!(vector.sessions[3].weights_after_execution.contains_key(&b));
    assert!((vector.sessions[3].gross_return - 0.05).abs() < 1e-12);
    assert!(vector.sessions[4].gross_return.abs() < 1e-12);
    assert!((vector.sessions[5].gross_return - (10_450.0 / 10_500.0 - 1.0)).abs() < 1e-12);
    assert!(vector.sessions[4].valuation_carried.contains(&b));
    assert!(vector.sessions[5].valuation_carried.contains(&b));
    assert!(!vector.sessions[6].valuation_carried.contains(&b));

    for (fast_session, vector_session) in fast.sessions.iter().zip(&vector.sessions) {
        let fast_open_equity = fast_session.open_after.account.equity.to_f64();
        let vector_open_equity =
            vector_session.nav / (1.0 + vector_session.gross_return) * 10_000.0;
        assert!(
            (fast_open_equity - vector_open_equity).abs() < 1e-8,
            "gap-case open parity on {}",
            fast_session.session.session_date
        );
    }

    assert_eq!(
        fast.sessions[4].open_sources[&b].session_date,
        fixture::date(4)
    );
    assert_eq!(
        fast.sessions[4].open_sources[&b].field,
        prajna_research::fast_event::PriceField::Open
    );
    assert_eq!(
        fast.sessions[4].close_sources[&b].session_date,
        fixture::date(4)
    );
    assert_eq!(
        fast.sessions[4].close_sources[&b].field,
        prajna_research::fast_event::PriceField::Close
    );
    let missing_open = &fast.sessions[4].open_after.portfolios[&VirtualPortfolioId(1)];
    assert_eq!(missing_open.positions[&b].quantity, "5000".parse().unwrap());
    assert_eq!(missing_open.cash, Notional::ZERO);
    assert_eq!(missing_open.dividend_receivable, "500".parse().unwrap());
    assert_eq!(missing_open.equity, "10500".parse().unwrap());
    let vector_missing_open = vector.sessions[4].portfolio_components.as_ref().unwrap();
    let missing_open_equity = missing_open.equity.to_f64();
    assert!(vector_missing_open.available_cash_weight.abs() < 1e-12);
    assert!(
        (vector_missing_open.dividend_receivable_weight
            - missing_open.dividend_receivable.to_f64() / missing_open_equity)
            .abs()
            < 1e-12
    );
    assert!(vector_missing_open.liquidation_receivable_weight.abs() < 1e-12);
    let paid_dividend = &fast.sessions[5].close.portfolios[&VirtualPortfolioId(1)];
    assert_eq!(
        paid_dividend.positions[&b].quantity,
        "5000".parse().unwrap()
    );
    assert_eq!(paid_dividend.cash, "500".parse().unwrap());
    assert_eq!(paid_dividend.dividend_receivable, Notional::ZERO);
    let vector_paid_dividend_close = vector.sessions[5]
        .portfolio_components_close
        .as_ref()
        .unwrap();
    let paid_dividend_equity = paid_dividend.equity.to_f64();
    assert!(
        (vector_paid_dividend_close.available_cash_weight
            - paid_dividend.cash.to_f64() / paid_dividend_equity)
            .abs()
            < 1e-12
    );
    assert!(vector_paid_dividend_close.dividend_receivable_weight.abs() < 1e-12);
    assert!(
        vector_paid_dividend_close
            .liquidation_receivable_weight
            .abs()
            < 1e-12
    );
    assert_eq!(
        fast.sessions[5].close_sources[&b].session_date,
        fixture::date(4)
    );
    assert_eq!(
        fast.sessions[5].close_sources[&b].field,
        prajna_research::fast_event::PriceField::Close
    );
    assert!(fast.fills.iter().all(|fill| {
        fill.fill.executed_at() != panel.sessions[4].ts_open
            && fill.fill.executed_at() != panel.sessions[5].ts_open
    }));

    for date in [fixture::date(5), fixture::date(6)] {
        let deferred = vector
            .executions
            .iter()
            .find(|execution| execution.session_date == date.to_string())
            .expect("missing-Bar sell remains an explicit deferred attempt");
        assert_eq!(
            deferred.kind,
            prajna_research::vector::VectorExecutionKind::Deferred
        );
        assert!(deferred.blocked.contains(&b));
    }
    let recovered = vector
        .executions
        .iter()
        .find(|execution| execution.session_date == fixture::date(7).to_string())
        .expect("pending sell is retried when a raw Bar returns");
    assert_eq!(
        recovered.kind,
        prajna_research::vector::VectorExecutionKind::Executed
    );
    assert!(fast.fills.iter().any(|fill| {
        fill.fill.executed_at() == panel.sessions[6].ts_open
            && fill.fill.order().instrument_id() == &b
            && fill.fill.order().side() == Side::Sell
    }));

    // Before the recovery Open, equity is 5,000 A + 5,000 B at 1.00 + 500
    // dividend receivable = 10,500. The recovered B Open at 0.99 makes it
    // 5,000 A + 4,950 B + 500 receivable = 10,450 before execution.
    assert_eq!(
        fast.sessions[6].open_sources[&b].session_date,
        fixture::date(7)
    );
    assert_eq!(
        fast.sessions[6].open_sources[&b].field,
        prajna_research::fast_event::PriceField::Open
    );
    let recovered_fast_open = &fast.sessions[6].open_after.portfolios[&VirtualPortfolioId(1)];
    assert_eq!(
        recovered_fast_open.positions[&a].quantity,
        "5225".parse().unwrap()
    );
    assert_eq!(recovered_fast_open.equity, "10450".parse().unwrap());
    let recovered_vector_open_equity =
        vector.sessions[6].nav / (1.0 + vector.sessions[6].gross_return) * 10_000.0;
    assert!((recovered_vector_open_equity - 10_450.0).abs() < 1e-8);

    assert_eq!(fast.terminal_settlements.len(), 1);
    assert_eq!(vector.terminal_settlements.len(), 1);
    for settlement in [
        &fast.terminal_settlements[0],
        &vector.terminal_settlements[0],
    ] {
        assert_eq!(settlement.session_date, fixture::date(7));
        assert_eq!(settlement.instrument_id, a);
        assert_eq!(settlement.quantity, "5225".parse().unwrap());
        assert_eq!(settlement.price, "0.9".parse().unwrap());
        assert_eq!(settlement.proceeds, "4702.5".parse().unwrap());
    }
    let terminal_close = &fast.sessions[6].close.portfolios[&VirtualPortfolioId(1)];
    assert!(terminal_close.positions.is_empty());
    assert_eq!(terminal_close.cash, "9927.5".parse().unwrap());
    assert_eq!(terminal_close.equity, terminal_close.cash);
    let vector_terminal_close = vector.sessions[6]
        .portfolio_components_close
        .as_ref()
        .unwrap();
    assert_eq!(vector_terminal_close.available_cash_weight, 1.0);
    assert_eq!(vector_terminal_close.dividend_receivable_weight, 0.0);
    assert_eq!(vector_terminal_close.liquidation_receivable_weight, 0.0);
}

#[test]
fn three_etf_ten_session_action_golden_reconciles_fast_event_and_vector() {
    let lake = fixture::lake();
    let f = fixture::fixture_with_action_prices(
        lake.path(),
        BTreeMap::from([
            ((0, 4), (0.9, 0.9)),
            ((0, 5), (0.99, 0.99)),
            ((0, 6), (0.66, 0.66)),
            ((0, 7), (0.66, 0.66)),
            ((0, 8), (0.66, 0.66)),
        ]),
    );
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    assert_eq!(panel.instruments.len(), 3);
    assert_eq!(panel.sessions.len(), 10);
    let a = f.ids[0].clone();
    let b = f.ids[1].clone();
    let actions = CorporateActionSchedule::new(
        "synthetic-corporate-actions@1",
        vec![
            CorporateActionRow {
                announcement_id: "dividend-a".into(),
                action_index: 0,
                instrument_id: a.clone(),
                kind: CorporateActionKind::Dividend,
                announced_date: fixture::date(1),
                announced_at: Some(fixture::time(1, 8)),
                available_at: fixture::time(1, 15),
                record_date: Some(fixture::date(3)),
                ex_date: Some(fixture::date(4)),
                pay_date: Some(fixture::date(5)),
                effective_date: None,
                cash_per_share: Some("0.1".into()),
                share_ratio: None,
                liquidation_cash_per_share: None,
                source_raw_sha256: "synthetic".into(),
                normalization_version: "synthetic-actions@1".into(),
            },
            CorporateActionRow {
                announcement_id: "conversion-a".into(),
                action_index: 0,
                instrument_id: a.clone(),
                kind: CorporateActionKind::ShareConversion,
                announced_date: fixture::date(1),
                announced_at: Some(fixture::time(1, 8)),
                available_at: fixture::time(1, 15),
                record_date: None,
                ex_date: None,
                pay_date: None,
                effective_date: Some(fixture::date(6)),
                cash_per_share: None,
                share_ratio: Some("1.5".into()),
                liquidation_cash_per_share: None,
                source_raw_sha256: "synthetic".into(),
                normalization_version: "synthetic-actions@1".into(),
            },
            CorporateActionRow {
                announcement_id: "liquidation-b".into(),
                action_index: 0,
                instrument_id: b.clone(),
                kind: CorporateActionKind::Liquidation,
                announced_date: fixture::date(1),
                announced_at: Some(fixture::time(1, 8)),
                available_at: fixture::time(1, 15),
                record_date: None,
                ex_date: None,
                pay_date: Some(fixture::date(10)),
                effective_date: Some(fixture::date(9)),
                cash_per_share: None,
                share_ratio: None,
                liquidation_cash_per_share: Some("0.8".into()),
                source_raw_sha256: "synthetic".into(),
                normalization_version: "synthetic-actions@1".into(),
            },
        ],
        f.ids
            .iter()
            .enumerate()
            .map(|(i, id)| CorporateActionCoverage {
                instrument_id: id.clone(),
                start: fixture::date(1),
                end: fixture::date(10),
                observed_actions: [2, 1, 0][i],
                unapplied_action_dates: vec![],
                unexplained_price_jumps: vec![],
                status: ActionCoverageStatus::Complete,
            })
            .collect(),
    )
    .unwrap();
    let decisions = vec![
        Decision {
            decision_session: panel.sessions[0].clone(),
            ranked: vec![],
            targets: BTreeMap::from([(a.clone(), 1.0)]),
        },
        Decision {
            decision_session: panel.sessions[6].clone(),
            ranked: vec![],
            targets: BTreeMap::from([(b.clone(), 1.0)]),
        },
    ];
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: "10000".parse().unwrap(),
        sizing: Sizing::VectorParity,
        costs: FastEventCosts::ZERO,
        lot_sizes: BTreeMap::new(),
    };
    let portfolio = [PortfolioRun {
        id: VirtualPortfolioId(1),
        capital: config.initial_cash,
        decisions: decisions.clone(),
        unfilled_entry: UnfilledEntry::Skip,
        rebalance: Rebalance::FullTarget,
    }];
    let fast = run_fast_event_pit_with_actions(
        &panel,
        &f.market,
        &RiskSignals::default(),
        &config,
        &portfolio,
        &actions,
    )
    .unwrap();
    let vector = run_vector_pit_with_actions(
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
    )
    .unwrap();
    let expected_open = [
        ("10000", "0", "0", "0", "0", "10000"),
        ("0", "0", "0", "10000", "0", "10000"),
        ("0", "0", "0", "10000", "0", "10000"),
        ("0", "1000", "0", "10000", "0", "10000"),
        ("0", "1000", "0", "10000", "0", "10900"),
        ("1000", "0", "0", "15000", "0", "10900"),
        ("1000", "0", "0", "15000", "0", "10900"),
        ("0", "0", "0", "0", "10900", "10900"),
        ("0", "0", "8720", "0", "0", "8720"),
        ("0", "0", "8720", "0", "0", "8720"),
    ];
    for (index, ((cash, dividend, liquidation, shares_a, shares_b, equity), session)) in
        expected_open.iter().zip(&fast.sessions).enumerate()
    {
        let account = &session.open_after;
        let portfolio = &account.portfolios[&VirtualPortfolioId(1)];
        assert_eq!(
            portfolio.cash,
            cash.parse().unwrap(),
            "open cash S{}",
            index + 1
        );
        assert_eq!(
            portfolio.dividend_receivable,
            dividend.parse().unwrap(),
            "open dividend receivable S{}",
            index + 1
        );
        assert_eq!(
            portfolio.liquidation_receivable,
            liquidation.parse().unwrap(),
            "open liquidation receivable S{}",
            index + 1
        );
        assert_eq!(
            portfolio
                .positions
                .get(&a)
                .map_or(Quantity::ZERO, |p| p.quantity),
            shares_a.parse().unwrap(),
            "open ETF A shares S{}",
            index + 1
        );
        assert_eq!(
            portfolio
                .positions
                .get(&b)
                .map_or(Quantity::ZERO, |p| p.quantity),
            shares_b.parse().unwrap(),
            "open ETF B shares S{}",
            index + 1
        );
        assert_eq!(
            portfolio.equity,
            equity.parse().unwrap(),
            "open NAV S{}",
            index + 1
        );
        account.validate().unwrap();
        session.close.validate().unwrap();
        assert_eq!(
            session.close.account.equity,
            equity.parse().unwrap(),
            "close NAV S{}",
            index + 1
        );
        let vector_open = vector.sessions[index].nav / (1.0 + vector.sessions[index].gross_return);
        assert!(
            (account.account.equity.to_f64() / 10_000.0 - vector_open).abs() < 1e-12,
            "Fast Event / Vector open NAV mismatch on S{}",
            index + 1
        );
        if index != 4 && index != 9 {
            assert_eq!(
                session.open_after,
                session.close,
                "open/close S{}",
                index + 1
            );
        }
    }
    assert_eq!(
        fast.sessions[4].close.portfolios[&VirtualPortfolioId(1)].cash,
        "1000".parse().unwrap()
    );
    assert_eq!(
        fast.sessions[4].close.portfolios[&VirtualPortfolioId(1)].dividend_receivable,
        Notional::ZERO
    );
    assert!(
        (vector.sessions[3]
            .portfolio_components
            .as_ref()
            .unwrap()
            .dividend_receivable_weight
            - 0.1)
            .abs()
            < 1e-12
    );
    assert_eq!(
        fast.sessions[9].close.portfolios[&VirtualPortfolioId(1)].cash,
        "8720".parse().unwrap()
    );
    assert_eq!(
        fast.sessions[9].close.portfolios[&VirtualPortfolioId(1)].liquidation_receivable,
        Notional::ZERO
    );
    assert_eq!(
        vector.sessions[4]
            .portfolio_components_close
            .as_ref()
            .unwrap()
            .available_cash_weight,
        1000.0 / 10_900.0
    );
    assert_eq!(
        vector.sessions[4]
            .portfolio_components_close
            .as_ref()
            .unwrap()
            .dividend_receivable_weight,
        0.0
    );
    assert_eq!(fast.return_basis, ReturnBasis::TotalReturn);
    assert!(fast.corporate_action_completeness.is_complete);
    assert!(vector.corporate_action_completeness.is_complete);
    assert_eq!(
        fast.corporate_action_ledger
            .iter()
            .map(|entry| entry.kind)
            .collect::<Vec<_>>(),
        [
            CorporateActionLedgerKind::DividendEntitlement,
            CorporateActionLedgerKind::DividendReceivable,
            CorporateActionLedgerKind::DividendPaid,
            CorporateActionLedgerKind::ShareConversion,
            CorporateActionLedgerKind::LiquidationReceivable,
            CorporateActionLedgerKind::LiquidationPaid,
        ]
    );
    assert_eq!(
        fast.corporate_action_ledger[4].corporate_action_value_delta,
        Some("-2180".parse().unwrap())
    );
    assert_eq!(fast.fills.len(), 3);
    assert!((vector.sessions[3].gross_return - 0.09).abs() < 1e-12);
}

#[test]
fn unknown_liquidation_and_unexplained_jump_are_reported_incomplete() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let derived = CorporateActionSchedule::from_total_return_index(
        "tri-backed-actions@1",
        &panel,
        &f.tri,
        Notional::ZERO,
    )
    .unwrap();
    assert_eq!(derived.actions().len(), 0);
    assert!(
        derived
            .coverage()
            .iter()
            .all(|coverage| coverage.status == ActionCoverageStatus::ObservedRawSetOnly)
    );
    let id = f.ids[0].clone();
    let actions = CorporateActionSchedule::new(
        "synthetic-unknown-liquidation@1",
        vec![CorporateActionRow {
            announcement_id: "unknown-liquidation".into(),
            action_index: 0,
            instrument_id: id.clone(),
            kind: CorporateActionKind::Liquidation,
            announced_date: fixture::date(1),
            announced_at: Some(fixture::time(1, 8)),
            available_at: fixture::time(1, 15),
            record_date: None,
            ex_date: None,
            pay_date: None,
            effective_date: Some(fixture::date(4)),
            cash_per_share: None,
            share_ratio: None,
            liquidation_cash_per_share: None,
            source_raw_sha256: "synthetic".into(),
            normalization_version: "synthetic-actions@1".into(),
        }],
        f.ids
            .iter()
            .enumerate()
            .map(|(i, id)| CorporateActionCoverage {
                instrument_id: id.clone(),
                start: fixture::date(1),
                end: fixture::date(10),
                observed_actions: usize::from(i == 0),
                unapplied_action_dates: vec![],
                unexplained_price_jumps: if i == 0 {
                    vec![fixture::date(4)]
                } else {
                    vec![]
                },
                status: if i == 1 {
                    ActionCoverageStatus::ObservedRawSetOnly
                } else {
                    ActionCoverageStatus::Complete
                },
            })
            .collect(),
    )
    .unwrap();
    let decisions = vec![Decision {
        decision_session: panel.sessions[0].clone(),
        ranked: vec![],
        targets: BTreeMap::from([(id.clone(), 1.0)]),
    }];
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: "10000".parse().unwrap(),
        sizing: Sizing::VectorParity,
        costs: FastEventCosts::ZERO,
        lot_sizes: BTreeMap::new(),
    };
    let portfolio = [PortfolioRun {
        id: VirtualPortfolioId(1),
        capital: config.initial_cash,
        decisions: decisions.clone(),
        unfilled_entry: UnfilledEntry::Skip,
        rebalance: Rebalance::FullTarget,
    }];
    let fast = run_fast_event_pit_with_actions(
        &panel,
        &f.market,
        &RiskSignals::default(),
        &config,
        &portfolio,
        &actions,
    )
    .unwrap();
    let vector = run_vector_pit_with_actions(
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
    )
    .unwrap();
    assert!(!fast.corporate_action_completeness.is_complete);
    assert!(!vector.corporate_action_completeness.is_complete);
    assert_eq!(
        fast.corporate_action_coverage
            .iter()
            .find(|coverage| coverage.instrument_id == id)
            .unwrap()
            .status,
        ActionCoverageStatus::Unknown
    );
    assert!(
        fast.corporate_action_completeness
            .reasons
            .iter()
            .any(|reason| reason.contains("liquidation amount is unknown"))
    );
    assert!(
        fast.corporate_action_completeness
            .reasons
            .iter()
            .any(|reason| reason.contains("unexplained price jumps"))
    );
    assert_eq!(
        fast.sessions[3].open_after.portfolios[&VirtualPortfolioId(1)].positions[&id].quantity,
        "10000".parse().unwrap(),
        "unknown liquidation value must not erase shares or fabricate zero cash"
    );
}

#[test]
fn complete_empty_action_schedule_preserves_price_return_results() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let id = f.ids[0].clone();
    let decisions = vec![Decision {
        decision_session: panel.sessions[0].clone(),
        ranked: vec![],
        targets: BTreeMap::from([(id, 1.0)]),
    }];
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: "10000".parse().unwrap(),
        sizing: Sizing::VectorParity,
        costs: FastEventCosts::ZERO,
        lot_sizes: BTreeMap::new(),
    };
    let portfolio = [PortfolioRun {
        id: VirtualPortfolioId(1),
        capital: config.initial_cash,
        decisions: decisions.clone(),
        unfilled_entry: UnfilledEntry::Skip,
        rebalance: Rebalance::FullTarget,
    }];
    let empty_actions = CorporateActionSchedule::new(
        "synthetic-no-actions@1",
        vec![],
        f.ids
            .iter()
            .map(|id| CorporateActionCoverage {
                instrument_id: id.clone(),
                start: fixture::date(1),
                end: fixture::date(10),
                observed_actions: 0,
                unapplied_action_dates: vec![],
                unexplained_price_jumps: vec![],
                status: ActionCoverageStatus::Complete,
            })
            .collect(),
    )
    .unwrap();
    let price_fast = prajna_research::fast_event::run_fast_event_pit(
        &panel,
        &f.market,
        &RiskSignals::default(),
        &config,
        &portfolio,
    )
    .unwrap();
    let total_return_fast = run_fast_event_pit_with_actions(
        &panel,
        &f.market,
        &RiskSignals::default(),
        &config,
        &portfolio,
        &empty_actions,
    )
    .unwrap();
    let costs = VectorCosts {
        commission_rate: 0.0,
        buy_slippage_bps: 0.0,
        sell_slippage_bps: 0.0,
        buy_tax_rate: 0.0,
        sell_tax_rate: 0.0,
    };
    let price_vector = prajna_research::vector::run_vector_pit(
        &panel,
        &decisions,
        &f.market,
        &RiskSignals::default(),
        &costs,
        config.initial_cash,
    )
    .unwrap();
    let total_return_vector = run_vector_pit_with_actions(
        &panel,
        &decisions,
        &f.market,
        &RiskSignals::default(),
        &costs,
        config.initial_cash,
        &empty_actions,
    )
    .unwrap();
    assert_eq!(total_return_fast.return_basis, ReturnBasis::TotalReturn);
    assert!(total_return_fast.corporate_action_completeness.is_complete);
    assert_eq!(price_fast.fills.len(), total_return_fast.fills.len());
    for (price, total_return) in price_fast.sessions.iter().zip(&total_return_fast.sessions) {
        assert_eq!(price.open_after, total_return.open_after);
        assert_eq!(price.close, total_return.close);
    }
    for (price, total_return) in price_vector
        .sessions
        .iter()
        .zip(&total_return_vector.sessions)
    {
        assert!((price.nav - total_return.nav).abs() < 1e-12);
        assert!((price.gross_return - total_return.gross_return).abs() < 1e-12);
    }
}

#[test]
fn fractional_share_conversion_rounds_down_and_records_equity_difference() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let id = f.ids[0].clone();
    let actions = CorporateActionSchedule::new(
        "synthetic-fractional-conversion@1",
        vec![CorporateActionRow {
            announcement_id: "fractional-conversion".into(),
            action_index: 0,
            instrument_id: id.clone(),
            kind: CorporateActionKind::ShareConversion,
            announced_date: fixture::date(1),
            announced_at: Some(fixture::time(1, 8)),
            available_at: fixture::time(1, 15),
            record_date: None,
            ex_date: None,
            pay_date: None,
            effective_date: Some(fixture::date(4)),
            cash_per_share: None,
            share_ratio: Some("1.5".into()),
            liquidation_cash_per_share: None,
            source_raw_sha256: "synthetic".into(),
            normalization_version: "synthetic-actions@1".into(),
        }],
        f.ids
            .iter()
            .enumerate()
            .map(|(i, instrument_id)| CorporateActionCoverage {
                instrument_id: instrument_id.clone(),
                start: fixture::date(1),
                end: fixture::date(10),
                observed_actions: usize::from(i == 0),
                unapplied_action_dates: vec![],
                unexplained_price_jumps: vec![],
                status: ActionCoverageStatus::Complete,
            })
            .collect(),
    )
    .unwrap();
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: "101".parse().unwrap(),
        sizing: Sizing::Lot,
        costs: FastEventCosts::ZERO,
        lot_sizes: f
            .ids
            .iter()
            .cloned()
            .map(|id| (id, "1".parse().unwrap()))
            .collect(),
    };
    let result = run_fast_event_pit_with_actions(
        &panel,
        &f.market,
        &RiskSignals::default(),
        &config,
        &[PortfolioRun {
            id: VirtualPortfolioId(1),
            capital: config.initial_cash,
            decisions: vec![Decision {
                decision_session: panel.sessions[0].clone(),
                ranked: vec![],
                targets: BTreeMap::from([(id.clone(), 1.0)]),
            }],
            unfilled_entry: UnfilledEntry::Skip,
            rebalance: Rebalance::FullTarget,
        }],
        &actions,
    )
    .unwrap();
    assert_eq!(
        result.semantic_version, "fast_event@11",
        "lot keeps the pre-cash-budget corporate-action identity"
    );
    let portfolio = &result.sessions[3].open_after.portfolios[&VirtualPortfolioId(1)];
    assert_eq!(portfolio.cash, Notional::ZERO);
    assert_eq!(portfolio.positions[&id].quantity, "151".parse().unwrap());
    assert_eq!(portfolio.equity, "151".parse().unwrap());
    let conversion = result
        .corporate_action_ledger
        .iter()
        .find(|entry| entry.kind == CorporateActionLedgerKind::ShareConversion)
        .unwrap();
    assert_eq!(conversion.quantity_before, Some("101".parse().unwrap()));
    assert_eq!(conversion.quantity_after, Some("151".parse().unwrap()));
    assert_eq!(conversion.share_ratio, Some("1.5".parse().unwrap()));
    assert_eq!(
        conversion.theoretical_quantity,
        Some("151.5".parse().unwrap())
    );
    assert_eq!(conversion.residual_quantity, Some("0.5".parse().unwrap()));
    assert_eq!(
        conversion.corporate_action_rounding,
        Some("-0.5".parse().unwrap())
    );
}

#[test]
fn stale_conversion_mark_is_incomplete_and_does_not_claim_zero_rounding() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let mut panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let id = f.ids[0].clone();
    let row = panel.instruments.binary_search(&id).unwrap() * panel.sessions.len() + 3;
    let has_bar = panel.grid.column("has_bar").unwrap().bool().unwrap();
    let mut bars = (0..has_bar.len())
        .map(|index| has_bar.get(index))
        .collect::<Vec<_>>();
    bars[row] = Some(false);
    panel
        .grid
        .with_column(Series::new("has_bar".into(), bars).into())
        .unwrap();
    for name in ["open_decimal", "close_decimal"] {
        let prices = panel.grid.column(name).unwrap().str().unwrap();
        let mut values = (0..prices.len())
            .map(|index| prices.get(index).map(str::to_owned))
            .collect::<Vec<_>>();
        values[row] = None;
        panel
            .grid
            .with_column(Series::new(name.into(), values).into())
            .unwrap();
    }
    let actions = CorporateActionSchedule::new(
        "synthetic-stale-conversion@1",
        vec![CorporateActionRow {
            announcement_id: "stale-conversion".into(),
            action_index: 0,
            instrument_id: id.clone(),
            kind: CorporateActionKind::ShareConversion,
            announced_date: fixture::date(1),
            announced_at: Some(fixture::time(1, 8)),
            available_at: fixture::time(1, 15),
            record_date: None,
            ex_date: None,
            pay_date: None,
            effective_date: Some(fixture::date(4)),
            cash_per_share: None,
            share_ratio: Some("1.5".into()),
            liquidation_cash_per_share: None,
            source_raw_sha256: "synthetic".into(),
            normalization_version: "synthetic-actions@1".into(),
        }],
        f.ids
            .iter()
            .enumerate()
            .map(|(i, instrument_id)| CorporateActionCoverage {
                instrument_id: instrument_id.clone(),
                start: fixture::date(1),
                end: fixture::date(10),
                observed_actions: usize::from(i == 0),
                unapplied_action_dates: vec![],
                unexplained_price_jumps: vec![],
                status: ActionCoverageStatus::Complete,
            })
            .collect(),
    )
    .unwrap();
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: "101".parse().unwrap(),
        sizing: Sizing::Lot,
        costs: FastEventCosts::ZERO,
        lot_sizes: f
            .ids
            .iter()
            .cloned()
            .map(|id| (id, "1".parse().unwrap()))
            .collect(),
    };
    let result = run_fast_event_pit_with_actions(
        &panel,
        &f.market,
        &RiskSignals::default(),
        &config,
        &[PortfolioRun {
            id: VirtualPortfolioId(1),
            capital: config.initial_cash,
            decisions: vec![Decision {
                decision_session: panel.sessions[0].clone(),
                ranked: vec![],
                targets: BTreeMap::from([(id, 1.0)]),
            }],
            unfilled_entry: UnfilledEntry::Skip,
            rebalance: Rebalance::FullTarget,
        }],
        &actions,
    )
    .unwrap();
    let conversion = result
        .corporate_action_ledger
        .iter()
        .find(|entry| entry.kind == CorporateActionLedgerKind::ShareConversion)
        .unwrap();
    assert_eq!(conversion.corporate_action_rounding, None);
    assert!(!result.corporate_action_completeness.is_complete);
    assert!(
        result
            .corporate_action_completeness
            .reasons
            .iter()
            .any(|reason| reason.contains("conversion rounding cannot be valued"))
    );
}

#[test]
fn liquidation_reconciles_shared_positions_across_virtual_portfolios() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let instrument = f.ids[0].clone();
    let actions = CorporateActionSchedule::new(
        "synthetic-shared-liquidation@1",
        vec![CorporateActionRow {
            announcement_id: "shared-liquidation".into(),
            action_index: 0,
            instrument_id: instrument.clone(),
            kind: CorporateActionKind::Liquidation,
            announced_date: fixture::date(1),
            announced_at: Some(fixture::time(1, 8)),
            available_at: fixture::time(1, 15),
            record_date: None,
            ex_date: None,
            pay_date: Some(fixture::date(5)),
            effective_date: Some(fixture::date(4)),
            cash_per_share: None,
            share_ratio: None,
            liquidation_cash_per_share: Some("0.8".into()),
            source_raw_sha256: "synthetic".into(),
            normalization_version: "synthetic-actions@1".into(),
        }],
        f.ids
            .iter()
            .enumerate()
            .map(|(i, id)| CorporateActionCoverage {
                instrument_id: id.clone(),
                start: fixture::date(1),
                end: fixture::date(10),
                observed_actions: usize::from(i == 0),
                unapplied_action_dates: vec![],
                unexplained_price_jumps: vec![],
                status: ActionCoverageStatus::Complete,
            })
            .collect(),
    )
    .unwrap();
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: "10000".parse().unwrap(),
        sizing: Sizing::VectorParity,
        costs: FastEventCosts::ZERO,
        lot_sizes: BTreeMap::new(),
    };
    let portfolios = [1, 2].map(|id| PortfolioRun {
        id: VirtualPortfolioId(id),
        capital: "5000".parse().unwrap(),
        decisions: vec![Decision {
            decision_session: panel.sessions[0].clone(),
            ranked: vec![],
            targets: BTreeMap::from([(instrument.clone(), 1.0)]),
        }],
        unfilled_entry: UnfilledEntry::Skip,
        rebalance: Rebalance::FullTarget,
    });
    let result = run_fast_event_pit_with_actions(
        &panel,
        &f.market,
        &RiskSignals::default(),
        &config,
        &portfolios,
        &actions,
    )
    .unwrap();
    let open = &result.sessions[3].open_after;
    assert!(open.account.positions.is_empty());
    assert_eq!(open.account.liquidation_receivable, "8000".parse().unwrap());
    assert_eq!(open.account.equity, "8000".parse().unwrap());
    for id in [VirtualPortfolioId(1), VirtualPortfolioId(2)] {
        assert_eq!(
            open.portfolios[&id].liquidation_receivable,
            "4000".parse().unwrap()
        );
    }
    open.validate().unwrap();
    assert_eq!(
        result.sessions[4].close.account.cash,
        "8000".parse().unwrap()
    );
    assert_eq!(result.corporate_action_ledger.len(), 4);
}
