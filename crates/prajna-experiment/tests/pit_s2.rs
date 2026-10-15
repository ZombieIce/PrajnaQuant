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
    }
    value
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
        if engine == "fast_event" {
            let result = report.runs[0].fast_event_result.as_ref().unwrap();
            assert_eq!(
                result.sessions[9].close.account.equity,
                "9979.06".parse().unwrap()
            );
            assert_eq!(result.risk_exits.len(), 4);
        } else {
            assert_eq!(report.runs[0].result.as_ref().unwrap().risk_exits.len(), 4);
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
    let vector_input = definition(&f, "vector");
    let mut fast_input = definition(&f, "fast_event");
    fast_input["fast_event"]["sizing"] = json!("vector_parity");
    let vector = Experiment::parse(&vector_input.to_string(), lake.path())
        .unwrap()
        .execute(lake.path(), 1)
        .unwrap();
    let fast = Experiment::parse(&fast_input.to_string(), lake.path())
        .unwrap()
        .execute(lake.path(), 2)
        .unwrap();
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
