#[path = "../../prajna-research/tests/common/pit.rs"]
mod fixture;
use prajna_experiment::{Experiment, ResultLevel, RunStatus, read_run_table};
use serde_json::{Value, json};
use std::process::Command;

fn definition(f: &fixture::Fixture, engine: &str) -> Value {
    let mut value = json!({
        "experiment_version":1,"dsv":f.dsv,"universe":f.catalog.version().unwrap(),
        "venue":"SZSE","strategy":"s2_pit_rotation","engine":engine,
        "availability_assumption":"none","sessions_per_year":252,
        "costs":{"commission_rate":0.0003,"buy_slippage_bps":2.0,"sell_slippage_bps":2.0,
            "buy_tax_rate":0.0,"sell_tax_rate":0.0},
        "parameter_space":{"list":[{"lookback":1,"top_k":1,"rebalance_every":2}]},
        "pit":f.input,
    });
    if engine == "fast_event" {
        value["fast_event"] = json!({"currency":"CNY","initial_cash":"10000","sizing":"lot",
            "rebalance":"full_target","minimum_commission":"5","portfolios":[{"id":1,"capital":"10000"}],
            "lot_size_source":{"kind":"instrument"}});
    } else if engine == "vector" {
        value["vector_pit"] = json!({"initial_capital":"10000"});
    }
    value
}

fn terminal_settlement_definition(f: &fixture::Fixture, engine: &str) -> Value {
    let mut value = definition(f, engine);
    value["pit"]["risk_notices"] = json!([]);
    value["pit"]["terminal_settlement"] = json!({"semantic_version":"mvp6-terminal-settlement@1"});
    value
}

#[test]
fn terminal_settlement_semantics_is_explicit_and_changes_run_identity() {
    let lake = fixture::lake();
    let f = fixture::fixture_options(lake.path(), 1.0, true);
    let mut legacy_input = definition(&f, "vector");
    legacy_input["pit"]["risk_notices"] = json!([]);
    let legacy = Experiment::parse(&legacy_input.to_string(), lake.path()).unwrap();
    let explicit = terminal_settlement_definition(&f, "vector");
    let enabled = Experiment::parse(&explicit.to_string(), lake.path()).unwrap();
    let legacy_run = legacy.expand(lake.path()).unwrap().remove(0);
    let enabled_run = enabled.expand(lake.path()).unwrap().remove(0);
    assert_ne!(legacy_run.run_id, enabled_run.run_id);
    assert!(legacy_run.run_spec.get("terminal_settlement").is_none());
    assert_eq!(
        enabled_run.run_spec["terminal_settlement"]["semantic_version"],
        "mvp6-terminal-settlement@1"
    );
    assert_eq!(
        enabled_run.run_spec["engine"]["semantic_version"],
        "vector@6"
    );
}

#[test]
fn shared_sizing_version_is_in_run_spec_and_result_and_reruns_are_stable() {
    let lake = fixture::lake();
    let f = fixture::fixture_options(lake.path(), 1.0, true);
    let fixed = json!({
        "version":"synthetic-fixed-etfs@1",
        "codes":["159001","159002","159003"]
    });

    let mut vector_input = definition(&f, "vector");
    vector_input["pit"]["fixed_universe"] = fixed.clone();
    let vector_experiment = Experiment::parse(&vector_input.to_string(), lake.path()).unwrap();
    let vector_run = &vector_experiment.expand(lake.path()).unwrap()[0];
    assert_eq!(
        vector_run.run_spec["engine"]["semantic_version"],
        "vector@4"
    );

    let mut parity_input = definition(&f, "fast_event");
    parity_input["fast_event"]["sizing"] = json!("vector_parity");
    parity_input["pit"]["fixed_universe"] = fixed.clone();
    let parity_experiment = Experiment::parse(&parity_input.to_string(), lake.path()).unwrap();
    let first = parity_experiment.execute(lake.path(), 1).unwrap();
    let second = parity_experiment.execute(lake.path(), 1).unwrap();
    let run = &first.runs[0];
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.run_spec["engine"]["semantic_version"], "fast_event@4");
    let fast = run.fast_event_result.as_ref().unwrap();
    assert_eq!(fast.semantic_version, "fast_event@4");
    assert_eq!(run.run_id, second.runs[0].run_id);
    assert_eq!(
        serde_json::to_vec(&run.summary).unwrap(),
        serde_json::to_vec(&second.runs[0].summary).unwrap()
    );

    let mut lot_input = definition(&f, "fast_event");
    lot_input["pit"]["fixed_universe"] = fixed.clone();
    let lot_experiment = Experiment::parse(&lot_input.to_string(), lake.path()).unwrap();
    let lot_run = &lot_experiment.expand(lake.path()).unwrap()[0];
    assert_eq!(
        lot_run.run_spec["engine"]["semantic_version"],
        "fast_event@1"
    );

    let mut terminal_lot = terminal_settlement_definition(&f, "fast_event");
    terminal_lot["pit"]["fixed_universe"] = fixed;
    let terminal_experiment = Experiment::parse(&terminal_lot.to_string(), lake.path()).unwrap();
    let terminal_run = &terminal_experiment.expand(lake.path()).unwrap()[0];
    assert_eq!(
        terminal_run.run_spec["engine"]["semantic_version"],
        "fast_event@3"
    );
    let terminal_report = terminal_experiment.execute(lake.path(), 1).unwrap();
    assert_eq!(
        terminal_report.runs[0]
            .fast_event_result
            .as_ref()
            .unwrap()
            .semantic_version,
        "fast_event@3"
    );
}

