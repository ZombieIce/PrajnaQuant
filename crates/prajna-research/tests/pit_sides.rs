#[path = "common/pit.rs"]
mod fixture;

use prajna_account::VirtualPortfolioId;
use prajna_data::{CorporateActionKind, CorporateActionRow};
use prajna_domain::{InstrumentId, Quantity};
use prajna_research::{
    etf_market::{DerivedMarket, MarketEvidence},
    etf_momentum::{
        CorporateActionRules, MVP6_MARKET_DRIVEN_CORPORATE_ACTION_RULES_VERSION, TotalReturnIndex,
    },
    fast_event::{
        FastEventConfig, FastEventCosts, FastEventResult, PortfolioRun, Rebalance, Sizing,
        run_fast_event_pit,
    },
    pit_strategy::{self, RiskSignals},
    strategy::{Decision, UnfilledEntry},
    vector::{VectorCosts, run_vector_pit},
};
use std::collections::BTreeMap;

fn try_build_tri_with_actions(
    root: &std::path::Path,
    fixture: &fixture::Fixture,
    actions: serde_json::Value,
    normalizer_version: &str,
    rules_version: &str,
) -> Result<TotalReturnIndex, prajna_research::factor::FactorError> {
    let raw = prajna_data::RawStore::open(root).unwrap();
    let mut hashes = prajna_data::read_manifest(root, &fixture.dsv)
        .unwrap()
        .core
        .inputs
        .into_iter()
        .map(|input| input.raw_sha256)
        .collect::<Vec<_>>();
    let announced_at = chrono::DateTime::parse_from_rfc3339("2023-01-02T00:00:00+08:00")
        .unwrap()
        .timestamp_millis();
    let action_bytes = serde_json::to_vec(&serde_json::json!({
        "announcements": [{
            "announcementId": "SYN-ACTION-1",
            "secCode": "159001",
            "secName": "Synthetic ETF",
            "announcementTitle": "Synthetic ETF corporate actions",
            "announcementTime": announced_at,
            "corporateActions": actions
        }],
        "totalRecordNum": 1,
        "hasMore": false
    }))
    .unwrap();
    hashes.push(
        raw.put(
            &action_bytes,
            prajna_data::SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: prajna_data::SourceKind::FileImport,
                source_id: "synthetic-cninfo-actions".into(),
                request: serde_json::json!({}),
                observed_at: "2026-10-15T00:00:00Z".into(),
                ingested_by: "pit-sides-test".into(),
            },
        )
        .unwrap(),
    );
    let manifest = prajna_data::publish_dataset(
        root,
        &raw,
        &prajna_data::NormalizerRegistry::with_builtins(),
        "szse-etf-frozen",
        normalizer_version,
        &hashes,
        "2026-10-15T00:00:00Z",
    )
    .unwrap();
    let evidence: MarketEvidence = serde_json::from_value(fixture.input["market"].clone()).unwrap();
    let market =
        DerivedMarket::from_dataset(root, &manifest.dsv, &fixture.catalog, evidence).unwrap();
    let rules = CorporateActionRules {
        version: rules_version.into(),
        unexplained_jump_threshold_bps: 1_000,
    };
    TotalReturnIndex::build_from_dataset(&market, root, &rules, &raw)
}

fn build_tri_with_actions(
    root: &std::path::Path,
    fixture: &fixture::Fixture,
    actions: serde_json::Value,
) -> TotalReturnIndex {
    try_build_tri_with_actions(root, fixture, actions, "2", "synthetic-tri-rules@1").unwrap()
}

