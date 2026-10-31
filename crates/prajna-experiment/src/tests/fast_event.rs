use super::*;

fn fast_fixture(root: &Path) -> (String, String, String) {
    const FIXTURE: &[u8] =
        include_bytes!("../../../../poc/poc0-benchmark/fixtures/dataset-v1.json");
    let lake = root.join("lake");
    let store = RawStore::open(&lake).unwrap();
    let raw_hash = store
        .put(
            FIXTURE,
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::Fixture,
                source_id: "poc0-dataset-v1".into(),
                request: json!({}),
                observed_at: "synthetic".into(),
                ingested_by: "prajna-experiment-tests".into(),
            },
        )
        .unwrap();
    let manifest = publish_dataset(
        &lake,
        &store,
        &NormalizerRegistry::with_builtins(),
        "synthetic-etf-daily",
        "3",
        &[raw_hash],
        "2026-10-03T00:00:00Z",
    )
    .unwrap();
    let universe = StaticUniverse::new(
        "fixture",
        ["A.SYNTH", "B.SYNTH"]
            .map(|id| id.parse::<InstrumentId>().unwrap())
            .to_vec(),
    )
    .unwrap();
    universe.store(&lake).unwrap();
    (
        lake.to_string_lossy().into_owned(),
        manifest.dsv,
        universe.id().unwrap(),
    )
}

fn fast_definition(dsv: &str, universe: &str) -> Value {
    let mut value = definition(dsv, universe);
    value["engine"] = json!("fast_event");
    value["fast_event"] = json!({"currency":"CNY","initial_cash":"1000000","sizing":"vector_parity","rebalance":"full_target","minimum_commission":"5","portfolios":[{"id":1,"capital":"800000"}],"lot_size_source":{"kind":"instrument"}});
    value
}

#[test]
fn unsupported_engine_modes_are_rejected_before_loading_data() {
    let temp = TempDir::new().unwrap();
    let mut value = fast_definition("missing-dataset", "missing-universe");
    value["fast_event"]["rebalance"] = json!("entry_only");
    value["fast_event"]["entry_budget"] = json!("remaining_cash");
    value["unfilled_entry"] = json!("retry");
    let error = Experiment::parse(&value.to_string(), temp.path())
        .unwrap_err()
        .to_string();
    for part in ["fast_event", "s2_rotation", "vector_parity", "entry_only"] {
        assert!(error.contains(part), "{error}");
    }
    value["engine"] = json!("accurate_event");
    value["backend"] = json!({"name":"nautilus_trader","version":"2.0.0rc6"});
    let error = Experiment::parse(&value.to_string(), temp.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("accurate_event"), "{error}");
    assert!(error.contains("vector_parity"), "{error}");
}

