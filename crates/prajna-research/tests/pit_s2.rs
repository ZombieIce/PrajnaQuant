#[path = "common/pit.rs"]
mod fixture;
use prajna_account::VirtualPortfolioId;
use prajna_data::RawStore;
use prajna_domain::{Notional, Quantity};
use prajna_research::{
    etf_eligible::EligibilityParams,
    fast_event::{
        FastEventConfig, FastEventCosts, PortfolioRun, Rebalance, Sizing, run_fast_event_pit,
    },
    pit_strategy::{self, PitS2Params},
    strategy::{ExecutionEvent, UnfilledEntry},
    vector::{VectorCosts, run_vector_pit},
};
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn three_etf_ten_session_hand_golden_and_daily_parity() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let eligibility: EligibilityParams =
        serde_json::from_value(f.input["eligibility"].clone()).unwrap();
    let plan = pit_strategy::plan(
        &f.catalog,
        &f.market,
        &f.tri,
        &f.risk,
        eligibility,
        &PitS2Params {
            lookback: 1,
            top_k: 1,
            rebalance_every: 2,
        },
        &panel.sessions,
    )
    .unwrap();
    assert!(plan.decisions[0].targets.is_empty());
    assert_eq!(
        plan.decisions[1].targets,
        BTreeMap::from([(f.ids[0].clone(), 1.0)])
    );
    assert_eq!(
        plan.decisions[2].targets,
        BTreeMap::from([(f.ids[1].clone(), 1.0)])
    );
    assert_eq!(
        plan.decisions[3].targets,
        BTreeMap::from([(f.ids[2].clone(), 1.0)])
    );
    assert!(!plan.inputs[0].eligible.universe.members.contains(&f.ids[2]));
    assert!(!plan.inputs[4].eligible.universe.members.contains(&f.ids[0]));
    let portfolios = vec![PortfolioRun {
        id: VirtualPortfolioId(1),
        capital: "10000".parse().unwrap(),
        decisions: plan.decisions.clone(),
        unfilled_entry: UnfilledEntry::Skip,
        rebalance: Rebalance::FullTarget,
    }];
    let mut config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: "10000".parse().unwrap(),
        sizing: Sizing::Lot,
        costs: FastEventCosts {
            commission_rate: "0.0003".parse().unwrap(),
            buy_slippage_rate: "0.0002".parse().unwrap(),
            sell_slippage_rate: "0.0002".parse().unwrap(),
            minimum_commission: "5".parse().unwrap(),
            ..FastEventCosts::ZERO
        },
        lot_sizes: f
            .ids
            .iter()
            .map(|id| (id.clone(), "100".parse().unwrap()))
            .collect(),
    };
    let lot = run_fast_event_pit(&panel, &f.market, &f.risk, &config, &portfolios).unwrap();
    let expected_cash = [
        "10000", "10000", "10000", "93.02", "93.02", "93.02", "93.02", "79.06", "79.06", "79.06",
    ];
    let expected_nav = [
        "10000", "10000", "10000", "9993.02", "9993.02", "9993.02", "9003.02", "9979.06",
        "9979.06", "9979.06",
    ];
    let expected_close = [
        "10000", "10000", "10000", "9993.02", "9993.02", "9993.02", "9993.02", "9979.06",
        "9979.06", "9979.06",
    ];
    let mut replay_cash = config.initial_cash;
    for (i, s) in lot.sessions.iter().enumerate() {
        s.open_after.validate().unwrap();
        s.close.validate().unwrap();
        assert_eq!(s.open_after.account.cash, expected_cash[i].parse().unwrap());
        assert_eq!(
            s.open_after.account.equity,
            expected_nav[i].parse().unwrap()
        );
        assert_eq!(s.close.account.equity, expected_close[i].parse().unwrap());
        let id = if i < 7 { &f.ids[0] } else { &f.ids[2] };
        let positions = &s.close.portfolios[&VirtualPortfolioId(1)].positions;
        if i < 3 {
            assert!(positions.is_empty());
        } else {
            assert_eq!(positions.len(), 1);
            assert_eq!(positions[id].quantity, "9900".parse::<Quantity>().unwrap());
        }

        let daily = lot
            .fills
            .iter()
            .filter(|fill| fill.fill.executed_at() == s.session.ts_open)
            .collect::<Vec<_>>();
        let fees: i128 = daily.iter().map(|f| f.fill.total_fees().mantissa()).sum();
        let expected_fees: Notional = if i == 3 {
            "6.98"
        } else if i == 7 {
            "13.96"
        } else {
            "0"
        }
        .parse()
        .unwrap();
        assert_eq!(fees, expected_fees.mantissa());
        for fill in daily {
            replay_cash =
                Notional::from_mantissa(replay_cash.mantissa() + fill.fill.cash_delta().mantissa())
                    .unwrap();
        }
        assert_eq!(replay_cash, s.close.account.cash);
    }
    assert_eq!(lot.fills.len(), 3);
    assert_eq!(
        lot.risk_exits
            .iter()
            .map(|(_, a)| (a.session_date, a.executed))
            .collect::<Vec<_>>(),
        vec![
            (fixture::date(5), false),
            (fixture::date(6), false),
            (fixture::date(7), false),
            (fixture::date(8), true)
        ]
    );
    assert!(
        lot.attempts
            .iter()
            .any(|a| matches!(&a.event, ExecutionEvent::Deferred { .. }))
    );
    assert!(lot.attempts.iter().any(|a| matches!(&a.event,ExecutionEvent::Executed {decision_session,attempt_session,applied,..}
        if decision_session.session_date == fixture::date(7) && attempt_session.session_date == fixture::date(8)
        && applied.contains_key(&f.ids[2]) && !applied.contains_key(&f.ids[1]))));

    config.sizing = Sizing::VectorParity;
    config.costs.minimum_commission = Notional::ZERO;
    let fast = run_fast_event_pit(&panel, &f.market, &f.risk, &config, &portfolios).unwrap();
    let vector = run_vector_pit(
        &panel,
        &plan.decisions,
        &f.market,
        &f.risk,
        &VectorCosts {
            commission_rate: 0.0003,
            buy_slippage_bps: 2.0,
            sell_slippage_bps: 2.0,
            buy_tax_rate: 0.0,
            sell_tax_rate: 0.0,
        },
        config.initial_cash,
    )
    .unwrap();
    for (f, v) in fast.sessions.iter().zip(&vector.sessions) {
        let open_equity = f
            .open_after
            .account
            .equity
            .to_string()
            .parse::<f64>()
            .unwrap()
            / 10000.0;
        assert!((open_equity - v.nav / (1.0 + v.gross_return)).abs() < 1e-12);
        for (id, pos) in &f.open_after.portfolios[&VirtualPortfolioId(1)].positions {
            let amount =
                pos.market_value.to_string().parse::<f64>().unwrap() / 10000.0 / open_equity;
            assert!((amount - v.weights_after_execution[id]).abs() < 1e-12);
        }
        f.open_after.validate().unwrap();
        f.close.validate().unwrap();
    }
    assert_eq!(
        fast.sessions[7].close.account.equity,
        "9985.00749875".parse().unwrap()
    );
    assert_eq!(
        vector.risk_exits,
        fast.risk_exits
            .into_iter()
            .map(|(_, a)| a)
            .collect::<Vec<_>>()
    );
}