#[test]
fn terminal_settlement_uses_last_catalog_bar_close_and_fast_vector_match() {
    use prajna_account::VirtualPortfolioId;

    let lake = fixture::lake();
    let f = fixture::fixture_options(lake.path(), 1.0, true);
    let fixed = json!({
        "version":"synthetic-fixed-etfs@1",
        "codes":["159001"]
    });
    let mut vector_input = terminal_settlement_definition(&f, "vector");
    let mut fast_input = terminal_settlement_definition(&f, "fast_event");
    vector_input["pit"]["fixed_universe"] = fixed.clone();
    fast_input["pit"]["fixed_universe"] = fixed;
    fast_input["fast_event"]["sizing"] = json!("vector_parity");
    let vector_experiment = Experiment::parse(&vector_input.to_string(), lake.path()).unwrap();
    let fast_experiment = Experiment::parse(&fast_input.to_string(), lake.path()).unwrap();
    let vector_run = vector_experiment.execute(lake.path(), 1).unwrap();
    let fast_run = fast_experiment.execute(lake.path(), 1).unwrap();
    assert_eq!(vector_run.runs[0].status, RunStatus::Completed);
    assert_eq!(fast_run.runs[0].status, RunStatus::Completed);

    let vector = vector_run.runs[0].result.as_ref().unwrap();
    let fast = fast_run.runs[0].fast_event_result.as_ref().unwrap();
    assert_eq!(fast.semantic_version, "fast_event@6");
    assert_eq!(vector.terminal_settlements.len(), 1);
    assert_eq!(fast.terminal_settlements.len(), 1);
    let vector_settlement = &vector.terminal_settlements[0];
    let fast_settlement = &fast.terminal_settlements[0];
    assert_eq!(vector_settlement.session_date, fixture::date(7));
    assert!(
        vector_settlement
            .instrument_id
            .to_string()
            .starts_with("159001-")
    );
    assert_eq!(vector_settlement.quantity, fast_settlement.quantity);
    assert_eq!(vector_settlement.price, fast_settlement.price);
    assert_eq!(vector_settlement.proceeds, fast_settlement.proceeds);
    // The fixture's terminal-session open is 0.9 and close is 1.0.
    // This hand-check proves the settlement uses close: 9,995 × 1.0 = 9,995.
    assert_eq!(vector_settlement.quantity, "9995".parse().unwrap());
    assert_eq!(vector_settlement.price, "1.0".parse().unwrap());
    assert_eq!(vector_settlement.proceeds, "9995".parse().unwrap());
    assert_eq!(fast_settlement.portfolio_id, Some(VirtualPortfolioId(1)));
    assert_eq!(
        fast.sessions[6].close.portfolios[&VirtualPortfolioId(1)]
            .positions
            .len(),
        0
    );
    assert_eq!(
        fast.sessions[6].close.portfolios[&VirtualPortfolioId(1)]
            .cash
            .mantissa()
            - fast.sessions[6].open_after.portfolios[&VirtualPortfolioId(1)]
                .cash
                .mantissa(),
        vector_settlement.proceeds.mantissa()
    );
    let close_components = vector.sessions[6]
        .portfolio_components_close
        .as_ref()
        .unwrap();
    assert!((close_components.available_cash_weight - 1.0).abs() < 1e-12);
    assert_eq!(close_components.liquidation_receivable_weight, 0.0);

    for (vector_session, fast_session) in vector.sessions.iter().zip(&fast.sessions) {
        let fast_open_equity = fast_session
            .open_after
            .account
            .equity
            .to_string()
            .parse::<f64>()
            .unwrap()
            / 10000.0;
        assert!(
            (fast_open_equity - vector_session.nav / (1.0 + vector_session.gross_return)).abs()
                < 1e-12,
            "daily parity failed on {}",
            vector_session.session_date
        );
    }

    for experiment in [&vector_experiment, &fast_experiment] {
        let stored = experiment
            .execute_and_store(
                lake.path(),
                env!("CARGO_MANIFEST_DIR"),
                ResultLevel::Standard,
                1,
            )
            .unwrap();
        let run_id = &stored.manifest.runs[0].run_id;
        let (_, batches) = read_run_table(
            lake.path(),
            &stored.manifest.experiment_id,
            &stored.manifest.execution.id,
            run_id,
            ResultLevel::Standard,
            "terminal_settlements",
        )
        .unwrap();
        assert_eq!(
            batches.iter().map(|batch| batch.num_rows()).sum::<usize>(),
            1
        );
    }
}

#[test]
fn terminal_settlement_uses_last_bar_before_a_multi_session_gap() {
    let lake = fixture::lake();
    let f = fixture::fixture_with_missing_bars_and_action_prices(
        lake.path(),
        1.0,
        true,
        std::collections::BTreeMap::from([((0, 5), (0.9, 1.0))]),
        std::collections::BTreeSet::from([(0, 6), (0, 7)]),
    );
    let fixed = json!({
        "version":"synthetic-fixed-etfs@1",
        "codes":["159001"]
    });

    let mut reports = Vec::new();
    for engine in ["vector", "fast_event"] {
        let mut input = terminal_settlement_definition(&f, engine);
        input["pit"]["fixed_universe"] = fixed.clone();
        if engine == "fast_event" {
            input["fast_event"]["sizing"] = json!("vector_parity");
        }
        let experiment = Experiment::parse(&input.to_string(), lake.path()).unwrap();
        let report = experiment.execute(lake.path(), 1).unwrap();
        assert_eq!(report.runs[0].status, RunStatus::Completed);
        let entry = if engine == "fast_event" {
            &report.runs[0]
                .fast_event_result
                .as_ref()
                .unwrap()
                .terminal_settlements[0]
        } else {
            &report.runs[0].result.as_ref().unwrap().terminal_settlements[0]
        };
        assert_eq!(entry.session_date, fixture::date(5));
        assert_eq!(entry.price, "1.0".parse().unwrap());
        reports.push(report);
    }
    let vector = reports[0].runs[0].result.as_ref().unwrap();
    let fast = reports[1].runs[0].fast_event_result.as_ref().unwrap();
    assert_eq!(
        vector.terminal_settlements[0].quantity,
        fast.terminal_settlements[0].quantity
    );
    assert_eq!(
        vector.terminal_settlements[0].proceeds,
        fast.terminal_settlements[0].proceeds
    );
    let settlement = &fast.terminal_settlements[0];
    assert_eq!(
        settlement.proceeds.mantissa(),
        settlement.quantity.mantissa()
    );
    assert_eq!(
        fast.sessions[4].close.account.cash.mantissa()
            - fast.sessions[4].open_after.account.cash.mantissa(),
        settlement.proceeds.mantissa()
    );
    assert!(fast.sessions[4].close.account.positions.is_empty());
    assert_eq!(vector.sessions.len(), fast.sessions.len());
    for (vector_session, fast_session) in vector.sessions.iter().zip(&fast.sessions) {
        assert_eq!(
            vector_session.session_date,
            fast_session.session.session_date.to_string()
        );
        for valuation in [
            &fast_session.open_after.account,
            &fast_session.close.account,
        ] {
            let holdings: f64 = valuation
                .positions
                .values()
                .map(|position| {
                    position.price.to_string().parse::<f64>().unwrap()
                        * position.quantity.to_string().parse::<f64>().unwrap()
                })
                .sum();
            let accounted_equity = valuation.cash.to_string().parse::<f64>().unwrap()
                + holdings
                + valuation
                    .dividend_receivable
                    .to_string()
                    .parse::<f64>()
                    .unwrap()
                + valuation
                    .liquidation_receivable
                    .to_string()
                    .parse::<f64>()
                    .unwrap();
            assert!(
                (valuation.equity.to_string().parse::<f64>().unwrap() - accounted_equity).abs()
                    < 1e-9
            );
        }
        let fast_open_nav = fast_session
            .open_after
            .account
            .equity
            .to_string()
            .parse::<f64>()
            .unwrap()
            / 10000.0;
        assert!(
            (fast_open_nav - vector_session.nav / (1.0 + vector_session.gross_return)).abs()
                < 1e-12,
            "gap daily parity failed on {}",
            vector_session.session_date
        );
    }
}