#[test]
fn dataset_actions_map_to_lifecycle_and_settle_after_termination() {
    let lake = fixture::lake();
    let f = fixture::fixture_options(lake.path(), 1.0, true);
    let factor_lifecycle = f
        .catalog
        .sina_factor_lifecycles()
        .into_iter()
        .find(|life| life.native_symbol == "159001")
        .unwrap();
    assert_eq!(factor_lifecycle.end_exclusive, Some(fixture::date(8)));
    let tri = build_tri_with_actions(
        lake.path(),
        &f,
        serde_json::json!([
            {
                "type": "dividend",
                "recordDate": "2023-01-03",
                "exDate": "2023-01-04",
                "payDate": "2023-01-08",
                "cashPerShare": "0.05"
            },
            {
                "type": "liquidation",
                "effectiveDate": "2023-01-09",
                "liquidationCashPerShare": "0.85"
            }
        ]),
    );

    assert_eq!(tri.actions().len(), 2);
    assert_eq!(
        tri.identity()["semantic_version"],
        "etf-total-return-index@2"
    );
    assert!(tri.identity().get("skipped_liquidation_actions").is_none());
    let instrument = f.catalog.resolve_row("159001", fixture::date(4)).unwrap();
    assert!(
        tri.actions()
            .iter()
            .all(|action| action.instrument_id == instrument)
    );
    assert_eq!(
        tri.point(&instrument, fixture::date(9)).unwrap().level,
        0.85
    );
}

#[test]
fn ex_date_only_share_conversion_is_usable_by_dataset_tri() {
    let lake = fixture::lake();
    let prices = BTreeMap::from([
        ((0, 4), (0.5, 0.5)),
        ((0, 5), (0.5, 0.5)),
        ((0, 6), (0.5, 0.5)),
        ((0, 7), (0.5, 0.5)),
        ((0, 8), (0.5, 0.5)),
        ((0, 9), (0.5, 0.5)),
        ((0, 10), (0.5, 0.5)),
    ]);
    let f = fixture::fixture_with_action_prices(lake.path(), prices);
    let tri = build_tri_with_actions(
        lake.path(),
        &f,
        serde_json::json!([{
            "type": "share_conversion",
            "exDate": "2023-01-04",
            "shareRatio": "2"
        }]),
    );

    assert_eq!(tri.actions().len(), 1);
    assert_eq!(tri.actions()[0].effective_date, Some(fixture::date(4)));
    assert_eq!(tri.point(&f.ids[0], fixture::date(4)).unwrap().level, 1.0);
}

