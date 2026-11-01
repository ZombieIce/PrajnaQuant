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
    json!({"schema_version":"supplier-adjusted/1","price_unit":"CNY_per_fund_unit","instrument_id":id,"method":"forward",
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
    for problem in ["missing", "misbound", "short", "unit"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        setup(root, true);
        let file = root.join("update-inputs/XSHG/adjusted.json");
        let mut value = supplier("510300.XSHG");
        match problem {
            "missing" => fs::remove_file(&file).unwrap(),
            "misbound" => {
                value["schema_version"] = json!(1);
                value.as_object_mut().unwrap().remove("price_unit");
                value["raw_dsv"] = json!(format!("dsv:sha256:{}", "0".repeat(64)));
                save(&file, &value);
            }
            "unit" => {
                value["price_unit"] = json!("unknown");
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
        assert_eq!(
            attempt.actions["total_return"],
            if matches!(state, "no_events" | "covered") {
                "unsupported_mixed_venue"
            } else {
                "incomplete_facts"
            }
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
fn retry_rejects_receipt_from_another_member_or_fixed_scope() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    setup(root, true);
    let attempt = run(root);
    let file = root.join(format!("pool-updates/attempts/{}.json", attempt.id));
    let mut corrupted = serde_json::to_value(&attempt).unwrap();
    corrupted["members"]["510300.XSHG"]["receipt"] =
        corrupted["members"]["159952.XSHE"]["receipt"].clone();
    save(&file, &corrupted);
    let response = cli(
        env!("CARGO_BIN_EXE_prajna-pool-update"),
        root,
        &[],
        &json!({"op":"retry","attempt":attempt.id}),
        false,
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("fixed update plan")
    );
    let config: pool_update::Config = serde_json::from_slice(
        &fs::read(root.join("pool-updates/configs/synthetic.json")).unwrap(),
    )
    .unwrap();
    let receipt = attempt.members["510300.XSHG"].receipt.as_ref().unwrap();
    assert!(
        prajna_data::daily_publish::verify_selection_scope(
            root,
            receipt,
            &config.members["510300.XSHG"].publication,
            "510300.XSHG",
            config.start.succ_opt().unwrap(),
            config.end
        )
        .is_err()
    );
    corrupted = serde_json::to_value(&attempt).unwrap();
    corrupted["start"] = json!("2025-01-01");
    save(&file, &corrupted);
    assert!(pool_update::get(root, &attempt.id).is_err());
}

#[cfg(unix)]
#[test]
fn frozen_supplies_never_write_through_raw_destination_symlinks() {
    for name in ["raw", "raw-sources"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("lake");
        setup(&root, true);
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join(name)).unwrap();
        let attempt = pool_update::prepare(&root, "synthetic").unwrap();
        let frozen = serde_json::to_value(&attempt).unwrap();
        assert!(
            frozen["adjusted_supplies"]["510300.XSHG"]["error"]
                .as_str()
                .unwrap()
                .contains("symbolic link")
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }
}

#[test]
#[ignore = "explicit synthetic production-browser input export"]
fn export_pool_update_fixture() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::env::var_os("PRAJNA_POOL_UPDATE_FIXTURE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    setup(Path::new(&root), false);
    fs::write(
        Path::new(&root).join("synthetic-pool-update-fixture"),
        b"synthetic only",
    )
    .unwrap();
}

#[test]
fn round1_bound_adjustment_preserves_original_bytes_and_version() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    setup(root, true);
    let first = run(root);
    let mut bound = supplier("510300.XSHG");
    bound.as_object_mut().unwrap().remove("price_unit");
    bound["schema_version"] = json!(1);
    bound["raw_dsv"] = json!(first.dataset.as_ref().unwrap().dsv);
    let bytes = format!("{}\n", serde_json::to_string_pretty(&bound).unwrap()).into_bytes();
    fs::write(root.join("update-inputs/XSHG/adjusted.json"), &bytes).unwrap();
    let expected = prajna_data::adjusted_series::import(root, &bytes).unwrap();
    let prepared = update(root, json!({"op":"retry","attempt":first.id}));
    let next = update(root, json!({"op":"pool_update","attempt":prepared.id}));
    assert!(next.price_ready);
    assert_eq!(
        next.members["510300.XSHG"].adjusted.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(
        fs::read(root.join("update-inputs/XSHG/adjusted.json")).unwrap(),
        bytes
    );
}

#[test]
fn round1_retry_old_attempt_does_not_regress_latest_member_success() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    setup(root, true);
    let first = run(root);
    let newer = run(root);
    let prepared = update(root, json!({"op":"retry","attempt":first.id}));
    let reused = update(root, json!({"op":"pool_update","attempt":prepared.id}));
    assert!(reused.price_ready);
    let listed = pool_update::list(root).unwrap();
    for id in MEMBERS {
        assert!(newer.members[id].succeeded_at > first.members[id].succeeded_at);
        assert_eq!(
            listed["last_member_success"][format!("bonds:2:{id}")]["succeeded_at"],
            json!(newer.members[id].succeeded_at)
        );
        assert_eq!(
            listed["last_member_success"][format!("bonds:2:{id}")]["attempt"],
            newer.id
        );
    }
}

#[test]
fn round1_retry_supplements_initially_unconfigured_inputs_in_same_plan() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    setup(root, true);
    let file = root.join("pool-updates/configs/synthetic.json");
    let configured: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let mut missing = configured.clone();
    missing["members"]["510300.XSHG"]["adjusted_file"] = Value::Null;
    missing["actions_file"] = Value::Null;
    save(&file, &missing);
    let first = run(root);
    assert!(!first.price_ready && first.dataset.is_some());
    let old = fs::read(root.join(format!("pool-updates/attempts/{}.json", first.id))).unwrap();
    save(&file, &configured);
    let prepared = update(root, json!({"op":"retry","attempt":first.id}));
    // Both paths and bytes are frozen at retry submission.
    fs::write(
        root.join("update-inputs/XSHG/adjusted.json"),
        b"changed after retry",
    )
    .unwrap();
    fs::write(
        root.join("update-inputs/actions.json"),
        b"changed after retry",
    )
    .unwrap();
    let next = update(root, json!({"op":"pool_update","attempt":prepared.id}));
    assert_eq!(next.plan, first.plan);
    assert_eq!(
        next.dataset.as_ref().unwrap().dsv,
        first.dataset.as_ref().unwrap().dsv
    );
    assert!(
        next.members
            .values()
            .all(|m| m.reused_from.as_deref() == Some(&first.id))
    );
    assert!(
        next.price_ready,
        "supplementation must work without creating a different plan"
    );
    assert_eq!(next.actions["complete"], true);
    assert_eq!(
        fs::read(root.join(format!("pool-updates/attempts/{}.json", first.id))).unwrap(),
        old
    );
}

#[test]
fn round1_contradictory_collection_reports_do_not_advance_success() {
    for status in [
        "failed",
        "empty",
        "not_attempted",
        "bad_summary",
        "missing_accepted",
        "overflow",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        setup(root, true);
        let good = run(root);
        let file = root.join("update-inputs/XSHE/report.json");
        let mut report: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        if matches!(status, "failed" | "empty" | "not_attempted" | "overflow") {
            let mut chunk = report["chunks"][0].clone();
            chunk["index"] = json!(1);
            chunk["status"] = json!(if status == "overflow" {
                "failed"
            } else {
                status
            });
            chunk["candidate_attempts"] = json!(if status == "not_attempted" { 0 } else { 1 });
            chunk["candidate_failures"] = json!([]);
            chunk["raw_objects"] = json!([]);
            chunk["actual_response_dates"] = json!([]);
            chunk["actual_response_range"] = Value::Null;
            report["chunks"].as_array_mut().unwrap().push(chunk);
            report["summary"]["chunks_requested"] = json!(2);
            report["summary"]["chunks_attempted"] =
                json!(if status == "not_attempted" { 1 } else { 2 });
            report["summary"]["candidate_http_attempts"] = json!(
                report["summary"]["candidate_http_attempts"]
                    .as_u64()
                    .unwrap()
                    + if status == "not_attempted" { 0 } else { 1 }
            );
            report["summary"]["accepted_response_chunks"] =
                json!(if status == "empty" { 2 } else { 1 });
            report["summary"]["empty_chunks"] = json!(u32::from(status == "empty"));
            report["summary"]["not_attempted_chunks"] = json!(u32::from(status == "not_attempted"));
            report["summary"]["failed_chunks"] =
                json!(u32::from(status == "failed" || status == "overflow"));
        } else if status == "missing_accepted" {
            report["chunks"][0]["raw_objects"] = json!([]);
            report["summary"]["raw_object_references"] = json!(0);
        } else {
            report["summary"]["failed_chunks"] = json!(1);
        }
        if status == "overflow" {
            report["chunks"][0]["candidate_attempts"] = json!(u32::MAX);
        }
        assert_eq!(report["status"], "complete");
        save(&file, &report);
        let rejected = run(root);
        assert!(
            !rejected.price_ready && rejected.dataset.is_none(),
            "contradictory {status}"
        );
        assert!(rejected.members["159952.XSHE"].succeeded_at.is_none());
        assert!(rejected.members["510300.XSHG"].receipt.is_some());
        assert_eq!(
            pool_update::list(root).unwrap()["last_price_ready"]["bonds:2"]["attempt"],
            good.id
        );
        if status == "failed" {
            report["status"] = json!("partial");
            save(&file, &report);
            let partial = run(root);
            assert!(!partial.price_ready && partial.dataset.is_none());
            assert_eq!(partial.members["159952.XSHE"].publication, "partial");
            assert!(partial.members["159952.XSHE"].receipt.is_some());
            assert!(partial.members["159952.XSHE"].succeeded_at.is_none());
        }
    }
}

#[test]
fn round1_retry_rejects_scope_changes_and_freezes_explicit_fact_version() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    setup(root, true);
    let first = run(root);
    let file = root.join("pool-updates/configs/synthetic.json");
    let config: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    for pointer in [
        "/start",
        "/members/510300.XSHG/publication/calendar_version",
        "/members/510300.XSHG/publication/status_snapshot",
        "/members/510300.XSHG/publication/report",
        "/members/510300.XSHG/collect",
        "/pool/version",
    ] {
        let mut changed = config.clone();
        let target = changed.pointer_mut(pointer).unwrap();
        *target = if pointer == "/start" {
            json!("2026-01-06")
        } else if pointer.ends_with("collect") {
            json!(true)
        } else if pointer.ends_with("version") && pointer.starts_with("/pool") {
            json!(1)
        } else {
            json!("different-fixed-input")
        };
        save(&file, &changed);
        let error = cli(
            env!("CARGO_BIN_EXE_prajna-pool-update"),
            root,
            &[],
            &json!({"op":"retry","attempt":first.id}),
            false,
        );
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("fixed plan scope")
        );
    }
    let version = prajna_data::corporate_facts::import(
        root,
        &fs::read(root.join("update-inputs/actions.json")).unwrap(),
    )
    .unwrap();
    let mut supplied = config.clone();
    supplied["actions_file"] = Value::Null;
    supplied["actions_version"] = json!(version);
    save(
        &root.join("pool-updates/configs/supplement.json"),
        &supplied,
    );
    let prepared = update(
        root,
        json!({"op":"retry","attempt":first.id,"config":"supplement"}),
    );
    supplied["actions_version"] = json!("changed-after-submission");
    save(
        &root.join("pool-updates/configs/supplement.json"),
        &supplied,
    );
    let next = update(root, json!({"op":"pool_update","attempt":prepared.id}));
    assert_eq!(next.plan, first.plan);
    assert!(next.price_ready);
    assert_eq!(next.actions["version"], version);
    assert_eq!(next.actions["complete"], true);
    let mut invalid = config;
    invalid["actions_version"] = json!(version);
    save(&root.join("pool-updates/configs/supplement.json"), &invalid);
    let error = cli(
        env!("CARGO_BIN_EXE_prajna-pool-update"),
        root,
        &[],
        &json!({"op":"retry","attempt":first.id,"config":"supplement"}),
        false,
    );
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not both")
    );
}