#[test]
fn terminal_settlement_skips_terminations_before_the_run_window() {
    use prajna_research::{pit_strategy, terminal_settlement::TerminalSettlementSchedule};

    let lake = fixture::lake();
    let f = fixture::fixture_options(lake.path(), 1.0, true);
    let native =
        prajna_research::load_panel(lake.path(), &f.dsv, &"SZSE".parse().unwrap()).unwrap();
    let mut panel = pit_strategy::lifecycle_panel(&native, &f.catalog).unwrap();
    panel
        .sessions
        .retain(|session| session.session_date >= fixture::date(9));

    let schedule = TerminalSettlementSchedule::from_market(&f.market, &panel).unwrap();
    assert!(schedule.is_empty());
}

#[test]
fn terminal_settlement_without_an_in_window_bar_fails_during_run_expansion() {
    let lake = fixture::lake();
    let missing_bars = (1..=7).map(|day| (0, day)).collect();
    let f = fixture::fixture_with_missing_bars(lake.path(), 1.0, true, missing_bars);
    let mut input = terminal_settlement_definition(&f, "vector");
    input["pit"]["fixed_universe"] = json!({
        "version":"synthetic-fixed-etfs@1",
        "codes":["159001"]
    });
    let experiment = Experiment::parse(&input.to_string(), lake.path()).unwrap();

    let error = experiment.expand(lake.path()).unwrap_err().to_string();
    assert!(
        error.contains("no available in-window Bar close"),
        "{error}"
    );
}