#[test]
fn fast_identity_covers_execution_semantics_and_legacy_s2_stays_fixed() {
    let temp = TempDir::new().unwrap();
    let (lake, dsv, universe) = fast_fixture(temp.path());
    let legacy = Experiment::parse(&definition(&dsv, &universe).to_string(), &lake).unwrap();
    let old = legacy.expand(&lake).unwrap()[0].run_id.clone();
    let mut explicit = definition(&dsv, &universe);
    explicit["unfilled_entry"] = json!("skip");
    assert_eq!(
        old,
        Experiment::parse(&explicit.to_string(), &lake)
            .unwrap()
            .expand(&lake)
            .unwrap()[0]
            .run_id
    );
    let base = fast_definition(&dsv, &universe);
    let run_id = Experiment::parse(&base.to_string(), &lake)
        .unwrap()
        .expand(&lake)
        .unwrap()[0]
        .run_id
        .clone();
    for (pointer, value) in [
        ("/fast_event/initial_cash", json!("1000001")),
        ("/fast_event/currency", json!("USD")),
        ("/fast_event/sizing", json!("lot")),
        ("/fast_event/minimum_commission", json!("6")),
        ("/fast_event/portfolios/0/capital", json!("800001")),
        ("/unfilled_entry", json!("retry")),
    ] {
        let mut changed = base.clone();
        if pointer == "/unfilled_entry" {
            changed["unfilled_entry"] = value;
        } else {
            *changed.pointer_mut(pointer).unwrap() = value;
        }
        if pointer.ends_with("currency") {
            assert!(Experiment::parse(&changed.to_string(), &lake).is_err());
            continue;
        }
        let changed = Experiment::parse(&changed.to_string(), &lake).unwrap();
        assert_ne!(
            run_id,
            changed.expand(&lake).unwrap()[0].run_id,
            "{pointer}"
        );
    }
    let mut changed = base.clone();
    changed["fast_event"]["sizing"] = json!("lot");
    changed["fast_event"]["rebalance"] = json!("entry_only");
    changed["fast_event"]["entry_budget"] = json!("remaining_cash");
    changed["unfilled_entry"] = json!("retry");
    let first = Experiment::parse(&changed.to_string(), &lake)
        .unwrap()
        .expand(&lake)
        .unwrap()[0]
        .run_id
        .clone();
    changed["fast_event"]["entry_budget"] = json!("fixed_initial");
    assert_ne!(
        first,
        Experiment::parse(&changed.to_string(), &lake)
            .unwrap()
            .expand(&lake)
            .unwrap()[0]
            .run_id
    );
    let mut changed = base.clone();
    changed["fast_event"]["lot_size_source"] =
        json!({"kind":"explicit","values":{"A.SYNTH":"100","B.SYNTH":"100","C.SYNTH":"100"}});
    let id = Experiment::parse(&changed.to_string(), &lake)
        .unwrap()
        .expand(&lake)
        .unwrap()[0]
        .run_id
        .clone();
    assert_ne!(id, run_id);
    changed["fast_event"]["lot_size_source"]["values"]["A.SYNTH"] = json!("200");
    assert_ne!(
        id,
        Experiment::parse(&changed.to_string(), &lake)
            .unwrap()
            .expand(&lake)
            .unwrap()[0]
            .run_id
    );
}