#[test]
fn round1_legacy_plan_can_explicitly_supplement_without_rewriting_old_records() {
    use sha2::Digest;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    setup(root, true);
    let config_file = root.join("pool-updates/configs/synthetic.json");
    let configured = fs::read(&config_file).unwrap();
    let mut missing: Value = serde_json::from_slice(&configured).unwrap();
    missing["members"]["510300.XSHG"]["adjusted_file"] = Value::Null;
    missing["actions_file"] = Value::Null;
    save(&config_file, &missing);
    let prepared = update(root, json!({"op":"prepare","config":"synthetic"}));
    let mut plan: Value = serde_json::from_slice(
        &fs::read(root.join(format!("pool-updates/plans/{}.json", prepared.plan))).unwrap(),
    )
    .unwrap();
    plan.as_object_mut().unwrap().remove("config_name");
    let plan_bytes = serde_json::to_vec(&plan).unwrap();
    let legacy_plan = format!("{:x}", sha2::Sha256::digest(&plan_bytes));
    let plan_path = root.join(format!("pool-updates/plans/{legacy_plan}.json"));
    fs::write(&plan_path, &plan_bytes).unwrap();
    let mut input = serde_json::to_value(&prepared).unwrap();
    input["plan"] = json!(legacy_plan);
    input.as_object_mut().unwrap().remove("supply_mapping");
    for folder in ["attempts", "attempt-inputs"] {
        save(
            &root.join(format!("pool-updates/{folder}/{}.json", prepared.id)),
            &input,
        );
    }
    let first = update(root, json!({"op":"pool_update","attempt":prepared.id}));
    assert!(!first.price_ready);
    let old_path = root.join(format!("pool-updates/attempts/{}.json", first.id));
    let old = fs::read(&old_path).unwrap();
    fs::write(&config_file, configured).unwrap();
    let next = update(
        root,
        json!({"op":"retry","attempt":first.id,"config":"synthetic"}),
    );
    let finished = update(root, json!({"op":"pool_update","attempt":next.id}));
    assert!(finished.price_ready);
    assert_eq!(finished.plan, legacy_plan);
    assert_eq!(finished.actions["complete"], true);
    assert_eq!(fs::read(plan_path).unwrap(), plan_bytes);
    assert_eq!(fs::read(old_path).unwrap(), old);
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