#[test]
fn terminal_settlement_is_skipped_after_a_successful_early_risk_exit() {
    let lake = fixture::lake();
    let f = fixture::fixture_options(lake.path(), 1.0, true);
    for engine in ["vector", "fast_event"] {
        let mut input = terminal_settlement_definition(&f, engine);
        input["pit"]["risk_notices"] = f.input["risk_notices"].clone();
        for opening in input["pit"]["market"]["openings"].as_array_mut().unwrap() {
            let is_first_instrument = opening["instrument_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("159001-"));
            if is_first_instrument && opening["session_date"] == fixture::date(6).to_string() {
                opening["source_independence"] = opening["announcement_coverage"].clone();
            }
            if is_first_instrument && opening["session_date"] == fixture::date(7).to_string() {
                opening["reference"]["adjusted_previous_close"] = json!("0.95");
            }
        }
        input["pit"]["fixed_universe"] = json!({
            "version":"synthetic-fixed-etfs@1",
            "codes":["159001"]
        });
        if engine == "fast_event" {
            input["fast_event"]["sizing"] = json!("vector_parity");
        }
        let experiment = Experiment::parse(&input.to_string(), lake.path()).unwrap();
        let report = experiment.execute(lake.path(), 1).unwrap();
        assert_eq!(report.runs[0].status, RunStatus::Completed);
        if engine == "fast_event" {
            let result = report.runs[0].fast_event_result.as_ref().unwrap();
            assert!(result.risk_exits.iter().any(|(_, exit)| exit.executed));
            assert!(result.terminal_settlements.is_empty());
        } else {
            let result = report.runs[0].result.as_ref().unwrap();
            assert!(result.risk_exits.iter().any(|exit| exit.executed));
            assert!(result.terminal_settlements.is_empty());
        }
    }
}

#[test]
fn v7_opening_evidence_builds_canonical_pit_context_deterministically() {
    use prajna_data::{
        NormalizerRegistry, RawStore, SourceKind, SourceRecordInput,
        cninfo::{
            CatalogLifecycle, CorporateActionV2, EvidenceLevel, Mvp6Session,
            StructuredCorporateActionsV2,
        },
        publish_dataset, read_manifest,
    };
    use prajna_experiment::{opening_evidence, pit::PitContext};
    use prajna_research::etf_market::{DerivedMarket, ExecutionStatusVersion, MarketEvidence};
    let start = chrono::NaiveDate::from_ymd_opt(2023, 4, 5).unwrap();

    let lake = fixture::lake();
    let f = fixture::fixture_with_market_inputs(
        lake.path(),
        ["159919", "159612", "159522"],
        std::collections::BTreeSet::from([(0, 8), (1, 6), (2, 7), (1, 8)]),
        std::collections::BTreeSet::from([(0, 4)]),
        std::collections::BTreeMap::from([
            ((0, 3), (1.1, 1.1)),
            ((1, 3), (0.9, 0.9)),
            ((0, 9), (1.0, 1.5)),
        ]),
        std::collections::BTreeMap::from([((0, 3), 1.0), ((1, 3), 1.0), ((0, 9), 1.0)]),
        start,
    );
    let raw = RawStore::open(lake.path()).unwrap();
    let v1 = read_manifest(lake.path(), &f.dsv).unwrap();
    let market_inputs = v1
        .core
        .inputs
        .iter()
        .map(|input| input.raw_sha256.clone())
        .collect::<Vec<_>>();
    let market = publish_dataset(
        lake.path(),
        &raw,
        &NormalizerRegistry::with_builtins(),
        "szse-etf-frozen",
        "4",
        &market_inputs,
        "2026-10-20T00:00:00Z",
    )
    .unwrap();

    let lifecycles = f
        .catalog
        .input()
        .lifecycles
        .iter()
        .map(|life| CatalogLifecycle {
            code: life.native_symbol.clone(),
            org_id: format!("synthetic-{}", life.native_symbol),
            legal_name: format!("Synthetic ETF {}", life.native_symbol),
            listing_date: life.listing.effective_date,
            termination_date: life.termination.as_ref().map(|fact| fact.effective_date),
            fund_category: "equity".into(),
            tracked_index: "CSI:300".into(),
            evidence_level: EvidenceLevel::Official,
            source_announcement_id: format!("synthetic-listing-{}", life.native_symbol),
            source_page: 1,
            change_kind: String::new(),
            available_from: None,
        })
        .collect::<Vec<_>>();
    let session = Mvp6Session {
        catalog_lifecycles: lifecycles,
        corporate_actions: StructuredCorporateActionsV2 {
            schema: "cninfo_corporateActions_structured_v2".into(),
            version: 2,
            actions: vec![CorporateActionV2 {
                code: "159919".into(),
                kind: "dividend".into(),
                announcement_date: fixture::date_at(start, 1),
                record_date: Some(fixture::date_at(start, 2)),
                ex_date: Some(fixture::date_at(start, 3)),
                pay_date: Some(fixture::date_at(start, 4)),
                cash_per_share: Some("0.001".into()),
                split_ratio: None,
                liquidation_per_share: None,
                evidence_level: EvidenceLevel::Official,
                source_announcement_id: "SYNTH-ACTION".into(),
                source_page: 1,
            }],
        },
        risk_events: Vec::new(),
    };
    let facts_bytes = serde_json::to_vec(&json!({
        "contract": "mvp-6-cninfo-facts-v1",
        "dsv": market.dsv.clone(),
        "session": session.clone(),
    }))
    .unwrap();
    let facts_hash = raw
        .put(
            &facts_bytes,
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::FileImport,
                source_id: "synthetic-facts-session".into(),
                request: json!({
                    "contract": "mvp-6-cninfo-facts-v1",
                    "market_dsv": market.dsv.clone(),
                }),
                observed_at: "2026-10-20T00:00:00Z".into(),
                ingested_by: "synthetic-test".into(),
            },
        )
        .unwrap();
    let mut v7_inputs = market_inputs.clone();
    v7_inputs.push(facts_hash);
    let v7 = publish_dataset(
        lake.path(),
        &raw,
        &NormalizerRegistry::with_corporate_actions_v7(market.dsv.clone()).unwrap(),
        "szse-etf-frozen",
        "7",
        &v7_inputs,
        "2026-10-20T00:00:00Z",
    )
    .unwrap();

    let catalog = f.catalog.clone();
    let generated = opening_evidence::generate(lake.path(), &v7.dsv, &catalog).unwrap();
    let generated_again = opening_evidence::generate(lake.path(), &v7.dsv, &catalog).unwrap();
    assert_eq!(
        opening_evidence::serialize(&generated).unwrap(),
        opening_evidence::serialize(&generated_again).unwrap()
    );

    let mut pit_input = f.input.clone();
    pit_input["market"] = serde_json::to_value(&generated).unwrap();
    pit_input["corporate_action_rules"] = json!({
        "version": "synthetic-actions@1",
        "unexplained_jump_threshold_bps": 1,
    });
    let pit_input = serde_json::from_value(pit_input).unwrap();
    let context = PitContext::build(&pit_input, lake.path(), &v7.dsv).unwrap();
    use prajna_research::etf_market::TradeState;
    let halt_id = context
        .catalog
        .input()
        .lifecycles
        .iter()
        .find(|life| life.native_symbol == "159919")
        .unwrap()
        .instrument_id()
        .unwrap();
    let regular = &context.market.cells()[&(halt_id.clone(), fixture::date_at(start, 1))];
    assert_eq!(
        regular.state,
        prajna_research::etf_market::TradeState::Tradable
    );
    assert_eq!(regular.execution_price, Some("1".parse().unwrap()));
    let missing_reference = &context.market.cells()[&(halt_id.clone(), fixture::date_at(start, 4))];
    assert_eq!(missing_reference.state, TradeState::Unknown);
    assert!(
        missing_reference
            .reasons
            .contains("missing_tdx_reference_price")
    );
    assert!(context.unknown_coverage.unknowns.iter().any(|cell| {
        cell.instrument_id == halt_id
            && cell.session_date == fixture::date_at(start, 4)
            && cell.reason == prajna_research::mvp6_coverage::UnknownReason::MissingReferencePrice
    }));
    let upper_limit = &context.market.cells()[&(halt_id.clone(), fixture::date_at(start, 3))];
    assert_eq!(upper_limit.state, TradeState::Tradable);
    assert_eq!(upper_limit.execution_price, Some("1.1".parse().unwrap()));
    assert!(!upper_limit.can_buy);
    assert!(upper_limit.can_sell);
    let rule_before_switch = generated
        .openings
        .iter()
        .find(|opening| {
            opening.instrument_id == halt_id && opening.session_date == fixture::date_at(start, 5)
        })
        .unwrap()
        .limit_rule
        .as_ref()
        .unwrap();
    let rule_on_switch = generated
        .openings
        .iter()
        .find(|opening| {
            opening.instrument_id == halt_id && opening.session_date == fixture::date_at(start, 6)
        })
        .unwrap()
        .limit_rule
        .as_ref()
        .unwrap();
    assert_ne!(rule_before_switch.version, rule_on_switch.version);
    assert_eq!(rule_on_switch.effective_from, fixture::date_at(start, 6));
    let switch_cell = &context.market.cells()[&(halt_id.clone(), fixture::date_at(start, 6))];
    assert_eq!(
        switch_cell.limit_rule_version.as_deref(),
        Some(rule_on_switch.version.as_str())
    );
    let lower_id = context
        .catalog
        .input()
        .lifecycles
        .iter()
        .find(|life| life.native_symbol == "159612")
        .unwrap()
        .instrument_id()
        .unwrap();
    let lower_limit = &context.market.cells()[&(lower_id.clone(), fixture::date_at(start, 3))];
    assert_eq!(lower_limit.state, TradeState::Tradable);
    assert_eq!(lower_limit.execution_price, Some("0.9".parse().unwrap()));
    assert!(lower_limit.can_buy);
    assert!(!lower_limit.can_sell);
    let unknown_category_id = context
        .catalog
        .input()
        .lifecycles
        .iter()
        .find(|life| life.native_symbol == "159522")
        .unwrap()
        .instrument_id()
        .unwrap();
    let unknown_category_cell =
        &context.market.cells()[&(unknown_category_id.clone(), fixture::date_at(start, 4))];
    assert_eq!(unknown_category_cell.state, TradeState::Tradable);
    assert!(
        unknown_category_cell
            .reasons
            .contains("unknown_preopen_price_limit_rule")
    );
    assert!(unknown_category_cell.can_buy && unknown_category_cell.can_sell);
    assert!(unknown_category_cell.execution_price.is_some());
    assert!(context.unknown_coverage.unknowns.iter().all(|cell| {
        cell.instrument_id != unknown_category_id || cell.session_date != fixture::date_at(start, 4)
    }));

    let no_bar = &context.market.cells()[&(halt_id.clone(), fixture::date_at(start, 8))];
    assert_eq!(no_bar.state, TradeState::Halted);
    assert_eq!(no_bar.amount, Some(prajna_domain::Notional::ZERO));
    assert!(context.unknown_coverage.unknowns.iter().all(|cell| {
        cell.instrument_id != halt_id || cell.session_date != fixture::date_at(start, 8)
    }));
    let untrusted_prices = &context.market.cells()[&(halt_id.clone(), fixture::date_at(start, 9))];
    assert_eq!(untrusted_prices.state, TradeState::Tradable);
    assert!(untrusted_prices.reasons.contains("untrusted_bar_prices"));
    assert!(untrusted_prices.can_buy && untrusted_prices.can_sell);
    assert!(untrusted_prices.upper_limit.is_some() && untrusted_prices.lower_limit.is_some());
    assert_eq!(untrusted_prices.execution_price, Some("1".parse().unwrap()));
    assert!(context.tri.integrity().iter().any(|integrity| {
        integrity.instrument_id == halt_id
            && integrity
                .unexplained_price_jumps
                .contains(&fixture::date_at(start, 3))
    }));
    assert!(context.unknown_coverage.unknowns.iter().all(|cell| {
        cell.instrument_id != halt_id || cell.session_date != fixture::date_at(start, 3)
    }));

    let report_path = lake.path().join("acceptance/mvp6-unknown-coverage.json");
    let first_report = std::fs::read(&report_path).unwrap();
    let _second_context = PitContext::build(&pit_input, lake.path(), &v7.dsv).unwrap();
    let second_report = std::fs::read(&report_path).unwrap();
    assert_eq!(first_report, second_report);

    let cli_input_path = lake.path().join("cli-input.json");
    std::fs::write(&cli_input_path, serde_json::to_vec(&pit_input).unwrap()).unwrap();
    let cli_output = Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
        .arg("mvp6-opening-evidence")
        .arg(&cli_input_path)
        .arg("--lake")
        .arg(lake.path())
        .arg("--dsv")
        .arg(&v7.dsv)
        .output()
        .unwrap();
    assert!(
        cli_output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&cli_output.stderr)
    );
    let cli_summary: serde_json::Value = serde_json::from_slice(&cli_output.stdout).unwrap();
    assert_eq!(cli_summary["execution_status"], "szse-derived-open@3");
    assert!(cli_summary["per_etf_unknown_coverage"].is_array());
    assert!(cli_summary.get("unknown_cells").is_none());
    let cli_expected = opening_evidence::generate(lake.path(), &v7.dsv, &catalog).unwrap();
    assert_eq!(
        std::fs::read(lake.path().join("acceptance/mvp6-opening-evidence-v3.json")).unwrap(),
        opening_evidence::serialize(&cli_expected).unwrap()
    );
    assert!(report_path.is_file());

    let mut narrow_registry = NormalizerRegistry::new();
    assert!(
        narrow_registry.register(prajna_data::SzseEtfFrozen::v4(vec![(159000, 159999)]).unwrap())
    );
    let alternate_market = publish_dataset(
        lake.path(),
        &raw,
        &narrow_registry,
        "szse-etf-frozen",
        "4",
        &market_inputs,
        "2026-10-20T00:00:00Z",
    )
    .unwrap();
    let wrong_parent_facts = serde_json::to_vec(&json!({
        "contract": "mvp-6-cninfo-facts-v1",
        "dsv": alternate_market.dsv.clone(),
        "session": session,
    }))
    .unwrap();
    let wrong_facts_hash = raw
        .put(
            &wrong_parent_facts,
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::FileImport,
                source_id: "synthetic-facts-session-wrong-parent".into(),
                request: json!({
                    "contract": "mvp-6-cninfo-facts-v1",
                    "market_dsv": alternate_market.dsv.clone(),
                }),
                observed_at: "2026-10-20T00:00:00Z".into(),
                ingested_by: "synthetic-test".into(),
            },
        )
        .unwrap();
    let mut wrong_v7_inputs = market_inputs;
    wrong_v7_inputs.push(wrong_facts_hash);
    let wrong_v7 = publish_dataset(
        lake.path(),
        &raw,
        &NormalizerRegistry::with_corporate_actions_v7(alternate_market.dsv.clone()).unwrap(),
        "szse-etf-frozen",
        "7",
        &wrong_v7_inputs,
        "2026-10-20T00:00:00Z",
    )
    .unwrap();
    let mismatch = DerivedMarket::from_dataset(
        lake.path(),
        &wrong_v7.dsv,
        &catalog,
        MarketEvidence {
            semantic_version: Some(ExecutionStatusVersion::V3),
            rule_version: "synthetic-mismatch@1".into(),
            start: catalog.input().coverage.start,
            end: catalog.input().coverage.end,
            coverage_gaps: std::collections::BTreeSet::new(),
            openings: Vec::new(),
        },
    )
    .unwrap_err();
    assert!(
        mismatch
            .to_string()
            .contains("different v4 Normalizer config")
    );
}