#[test]
fn mvp6_market_driven_tri_skips_undated_liquidation_and_preserves_other_actions() {
    let lake = fixture::lake();
    let prices = (1..=10)
        .map(|day| {
            let close = if day < 5 { 1.0 } else { 0.5 };
            ((0, day), (close, close))
        })
        .collect();
    let f = fixture::fixture_with_action_prices(lake.path(), prices);
    let raw = prajna_data::RawStore::open(lake.path()).unwrap();
    let raw_hash = raw
        .put(
            b"synthetic corporate-action rows",
            prajna_data::SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: prajna_data::SourceKind::FileImport,
                source_id: "synthetic-corporate-actions".into(),
                request: serde_json::json!({}),
                observed_at: "2026-10-15T00:00:00Z".into(),
                ingested_by: "pit-sides-test".into(),
            },
        )
        .unwrap();
    let instrument = f.ids[0].clone();
    let row = |action_index, kind, ex_date, cash, ratio, liquidation_cash| CorporateActionRow {
        announcement_id: "SYN-ACTION-1".into(),
        action_index,
        instrument_id: instrument.clone(),
        kind,
        announced_date: fixture::date(1),
        announced_at: None,
        available_at: fixture::time(2, 15),
        record_date: None,
        ex_date,
        pay_date: None,
        effective_date: ex_date,
        cash_per_share: cash,
        share_ratio: ratio,
        liquidation_cash_per_share: liquidation_cash,
        source_raw_sha256: raw_hash.clone(),
        normalization_version: "szse-corporate-action@3".into(),
    };
    let dividend = row(
        0,
        CorporateActionKind::Dividend,
        Some(fixture::date(4)),
        Some("0.05".into()),
        None,
        None,
    );
    let split = row(
        1,
        CorporateActionKind::ShareConversion,
        Some(fixture::date(5)),
        None,
        Some("2".into()),
        None,
    );
    let liquidation_without_date = row(
        2,
        CorporateActionKind::Liquidation,
        None,
        None,
        None,
        Some("0.85".into()),
    );
    let liquidation_without_amount = row(
        3,
        CorporateActionKind::Liquidation,
        Some(fixture::date(6)),
        None,
        None,
        None,
    );
    let actions = vec![
        dividend.clone(),
        split.clone(),
        liquidation_without_date,
        liquidation_without_amount,
    ];
    let new_rules = CorporateActionRules {
        version: MVP6_MARKET_DRIVEN_CORPORATE_ACTION_RULES_VERSION.into(),
        unexplained_jump_threshold_bps: 1_000,
    };
    let tri = TotalReturnIndex::build_with_actions(&f.market, &actions, &new_rules, &f.dsv, &raw)
        .unwrap();
    let control = TotalReturnIndex::build_with_actions(
        &f.market,
        &[dividend, split],
        &CorporateActionRules {
            version: "synthetic-tri-rules@1".into(),
            unexplained_jump_threshold_bps: 1_000,
        },
        &f.dsv,
        &raw,
    )
    .unwrap();

    assert_eq!(tri.actions().len(), 2);
    assert_eq!(
        tri.identity()["semantic_version"],
        "etf-total-return-index@3"
    );
    assert_eq!(
        tri.identity()["skipped_liquidation_actions"][0]["reason"],
        "liquidation_ignored_by_mvp6_market_driven_semantics"
    );
    assert!(tri.skipped_liquidation_actions()[0].missing_effective_date);
    assert!(!tri.skipped_liquidation_actions()[0].missing_liquidation_amount);
    assert!(!tri.skipped_liquidation_actions()[1].missing_effective_date);
    assert!(tri.skipped_liquidation_actions()[1].missing_liquidation_amount);
    assert_eq!(
        tri.identity()["skipped_liquidation_actions"][0]["announcement_id"],
        "SYN-ACTION-1"
    );
    assert_eq!(
        tri.identity()["skipped_liquidation_actions"][0]["action_index"],
        2
    );
    assert_eq!(tri.skipped_liquidation_actions().len(), 2);
    assert_eq!(
        tri.point(&instrument, fixture::date(4)).unwrap().level,
        1.05
    );
    assert_eq!(
        tri.point(&instrument, fixture::date(5)).unwrap().level,
        1.05
    );
    assert_eq!(
        control.identity()["semantic_version"],
        "etf-total-return-index@2"
    );
    assert!(
        control
            .identity()
            .get("skipped_liquidation_actions")
            .is_none()
    );

    for day in 1..=10 {
        let date = fixture::date(day);
        assert_eq!(
            tri.point(&instrument, date)
                .map(|point| point.level.to_bits()),
            control
                .point(&instrument, date)
                .map(|point| point.level.to_bits()),
            "dividend/split TRI differs on {date}"
        );
    }

    let legacy = TotalReturnIndex::build_with_actions(
        &f.market,
        &actions,
        &CorporateActionRules {
            version: "synthetic-tri-rules@1".into(),
            unexplained_jump_threshold_bps: 1_000,
        },
        &f.dsv,
        &raw,
    );
    assert!(
        legacy.is_err(),
        "legacy TRI must still reject an undated liquidation"
    );
}

