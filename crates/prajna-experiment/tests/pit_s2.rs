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
        "vector@3"
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
    assert_eq!(fast.semantic_version, "fast_event@3");
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