#[test]
fn cninfo_risk_input_is_bound_to_run_and_exits_on_next_sellable_open() {
    use prajna_data::{
        RawStore, SourceKind, SourceRecordInput,
        cninfo::{FundIndex, IndexDataset, PageSet},
    };
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let raw = RawStore::open(lake.path()).unwrap();
    let bytes = serde_json::to_vec(&json!({"totalAnnouncement":1,"announcements":[{
        "announcementId":"synthetic-termination", "secCode":"159001","orgId":"synthetic",
        "announcementTitle":"合成沪深300交易型开放式指数证券投资基金终止基金合同议案",
        "announcementTime":1672704000000_i64
    }]}))
    .unwrap();
    let hash = raw
        .put(
            &bytes,
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::FileImport,
                source_id: "synthetic".into(),
                request: json!({}),
                observed_at: "2026-10-16T00:00:00Z".into(),
                ingested_by: "test".into(),
            },
        )
        .unwrap();
    let mut pages = PageSet::default();
    pages.push(1, &bytes).unwrap();
    let mut index = IndexDataset::new(fixture::date(1), fixture::date(10)).unwrap();
    index.source_raw_sha256.insert(hash);
    index
        .funds
        .push(FundIndex::new("159001".into(), pages.finish().unwrap().0));
    for engine in ["vector", "fast_event"] {
        let mut input = definition(&f, engine);
        input["pit"]["risk_notices"] = json!([]);
        let old = Experiment::parse(&input.to_string(), lake.path()).unwrap();
        input["pit"]["cninfo_index"] = json!(index);
        let experiment = Experiment::parse(&input.to_string(), lake.path()).unwrap();
        let runs = experiment.expand(lake.path()).unwrap();
        assert_ne!(runs[0].run_id, old.expand(lake.path()).unwrap()[0].run_id);
        assert_eq!(
            runs[0].run_spec["pit"]["risk_signals"]["signals"][0]["available_at"],
            json!(fixture::time(4, 15))
        );
        let report = experiment.execute(lake.path(), 1).unwrap();
        assert_eq!(
            report.runs[0].status,
            RunStatus::Completed,
            "{:?}",
            report.runs[0].error
        );
        let exits: Vec<_> = if engine == "vector" {
            report.runs[0]
                .result
                .as_ref()
                .unwrap()
                .risk_exits
                .iter()
                .collect()
        } else {
            report.runs[0]
                .fast_event_result
                .as_ref()
                .unwrap()
                .risk_exits
                .iter()
                .map(|(_, exit)| exit)
                .collect()
        };
        assert!(
            exits
                .iter()
                .any(|exit| exit.executed && exit.session_date == fixture::date(8))
        );
        assert!(
            !exits
                .iter()
                .any(|exit| exit.session_date < fixture::date(5))
        );
    }
}