#[test]
fn mvp6_dataset_tri_skips_liquidation_and_records_input_identity() {
    let lake = fixture::lake();
    let f = fixture::fixture_options(lake.path(), 1.0, true);
    let control = &f.tri;
    let tri = try_build_tri_with_actions(
        lake.path(),
        &f,
        serde_json::json!([{
            "type": "liquidation",
            "effectiveDate": "2023-01-09",
            "liquidationCashPerShare": "0.85"
        }]),
        "2",
        MVP6_MARKET_DRIVEN_CORPORATE_ACTION_RULES_VERSION,
    )
    .unwrap();

    assert!(tri.actions().is_empty());
    assert_eq!(tri.skipped_liquidation_actions().len(), 1);
    assert_eq!(
        tri.identity()["semantic_version"],
        "etf-total-return-index@3"
    );
    assert_eq!(tri.identity()["actions"].as_array().unwrap().len(), 0);
    assert_eq!(tri.identity()["input_actions"].as_array().unwrap().len(), 1);
    assert_eq!(
        tri.identity()["skipped_liquidation_actions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let instrument = f.catalog.resolve_row("159001", fixture::date(4)).unwrap();
    for day in 1..=10 {
        let date = fixture::date(day);
        assert_eq!(
            tri.point(&instrument, date)
                .map(|point| point.level.to_bits()),
            control
                .point(&instrument, date)
                .map(|point| point.level.to_bits()),
            "dataset liquidation changed TRI on {date}"
        );
    }
}

fn case(
    initial: f64,
    target: BTreeMap<InstrumentId, f64>,
    lower: bool,
    commission: &str,
    expected: [(&str, &str, &str); 2],
    deferred: [bool; 2],
) -> (Vec<FastEventResult>, prajna_research::vector::VectorResult) {
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
    let mut results = Vec::new();
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
            positions
                .get(&f.ids[0])
                .map_or(Quantity::ZERO, |p| p.quantity),
            expected[i].0.parse::<Quantity>().unwrap(),
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
            deferred[i],
            "{sizing:?}"
        );
        if deferred[i] {
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
        results.push(result);
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
        "10000".parse().unwrap(),
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
        deferred[1]
    );
    for (session, fast_equity) in vector.sessions.iter().zip(parity_nav) {
        assert!((session.nav / (1.0 + session.gross_return) * 10000.0 - fast_equity).abs() < 1e-8);
    }
    (results, vector)
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
        [false; 2],
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
        [false; 2],
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
        [false; 2],
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
        [false; 2],
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
        [false; 2],
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
        [false; 2],
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
        [false; 2],
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
        [true; 2],
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
        [true; 2],
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
        [false; 2],
    );
}

#[test]
fn lower_limit_allows_exact_buy_fee_only_adjustment_despite_float_roundoff() {
    let results = case(
        0.2997,
        BTreeMap::from([
            ("A.SZSE".parse().unwrap(), 0.3),
            ("B.SZSE".parse().unwrap(), 0.0997),
        ]),
        true,
        "0.01",
        [
            ("2900", "900", "6162"),
            ("2988.017991", "993.017979009", "5979.023999991"),
        ],
        [false; 2],
    );
    let parity = &results.0[1];
    assert_eq!(
        parity.sessions[2].open_after.account.equity,
        "9970.03".parse().unwrap()
    );
    assert_eq!(
        parity.sessions[3].open_after.account.equity,
        "9960.05997".parse().unwrap()
    );
    assert_eq!(parity.account.fee_settlements().len(), 1);
    let fee = &parity.account.fee_settlements()[0];
    assert_eq!(fee.fee_side(), prajna_account::Side::Buy);
    assert_eq!(fee.fee_basis(), "2.991009".parse().unwrap());
    assert_eq!(fee.total_fees(), "0.02991009".parse().unwrap());
    assert_eq!(parity.fills.len(), 2);
    assert!(
        parity
            .fills
            .iter()
            .all(|f| f.fill.order().side() == prajna_account::Side::Buy)
    );
    assert_eq!(
        parity.fills[0].fill.order().quantity(),
        "2988.017991".parse().unwrap()
    );
    assert_eq!(
        parity.fills[1].fill.order().quantity(),
        "993.017979009".parse().unwrap()
    );
    assert_eq!(
        parity.fills[1].fill.total_fees(),
        "9.94011991".parse().unwrap()
    );
}