#[test]
fn fixed_list_universe_intersects_catalog_lifecycle_and_versions_the_list() {
    use prajna_research::pit_universe::FixedUniverseSpec;

    let lake = fixture::lake();
    let f = fixture::fixture_options(lake.path(), 1.0, true);
    let raw = RawStore::open(lake.path()).unwrap();
    let mut catalog_input = f.catalog.input().clone();
    catalog_input.broad_indices = None;
    let catalog =
        prajna_research::pit_universe::HistoricalCatalog::build(catalog_input, &raw).unwrap();
    assert!(catalog.universe(fixture::date(3)).is_err());
    let listed = FixedUniverseSpec::new(
        "mvp-6-fixed-etfs@1",
        ["159001", "159003", "159009"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
    .unwrap();

    let before_listing = catalog.fixed_universe(fixture::date(3), &listed).unwrap();
    assert_eq!(before_listing.members, vec![f.ids[0].clone()]);
    assert_eq!(before_listing.excluded[&f.ids[2]], "before_listing");
    let first_eligible_day = catalog.fixed_universe(fixture::date(4), &listed).unwrap();
    assert_eq!(
        first_eligible_day.members,
        vec![f.ids[0].clone(), f.ids[2].clone()]
    );
    assert!(first_eligible_day.excluded.contains_key(&f.ids[1]));
    let after_termination = catalog.fixed_universe(fixture::date(8), &listed).unwrap();
    assert!(!after_termination.members.contains(&f.ids[0]));
    assert_eq!(after_termination.excluded[&f.ids[0]], "terminated");
    assert_eq!(
        before_listing.run_spec.history_claim.as_deref(),
        Some("retrospective_static")
    );
    assert_eq!(
        before_listing.run_spec.warnings.as_ref().unwrap(),
        &std::collections::BTreeSet::from(["survivorship_bias".into()])
    );

    let revised = FixedUniverseSpec::new(
        "mvp-6-fixed-etfs@1",
        ["159001", "159003"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
    .unwrap();
    let revised_universe = catalog.fixed_universe(fixture::date(3), &revised).unwrap();
    assert_ne!(
        before_listing.universe_version,
        revised_universe.universe_version
    );
    assert_eq!(
        before_listing.run_spec.fixed_list_version.as_deref(),
        Some("mvp-6-fixed-etfs@1")
    );
}

#[test]
fn fewer_than_top_k_is_equal_weight_and_empty_set_is_cash() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let plan = pit_strategy::plan(
        &f.catalog,
        &f.market,
        &f.tri,
        &f.risk,
        serde_json::from_value(f.input["eligibility"].clone()).unwrap(),
        &PitS2Params {
            lookback: 1,
            top_k: 3,
            rebalance_every: 2,
        },
        &panel.sessions,
    )
    .unwrap();
    assert!(plan.decisions[0].targets.is_empty());
    assert_eq!(
        plan.decisions[1].targets,
        BTreeMap::from([(f.ids[0].clone(), 0.5), (f.ids[1].clone(), 0.5)])
    );
    assert_eq!(
        plan.decisions[2].targets,
        BTreeMap::from([(f.ids[1].clone(), 0.5), (f.ids[2].clone(), 0.5)])
    );
    assert_eq!(
        plan.decisions[3].targets,
        BTreeMap::from([(f.ids[2].clone(), 1.0)])
    );
}

#[test]
fn risk_exit_is_independent_of_cadence_preserves_other_quantity_and_never_reenters() {
    use prajna_research::strategy::Decision;
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let decisions = vec![
        Decision {
            decision_session: panel.sessions[2].clone(),
            ranked: vec![],
            targets: BTreeMap::from([(f.ids[0].clone(), 0.5), (f.ids[1].clone(), 0.5)]),
        },
        // A later ordinary signal cannot cancel a known Risk Exit or re-enter A.
        Decision {
            decision_session: panel.sessions[8].clone(),
            ranked: vec![],
            targets: BTreeMap::from([(f.ids[0].clone(), 1.0)]),
        },
    ];
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: "10000".parse().unwrap(),
        sizing: Sizing::VectorParity,
        costs: FastEventCosts {
            commission_rate: "0.0003".parse().unwrap(),
            buy_slippage_rate: "0.0002".parse().unwrap(),
            sell_slippage_rate: "0.0002".parse().unwrap(),
            ..FastEventCosts::ZERO
        },
        lot_sizes: BTreeMap::new(),
    };
    let portfolios = [PortfolioRun {
        id: VirtualPortfolioId(1),
        capital: config.initial_cash,
        decisions: decisions.clone(),
        unfilled_entry: UnfilledEntry::Skip,
        rebalance: Rebalance::FullTarget,
    }];
    let fast = run_fast_event_pit(&panel, &f.market, &f.risk, &config, &portfolios).unwrap();
    let vector = run_vector_pit(
        &panel,
        &decisions,
        &f.market,
        &f.risk,
        &VectorCosts {
            commission_rate: 0.0003,
            buy_slippage_bps: 2.0,
            sell_slippage_bps: 2.0,
            buy_tax_rate: 0.0,
            sell_tax_rate: 0.0,
        },
        config.initial_cash,
    )
    .unwrap();
    let held = |i: usize| &fast.sessions[i].close.portfolios[&VirtualPortfolioId(1)].positions;
    assert_eq!(held(3)[&f.ids[1]].quantity, held(7)[&f.ids[1]].quantity);
    assert!(!held(7).contains_key(&f.ids[0]));
    assert!(held(9).is_empty());
    assert_eq!(
        fast.fills
            .iter()
            .filter(|f| f.fill.executed_at() == panel.sessions[7].ts_open)
            .count(),
        1
    );
    for (f, v) in fast.sessions.iter().zip(&vector.sessions) {
        let open = f
            .open_after
            .account
            .equity
            .to_string()
            .parse::<f64>()
            .unwrap()
            / 10000.0;
        assert!((open - v.nav / (1.0 + v.gross_return)).abs() < 1e-12);
    }
}

#[test]
fn risk_at_open_waits_until_next_open_and_low_nav_is_annotation_only() {
    use prajna_research::{
        pit_strategy::{RiskKind, RiskNotice, RiskSignals},
        pit_universe::Publication,
    };
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let mut notice = f.risk.signals[0].notice.clone();
    let at = "2023-01-04T09:30:00+08:00".parse().unwrap();
    notice.evidence.publication = Publication::Exact { timestamp: at };
    let mut warning = RiskNotice {
        kind: RiskKind::LowNavWarning,
        ..notice.clone()
    };
    warning.instrument_id = f.ids[1].clone();
    let raw = prajna_data::RawStore::open(lake.path()).unwrap();
    let risk = RiskSignals::build(&f.catalog, vec![notice, warning], &raw).unwrap();
    assert!(risk.active_before_open(at).is_empty());
    assert_eq!(
        risk.active(fixture::time(4, 15)),
        std::collections::BTreeSet::from([f.ids[0].clone()])
    );
}

#[test]
fn upper_limit_skips_only_buys_and_lower_limit_defers_required_sells() {
    use prajna_research::{
        etf_market::{DerivedMarket, MarketEvidence},
        pit_strategy::RiskSignals,
        strategy::Decision,
    };
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    // B's raw open 1 is its tick-rounded upper limit with reference 0.909.
    let mut evidence: MarketEvidence = serde_json::from_value(f.input["market"].clone()).unwrap();
    for cell in &mut evidence.openings {
        if cell.instrument_id == f.ids[1] && cell.session_date == fixture::date(4) {
            cell.reference.as_mut().unwrap().adjusted_previous_close = "0.909".parse().unwrap();
        }
    }
    let market = DerivedMarket::from_dataset(lake.path(), &f.dsv, &f.catalog, evidence).unwrap();
    assert!(!market.cells()[&(f.ids[1].clone(), fixture::date(4))].permits(true));
    assert!(market.cells()[&(f.ids[1].clone(), fixture::date(4))].permits(false));
    assert!(!market.cells()[&(f.ids[0].clone(), fixture::date(7))].permits(false));
    assert!(market.cells()[&(f.ids[0].clone(), fixture::date(7))].permits(true));
    let decisions = vec![Decision {
        decision_session: panel.sessions[2].clone(),
        ranked: vec![],
        targets: BTreeMap::from([(f.ids[1].clone(), 1.0)]),
    }];
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: "10000".parse().unwrap(),
        sizing: Sizing::VectorParity,
        costs: FastEventCosts::ZERO,
        lot_sizes: BTreeMap::new(),
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
    assert!(fast.fills.is_empty());
    assert_eq!(fast.sessions[9].close.account.cash, config.initial_cash);
    let vector = run_vector_pit(
        &panel,
        &decisions,
        &market,
        &RiskSignals::default(),
        &VectorCosts {
            commission_rate: 0.0,
            buy_slippage_bps: 0.0,
            sell_slippage_bps: 0.0,
            buy_tax_rate: 0.0,
            sell_tax_rate: 0.0,
        },
        config.initial_cash,
    )
    .unwrap();
    assert!(vector.sessions.iter().all(|s| s.nav == 1.0));
    assert_eq!(
        vector.executions[0].skipped_buys,
        std::collections::BTreeSet::from([f.ids[1].clone()])
    );
}

#[test]
fn last_close_risk_notice_remains_pending_without_fabricated_fill() {
    use prajna_research::{
        pit_strategy::RiskSignals, pit_universe::Publication, strategy::Decision,
    };
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    let mut notice = f.risk.signals[0].notice.clone();
    notice.evidence.publication = Publication::Exact {
        timestamp: panel.sessions[9].ts_close,
    };
    let risk = RiskSignals::build(
        &f.catalog,
        vec![notice],
        &prajna_data::RawStore::open(lake.path()).unwrap(),
    )
    .unwrap();
    let decisions = vec![Decision {
        decision_session: panel.sessions[2].clone(),
        ranked: vec![],
        targets: BTreeMap::from([(f.ids[0].clone(), 1.0)]),
    }];
    let config = FastEventConfig {
        currency: "CNY".parse().unwrap(),
        venues: BTreeSet::from(["SZSE".parse().unwrap()]),
        initial_cash: "10000".parse().unwrap(),
        sizing: Sizing::VectorParity,
        costs: FastEventCosts::ZERO,
        lot_sizes: BTreeMap::new(),
    };
    let fast = run_fast_event_pit(
        &panel,
        &f.market,
        &risk,
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
        &f.market,
        &risk,
        &VectorCosts {
            commission_rate: 0.0,
            buy_slippage_bps: 0.0,
            sell_slippage_bps: 0.0,
            buy_tax_rate: 0.0,
            sell_tax_rate: 0.0,
        },
        config.initial_cash,
    )
    .unwrap();
    assert_eq!(fast.fills.len(), 1);
    assert!(fast.risk_exits.is_empty());
    assert_eq!(
        fast.pending_risk_exits,
        vec![(VirtualPortfolioId(1), f.ids[0].clone())]
    );
    assert_eq!(vector.pending_risk_exits, vec![f.ids[0].clone()]);
}
#[test]
fn cninfo_classifications_trigger_risk_only_after_p6_next_session_close() {
    use prajna_data::{
        RawStore, SourceKind, SourceRecordInput,
        cninfo::{FundIndex, IndexDataset, PageSet},
    };
    use prajna_research::pit_strategy::RiskSignals;
    use serde_json::json;
    let tmp = fixture::lake();
    let f = fixture::fixture(tmp.path());
    let raw = RawStore::open(tmp.path()).unwrap();
    let bytes = serde_json::to_vec(&json!({
        "totalAnnouncement": 4,
        "announcements": [
            {"announcementId":"meeting","secCode":"159001","orgId":"synthetic",
             "announcementTitle":"合成沪深300交易型开放式指数证券投资基金持有人大会计票日停牌提示",
             "announcementTime":1672617600000_i64},
            {"announcementId":"termination","secCode":"159002","orgId":"synthetic",
             "announcementTitle":"合成沪深300交易型开放式指数证券投资基金终止基金合同议案",
             "announcementTime":1672617600000_i64},
            {"announcementId":"liquidation","secCode":"159003","orgId":"synthetic",
             "announcementTitle":"合成沪深300交易型开放式指数证券投资基金清算提示",
             "announcementTime":1672876800000_i64},
            {"announcementId":"report","secCode":"159001","orgId":"synthetic",
             "announcementTitle":"合成沪深300交易型开放式指数证券投资基金年度报告",
             "announcementTime":1672617600000_i64}
        ]
    }))
    .unwrap();
    let hash = raw
        .put(
            &bytes,
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::FileImport,
                source_id: "synthetic-cninfo".into(),
                request: json!({}),
                observed_at: "2026-10-16T00:00:00Z".into(),
                ingested_by: "test".into(),
            },
        )
        .unwrap();
    let mut pages = PageSet::default();
    pages.push(1, &bytes).unwrap();
    let (announcements, _) = pages.finish().unwrap();
    let mut index = IndexDataset::new(fixture::date(1), fixture::date(10)).unwrap();
    index.source_raw_sha256.insert(hash);
    for code in ["159001", "159002", "159003"] {
        let fund = FundIndex::new(
            code.into(),
            announcements
                .iter()
                .filter(|a| a.code == code)
                .cloned()
                .collect(),
        );
        index.funds.push(fund);
    }
    let risk = RiskSignals::build_from_cninfo(&f.catalog, vec![], &index, &raw).unwrap();
    assert!(risk.active(fixture::time(2, 15)).is_empty());
    assert!(risk.active_before_open(fixture::time(3, 9)).is_empty());
    assert_eq!(
        risk.active(fixture::time(3, 15)),
        f.ids[..2].iter().cloned().collect()
    );
    assert_eq!(
        risk.active_before_open(fixture::time(4, 9)),
        f.ids[..2].iter().cloned().collect()
    );
    assert_eq!(risk.active(fixture::time(6, 15)).len(), 3);
    assert!(
        risk.signals
            .iter()
            .all(|s| s.notice.evidence.locator.contains("cninfo_title_rules_v2"))
    );
    index.unknown.push("missing_page".into());
    assert!(RiskSignals::build_from_cninfo(&f.catalog, vec![], &index, &raw).is_err());
}
