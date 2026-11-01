#[path = "../../prajna-data/tests/support/opening_fixture.rs"]
mod fixture;
use prajna_experiment::{
    pool_update::{self, Attempt},
    pools,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

const MEMBERS: [&str; 2] = ["159952.XSHE", "510300.XSHG"];
const DATES: [&str; 3] = ["2026-01-05", "2026-01-06", "2026-01-07"];
fn save(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}
fn supplier(id: &str) -> Value {
    json!({"schema_version":"supplier-adjusted/1","instrument_id":id,"method":"forward",
        "method_version":"synthetic-unadjusted/1","source":{"source_id":"synthetic","source_version":"1","source_ref":"synthetic://fixture"},
        "observed_at":"2026-10-28T00:00:00Z","retrospective_assumption":"synthetic supplier revision, not PIT",
        "close_availability_assumption":"domestic close known after close","start":DATES[0],"end":DATES[2],
        "rows":DATES.map(|date| json!({"trade_date":date,"open":"10","close":"10.1","high":"11","low":"9"}))})
}
fn setup(root: &Path, publish_pool: bool) {
    fs::create_dir_all(root).unwrap();
    let mut members = serde_json::Map::new();
    for (venue, id) in [("XSHG", "510300.XSHG"), ("XSHE", "159952.XSHE")] {
        let input = root.join(format!("update-inputs/{venue}"));
        let (publication, _) = fixture::prepare(
            &input,
            venue,
            prajna_domain::InstrumentKind::Etf,
            false,
            None,
        );
        let adjustment = input.join("adjusted.json");
        save(&adjustment, &supplier(id));
        members.insert(
            id.into(),
            json!({"publication":publication,"collect":false,"adjusted_file":adjustment}),
        );
    }
    let actions = root.join("update-inputs/actions.json");
    save(
        &actions,
        &json!({"schema_version":1,"observed_at":"2026-10-28T00:00:00Z",
        "coverage": MEMBERS.map(|id| json!({"instrument_id":id,"kind":"etf","start":DATES[0],"end":DATES[2],
            "state":"no_events","reason":"explicit synthetic no-event scope",
            "source":{"source_id":"synthetic","source_version":"1","source_ref":"synthetic://scope"},
            "available_at":"2026-01-04T00:00:00Z","facts":[]}))}),
    );
    save(
        &root.join("pool-updates/configs/synthetic.json"),
        &json!({"schema_version":1,
        "pool":{"id":"bonds","version":2},"start":DATES[0],"end":DATES[2],"members":members,
        "actions_file":actions,"actions_version":null}),
    );
    if publish_pool {
        let original = pools::command(root, pools::Command::List {}).unwrap();
        let draft = serde_json::from_value(original["data"]["pools"][2]["draft"].clone()).unwrap();
        pools::command(
            root,
            pools::Command::Save {
                id: "bonds".into(),
                expected: draft,
                members: MEMBERS.map(str::to_string).to_vec(),
            },
        )
        .unwrap();
        pools::command(
            root,
            pools::Command::Publish {
                id: "bonds".into(),
                expected: MEMBERS.map(str::to_string).to_vec(),
            },
        )
        .unwrap();
    }
}
fn cli(binary: &str, root: &Path, args: &[&str], request: &Value, success: bool) -> Value {
    let mut child = Command::new(binary)
        .arg("--lake")
        .arg(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.success(),
        success,
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn update(root: &Path, input: Value) -> Attempt {
    let value = cli(
        env!("CARGO_BIN_EXE_prajna-pool-update"),
        root,
        &[],
        &input,
        true,
    );
    serde_json::from_value(value["data"]["attempt"].clone()).unwrap()
}
fn run(root: &Path) -> Attempt {
    let prepared = update(root, json!({"op":"prepare","config":"synthetic"}));
    update(root, json!({"op":"pool_update","attempt":prepared.id}))
}
fn selection(attempt: &Attempt) -> Value {
    let adjusted = attempt
        .members
        .iter()
        .map(|(id, m)| (id.clone(), json!(m.adjusted)))
        .collect::<serde_json::Map<_, _>>();
    json!({"dsv":attempt.dataset.as_ref().unwrap().dsv,"pool":{"id":"bonds","version":2},
        "strategy":"buy_and_hold","adjusted":adjusted,"parameter_space":{"list":[{"params_version":1}]}})
}

#[test]
fn complete_production_cli_update_freeze_research_and_immutable_rerun() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    setup(root, true);
    let attempt = run(root);
    assert!(attempt.price_ready);
    assert_eq!(attempt.members.len(), 2);
    assert_eq!(attempt.actions["complete"], true);
    let plan = cli(
        env!("CARGO_BIN_EXE_prajna-freeze-selection"),
        root,
        &["--plan"],
        &selection(&attempt),
        true,
    );
    assert_eq!(plan["data"]["runnable"], true);
    let frozen = cli(
        env!("CARGO_BIN_EXE_prajna-freeze-selection"),
        root,
        &[],
        &selection(&attempt),
        true,
    );
    let definition = frozen["data"]["definition"].clone();
    save(&root.join("frozen.json"), &definition);
    let result = Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
        .arg("run")
        .arg(root.join("frozen.json"))
        .arg("--lake")
        .arg(root)
        .args(["--level", "standard"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let old_output: Value = serde_json::from_slice(&result.stdout).unwrap();
    let read_result = || {
        Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
            .arg("result")
            .arg(old_output["exp"].as_str().unwrap())
            .arg(old_output["exe"].as_str().unwrap())
            .arg("--lake")
            .arg(root)
            .output()
            .unwrap()
    };
    let saved = read_result();
    assert!(saved.status.success());
    let golden: Value = serde_json::from_slice(&saved.stdout).unwrap();
    let final_close = golden["data"]["runs"][0]["series"]["account_valuations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["session_date"] == DATES[2] && r["valuation_point"] == "close")
        .unwrap();
    assert_eq!(final_close["cash"], "500.44006");
    assert_eq!(final_close["equity"], "1009490.44006");
    assert_eq!(final_close["dividend_receivable"], "0");
    let momentum = json!({"op":"submit","request":{"dsv":attempt.dataset.as_ref().unwrap().dsv,
        "pool":{"id":"bonds","version":2},"adjusted":selection(&attempt)["adjusted"],"windows":[1]}});
    let report = cli(
        env!("CARGO_BIN_EXE_prajna-momentum"),
        root,
        &[],
        &momentum,
        true,
    );
    assert!(report["data"]["id"].is_string());
    let persisted = cli(
        env!("CARGO_BIN_EXE_prajna-momentum"),
        root,
        &[],
        &json!({"op":"read","id":report["data"]["id"]}),
        true,
    );
    assert_eq!(persisted["data"]["id"], report["data"]["id"]);
    // Frozen inputs are unaffected by later pool edits and a same-plan retry.
    pools::command(
        root,
        pools::Command::Save {
            id: "bonds".into(),
            expected: MEMBERS.map(str::to_string).to_vec(),
            members: vec!["510300.XSHG".into()],
        },
    )
    .unwrap();
    pools::command(
        root,
        pools::Command::Publish {
            id: "bonds".into(),
            expected: vec!["510300.XSHG".into()],
        },
    )
    .unwrap();
    let retry = update(root, json!({"op":"retry","attempt":attempt.id}));
    let second = update(root, json!({"op":"pool_update","attempt":retry.id}));
    assert_eq!(
        attempt.dataset.as_ref().unwrap().dsv,
        second.dataset.as_ref().unwrap().dsv
    );
    assert!(
        second
            .members
            .values()
            .all(|m| m.reused_from.as_deref() == Some(&attempt.id))
    );
    assert_eq!(
        serde_json::to_value(pool_update::get(root, &attempt.id).unwrap()).unwrap(),
        serde_json::to_value(&attempt).unwrap()
    );
    let repeated = update(root, json!({"op":"pool_update","attempt":attempt.id}));
    assert_eq!(
        serde_json::to_value(repeated).unwrap(),
        serde_json::to_value(&attempt).unwrap()
    );
    prajna_data::pool_publish::rebuild(root, attempt.dataset.as_ref().unwrap()).unwrap();
    let rerun = Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
        .arg("run")
        .arg(root.join("frozen.json"))
        .arg("--lake")
        .arg(root)
        .args(["--level", "standard"])
        .output()
        .unwrap();
    assert!(rerun.status.success());
    let replay: Value = serde_json::from_slice(&rerun.stdout).unwrap();
    assert_eq!(old_output["exp"], replay["exp"]);
    assert_eq!(old_output["exe"], replay["exe"]);
    assert_eq!(saved.stdout, read_result().stdout);
    assert_eq!(
        fs::read(root.join("frozen.json")).unwrap(),
        serde_json::to_vec(&definition).unwrap()
    );
}

#[test]
fn empty_and_source_stopped_keep_good_member_retry_and_old_success() {
    for status in ["failed", "source_stopped"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        setup(root, true);
        let old = run(root);
        let file = root.join("update-inputs/XSHE/report.json");
        let original = fs::read(&file).unwrap();
        let mut bad: Value = serde_json::from_slice(&original).unwrap();
        bad["status"] = json!(status);
        bad["chunks"] = json!([]);
        save(&file, &bad);
        let failed = run(root);
        assert!(!failed.price_ready && failed.dataset.is_none());
        assert!(failed.members["510300.XSHG"].receipt.is_some());
        assert!(failed.members["159952.XSHE"].succeeded_at.is_none());
        assert_eq!(
            pool_update::list(root).unwrap()["last_price_ready"]["bonds:2"]["attempt"],
            old.id
        );
        fs::write(&file, original).unwrap();
        let next = update(root, json!({"op":"retry","attempt":failed.id}));
        let recovered = update(root, json!({"op":"pool_update","attempt":next.id}));
        assert!(recovered.price_ready);
        assert_eq!(
            recovered.members["510300.XSHG"].reused_from.as_deref(),
            Some(failed.id.as_str())
        );
        assert!(recovered.members["159952.XSHE"].reused_from.is_none());
        prajna_data::pool_publish::rebuild(root, old.dataset.as_ref().unwrap()).unwrap();
    }
}

#[test]
fn missing_misbound_and_short_adjustments_need_input_without_losing_raw_publication() {
    for problem in ["missing", "misbound", "short"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        setup(root, true);
        let file = root.join("update-inputs/XSHG/adjusted.json");
        let mut value = supplier("510300.XSHG");
        match problem {
            "missing" => fs::remove_file(&file).unwrap(),
            "misbound" => {
                value["schema_version"] = json!(1);
                value["raw_dsv"] = json!(format!("dsv:sha256:{}", "0".repeat(64)));
                save(&file, &value);
            }
            _ => {
                value["rows"].as_array_mut().unwrap().pop();
                save(&file, &value);
            }
        }
        let attempt = run(root);
        assert!(!attempt.price_ready && attempt.dataset.is_some());
        assert!(attempt.members["510300.XSHG"].adjustment_error.is_some());
        save(&file, &supplier("510300.XSHG"));
        let next = update(root, json!({"op":"retry","attempt":attempt.id}));
        // The supplier file is frozen at submission, not re-read by the worker.
        fs::write(&file, b"broken after submission").unwrap();
        let finished = update(root, json!({"op":"pool_update","attempt":next.id}));
        assert!(finished.price_ready);
        assert_eq!(attempt.dataset.unwrap().dsv, finished.dataset.unwrap().dsv);
        assert!(
            pool_update::get(root, &attempt.id).unwrap().members["510300.XSHG"]
                .adjustment_error
                .is_some()
        );
    }
}

#[test]
fn action_four_states_do_not_block_price_research() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    setup(root, true);
    for state in ["no_events", "unknown", "missing_facts", "covered"] {
        let file = root.join("update-inputs/actions.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        for scope in value["coverage"].as_array_mut().unwrap() {
            scope["state"] = json!(state);
            if state == "covered" {
                scope["facts"] = json!([{"event_id":"synthetic-split","kind":"share_conversion","record_date":null,"ex_date":null,"pay_date":null,
                    "effective_date":DATES[1],"cash_per_share":null,"share_ratio":"2",
                    "source":{"source_id":"synthetic","source_version":"1","source_ref":"synthetic://fact"},
                    "available_at":"2026-01-04T00:00:00Z"}]);
            }
        }
        save(&file, &value);
        let attempt = run(root);
        assert!(attempt.price_ready);
        assert_eq!(attempt.actions["members"]["510300.XSHG"]["state"], state);
        assert_eq!(
            attempt.actions["complete"],
            matches!(state, "no_events" | "covered")
        );
        let report = cli(
            env!("CARGO_BIN_EXE_prajna-momentum"),
            root,
            &[],
            &json!({"op":"submit","request":{
            "dsv":attempt.dataset.unwrap().dsv,"pool":{"id":"bonds","version":2},
            "adjusted":attempt.members.iter().map(|(id,m)| (id.clone(),json!(m.adjusted))).collect::<serde_json::Map<_,_>>(),"windows":[1]}}),
            true,
        );
        assert!(report["data"]["id"].is_string());
    }
}

#[test]
fn rejected_plan_and_frozen_identity_cannot_be_bypassed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    setup(root, true);
    let prepared = update(root, json!({"op":"prepare","config":"synthetic"}));
    let path = root.join(format!("pool-updates/plans/{}.json", prepared.plan));
    let mut plan: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    plan["config"]["start"] = json!("2025-01-01");
    save(&path, &plan);
    cli(
        env!("CARGO_BIN_EXE_prajna-pool-update"),
        root,
        &[],
        &json!({"op":"pool_update","attempt":prepared.id}),
        false,
    );
    cli(
        env!("CARGO_BIN_EXE_prajna-pool-update"),
        root,
        &[],
        &json!({"op":"prepare","config":"../synthetic"}),
        false,
    );
}

#[test]
fn interrupted_attempt_requires_explicit_retry_and_preserves_partial_facts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    setup(root, true);
    let attempt = update(root, json!({"op":"prepare","config":"synthetic"}));
    let next = update(
        root,
        json!({"op":"retry","attempt":attempt.id,"interrupted":true}),
    );
    assert_eq!(
        pool_update::get(root, &attempt.id).unwrap().status,
        "interrupted"
    );
    let recovered = update(root, json!({"op":"pool_update","attempt":next.id}));
    assert!(recovered.price_ready);
}

#[test]
#[ignore = "explicit synthetic production-browser input export"]
fn export_pool_update_fixture() {
    let root = std::env::var_os("PRAJNA_POOL_UPDATE_FIXTURE_ROOT")
        .expect("explicit synthetic export root");
    setup(Path::new(&root), false);
    fs::write(
        Path::new(&root).join("synthetic-pool-update-fixture"),
        b"synthetic only",
    )
    .unwrap();
}

#[test]
fn live_collection_stop_blocks_remaining_network_calls_and_status_mismatch_is_visible() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    setup(root, true);
    let config_file = root.join("pool-updates/configs/synthetic.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&config_file).unwrap()).unwrap();
    for member in config["members"].as_object_mut().unwrap().values_mut() {
        member["collect"] = json!(true);
    }
    save(&config_file, &config);
    let attempt = pool_update::prepare(root, "synthetic").unwrap();
    let mut calls = 0;
    let result = pool_update::execute_with_collection(root, &attempt.id, |input, _| {
        calls += 1;
        let report_path = input.join("report.json");
        let mut report: prajna_data::daily_raw::DailyRawReport =
            serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
        report.status = prajna_data::daily_raw::CollectionStatus::SourceStopped;
        report.chunks.clear();
        Ok(prajna_data::daily_raw::CollectionRun {
            report,
            report_path,
        })
    })
    .unwrap();
    assert_eq!(calls, 1);
    assert!(!result.price_ready && result.dataset.is_none());
    assert!(result.members.values().all(|m| m.succeeded_at.is_none()));
    assert!(
        result.members["510300.XSHG"]
            .error
            .as_ref()
            .unwrap()
            .contains("source stopped")
    );
    setup(root, false);
    let mut config: Value = serde_json::from_slice(&fs::read(&config_file).unwrap()).unwrap();
    config["members"]["510300.XSHG"]["publication"]["status_snapshot"] =
        config["members"]["159952.XSHE"]["publication"]["status_snapshot"].clone();
    save(&config_file, &config);
    let result = run(root);
    assert!(result.members["159952.XSHE"].receipt.is_some());
    assert!(result.members["510300.XSHG"].error.is_some());
    assert!(result.dataset.is_none());
}

#[test]
fn report_source_conflict_does_not_publish_or_extend_previous_coverage() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    setup(root, true);
    let previous = run(root);
    let file = root.join("update-inputs/XSHG/report.json");
    let mut report: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    report["chunks"][0]["raw_objects"][0]["source"] = json!("unapproved-source");
    save(&file, &report);
    let current = run(root);
    assert!(!current.price_ready);
    assert!(
        current.members["510300.XSHG"]
            .error
            .as_ref()
            .unwrap()
            .contains("source record")
    );
    assert_eq!(
        pool_update::list(root).unwrap()["last_price_ready"]["bonds:2"]["attempt"],
        previous.id
    );
}