#[test]
fn real_sub_tolerance_sized_sell_still_defers_instead_of_becoming_fee_only() {
    let results = case(
        0.2997,
        BTreeMap::from([
            ("A.SZSE".parse().unwrap(), 0.299999999999999),
            ("B.SZSE".parse().unwrap(), 0.0997),
        ]),
        true,
        "0.01",
        [("2900", "900", "6162"), ("2988.017991", "0", "6982.012009")],
        [false, true],
    );
    assert_eq!(
        results.0[1].sessions[3].open_after.account.equity,
        "9970.03".parse().unwrap()
    );
    assert!(results.0[1].account.fee_settlements().is_empty());
}

#[test]
fn adjacent_fee_perturbation_preserves_the_real_money_sized_sell() {
    let results = case(
        0.2997,
        BTreeMap::from([
            ("A.SZSE".parse().unwrap(), 0.3),
            ("B.SZSE".parse().unwrap(), 0.09970000000000002),
        ]),
        true,
        "0.01",
        [("2900", "900", "6162"), ("2988.017991", "0", "6982.012009")],
        [false, true],
    );
    assert_eq!(
        results.0[1].sessions[3].open_after.account.equity,
        "9970.03".parse().unwrap()
    );
    assert!(results.0[1].account.fee_settlements().is_empty());
    let a = results.0[1].fills[0].fill.order().instrument_id();
    let trace = results
        .1
        .executions
        .iter()
        .find(|e| e.session_date == "2023-01-04")
        .unwrap()
        .pit_sizing
        .as_ref()
        .unwrap();
    assert_eq!(trace.equity, "9970.03".parse().unwrap());
    assert_eq!(trace.fees, "9.970030000000001994".parse().unwrap());
    assert_eq!(
        trace.desired_quantities[a],
        "2988.017990999999999402".parse().unwrap()
    );
    assert_eq!(trace.required_sides[a], "sell");
}

