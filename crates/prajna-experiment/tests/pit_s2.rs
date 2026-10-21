#[path = "../../prajna-research/tests/common/pit.rs"]
mod fixture;
use prajna_experiment::{Experiment, ResultLevel, RunStatus, read_run_table};
use serde_json::{Value, json};

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

#[test]
fn v7_opening_evidence_builds_canonical_pit_context_deterministically() {
    use prajna_data::{
        NormalizerRegistry, RawStore, SourceKind, SourceRecordInput,
        cninfo::{
            CatalogLifecycle, Citation, CitationInput, CorporateActionV2, EventFact, EvidenceLevel,
            Mvp6Session, PdfText, RiskEventV1, StructuredCorporateActionsV2,
        },
        publish_dataset, read_manifest,
    };
    use prajna_experiment::{opening_evidence, pit::PitContext};
    use prajna_research::etf_market::{DerivedMarket, ExecutionStatusVersion, MarketEvidence};

    struct SyntheticPdf;
    impl PdfText for SyntheticPdf {
        fn text(&mut self, _pdf: &[u8]) -> prajna_data::cninfo::Result<String> {
            Ok("synthetic suspension event".into())
        }
    }

    let lake = fixture::lake();
    let f = fixture::fixture_with_symbols_missing_bars(
        lake.path(),
        ["159919", "159612", "159952"],
        std::collections::BTreeSet::from([(1, 6), (2, 7), (1, 8)]),
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
                announcement_date: fixture::date(1),
                record_date: Some(fixture::date(2)),
                ex_date: Some(fixture::date(3)),
                pay_date: Some(fixture::date(4)),
                cash_per_share: Some("0.001".into()),
                split_ratio: None,
                liquidation_per_share: None,
                evidence_level: EvidenceLevel::Official,
                source_announcement_id: "SYNTH-ACTION".into(),
                source_page: 1,
            }],
        },
        risk_events: Vec::<RiskEventV1>::new(),
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

    let events = vec![
        EventFact {
            code: "159919".into(),
            kind: "suspension".into(),
            announcement_date: "2023-01-04".into(),
            effective_date: "2023-01-05".into(),
            session_part: "morning".into(),
            evidence_level: "official".into(),
            source_announcement_id: "SYNTH-HALT-OPENING".into(),
            source_page: "1".into(),
        },
        EventFact {
            code: "159612".into(),
            kind: "suspension".into(),
            announcement_date: "2023-01-05".into(),
            effective_date: "2023-01-06".into(),
            session_part: "full_day".into(),
            evidence_level: "official".into(),
            source_announcement_id: "SYNTH-HALT-PREVIOUS".into(),
            source_page: "1".into(),
        },
        EventFact {
            code: "159952".into(),
            kind: "suspension".into(),
            announcement_date: "2023-01-07".into(),
            effective_date: "2023-01-07".into(),
            session_part: "full_day".into(),
            evidence_level: "official".into(),
            source_announcement_id: "SYNTH-HALT-SAME-DAY".into(),
            source_page: "1".into(),
        },
        EventFact {
            code: "159919".into(),
            kind: "suspension".into(),
            announcement_date: "2023-01-07".into(),
            effective_date: "2023-01-08".into(),
            session_part: "full_day".into(),
            evidence_level: "official".into(),
            source_announcement_id: "SYNTH-HALT-CONFLICT".into(),
            source_page: "1".into(),
        },
    ];
    let events = [
        events,
        vec![EventFact {
            code: "159612".into(),
            kind: "suspension".into(),
            announcement_date: "2023-01-08".into(),
            effective_date: "2023-01-09".into(),
            session_part: "open".into(),
            evidence_level: "official".into(),
            source_announcement_id: "SYNTH-HALT-OPEN-ALIAS".into(),
            source_page: "1".into(),
        }],
    ]
    .concat();
    let mut citations = Vec::new();
    for event in &events {
        let pdf = format!(
            "%PDF-1.4 synthetic document for {}",
            event.source_announcement_id
        );
        let event_hash = raw
            .put(
                pdf.as_bytes(),
                SourceRecordInput {
                    content_type: "application/pdf".into(),
                    source_kind: SourceKind::FileImport,
                    source_id: format!("synthetic-pdf-{}", event.source_announcement_id),
                    request: json!({
                        "url": format!("https://example.invalid/finalpage/{}/synthetic.pdf", event.announcement_date)
                    }),
                    observed_at: "2026-10-20T00:00:00Z".into(),
                    ingested_by: "synthetic-test".into(),
                },
            )
            .unwrap();
        let mut converter = SyntheticPdf;
        for field in [
            "announcement_date",
            "effective_date",
            "kind",
            "session_part",
        ] {
            citations.push(
                Citation::from_store(
                    &raw,
                    &mut converter,
                    CitationInput {
                        fact_source: event.source_announcement_id.clone(),
                        field: field.into(),
                        announcement_id: event.source_announcement_id.clone(),
                        page: 1,
                        raw_sha256: event_hash.clone(),
                        excerpt: "synthetic suspension event".into(),
                        verified_by: "synthetic-test".into(),
                        verified_at: "2026-10-20T00:00:00Z".into(),
                    },
                )
                .unwrap(),
            );
        }
    }
    let catalog = f.catalog.clone();
    let generated =
        opening_evidence::generate(lake.path(), &v7.dsv, &catalog, &events, &citations).unwrap();
    let generated_again =
        opening_evidence::generate(lake.path(), &v7.dsv, &catalog, &events, &citations).unwrap();
    assert_eq!(
        opening_evidence::serialize(&generated).unwrap(),
        opening_evidence::serialize(&generated_again).unwrap()
    );

    let mut pit_input = f.input.clone();
    pit_input["market"] = serde_json::to_value(&generated).unwrap();
    pit_input["corporate_action_rules"] = json!({
        "version": "synthetic-actions@1",
        "unexplained_jump_threshold_bps": 100_000,
    });
    let pit_input = serde_json::from_value(pit_input).unwrap();
    let context = PitContext::build(&pit_input, lake.path(), &v7.dsv).unwrap();
    let halt_id = context
        .catalog
        .input()
        .lifecycles
        .iter()
        .find(|life| life.native_symbol == "159919")
        .unwrap()
        .instrument_id()
        .unwrap();
    let regular = &context.market.cells()[&(halt_id.clone(), fixture::date(1))];
    let opening_halt = &context.market.cells()[&(halt_id.clone(), fixture::date(5))];
    assert_eq!(
        regular.state,
        prajna_research::etf_market::TradeState::Tradable
    );
    assert_eq!(regular.execution_price, Some("1".parse().unwrap()));
    assert_eq!(
        opening_halt.state,
        prajna_research::etf_market::TradeState::Tradable
    );
    assert_eq!(opening_halt.execution_price, Some("1".parse().unwrap()));
    assert!(opening_halt.reasons.contains("official_opening_halt"));
    assert!(opening_halt.reasons.contains("execution_at_first_trade"));
    assert_eq!(opening_halt.available_at, Some(fixture::time(5, 15)));
    let opening_evidence = generated
        .openings
        .iter()
        .find(|opening| {
            opening.instrument_id == opening_halt.instrument_id
                && opening.session_date == fixture::date(5)
        })
        .unwrap();
    assert!(matches!(
        opening_evidence.halt.as_ref().unwrap().evidence.publication,
        prajna_research::pit_universe::Publication::DateOnly { date }
            if date == fixture::date(4)
    ));
    let halted_id = context
        .catalog
        .input()
        .lifecycles
        .iter()
        .find(|life| life.native_symbol == "159612")
        .unwrap()
        .instrument_id()
        .unwrap();
    let same_day_id = context
        .catalog
        .input()
        .lifecycles
        .iter()
        .find(|life| life.native_symbol == "159952")
        .unwrap()
        .instrument_id()
        .unwrap();
    let previous_day_halt = &context.market.cells()[&(halted_id.clone(), fixture::date(6))];
    let same_day_halt = &context.market.cells()[&(same_day_id, fixture::date(7))];
    let conflict = &context.market.cells()[&(halt_id.clone(), fixture::date(8))];
    let missing_without_notice = &context.market.cells()[&(halted_id.clone(), fixture::date(8))];
    use prajna_research::etf_market::TradeState;
    assert_eq!(previous_day_halt.state, TradeState::Halted);
    assert_eq!(same_day_halt.state, TradeState::Unknown);
    assert_eq!(conflict.state, TradeState::Unknown);
    assert!(conflict.reasons.contains("halt_trade_conflict"));
    assert_eq!(missing_without_notice.state, TradeState::Unknown);
    let open_alias = &context.market.cells()[&(halted_id.clone(), fixture::date(9))];
    assert_eq!(open_alias.state, TradeState::Tradable);
    assert!(open_alias.reasons.contains("official_opening_halt"));
    assert!(open_alias.reasons.contains("execution_at_first_trade"));
    assert!(
        previous_day_halt
            .available_at
            .is_some_and(|at| at >= fixture::time(6, 15))
    );
    assert!(
        same_day_halt
            .available_at
            .is_some_and(|at| at >= fixture::time(7, 15))
    );

    let report_path = lake.path().join("acceptance/mvp6-unknown-coverage.json");
    let first_report = std::fs::read(&report_path).unwrap();
    let _second_context = PitContext::build(&pit_input, lake.path(), &v7.dsv).unwrap();
    let second_report = std::fs::read(&report_path).unwrap();
    assert_eq!(first_report, second_report);

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