#[test]
fn fast_levels_threads_replay_and_promotion_preserve_exact_content() {
    let temp = TempDir::new().unwrap();
    let (lake, dsv, universe) = fast_fixture(temp.path());
    for (kind, params) in [
        ("buy_and_hold", json!({"list":[{"params_version":1}]})),
        (
            "ma_crossover",
            json!({"grid":{"params_version":[1],"short_window":[1,2],"long_window":[3,5]}}),
        ),
        (
            "s2_rotation",
            definition(&dsv, &universe)["parameter_space"].clone(),
        ),
    ] {
        let mut value = fast_definition(&dsv, &universe);
        value["strategy"] = json!(kind);
        value["parameter_space"] = params;
        let experiment = Experiment::parse(&value.to_string(), &lake).unwrap();
        let a = experiment.execute(&lake, 1).unwrap();
        let b = experiment.execute(&lake, 4).unwrap();
        assert!(
            a.runs.iter().all(|r| r.status == RunStatus::Completed),
            "{kind}: {:?}",
            a.runs
        );
        assert_eq!(
            serde_json::to_vec(&a.runs).unwrap(),
            serde_json::to_vec(&b.runs).unwrap()
        );
        for (left, right) in a.runs.iter().zip(&b.runs) {
            for table in [
                "account_valuations",
                "portfolio_valuations",
                "orders",
                "fills",
                "fee_settlements",
            ] {
                assert_eq!(
                    storage::execution_rows(left, table).unwrap(),
                    storage::execution_rows(right, table).unwrap()
                );
            }
        }
        let summary_report = experiment
            .execute_level(Path::new(&lake), 4, ResultLevel::Summary)
            .unwrap();
        assert!(
            summary_report
                .runs
                .iter()
                .all(|r| r.fast_event_result.is_none())
        );
        let identity = test_execution_identity(&experiment.id().unwrap(), 'a');
        let summary = storage::store_execution(
            &experiment,
            &identity,
            &summary_report,
            Path::new(&lake),
            ResultLevel::Summary,
            4,
            "synthetic".into(),
        )
        .unwrap();
        assert!(!summary.path.join("runs").exists());
        assert_eq!(summary.manifest.runs[0].accounting_verified, Some(true));
        assert_eq!(summary.manifest.runs[0].not_saved.len(), 6);
        assert!(summary.manifest.runs[0].not_produced_by_engine.is_empty());
        let ids = a.runs.iter().map(|r| r.run_id.clone()).collect::<Vec<_>>();
        let standard = storage::promote(
            &experiment,
            &identity,
            Path::new(&lake),
            &identity.id,
            &ids,
            ResultLevel::Standard,
            1,
        )
        .unwrap();
        assert_eq!(standard.manifest.runs[0].table_hashes.len(), 3);
        assert_eq!(
            standard.manifest.runs[0].not_saved,
            vec!["orders", "fills", "fee_settlements"]
        );
        let full = storage::promote(
            &experiment,
            &identity,
            Path::new(&lake),
            &identity.id,
            &ids,
            ResultLevel::Full,
            4,
        )
        .unwrap();
        assert_eq!(full.manifest.runs[0].table_hashes.len(), 6);
        assert!(full.manifest.runs[0].not_saved.is_empty());
        assert_eq!(
            summary.manifest.summary_logical_hash,
            full.manifest.summary_logical_hash
        );
        let replay = storage::store_execution(
            &experiment,
            &identity,
            &b,
            Path::new(&lake),
            ResultLevel::Summary,
            1,
            "synthetic".into(),
        )
        .unwrap();
        assert_eq!(replay.action, StoreAction::Replayed);
        value["engine"] = json!("vector");
        value.as_object_mut().unwrap().remove("fast_event");
        let vector = Experiment::parse(&value.to_string(), &lake)
            .unwrap()
            .execute(&lake, 2)
            .unwrap();
        assert!(vector.runs.iter().all(|r| r.result.is_some()));
    }
}

#[test]
fn native_strategies_use_declared_universe_and_next_session_open() {
    let temp = TempDir::new().unwrap();
    let (lake, dsv, universe) = fast_fixture(temp.path());
    let mut value = fast_definition(&dsv, &universe);
    value["strategy"] = json!("buy_and_hold");
    value["parameter_space"] = json!({"list":[{"params_version":1}]});
    let experiment = Experiment::parse(&value.to_string(), &lake).unwrap();
    let report = experiment.execute(&lake, 2).unwrap();
    let result = report.runs[0].fast_event_result.as_ref().unwrap();
    assert!(
        result
            .fills
            .iter()
            .all(|f| f.fill.order().instrument_id().to_string() != "C.SYNTH")
    );
    assert!(
        result
            .fills
            .iter()
            .all(|f| f.fill.executed_at() > result.sessions[0].session.ts_close)
    );
    assert!(result.sessions[0].close.account.positions.is_empty());
    assert_eq!(
        result.sessions[0].close.unallocated_capital,
        "200000".parse().unwrap()
    );
    for session in &result.sessions {
        session.open_after.validate().unwrap();
        session.close.validate().unwrap();
    }
}