#[test]
fn pit_experiment_identity_engine_results_and_persistence() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    assert_eq!(f.ids.len(), 3);
    for engine in ["vector", "fast_event"] {
        let input = definition(&f, engine);
        let experiment = Experiment::parse(&input.to_string(), lake.path()).unwrap();
        let runs = experiment.expand(lake.path()).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].run_spec["session_inputs"].as_array().unwrap().len(),
            10
        );
        assert_eq!(
            runs[0].run_spec["pit"]["derived_market_version"],
            f.market.version()
        );
        assert_eq!(
            runs[0].run_spec["pit"]["return_index_version"],
            f.tri.version()
        );
        assert_eq!(
            runs[0].run_spec["unknown_coverage"]["thresholds"],
            json!({
                "total_unknown_percent": 5,
                "per_instrument_unknown_percent": 20,
            })
        );
        assert!(matches!(
            runs[0].run_spec["unknown_coverage"]["engineering_acceptance"].as_str(),
            Some("passed" | "failed")
        ));
        assert_eq!(
            runs[0].run_spec["pit"]["risk_signals"]["signals"]
                .as_array()
                .unwrap()
                .len(),
            f.risk.signals.len()
        );
        assert_eq!(
            runs[0].run_spec["session_inputs"][6]["eligible"]["universe"]["members"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let report = experiment.execute(lake.path(), 2).unwrap();
        assert_eq!(
            report.runs[0].status,
            RunStatus::Completed,
            "{:?}",
            report.runs[0].error
        );
        assert!(
            lake.path()
                .join("acceptance/mvp6-unknown-coverage.json")
                .is_file()
        );
        if engine == "fast_event" {
            let result = report.runs[0].fast_event_result.as_ref().unwrap();
            assert_eq!(
                result.sessions[9].close.account.equity,
                "9979.06".parse().unwrap()
            );
            assert_eq!(result.risk_exits.len(), 4);
        } else {
            let result = report.runs[0].result.as_ref().unwrap();
            assert_eq!(result.risk_exits.len(), 4);
            assert_eq!(result.pit_initial_capital, Some("10000".parse().unwrap()));
        }

        let stored = experiment
            .execute_and_store(
                lake.path(),
                env!("CARGO_MANIFEST_DIR"),
                ResultLevel::Summary,
                1,
            )
            .unwrap();
        let exp = experiment.id().unwrap();
        let exe = stored.manifest.execution.id;
        let run = &stored.manifest.runs[0].run_id;
        read_run_table(
            lake.path(),
            &exp,
            &exe,
            run,
            ResultLevel::Summary,
            "summary",
        )
        .unwrap();
        let printed = stored.manifest.runs[0]
            .run_spec
            .as_ref()
            .unwrap()
            .to_string();
        assert!(printed.contains("session_inputs"));
        assert!(printed.contains("risk_signals"));
        if engine == "vector" {
            assert_eq!(
                stored.manifest.runs[0].run_spec.as_ref().unwrap()["engine"]["initial_capital"],
                "10000"
            );
        }
        let promoted = experiment
            .promote(
                lake.path(),
                env!("CARGO_MANIFEST_DIR"),
                &exe,
                std::slice::from_ref(run),
                ResultLevel::Full,
                2,
            )
            .unwrap();
        assert_eq!(promoted.manifest.runs[0].result_level, ResultLevel::Full);
        if engine == "vector" {
            assert_eq!(
                promoted.manifest.runs[0].run_spec.as_ref().unwrap()["engine"]["initial_capital"],
                "10000"
            );
        }
        let table = if engine == "vector" {
            "executions"
        } else {
            "orders"
        };
        let (_, batches) =
            read_run_table(lake.path(), &exp, &exe, run, ResultLevel::Full, table).unwrap();
        assert!(format!("{batches:?}").contains("risk_exit"));
        for (pointer, value) in [
            ("/pit/eligibility/minimum_average_amount", json!("10000001")),
            ("/parameter_space/list/0/lookback", json!(2)),
            ("/parameter_space/list/0/top_k", json!(3)),
            ("/parameter_space/list/0/rebalance_every", json!(5)),
            ("/pit/market/rule_version", json!("synthetic-execution@2")),
            (
                "/pit/risk_notices/0/evidence/publication/timestamp",
                json!(fixture::time(3, 15)),
            ),
        ] {
            let mut changed = input.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            let other = Experiment::parse(&changed.to_string(), lake.path()).unwrap();
            assert_ne!(exp, other.id().unwrap());
            assert_ne!(runs[0].run_id, other.expand(lake.path()).unwrap()[0].run_id);
        }
        let path = promoted.path.join("execution.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest["runs"][0]["run_spec"]["pit"]["execution_rule_version"] = json!("tampered");
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(
            read_run_table(
                lake.path(),
                &exp,
                &exe,
                run,
                ResultLevel::Summary,
                "summary"
            )
            .is_err()
        );
    }
}

#[test]
fn unknown_floor_failure_is_recorded_without_failing_the_run() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let mut input = definition(&f, "vector");
    input["pit"]["market"]["openings"][0]["limit_rule"] = Value::Null;
    input["pit"]["market"]["openings"][1]["limit_rule"] = Value::Null;

    let experiment = Experiment::parse(&input.to_string(), lake.path()).unwrap();
    let runs = experiment.expand(lake.path()).unwrap();
    let coverage = &runs[0].run_spec["unknown_coverage"];
    assert_eq!(coverage["denominator_cells"], 27);
    assert_eq!(coverage["unknown_cells"], 3);
    assert_eq!(coverage["unknown_percent"], "11.11%");
    assert_eq!(coverage["engineering_acceptance"], "failed");
    assert_eq!(coverage["thresholds"]["total_unknown_percent"], 5);
    assert_eq!(coverage["thresholds"]["per_instrument_unknown_percent"], 20);

    let report = experiment.execute(lake.path(), 1).unwrap();
    assert_eq!(report.runs[0].status, RunStatus::Completed);
    assert_eq!(
        report.runs[0].run_spec["unknown_coverage"]["engineering_acceptance"],
        "failed"
    );
    let local_report: Value = serde_json::from_slice(
        &std::fs::read(lake.path().join("acceptance/mvp6-unknown-coverage.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(local_report["unknowns"].as_array().unwrap().len(), 3);
    assert!(
        local_report["unknowns"]
            .as_array()
            .unwrap()
            .iter()
            .all(|cell| cell["reason"]["kind"].is_string())
    );
}

#[test]
fn vector_pit_capital_is_explicit_positive_and_identity_bearing() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let input = definition(&f, "vector");
    let original = Experiment::parse(&input.to_string(), lake.path()).unwrap();
    let run = original.expand(lake.path()).unwrap().remove(0);
    assert_eq!(run.run_spec["engine"]["initial_capital"], "10000");
    let mut changed = input.clone();
    changed["vector_pit"]["initial_capital"] = json!("1");
    let other = Experiment::parse(&changed.to_string(), lake.path()).unwrap();
    assert_ne!(original.id().unwrap(), other.id().unwrap());
    assert_ne!(run.run_id, other.expand(lake.path()).unwrap()[0].run_id);
    let mut equivalent = input.clone();
    equivalent["vector_pit"]["initial_capital"] = json!("10000.000");
    assert_eq!(
        original.id().unwrap(),
        Experiment::parse(&equivalent.to_string(), lake.path())
            .unwrap()
            .id()
            .unwrap()
    );
    for capital in ["0", "-1", "not-a-decimal"] {
        changed["vector_pit"]["initial_capital"] = json!(capital);
        assert!(Experiment::parse(&changed.to_string(), lake.path()).is_err());
    }
    changed.as_object_mut().unwrap().remove("vector_pit");
    assert!(
        Experiment::parse(&changed.to_string(), lake.path())
            .unwrap_err()
            .to_string()
            .contains("initial_capital")
    );
    let mut fast = definition(&f, "fast_event");
    fast["vector_pit"] = input["vector_pit"].clone();
    assert!(Experiment::parse(&fast.to_string(), lake.path()).is_err());
}

#[test]
fn pit_rejects_ambiguous_coverage_and_legacy_override() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    for (pointer, value) in [
        ("/universe", json!("wrong-catalog")),
        (
            "/availability_assumption",
            json!("treat_unknown_as_ts_close"),
        ),
        ("/pit/market/coverage_gaps", json!(["missing-session"])),
        ("/strategy", json!("s2_rotation")),
        ("/fast_event/rebalance", json!("entry_only")),
        ("/unfilled_entry", json!("retry")),
    ] {
        let mut input = definition(&f, "fast_event");
        if pointer == "/unfilled_entry" {
            input["unfilled_entry"] = value;
        } else {
            *input.pointer_mut(pointer).unwrap() = value;
        }
        assert!(
            Experiment::parse(&input.to_string(), lake.path()).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn pit_cli_grid_replays_and_experiment_vector_parity_matches_daily() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let mut vector_input = definition(&f, "vector");
    let mut fast_input = definition(&f, "fast_event");
    let fixed = json!({
        "version":"synthetic-fixed-etfs@1",
        "codes":["159001","159002"]
    });
    vector_input["pit"]["fixed_universe"] = fixed.clone();
    fast_input["pit"]["fixed_universe"] = fixed;
    fast_input["fast_event"]["sizing"] = json!("vector_parity");
    let vector_experiment = Experiment::parse(&vector_input.to_string(), lake.path()).unwrap();
    let fast_experiment = Experiment::parse(&fast_input.to_string(), lake.path()).unwrap();
    let mut revised_input = vector_input.clone();
    revised_input["pit"]["fixed_universe"]["codes"] = json!(["159001"]);
    let revised = Experiment::parse(&revised_input.to_string(), lake.path()).unwrap();
    assert_ne!(vector_experiment.id().unwrap(), revised.id().unwrap());
    let vector_run = &vector_experiment.expand(lake.path()).unwrap()[0];
    let revised_run = &revised.expand(lake.path()).unwrap()[0];
    assert_ne!(vector_run.run_id, revised_run.run_id);
    assert_eq!(
        vector_run.run_spec["pit"]["fixed_universe"]["version"],
        "synthetic-fixed-etfs@1"
    );
    assert_eq!(
        vector_run.run_spec["session_inputs"][3]["eligible"]["universe"]["run_spec"]["history_claim"],
        "retrospective_static"
    );
    assert_eq!(
        vector_run.run_spec["session_inputs"][3]["eligible"]["universe"]["run_spec"]["fixed_list"],
        json!(["159001", "159002"])
    );
    let vector = vector_experiment.execute(lake.path(), 1).unwrap();
    let fast = fast_experiment.execute(lake.path(), 2).unwrap();
    assert_eq!(vector.runs[0].status, RunStatus::Completed);
    assert_eq!(fast.runs[0].status, RunStatus::Completed);
    for (v, f) in vector.runs[0]
        .result
        .as_ref()
        .unwrap()
        .sessions
        .iter()
        .zip(&fast.runs[0].fast_event_result.as_ref().unwrap().sessions)
    {
        let equity = f
            .open_after
            .account
            .equity
            .to_string()
            .parse::<f64>()
            .unwrap()
            / 10000.0;
        assert!((equity - v.nav / (1.0 + v.gross_return)).abs() < 1e-12);
    }
    let mut input = fast_input;
    input["parameter_space"] = json!({"grid":{
        "lookback":[20,60],"top_k":[1,3],"rebalance_every":[5,20],
    }});
    let path = lake.path().join("definition.json");
    std::fs::write(&path, input.to_string()).unwrap();
    let invoke = |threads: &str| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
            .args(["run"])
            .arg(&path)
            .arg("--lake")
            .arg(lake.path())
            .args(["--threads", threads])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let first = invoke("1");
    let replay = invoke("2");
    assert_eq!(first["run_count"], 8);
    assert_eq!(first["failed_count"], 0);
    assert_eq!(first["exp"], replay["exp"]);
    assert_eq!(first["exe"], replay["exe"]);
    assert_eq!(replay["action"], "replayed");
}

#[test]
fn pit_defaults_to_versioned_mvp6_fixed_universe() {
    let lake = fixture::lake();
    let f = fixture::fixture(lake.path());
    let mut mvp6_input = definition(&f, "vector");
    mvp6_input["pit"]
        .as_object_mut()
        .unwrap()
        .remove("fixed_universe");
    let mvp6 = Experiment::parse(&mvp6_input.to_string(), lake.path()).unwrap();
    let mvp6_runs = mvp6.expand(lake.path()).unwrap();
    let mvp6_run = &mvp6_runs[0];

    let legacy_input = definition(&f, "vector");
    let legacy = Experiment::parse(&legacy_input.to_string(), lake.path()).unwrap();
    let legacy_run = &legacy.expand(lake.path()).unwrap()[0];

    assert_ne!(mvp6.id().unwrap(), legacy.id().unwrap());
    assert_ne!(mvp6_run.run_id, legacy_run.run_id);
    assert_eq!(
        mvp6_run.run_spec["pit"]["fixed_universe"]["version"],
        "mvp6-fixed-etf@2"
    );
    assert_eq!(
        mvp6_run.run_spec["pit"]["fixed_universe"]["codes"],
        json!([])
    );
    assert_eq!(
        mvp6_run.run_spec["pit"]["fixed_universe"]["excluded_codes"],
        json!({
            "159919":"unknown_limit_rule_across_evidenced_window:szse-fund-limit-rules@1",
            "159560":"unknown_limit_rule_across_evidenced_window:szse-fund-limit-rules@1",
            "159522":"unknown_limit_rule_across_evidenced_window:szse-fund-limit-rules@1",
            "159612":"unknown_limit_rule_across_evidenced_window:szse-fund-limit-rules@1",
            "159952":"unknown_limit_rule_across_evidenced_window:szse-fund-limit-rules@1"
        })
    );
    assert_eq!(
        mvp6_run.run_spec["pit"]["fixed_universe"]["minimum_code_count"],
        3
    );
    assert_eq!(
        mvp6_run.run_spec["session_inputs"][3]["eligible"]["universe"]["run_spec"]["fixed_list"],
        json!([])
    );
    assert_eq!(
        mvp6_run.run_spec["session_inputs"][3]["eligible"]["universe"]["run_spec"]["fixed_list_version"],
        "mvp6-fixed-etf@2"
    );
    assert_eq!(
        mvp6_run.run_spec["session_inputs"][3]["eligible"]["universe"]["run_spec"]["fixed_list_exclusions"]
            ["159919"],
        "unknown_limit_rule_across_evidenced_window:szse-fund-limit-rules@1"
    );
    assert_eq!(
        mvp6_run.run_spec["session_inputs"][3]["eligible"]["universe"]["run_spec"]["warnings"],
        json!(["p27_19_minimum_fixed_list_not_met", "survivorship_bias"])
    );
    assert_eq!(
        mvp6_run.run_spec["session_inputs"][3]["eligible"]["universe"]["run_spec"]["history_claim"],
        "retrospective_static"
    );
    let report = mvp6.execute(lake.path(), 1).unwrap();
    assert_eq!(report.runs[0].status, RunStatus::Failed);
    assert!(
        report.runs[0]
            .error
            .as_deref()
            .unwrap()
            .contains("P27-19 escalation")
    );
    assert_eq!(
        report.runs[0].run_spec["pit"]["fixed_universe"]["codes"],
        json!([])
    );
}