#[test]
fn money_and_quantity_boundaries_depend_on_capital_price_and_fee_components() {
    for (capital, price, commission, tax, slip, b, pre, fees, delta) in [
        (
            "1",
            1.0,
            "0.01",
            "0",
            "0",
            0.09970000000000002,
            "0.997003",
            "0.000997003",
            0,
        ),
        (
            "10000",
            1.0,
            "0.01",
            "0",
            "0",
            0.09970000000000002,
            "9970.03",
            "9.970030000000001994",
            -598,
        ),
        (
            "10000",
            2.0,
            "0.01",
            "0",
            "0",
            0.09970000000000002,
            "9970.03",
            "9.970030000000001994",
            -299,
        ),
        (
            "10000",
            7.5,
            "0.01",
            "0",
            "0",
            0.09970000000000002,
            "9970.03",
            "9.970030000000001994",
            -80,
        ),
        (
            "1000000",
            0.25,
            "0.01",
            "0",
            "0",
            0.09970000000000002,
            "997003",
            "997.003000000000199401",
            -239280,
        ),
        (
            "10000",
            2.0,
            "0.004",
            "0.002",
            "0.004",
            0.09970000000000002,
            "9970.03",
            "9.970030000000001995",
            -299,
        ),
        (
            "250000",
            3.25,
            "0.005",
            "0",
            "0",
            0.19970000000000002,
            "249625.375",
            "249.625375000000024963",
            -2304,
        ),
        (
            "10000",
            1e6,
            "0.01",
            "0",
            "0",
            0.09970000000000002,
            "9970.03",
            "9.970030000000001994",
            0,
        ),
    ] {
        let lake = fixture::lake();
        let f = fixture::fixture_at_price(lake.path(), price);
        assert_eq!(f.market.sessions().len(), 10);
        assert!(!f.tri.version().is_empty());
        assert!(!f.risk.signals.is_empty());
        let native =
            prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
        let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
        let mut evidence: MarketEvidence =
            serde_json::from_value(f.input["market"].clone()).unwrap();
        for cell in &mut evidence.openings {
            if cell.instrument_id == f.ids[0] && cell.session_date == fixture::date(4) {
                let reference = price
                    .to_string()
                    .parse::<prajna_domain::Notional>()
                    .unwrap()
                    .checked_div_decimal("0.9".parse().unwrap())
                    .unwrap();
                cell.reference.as_mut().unwrap().adjusted_previous_close =
                    prajna_domain::Price::from_mantissa(reference.mantissa()).unwrap();
            }
        }
        let market =
            DerivedMarket::from_dataset(lake.path(), &f.dsv, &f.catalog, evidence).unwrap();
        assert!(!pit_strategy::permits(
            &market,
            &panel,
            &f.ids[0],
            &panel.sessions[3],
            false
        ));
        assert!(pit_strategy::permits(
            &market,
            &panel,
            &f.ids[0],
            &panel.sessions[3],
            true
        ));
        let decisions = vec![
            Decision {
                decision_session: panel.sessions[0].clone(),
                ranked: vec![],
                targets: BTreeMap::from([(f.ids[0].clone(), 0.2997)]),
            },
            Decision {
                decision_session: panel.sessions[2].clone(),
                ranked: vec![],
                targets: BTreeMap::from([(f.ids[0].clone(), 0.3), (f.ids[1].clone(), b)]),
            },
        ];
        let config = FastEventConfig {
            currency: "CNY".parse().unwrap(),
            venue: "SZSE".parse().unwrap(),
            initial_cash: capital.parse().unwrap(),
            sizing: Sizing::VectorParity,
            lot_sizes: BTreeMap::new(),
            costs: FastEventCosts {
                commission_rate: commission.parse().unwrap(),
                buy_tax_rate: tax.parse().unwrap(),
                sell_tax_rate: tax.parse().unwrap(),
                buy_slippage_rate: slip.parse().unwrap(),
                sell_slippage_rate: slip.parse().unwrap(),
                ..FastEventCosts::ZERO
            },
        };
        let fast = run_fast_event_pit(
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
        let vector = run_vector_pit(
            &panel,
            &decisions,
            &market,
            &RiskSignals::default(),
            &VectorCosts {
                commission_rate: commission.parse().unwrap(),
                buy_tax_rate: tax.parse().unwrap(),
                sell_tax_rate: tax.parse().unwrap(),
                buy_slippage_bps: slip.parse::<f64>().unwrap() * 10000.0,
                sell_slippage_bps: slip.parse::<f64>().unwrap() * 10000.0,
            },
            config.initial_cash,
        )
        .unwrap();
        let event = vector
            .executions
            .iter()
            .find(|e| e.session_date == "2023-01-04")
            .unwrap();
        assert_eq!(
            event.kind == prajna_research::vector::VectorExecutionKind::Deferred,
            delta < 0
        );
        let trace = event.pit_sizing.as_ref().unwrap();
        assert_eq!(trace.equity, pre.parse().unwrap());
        assert_eq!(trace.fees, fees.parse().unwrap());
        let held = &fast.sessions[2].open_after.portfolios[&VirtualPortfolioId(1)].positions;
        assert_eq!(
            trace.desired_quantities[&f.ids[0]].mantissa() - held[&f.ids[0]].quantity.mantissa(),
            delta
        );
        assert_eq!(
            trace.required_sides[&f.ids[0]],
            if delta < 0 { "sell" } else { "buy" }
        );
        if delta < 0 {
            assert!(
                fast.fills
                    .iter()
                    .all(|f| f.fill.executed_at() != panel.sessions[3].ts_open)
            );
            assert_eq!(
                fast.sessions[3].open_after.account.cash,
                fast.sessions[2].open_after.account.cash
            );
            assert_eq!(
                fast.sessions[3].open_after.account.equity,
                pre.parse().unwrap()
            );
        } else {
            assert_eq!(fast.account.fee_settlements().len(), 1);
            assert_eq!(
                fast.account.fee_settlements()[0].fee_side(),
                prajna_account::Side::Buy
            );
        }
        let capital = capital.parse::<f64>().unwrap();
        for (f, v) in fast.sessions.iter().zip(&vector.sessions) {
            f.open_after.validate().unwrap();
            f.close.validate().unwrap();
            let equity = f
                .open_after
                .account
                .equity
                .to_string()
                .parse::<f64>()
                .unwrap()
                / capital;
            assert!((v.nav / (1.0 + v.gross_return) - equity).abs() < 1e-12);
            let portfolio = &f.open_after.portfolios[&VirtualPortfolioId(1)];
            for id in &panel.instruments {
                let actual = portfolio
                    .positions
                    .get(id)
                    .map_or(0.0, |p| p.market_value.to_string().parse::<f64>().unwrap())
                    / f.open_after
                        .account
                        .equity
                        .to_string()
                        .parse::<f64>()
                        .unwrap();
                assert!(
                    (v.weights_after_execution.get(id).copied().unwrap_or(0.0) - actual).abs()
                        < 1e-12
                );
            }
        }
    }
}

#[test]
fn upper_limit_blocks_buy_fee_only_and_resizes_the_other_leg_without_that_fee() {
    let results = case(
        0.2997,
        BTreeMap::from([
            ("A.SZSE".parse().unwrap(), 0.3),
            ("B.SZSE".parse().unwrap(), 0.0997),
        ]),
        false,
        "0.01",
        [
            ("2900", "900", "6162"),
            ("2988.017991", "993.020961044973", "5979.050928045027"),
        ],
        [false; 2],
    );
    let parity = &results.0[1];
    assert!(parity.account.fee_settlements().is_empty());
    assert_eq!(
        parity.sessions[3].open_after.account.equity,
        "9960.08988009".parse().unwrap()
    );
    assert_eq!(parity.fills.len(), 2);
    assert_eq!(
        parity.fills[1].fill.total_fees(),
        "9.94011991".parse().unwrap()
    );
}

#[test]
fn exact_held_quantities_survive_retrying_a_deferred_target() {
    case(
        0.002997,
        BTreeMap::from([
            ("A.SZSE".parse().unwrap(), 0.002999999999999),
            ("B.SZSE".parse().unwrap(), 0.099997),
        ]),
        true,
        "0.01",
        [
            ("0", "900", "9091"),
            ("29.9691017991", "0", "9969.7311982009"),
        ],
        [false, true],
    );
}

#[test]
fn neighboring_exact_fee_only_targets_keep_direction_despite_float_sign_variations() {
    for (initial, target, b, lot_a, lot_b, lot_cash, a, quantity_b, cash) in [
        (
            0.1998,
            0.2,
            0.0998,
            "1900",
            "900",
            "7172",
            "1994.007996",
            "995.009990004",
            "6981.021993996",
        ),
        (
            0.5994,
            0.6,
            0.0994,
            "5900",
            "900",
            "3132",
            "5958.071964",
            "987.053922036",
            "2984.994053964",
        ),
        (
            0.6993,
            0.7,
            0.0993,
            "6900",
            "900",
            "2122",
            "6944.097951",
            "985.069895049",
            "1990.972083951",
        ),
        (
            0.0997,
            0.1,
            0.2997,
            "900",
            "2900",
            "6162",
            "996.005991",
            "2985.029955027",
            "5979.023963973",
        ),
    ] {
        let results = case(
            initial,
            BTreeMap::from([
                ("A.SZSE".parse().unwrap(), target),
                ("B.SZSE".parse().unwrap(), b),
            ]),
            true,
            "0.01",
            [(lot_a, lot_b, lot_cash), (a, quantity_b, cash)],
            [false; 2],
        );
        let parity = &results.0[1];
        assert_eq!(parity.account.fee_settlements().len(), 1);
        assert_eq!(
            parity.account.fee_settlements()[0].fee_side(),
            prajna_account::Side::Buy
        );
        assert_eq!(parity.fills.len(), 2);
        assert!(
            parity
                .fills
                .iter()
                .all(|f| f.fill.order().side() == prajna_account::Side::Buy)
        );
    }
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