#[test]
fn fast_invariant_failure_is_failed_while_other_runs_continue() {
    use polars::prelude::{NamedFrom, Series};
    let temp = TempDir::new().unwrap();
    let (lake, dsv, universe) = fast_fixture(temp.path());
    let only = StaticUniverse::new("one", vec!["A.SYNTH".parse().unwrap()]).unwrap();
    only.store(&lake).unwrap();
    let mut value = fast_definition(&dsv, &universe);
    value["universe"] = json!(only.id().unwrap());
    value["strategy"] = json!("buy_and_hold");
    value["parameter_space"] = json!({"list":[{"params_version":1}]});
    value["fast_event"]["initial_cash"] = json!("0.000000000000000002");
    value["fast_event"]["portfolios"] = json!([{"id":1,"capital":"0.000000000000000001"},{"id":2,"capital":"0.000000000000000001"}]);
    for v in value["costs"].as_object_mut().unwrap().values_mut() {
        *v = json!(0.0);
    }
    let experiment = Experiment::parse(&value.to_string(), &lake).unwrap();
    let mut runs = experiment.expand(&lake).unwrap();
    let mut no_trade = runs[0].clone();
    no_trade.run_id =
        "run:sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into();
    no_trade.strategy = RunStrategy::Native(VectorStrategy::MaCrossover(Default::default()));
    runs.push(no_trade);
    let mut panel = load_panel(&lake, &dsv, &"SYNTH".parse().unwrap()).unwrap();
    let has_bar = panel.grid.column("has_bar").unwrap().bool().unwrap();
    let valid = (0..has_bar.len())
        .map(|i| has_bar.get(i) == Some(true))
        .collect::<Vec<_>>();
    panel
        .grid
        .with_column(
            Series::new(
                "open_decimal".into(),
                valid.iter().map(|v| v.then_some("1")).collect::<Vec<_>>(),
            )
            .into(),
        )
        .unwrap();
    panel
        .grid
        .with_column(
            Series::new(
                "close_decimal".into(),
                valid.iter().map(|v| v.then_some("0.5")).collect::<Vec<_>>(),
            )
            .into(),
        )
        .unwrap();
    let statuses = load_execution_status(&lake, &dsv).unwrap();
    let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    let results = pool.install(|| {
        runs.par_iter()
            .map(|run| {
                engine::execute(
                    &experiment,
                    run,
                    &HashMap::new(),
                    &panel,
                    &panel,
                    &statuses,
                    &HashMap::new(),
                    &experiment.vector_costs(),
                    AvailabilityAssumption::None,
                    only.members(),
                    ResultLevel::Summary,
                    None,
                    None,
                )
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(results[0].status, RunStatus::Failed);
    assert!(
        results[0].error.as_ref().unwrap().contains("Invariant"),
        "{:?}",
        results[0].error
    );
    assert!(results[0].summary.is_none());
    assert_eq!(
        results[1].status,
        RunStatus::Completed,
        "{:?}",
        results[1].error
    );
}

#[test]
fn full_orders_explain_zero_quantity_lot_and_cash_attempts() {
    let temp = TempDir::new().unwrap();
    let (lake, dsv, _) = fast_fixture(temp.path());
    let universe = StaticUniverse::new("one", vec!["A.SYNTH".parse().unwrap()]).unwrap();
    universe.store(&lake).unwrap();
    for (cash, reason) in [("1", "lot_rounding"), ("10000", "insufficient_cash")] {
        let mut value = fast_definition(&dsv, &universe.id().unwrap());
        value["strategy"] = json!("buy_and_hold");
        value["parameter_space"] = json!({"list":[{"params_version":1}]});
        value["fast_event"]["sizing"] = json!("lot");
        value["fast_event"]["initial_cash"] = json!(cash);
        value["fast_event"]["portfolios"] = json!([{"id":1,"capital":cash}]);
        // High minimum commission guarantees cash reduction independently of fixture prices.
        if reason == "insufficient_cash" {
            value["fast_event"]["minimum_commission"] = json!("10000");
        }
        let experiment = Experiment::parse(&value.to_string(), &lake).unwrap();
        let report = experiment.execute(&lake, 2).unwrap();
        let run = &report.runs[0];
        assert_eq!(run.status, RunStatus::Completed, "{:?}", run.error);
        let rows = storage::execution_rows(run, "orders").unwrap();
        assert!(
            rows.iter().any(|(_, payload)| {
                let v: Value = serde_json::from_str(payload).unwrap();
                v["record_type"] == "sizing_adjustment" && v["reason"] == reason
            }),
            "{reason}: {rows:?}"
        );
        assert!(
            run.fast_event_result
                .as_ref()
                .unwrap()
                .account
                .orders()
                .values()
                .all(|o| o.quantity() > prajna_domain::Quantity::ZERO)
        );
    }
}
