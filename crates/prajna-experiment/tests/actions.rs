//! Independent scale-18 hand ledger, exercised through production publication,
//! frozen selection, execution, and persisted-result CLIs. No real market data.
use prajna_data::{
    corporate_facts,
    daily_publish::{DailyPublicationSelection, publish_daily},
};
use prajna_domain::InstrumentKind;
use prajna_experiment::{
    Experiment, ResultLevel, read_run_table,
    selection::{Selection, freeze},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
#[allow(dead_code)]
#[path = "support/selection_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/multi_venue.rs"]
mod multi;
#[path = "../../prajna-data/tests/support/opening_fixture.rs"]
mod opening;
const DATES: [&str; 10] = [
    "2026-01-05",
    "2026-01-06",
    "2026-01-07",
    "2026-01-08",
    "2026-01-09",
    "2026-01-12",
    "2026-01-13",
    "2026-01-14",
    "2026-01-15",
    "2026-01-16",
];
fn source() -> Value {
    json!({"source_id":"synthetic","source_version":"1","source_ref":"hand-ledger"})
}
fn facts(instrument: &str) -> Value {
    json!({"schema_version":1,"observed_at":"2026-10-30T00:00:00Z","coverage":[{
    "instrument_id":instrument,"kind":"etf","start":DATES[0],"end":DATES[9],"state":"covered","reason":"synthetic full scope",
    "source":source(),"available_at":"2026-01-01T00:00:00Z","facts":[
        {"event_id":"dividend","kind":"dividend","record_date":DATES[2],"ex_date":DATES[3],"pay_date":DATES[4],"effective_date":null,"cash_per_share":"2","share_ratio":null,"source":source(),"available_at":"2026-01-01T00:00:00Z"},
        {"event_id":"split","kind":"share_conversion","record_date":null,"ex_date":null,"pay_date":null,"effective_date":DATES[6],"cash_per_share":null,"share_ratio":"2","source":source(),"available_at":"2026-01-01T00:00:00Z"}
    ]}]})
}
fn setup(root: &Path) -> Value {
    let bars = [
        ("10", "10", "11", "9"),
        ("10", "10", "11", "9"),
        ("10", "10", "11", "9"),
        ("8", "8", "9", "7"),
        ("8", "8", "9", "7"),
        ("8", "8", "9", "7"),
        ("4", "4", "5", "3"),
        ("4", "4", "5", "3"),
        ("4", "4", "5", "3"),
        ("4", "4", "5", "3"),
    ];
    let (selection, _) = opening::prepare(
        &root.join("input"),
        "XSHG",
        InstrumentKind::Etf,
        false,
        Some((&DATES, &bars)),
    );
    let dsv = publish_daily(
        root,
        serde_json::from_value::<DailyPublicationSelection>(selection).unwrap(),
        Path::new("receipt.json"),
    )
    .unwrap()
    .dsv;
    let adjusted = fixture::import_closes(
        root,
        &dsv,
        &DATES,
        &["10", "11", "12", "13", "14", "15", "16", "17", "18", "19"],
    );
    let version =
        corporate_facts::import(root, &serde_json::to_vec(&facts("510300.XSHG")).unwrap()).unwrap();
    json!({"dsv":dsv,"instrument":"510300.XSHG","strategy":"single_etf_timing",
        "adjusted":{"510300.XSHG":adjusted},"corporate_actions":{"version":version,"dividend_tax_rate":"0.1"},
        "parameter_space":{"list":[{"params_version":1,"ma_window":2,"rebalance_every":2}]},
        "settings":{"initial_cash":"10005","commission_rate":0,"minimum_commission":"5","buy_slippage_bps":0,"sell_slippage_bps":0,"buy_tax_rate":0,"sell_tax_rate":0}})
}
fn frozen(root: &Path, request: &Value) -> Value {
    freeze(
        root,
        &serde_json::from_value::<Selection>(request.clone()).unwrap(),
    )
    .unwrap()["data"]["definition"]
        .clone()
}
fn execute(root: &Path, request: &Value) -> prajna_experiment::ExecutionReport {
    Experiment::parse(&frozen(root, request).to_string(), root)
        .unwrap()
        .execute(root, 1)
        .unwrap()
}
#[test]
fn registration_receivable_payment_split_and_scheduled_reinvestment_match_hand_ledger() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let request = setup(root);
    let definition = frozen(root, &request);
    let experiment = Experiment::parse(&definition.to_string(), root).unwrap();
    assert_eq!(
        experiment.run_definitions(root).unwrap()[0].run_spec["engine"]["semantic_version"],
        "fast_event@15"
    );
    let report = experiment.execute(root, 1).unwrap();
    let run = &report.runs[0];
    assert!(run.error.is_none(), "{:?}", run.error);
    let result = run.fast_event_result.as_ref().unwrap();
    assert_eq!(result.semantic_version, "fast_event@15");
    assert!(result.corporate_action_completeness.is_complete);
    // Buy 1,000 at raw 10 + commission 5. Ex-date: 2,000 gross, tax 200,
    // net receivable 1,800, raw holdings 8,000: NAV amount 9,800.
    // Pay-day close transfers 1,800 without trading. Next normal decision is
    // day5, executed day6: 2:1 split -> 2,000; buy400 at4 + fee5 -> cash195.
    let cash = [
        "10005", "0", "0", "1800", "1800", "195", "195", "195", "195",
    ];
    let receivable = ["0", "0", "1800", "0", "0", "0", "0", "0", "0"];
    let quantity = [
        "0", "1000", "1000", "1000", "1000", "2400", "2400", "2400", "2400",
    ];
    let equity = [
        "10005", "10000", "9800", "9800", "9800", "9795", "9795", "9795", "9795",
    ];
    for (i, session) in result.sessions.iter().enumerate() {
        session.open_after.validate().unwrap();
        session.close.validate().unwrap();
        let account = &session.close.account;
        assert_eq!(account.cash.to_string(), cash[i]);
        assert_eq!(account.dividend_receivable.to_string(), receivable[i]);
        assert_eq!(account.equity.to_string(), equity[i]);
        assert_eq!(
            account
                .positions
                .get(&"510300.XSHG".parse().unwrap())
                .map(|p| p.quantity.to_string())
                .unwrap_or_else(|| "0".into()),
            quantity[i]
        );
    }
    assert_eq!(result.sessions[3].open_after.account.cash.to_string(), "0"); // pay date open cannot spend receivable
    assert_eq!(result.account.fills().len(), 2);
    let fills = result
        .account
        .fills()
        .iter()
        .map(|f| serde_json::to_value(f).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fills[0]["raw_price"], "10");
    assert_eq!(fills[1]["raw_price"], "4");
    assert_eq!(fills[1]["order"]["quantity"], "400");
    let ledger = serde_json::to_value(&result.corporate_action_ledger).unwrap();
    assert_eq!(
        ledger
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "dividend_entitlement",
            "dividend_receivable",
            "dividend_paid",
            "share_conversion"
        ]
    );
    assert_eq!(ledger[0]["quantity_before"], "1000");
    assert_eq!(ledger[1]["gross_amount"], "2000");
    assert_eq!(ledger[1]["tax_amount"], "200");
    assert!(
        (run.summary.as_ref().unwrap().total_return.unwrap() - (9795.0 / 10005.0 - 1.0)).abs()
            < 1e-12
    );
    // Change only retrospective research prices while preserving all above-MA
    // decisions: raw execution and the entire hand ledger must stay identical.
    let different = fixture::import_closes(
        root,
        request["dsv"].as_str().unwrap(),
        &DATES,
        &["20", "22", "24", "26", "28", "30", "32", "34", "36", "38"],
    );
    let mut changed = request.clone();
    changed["adjusted"]["510300.XSHG"] = json!(different);
    let other = execute(root, &changed);
    let other = other.runs[0].fast_event_result.as_ref().unwrap();
    assert_eq!(
        serde_json::to_value(&result.corporate_action_ledger).unwrap(),
        serde_json::to_value(&other.corporate_action_ledger).unwrap()
    );
    assert_eq!(
        other
            .sessions
            .last()
            .unwrap()
            .close
            .account
            .equity
            .to_string(),
        "9795"
    );
}

fn cli(root: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
        .args(args)
        .arg("--lake")
        .arg(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn frozen_cli_to_standard_persisted_result_and_revision_isolation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let request = setup(root);
    let mut child = Command::new(env!("CARGO_BIN_EXE_prajna-freeze-selection"))
        .arg("--lake")
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let frozen: Value = serde_json::from_slice(&output.stdout).unwrap();
    let definition = &frozen["data"]["definition"];
    let file = root.join("experiment.json");
    fs::write(&file, definition.to_string()).unwrap();
    let execution = cli(
        root,
        &[
            "run",
            file.to_str().unwrap(),
            "--level",
            "standard",
            "--threads",
            "1",
        ],
    );
    assert_eq!(execution["failed_count"], 0);
    let exp = execution["exp"].as_str().unwrap();
    let exe = execution["exe"].as_str().unwrap();
    let stored = root
        .join("experiments")
        .join(exp.trim_start_matches("exp:sha256:"))
        .join("executions")
        .join(exe.trim_start_matches("exe:sha256:"))
        .join("execution.json");
    let manifest: Value = serde_json::from_slice(&fs::read(stored).unwrap()).unwrap();
    assert_eq!(
        manifest["runs"][0]["run_spec"]["corporate_actions"]["version"],
        request["corporate_actions"]["version"]
    );
    assert_eq!(
        manifest["runs"][0]["run_spec"]["engine"]["semantic_version"],
        "fast_event@15"
    );
    let read = cli(root, &["result", exp, exe]);
    let run = &read["data"]["runs"][0];
    assert!(
        run["summary"]["corporate_action_completeness"]["is_complete"]
            .as_bool()
            .unwrap()
    );
    assert!(
        run["tables"]["saved"]
            .as_array()
            .unwrap()
            .contains(&json!("corporate_actions"))
    );
    let (_, batches) = read_run_table(
        root,
        exp,
        exe,
        run["run_id"].as_str().unwrap(),
        ResultLevel::Standard,
        "corporate_actions",
    )
    .unwrap();
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 4);
    let entries = batches
        .iter()
        .flat_map(|batch| {
            let payload = batch
                .column_by_name("payload")
                .unwrap()
                .as_any()
                .downcast_ref::<arrow_array::StringArray>()
                .unwrap();
            (0..batch.num_rows())
                .map(|i| serde_json::from_str::<Value>(payload.value(i)).unwrap())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(entries[0]["kind"], "dividend_entitlement");
    assert_eq!(entries[0]["quantity_before"], "1000");
    assert_eq!(entries[1]["kind"], "dividend_receivable");
    assert_eq!(entries[1]["gross_amount"], "2000");
    assert_eq!(entries[1]["tax_amount"], "200");
    assert_eq!(entries[1]["net_amount"], "1800");
    assert_eq!(entries[2]["kind"], "dividend_paid");
    assert_eq!(entries[3]["kind"], "share_conversion");
    assert_eq!(entries[3]["quantity_after"], "2000");
    let rows = run["series"]["account_valuations"].as_array().unwrap();
    assert!(rows.iter().any(|r| r["equity"] == "9795"));
    let mut revised = facts("510300.XSHG");
    revised["coverage"][0]["facts"][0]["cash_per_share"] = json!("1");
    let version = corporate_facts::import(root, &serde_json::to_vec(&revised).unwrap()).unwrap();
    let mut changed = request.clone();
    changed["corporate_actions"]["version"] = json!(version);
    let other = Experiment::parse(&frozen_definition(root, &changed).to_string(), root).unwrap();
    assert_ne!(other.id().unwrap(), exp);
    assert_eq!(cli(root, &["result", exp, exe]), read); // old saved result is immutable
    assert_eq!(
        cli(
            root,
            &["run", file.to_str().unwrap(), "--level", "standard"]
        )["action"],
        "replayed"
    );
}
fn frozen_definition(root: &Path, request: &Value) -> Value {
    frozen(root, request)
}

#[test]
fn four_states_missing_fields_scope_and_invalid_bindings_fail_visibly() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let request = setup(root);
    let original = frozen(root, &request);
    let legacy = {
        let mut r = request.clone();
        r.as_object_mut().unwrap().remove("corporate_actions");
        frozen(root, &r)
    };
    assert!(legacy.get("corporate_actions").is_none());
    for state in ["covered", "no_events", "missing_facts", "unknown"] {
        let mut input = facts("510300.XSHG");
        input["coverage"][0]["state"] = json!(state);
        if state != "covered" {
            input["coverage"][0]["facts"] = json!([]);
        }
        let id = corporate_facts::import(root, &serde_json::to_vec(&input).unwrap()).unwrap();
        let mut r = request.clone();
        r["corporate_actions"]["version"] = json!(id);
        let report = execute(root, &r);
        assert!(report.runs[0].error.is_none());
        assert_eq!(
            report.runs[0]
                .summary
                .as_ref()
                .unwrap()
                .corporate_action_completeness
                .as_ref()
                .unwrap()
                .is_complete,
            matches!(state, "covered" | "no_events")
        );
    }
    for field in [
        "record_date",
        "ex_date",
        "pay_date",
        "cash_per_share",
        "source",
        "available_at",
    ] {
        let mut input = facts("510300.XSHG");
        input["coverage"][0]["state"] = json!("missing_facts");
        input["coverage"][0]["facts"][0][field] = Value::Null;
        let id = corporate_facts::import(root, &serde_json::to_vec(&input).unwrap()).unwrap();
        let mut r = request.clone();
        r["corporate_actions"]["version"] = json!(id);
        let report = execute(root, &r);
        assert!(
            report.runs[0].error.is_none(),
            "{field}: {:?}",
            report.runs[0].error
        );
        let completeness = report.runs[0]
            .summary
            .as_ref()
            .unwrap()
            .corporate_action_completeness
            .as_ref()
            .unwrap();
        assert!(!completeness.is_complete);
        assert!(completeness.reasons.iter().any(|r| r.contains(field)));
    }
    for (key, value) in [
        ("version", json!("caf:sha256:missing")),
        ("dividend_tax_rate", json!("1.1")),
    ] {
        let mut invalid = original.clone();
        invalid["corporate_actions"][key] = value;
        assert!(Experiment::parse(&invalid.to_string(), root).is_err());
    }
    for (key, value) in [
        ("engine", json!("vector")),
        ("pit", json!({})),
        ("venues", json!(["XSHG", "XSHE"])),
    ] {
        let mut invalid = original.clone();
        invalid[key] = value;
        assert!(Experiment::parse(&invalid.to_string(), root).is_err());
    }
    let mut outside = facts("159952.XSHE");
    outside["coverage"][0]["facts"] = json!([]);
    outside["coverage"][0]["state"] = json!("no_events");
    let id = corporate_facts::import(root, &serde_json::to_vec(&outside).unwrap()).unwrap();
    let mut r = request.clone();
    r["corporate_actions"]["version"] = json!(id);
    assert!(
        !execute(root, &r).runs[0]
            .summary
            .as_ref()
            .unwrap()
            .corporate_action_completeness
            .as_ref()
            .unwrap()
            .is_complete
    );
}

#[test]
fn record_holder_keeps_cash_right_after_sale_and_ex_date_buyer_has_no_right() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut request = setup(root);
    request["parameter_space"]["list"][0]["rebalance_every"] = json!(1);
    for (closes, expected_right, expected_buy_date) in [
        (
            ["10", "12", "11", "10", "9", "8", "7", "6", "5", "4"],
            "1000",
            DATES[2],
        ),
        (
            ["12", "11", "12", "13", "14", "15", "16", "17", "18", "19"],
            "0",
            DATES[3],
        ),
    ] {
        request["adjusted"]["510300.XSHG"] = json!(fixture::import_closes(
            root,
            request["dsv"].as_str().unwrap(),
            &DATES,
            &closes
        ));
        let report = execute(root, &request);
        let run = &report.runs[0];
        assert!(run.error.is_none(), "{:?}", run.error);
        let result = run.fast_event_result.as_ref().unwrap();
        let entitlement = result
            .corporate_action_ledger
            .iter()
            .find(|r| serde_json::to_value(r.kind).unwrap() == "dividend_entitlement")
            .unwrap();
        assert_eq!(
            entitlement.quantity_before.unwrap().to_string(),
            expected_right
        );
        let first = serde_json::to_value(&result.account.fills()[0]).unwrap();
        assert_eq!(
            first["executed_at"],
            json!(
                result
                    .sessions
                    .iter()
                    .find(|s| s.session.session_date.to_string() == expected_buy_date)
                    .unwrap()
                    .session
                    .ts_open
            )
        );
        if expected_right == "1000" {
            assert_eq!(result.sessions[2].close.account.positions.len(), 0); // ex-date sale
            assert_eq!(
                result.sessions[2]
                    .close
                    .account
                    .dividend_receivable
                    .to_string(),
                "1800"
            );
            assert_eq!(result.sessions[3].close.account.cash.to_string(), "9795");
        } else {
            assert!(
                result
                    .sessions
                    .iter()
                    .all(|s| s.close.account.dividend_receivable.to_string() == "0")
            );
            assert!(
                result
                    .corporate_action_ledger
                    .iter()
                    .filter(|r| r.announcement_id == r#"["510300.XSHG","dividend"]"#)
                    .all(|r| r.net_amount.to_string() == "0")
            );
        }
    }
}

#[test]
fn fractional_split_reports_rounding_and_off_session_fact_remains_incomplete() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let request = setup(root);
    let mut input = facts("510300.XSHG");
    input["coverage"][0]["facts"][1]["share_ratio"] = json!("1.0005");
    let id = corporate_facts::import(root, &serde_json::to_vec(&input).unwrap()).unwrap();
    let mut r = request.clone();
    r["corporate_actions"]["version"] = json!(id);
    let report = execute(root, &r);
    let result = report.runs[0].fast_event_result.as_ref().unwrap();
    let split = result
        .corporate_action_ledger
        .iter()
        .find(|r| r.announcement_id == r#"["510300.XSHG","split"]"#)
        .unwrap();
    assert_eq!(split.theoretical_quantity.unwrap().to_string(), "1000.5");
    assert_eq!(split.quantity_after.unwrap().to_string(), "1000");
    assert_eq!(split.residual_quantity.unwrap().to_string(), "0.5");
    assert_eq!(split.corporate_action_rounding.unwrap().to_string(), "-2");
    input["coverage"][0]["facts"][1]["effective_date"] = json!("2026-01-11");
    let id = corporate_facts::import(root, &serde_json::to_vec(&input).unwrap()).unwrap();
    r["corporate_actions"]["version"] = json!(id);
    let report = execute(root, &r);
    let result = report.runs[0].fast_event_result.as_ref().unwrap();
    assert!(!result.corporate_action_completeness.is_complete);
    assert!(
        result
            .corporate_action_completeness
            .reasons
            .iter()
            .any(|r| r.contains("unsupported Session"))
    );
    // Corruption is rejected before any execution, rather than silently dropping facts.
    let version = r["corporate_actions"]["version"].as_str().unwrap();
    fs::write(
        root.join("corporate-facts").join(format!(
            "{}.json",
            version.trim_start_matches("caf:sha256:")
        )),
        b"corrupt",
    )
    .unwrap();
    assert!(
        Experiment::parse(
            &frozen(root, &request).to_string().replace(
                request["corporate_actions"]["version"].as_str().unwrap(),
                version
            ),
            root
        )
        .is_err()
    );
}

#[test]
fn either_domestic_venue_runs_with_explicit_no_event_scope() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for instrument in ["510300.XSHG", "159952.XSHE"] {
        let venue = instrument.split_once('.').unwrap().1;
        let bars = [("10", "10", "11", "9"); 10];
        let (selection, _) = opening::prepare(
            &root.join(venue),
            venue,
            InstrumentKind::Etf,
            false,
            Some((&DATES, &bars)),
        );
        let dsv = publish_daily(
            root,
            serde_json::from_value(selection).unwrap(),
            Path::new(&format!("receipt-{venue}.json")),
        )
        .unwrap()
        .dsv;
        let mut input = facts(instrument);
        input["coverage"][0]["state"] = json!("no_events");
        input["coverage"][0]["facts"] = json!([]);
        let id = corporate_facts::import(root, &serde_json::to_vec(&input).unwrap()).unwrap();
        let request = json!({"dsv":dsv,"instrument":instrument,"strategy":"buy_and_hold","corporate_actions":{"version":id,"dividend_tax_rate":"0"}});
        let report = execute(root, &request);
        assert!(report.runs[0].error.is_none(), "{:?}", report.runs[0].error);
        assert!(
            report.runs[0]
                .fast_event_result
                .as_ref()
                .unwrap()
                .corporate_action_completeness
                .is_complete
        );
        let mut old = request.clone();
        old.as_object_mut().unwrap().remove("corporate_actions");
        let legacy = frozen(root, &old);
        assert_eq!(legacy["fast_event"]["rebalance"], "entry_only");
        assert!(legacy.get("corporate_actions").is_none());
    }
}

#[test]
fn same_venue_same_event_names_keep_instrument_identity_through_saved_execution() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let definition_file = root.join("experiment.json");
    multi::prepare_same_venue(root, &definition_file);
    let mut definition: Value =
        serde_json::from_slice(&fs::read(&definition_file).unwrap()).unwrap();
    definition["costs"]["commission_rate"] = json!(0);
    let mut first = facts("510300.XSHG")["coverage"][0].clone();
    first["end"] = json!(DATES[2]);
    first["facts"].as_array_mut().unwrap().truncate(1);
    first["facts"][0]["record_date"] = json!(DATES[1]);
    first["facts"][0]["ex_date"] = json!(DATES[2]);
    first["facts"][0]["pay_date"] = json!(DATES[2]);
    first["facts"][0]["cash_per_share"] = json!("1");
    let mut second = first.clone();
    second["instrument_id"] = json!("510880.XSHG");
    second["facts"][0]["cash_per_share"] = json!("2");
    for name in ["dividend", "dividend/510300.XSHG:\"汉字"] {
        first["facts"][0]["event_id"] = json!(name);
        second["facts"][0]["event_id"] = json!(name);
        let input = json!({"schema_version":1,"observed_at":"2026-10-30T00:00:00Z","coverage":[first,second]});
        let version = corporate_facts::import(root, &serde_json::to_vec(&input).unwrap()).unwrap();
        definition["corporate_actions"] = json!({"version":version,"dividend_tax_rate":"0"});
        let experiment = Experiment::parse(&definition.to_string(), root).unwrap();
        let report = experiment.execute(root, 1).unwrap();
        assert!(report.runs[0].error.is_none(), "{:?}", report.runs[0].error);
        let result = report.runs[0].fast_event_result.as_ref().unwrap();
        assert!(result.corporate_action_completeness.is_complete);
        assert_eq!(result.corporate_action_ledger.len(), 6);
        assert_eq!(
            result
                .sessions
                .last()
                .unwrap()
                .close
                .account
                .cash
                .to_string(),
            "1500"
        );
        assert_eq!(
            result
                .sessions
                .last()
                .unwrap()
                .close
                .account
                .equity
                .to_string(),
            "11600"
        );
        let ids = result
            .corporate_action_ledger
            .iter()
            .map(|r| r.announcement_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 2);
        for entry in &result.corporate_action_ledger {
            assert_eq!(
                serde_json::from_str::<Value>(&entry.announcement_id).unwrap(),
                json!([entry.instrument_id.to_string(), name])
            );
            assert_eq!(
                entry.net_amount.to_string(),
                if entry.instrument_id.to_string() == "510300.XSHG" {
                    "500"
                } else {
                    "1000"
                }
            );
        }
        fs::write(&definition_file, definition.to_string()).unwrap();
        let execution = cli(
            root,
            &[
                "run",
                definition_file.to_str().unwrap(),
                "--level",
                "standard",
            ],
        );
        assert_eq!(execution["failed_count"], 0);
        let read = cli(
            root,
            &[
                "result",
                execution["exp"].as_str().unwrap(),
                execution["exe"].as_str().unwrap(),
            ],
        );
        let run = &read["data"]["runs"][0];
        let (_, batches) = read_run_table(
            root,
            execution["exp"].as_str().unwrap(),
            execution["exe"].as_str().unwrap(),
            run["run_id"].as_str().unwrap(),
            ResultLevel::Standard,
            "corporate_actions",
        )
        .unwrap();
        assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 6);
        // Same-security duplicate event names remain invalid at the source boundary.
        let mut duplicate = input.clone();
        let fact = duplicate["coverage"][0]["facts"][0].clone();
        duplicate["coverage"][0]["facts"]
            .as_array_mut()
            .unwrap()
            .push(fact);
        assert!(corporate_facts::import(root, &serde_json::to_vec(&duplicate).unwrap()).is_err());
    }
}

fn holding_definition(root: &Path) -> Value {
    let mut definition = frozen(root, &setup(root));
    definition["strategy"] = json!("buy_and_hold");
    definition["unfilled_entry"] = json!("retry");
    definition["parameter_space"] = json!({"list":[{"params_version":1}]});
    definition["holding_benchmark"] = json!({"semantic_version":"holding-benchmark@1","kind":"single_etf",
        "reinvestment_calendar":{"semantic_version":"session-close-calendar@1","decision_dates":[DATES[3],DATES[4],DATES[9]]}});
    definition
}
#[test]
fn holding_benchmark_hand_ledger_and_formal_cli_standard_roundtrip() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let definition = holding_definition(root);
    let experiment = Experiment::parse(&definition.to_string(), root).unwrap();
    let report = experiment.execute(root, 1).unwrap();
    assert!(report.runs[0].error.is_none(), "{:?}", report.runs[0].error);
    let result = report.runs[0].fast_event_result.as_ref().unwrap();
    assert_eq!(result.semantic_version, "fast_event@16");
    // Evaluation starts day1 (day0 warm-up). Entry at day2 open; ex-date
    // creates 1,800 receivable. Day4 open cannot spend close's payment.
    // Declared day4 close buys 200 at day5 open, fee5, residual195.
    // Day6 split doubles 1,200 -> 2,400 with no trade/fee.
    let cash = ["10005", "0", "0", "1800", "195", "195", "195", "195", "195"];
    let receivable = ["0", "0", "1800", "0", "0", "0", "0", "0", "0"];
    let quantities = [
        "0", "1000", "1000", "1000", "1200", "2400", "2400", "2400", "2400",
    ];
    let equity = [
        "10005", "10000", "9800", "9800", "9795", "9795", "9795", "9795", "9795",
    ];
    for (i, session) in result.sessions.iter().enumerate() {
        session.open_after.validate().unwrap();
        session.close.validate().unwrap();
        let account = &session.close.account;
        assert_eq!(account.cash.to_string(), cash[i]);
        assert_eq!(account.dividend_receivable.to_string(), receivable[i]);
        assert_eq!(account.equity.to_string(), equity[i]);
        assert_eq!(
            account
                .positions
                .values()
                .next()
                .map(|p| p.quantity.to_string())
                .unwrap_or_else(|| "0".into()),
            quantities[i]
        );
    }
    assert_eq!(result.fills.len(), 2);
    assert_eq!(
        result.fills[0].fill.order().submitted_at(),
        result.sessions[1].session.ts_open
    );
    assert_eq!(
        result.fills[1].fill.executed_at(),
        result.sessions[4].session.ts_open
    );
    assert!(result.attempts.iter().any(|a| matches!(
        a.event,
        prajna_research::strategy::ExecutionEvent::PendingAtEnd { .. }
    )));
    assert_eq!(
        report.runs[0].summary.as_ref().unwrap().total_return,
        Some(9795.0 / 10005.0 - 1.0)
    );
    let file = root.join("holding.json");
    fs::write(&file, definition.to_string()).unwrap();
    let stored = cli(
        root,
        &[
            "run",
            file.to_str().unwrap(),
            "--level",
            "standard",
            "--threads",
            "1",
        ],
    );
    assert_eq!(stored["failed_count"], 0);
    let exp = stored["exp"].as_str().unwrap();
    let exe = stored["exe"].as_str().unwrap();
    let read = cli(root, &["result", exp, exe]);
    let run = &read["data"]["runs"][0];
    for table in [
        "account_valuations",
        "portfolio_valuations",
        "orders",
        "fills",
        "fee_settlements",
        "corporate_actions",
    ] {
        let (_, batches) = read_run_table(
            root,
            exp,
            exe,
            run["run_id"].as_str().unwrap(),
            ResultLevel::Standard,
            table,
        )
        .unwrap();
        if table != "fee_settlements" {
            assert!(!batches.is_empty());
        }
        if table == "orders" || table == "account_valuations" {
            let values = batches
                .iter()
                .flat_map(|b| {
                    b.column(1)
                        .as_any()
                        .downcast_ref::<arrow_array::StringArray>()
                        .unwrap()
                        .iter()
                        .flatten()
                        .map(|s| serde_json::from_str::<Value>(s).unwrap())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            if table == "account_valuations" {
                let closes = values
                    .iter()
                    .filter(|v| v["valuation_point"] == "close")
                    .collect::<Vec<_>>();
                assert_eq!(closes.len(), equity.len());
                for (i, row) in closes.iter().enumerate() {
                    assert_eq!(row["cash"], cash[i]);
                    assert_eq!(row["dividend_receivable"], receivable[i]);
                    assert_eq!(row["equity"], equity[i]);
                    assert_eq!(row["session_date"], DATES[i + 1]);
                }
                continue;
            }
            assert!(values.iter().any(|v| v["purpose"] == "initial_entry"));
            assert!(
                values
                    .iter()
                    .any(|v| v["purpose"] == "calendar_cash_reinvestment"
                        && v["kind"] == "pending_at_end")
            );
        }
    }
    let spec = &experiment.run_definitions(root).unwrap()[0].run_spec;
    assert_eq!(
        spec["holding_benchmark"]["execution_rules"][0]["price_increment"],
        "0.001"
    );
    assert_eq!(
        spec["holding_benchmark"]["execution_rules"][0]["lot_size"],
        "100"
    );
    assert_eq!(spec["engine"]["semantic_version"], "fast_event@16");
}
#[test]
fn holding_binding_validation_versions_identity_and_incomplete_facts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let definition = holding_definition(root);
    let id = |value: &Value| {
        Experiment::parse(&value.to_string(), root)
            .unwrap()
            .run_definitions(root)
            .unwrap()[0]
            .run_id
            .clone()
    };
    let benchmark_id = id(&definition);
    let mut old = definition.clone();
    old.as_object_mut().unwrap().remove("holding_benchmark");
    let old_id = id(&old);
    assert_ne!(benchmark_id, old_id);
    assert_eq!(
        Experiment::parse(&old.to_string(), root)
            .unwrap()
            .run_definitions(root)
            .unwrap()[0]
            .run_spec["engine"]["semantic_version"],
        "fast_event@15"
    );
    let mut dates = definition.clone();
    dates["holding_benchmark"]["reinvestment_calendar"]["decision_dates"] = json!([]);
    assert_ne!(id(&dates), benchmark_id);
    assert_eq!(id(&old), old_id);
    for (key, value) in [
        ("corporate_actions", Value::Null),
        ("evaluation", Value::Null),
        ("engine", json!("vector")),
        ("strategy", json!("single_etf_timing")),
    ] {
        let mut bad = definition.clone();
        bad[key] = value;
        assert!(Experiment::parse(&bad.to_string(), root).is_err(), "{key}");
    }
    for invalid_dates in [
        json!([DATES[4], DATES[3]]),
        json!([DATES[4], DATES[4]]),
        json!([DATES[0]]),
        json!(["2026-01-11"]),
        json!(["2027-01-01"]),
    ] {
        let mut bad = definition.clone();
        bad["holding_benchmark"]["reinvestment_calendar"]["decision_dates"] = invalid_dates;
        assert!(Experiment::parse(&bad.to_string(), root).is_err());
    }
    let mut bad_lot = definition.clone();
    bad_lot["fast_event"]["lot_size_source"] =
        json!({"kind":"explicit","values":{"510300.XSHG":"3"}});
    assert!(Experiment::parse(&bad_lot.to_string(), root).is_err());
    for path in ["semantic_version", "kind"] {
        let mut bad = definition.clone();
        bad["holding_benchmark"][path] = json!("unsupported");
        assert!(Experiment::parse(&bad.to_string(), root).is_err());
    }
    let mut input = facts("510300.XSHG");
    input["coverage"][0]["facts"][0]["pay_date"] = Value::Null;
    input["coverage"][0]["state"] = json!("missing_facts");
    let version = corporate_facts::import(root, &serde_json::to_vec(&input).unwrap()).unwrap();
    let mut incomplete = definition;
    incomplete["corporate_actions"]["version"] = json!(version);
    let report = Experiment::parse(&incomplete.to_string(), root)
        .unwrap()
        .execute(root, 1)
        .unwrap();
    assert!(
        !report.runs[0]
            .fast_event_result
            .as_ref()
            .unwrap()
            .corporate_action_completeness
            .is_complete
    );
}

#[test]
fn holding_pool_formal_definition_and_nonzero_execution_costs() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let file = root.join("pool.json");
    multi::prepare_same_venue(root, &file);
    let mut definition: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let (dsv, _) = fixture::prepare_executable_multi_venue(root, true, None);
    definition["dsv"] = json!(dsv);
    let members = ["510300.XSHG", "510880.XSHG"];
    let coverage = members.map(|id| json!({"instrument_id":id,"kind":"etf","start":DATES[0],"end":DATES[2],"state":"no_events","reason":"synthetic","source":source(),"available_at":"2026-01-01T00:00:00Z","facts":[]}));
    let version = corporate_facts::import(
        root,
        &serde_json::to_vec(
            &json!({"schema_version":1,"observed_at":"2026-10-30T00:00:00Z","coverage":coverage}),
        )
        .unwrap(),
    )
    .unwrap();
    definition["corporate_actions"] = json!({"version":version,"dividend_tax_rate":"0"});
    definition["adjusted_versions"] = members
        .iter()
        .map(|id| {
            (
                (*id).to_string(),
                json!(fixture::import_adjusted(
                    root,
                    definition["dsv"].as_str().unwrap(),
                    id,
                    DATES[0],
                    DATES[2],
                    &DATES[..3]
                )),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    definition["evaluation"] = json!({"semantic_version":"common-window@1","common_start":DATES[0],"common_end":DATES[2],"start":DATES[0],"end":DATES[2],"warmup_sessions":0});
    definition["costs"]["commission_rate"] = json!(0);
    definition["fast_event"]["minimum_commission"] = json!("5");
    definition["holding_benchmark"] = json!({"semantic_version":"holding-benchmark@1","kind":"equal_weight_pool","reinvestment_calendar":{"semantic_version":"session-close-calendar@1","decision_dates":[]}});
    let report = Experiment::parse(&definition.to_string(), root)
        .unwrap()
        .execute(root, 1)
        .unwrap();
    let result = report.runs[0].fast_event_result.as_ref().unwrap();
    assert_eq!(result.fills.len(), 2);
    assert_eq!(result.sessions[1].close.account.cash.to_string(), "90");
    assert_eq!(result.sessions[1].close.account.equity.to_string(), "9990");
    let quantities = result.sessions[1]
        .close
        .account
        .positions
        .values()
        .map(|p| p.quantity.to_string())
        .collect::<Vec<_>>();
    assert_eq!(quantities, vec!["500", "490"]);
    fs::write(&file, definition.to_string()).unwrap();
    let stored = cli(
        root,
        &[
            "run",
            file.to_str().unwrap(),
            "--level",
            "standard",
            "--threads",
            "1",
        ],
    );
    assert_eq!(stored["failed_count"], 0);
    let read = cli(
        root,
        &[
            "result",
            stored["exp"].as_str().unwrap(),
            stored["exe"].as_str().unwrap(),
        ],
    );
    assert!(
        read["data"]["runs"][0]["tables"]["saved"]
            .as_array()
            .unwrap()
            .contains(&json!("fills"))
    );
    definition["holding_benchmark"]["kind"] = json!("single_etf");
    assert!(Experiment::parse(&definition.to_string(), root).is_err());
    let second = tempfile::tempdir().unwrap();
    let mut definition = holding_definition(second.path());
    definition["costs"]["commission_rate"] = json!(0.01);
    definition["costs"]["buy_slippage_bps"] = json!(100);
    definition["costs"]["buy_tax_rate"] = json!(0.01);
    let report = Experiment::parse(&definition.to_string(), second.path())
        .unwrap()
        .execute(second.path(), 1)
        .unwrap();
    let result = report.runs[0].fast_event_result.as_ref().unwrap();
    assert_eq!(result.fills[0].fill.order().quantity().to_string(), "900");
    assert_eq!(result.fills[0].fill.price().to_string(), "10.1");
    assert_eq!(result.sessions[1].close.account.cash.to_string(), "733.2");
    assert_eq!(
        result.sessions[1].close.account.equity.to_string(),
        "9733.2"
    );
    definition["costs"]["commission_rate"] = json!(0);
    definition["costs"]["buy_tax_rate"] = json!(0);
    definition["costs"]["buy_slippage_bps"] = json!(0.1);
    let report = Experiment::parse(&definition.to_string(), second.path())
        .unwrap()
        .execute(second.path(), 1)
        .unwrap();
    let result = report.runs[0].fast_event_result.as_ref().unwrap();
    assert_eq!(result.fills[0].fill.price().to_string(), "10.001");
    assert_eq!(result.sessions[1].close.account.cash.to_string(), "999.1");
}

// --- #375 frozen strategy-benchmark comparison -----------------------------

use prajna_experiment::{
    comparison::{self, ReinvestmentPolicy},
    pools::{self, Command as PoolCommand},
    selection::Settings,
};

fn selection_of(request: &Value) -> Selection {
    serde_json::from_value(request.clone()).unwrap()
}

fn freeze_comparison_cli(root: &Path, selection: &Selection, policy: ReinvestmentPolicy) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_prajna-freeze-comparison"))
        .arg("--lake")
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            serde_json::to_vec(&json!({"selection": selection, "reinvestment_calendar": policy}))
                .unwrap()
                .as_slice(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn reject_comparison(root: &Path, request: &Value) -> &'static str {
    comparison::freeze(
        root,
        &comparison::Request {
            selection: selection_of(request),
            reinvestment_calendar: ReinvestmentPolicy::StrategyRebalance,
        },
    )
    .unwrap_err()
    .code
}

fn run_definition(root: &Path, name: &str, definition: &Value) -> Value {
    let file = root.join(name);
    fs::write(&file, definition.to_string()).unwrap();
    let execution = cli(
        root,
        &[
            "run",
            file.to_str().unwrap(),
            "--level",
            "standard",
            "--threads",
            "1",
        ],
    );
    assert_eq!(execution["failed_count"], 0, "{execution}");
    execution
}

fn read_result(root: &Path, execution: &Value) -> Value {
    cli(
        root,
        &[
            "result",
            execution["exp"].as_str().unwrap(),
            execution["exe"].as_str().unwrap(),
        ],
    )
}

fn close_rows(result: &Value) -> Vec<Value> {
    result["data"]["runs"][0]["series"]["account_valuations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["valuation_point"] == "close")
        .cloned()
        .collect()
}

fn single_position(row: &Value) -> &str {
    row["positions"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .and_then(|position| position["quantity"].as_str())
        .unwrap_or("0")
}

#[test]
fn frozen_single_etf_comparison_runs_reads_and_keeps_first_valuation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let request = setup(root);
    let selection = selection_of(&request);
    let frozen = freeze_comparison_cli(root, &selection, ReinvestmentPolicy::StrategyRebalance);
    let data = &frozen["data"];
    assert_eq!(data["semantic_version"], "strategy-benchmark-comparison@1");
    assert_eq!(data["benchmark"]["kind"], "single_etf");
    assert_eq!(data["comparability"]["initial_cash"], "10005");
    assert_eq!(data["comparability"]["evaluation"]["start"], DATES[1]);
    assert_eq!(data["comparability"]["evaluation"]["end"], DATES[9]);
    assert_eq!(
        data["comparability"]["reinvestment_calendar"]["decision_dates"],
        json!([DATES[3], DATES[5], DATES[7], DATES[9]])
    );
    let checks = data["comparability"]["checks"].as_array().unwrap();
    for field in [
        "dsv",
        "universe",
        "costs",
        "adjusted_versions",
        "evaluation",
        "corporate_actions",
    ] {
        assert!(checks.iter().any(|value| value == field), "{field}");
    }

    let strategy_run = run_definition(
        root,
        "comparison-strategy.json",
        &data["strategy"]["definition"],
    );
    let benchmark_run = run_definition(
        root,
        "comparison-benchmark.json",
        &data["benchmark"]["definition"],
    );
    let strategy = read_result(root, &strategy_run);
    let benchmark = read_result(root, &benchmark_run);
    assert_eq!(benchmark_run["exp"], data["benchmark"]["experiment"]);
    assert_eq!(
        benchmark["data"]["runs"][0]["run_id"],
        data["benchmark"]["run"]
    );

    let rows = close_rows(&benchmark);
    assert_eq!(rows.len(), 9);
    let expected = [
        ("10005", "0", "10005", "0"),
        ("0", "1000", "10000", "0"),
        ("0", "1000", "9800", "1800"),
        ("1800", "1000", "9800", "0"),
        ("1800", "1000", "9800", "0"),
        ("195", "2400", "9795", "0"),
        ("195", "2400", "9795", "0"),
        ("195", "2400", "9795", "0"),
        ("195", "2400", "9795", "0"),
    ];
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row["session_date"], DATES[index + 1]);
        assert_eq!(row["cash"], expected[index].0);
        assert_eq!(single_position(row), expected[index].1);
        assert_eq!(row["equity"], expected[index].2);
        assert_eq!(row["dividend_receivable"], expected[index].3);
    }
    let summary = &benchmark["data"]["runs"][0]["summary"];
    assert_eq!(benchmark["data"]["runs"][0]["accounting_verified"], true);
    assert_eq!(
        summary["total_return"].as_f64().unwrap(),
        9795.0 / 10005.0 - 1.0
    );
    assert_eq!(
        summary["corporate_action_completeness"]["is_complete"],
        true
    );
    assert_eq!(close_rows(&strategy).len(), rows.len());

    let no_reinvestment = freeze_comparison_cli(root, &selection, ReinvestmentPolicy::None);
    assert_eq!(
        no_reinvestment["data"]["comparability"]["reinvestment_calendar"]["decision_dates"],
        json!([])
    );
    assert_ne!(
        no_reinvestment["data"]["benchmark"]["run"],
        data["benchmark"]["run"]
    );
}

#[test]
fn comparison_rejects_unsupported_or_incomplete_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let request = setup(root);
    let mut no_actions = request.clone();
    no_actions
        .as_object_mut()
        .unwrap()
        .remove("corporate_actions");
    assert_eq!(
        reject_comparison(root, &no_actions),
        "comparison_requires_corporate_actions"
    );
    let mut multiple = request.clone();
    let first = multiple["parameter_space"]["list"][0].clone();
    let mut second = first.clone();
    second["ma_window"] = json!(3);
    multiple["parameter_space"]["list"] = json!([first, second]);
    assert_eq!(
        reject_comparison(root, &multiple),
        "comparison_requires_single_run"
    );
    let mut unsupported = request;
    unsupported["strategy"] = json!("ma_crossover");
    unsupported["parameter_space"] =
        json!({"list":[{"params_version":1,"short_window":2,"long_window":3}]});
    assert_eq!(
        reject_comparison(root, &unsupported),
        "comparison_unsupported_strategy"
    );
}

#[test]
fn frozen_equal_weight_comparison_uses_actual_lots_and_marks_incomplete_facts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let dsv = fixture::prepare_executable_days(root, true, &DATES);
    let members = ["510300.XSHG", "510880.XSHG"];
    let adjusted = members
        .iter()
        .map(|member| {
            (
                (*member).to_string(),
                fixture::import_adjusted(root, &dsv, member, DATES[0], DATES[9], &DATES),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let version =
        corporate_facts::import(root, &serde_json::to_vec(&facts("510300.XSHG")).unwrap()).unwrap();
    let registry = pools::command(root, PoolCommand::List {}).unwrap();
    let draft = serde_json::from_value(
        registry["data"]["pools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pool| pool["id"] == "dividend")
            .unwrap()["draft"]
            .clone(),
    )
    .unwrap();
    let expected_members: Vec<String> = members.iter().map(|member| (*member).into()).collect();
    pools::command(
        root,
        PoolCommand::Save {
            id: "dividend".into(),
            members: expected_members.clone(),
            expected: draft,
        },
    )
    .unwrap();
    let published = pools::command(
        root,
        PoolCommand::Publish {
            id: "dividend".into(),
            expected: expected_members,
        },
    )
    .unwrap();
    let pool_version = published["data"]["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pool| pool["id"] == "dividend")
        .unwrap()["versions"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["version"]
        .as_u64()
        .unwrap() as usize;
    let selection = Selection {
        dsv,
        instrument: String::new(),
        pool: Some(pools::Reference {
            id: "dividend".into(),
            version: pool_version,
        }),
        strategy: "s2_rotation".into(),
        parameter_space: Some(
            json!({"list":[{"short":1,"long":2,"vol":2,"w_s":0.5,"w_l":0.5,"w_v":0.0,"trend":null,"top_k":1,"rebalance_every":1}]}),
        ),
        settings: Settings {
            initial_cash: "10000".into(),
            commission_rate: 0.0,
            minimum_commission: "5".into(),
            buy_slippage_bps: 0.0,
            sell_slippage_bps: 0.0,
            buy_tax_rate: 0.0,
            sell_tax_rate: 0.0,
        },
        adjusted,
        corporate_actions: Some(prajna_experiment::actions::Binding {
            version,
            dividend_tax_rate: "0.1".into(),
        }),
    };
    let frozen = freeze_comparison_cli(root, &selection, ReinvestmentPolicy::StrategyRebalance);
    let data = &frozen["data"];
    assert_eq!(data["benchmark"]["kind"], "equal_weight_pool");
    assert_eq!(
        data["comparability"]["reinvestment_calendar"]["decision_dates"],
        json!([
            DATES[3], DATES[4], DATES[5], DATES[6], DATES[7], DATES[8], DATES[9]
        ])
    );
    let strategy_run = run_definition(root, "pool-strategy.json", &data["strategy"]["definition"]);
    let benchmark_run = run_definition(
        root,
        "pool-benchmark.json",
        &data["benchmark"]["definition"],
    );
    let benchmark = read_result(root, &benchmark_run);
    let rows = close_rows(&benchmark);
    assert_eq!(rows.len(), 8);
    assert_eq!(rows[0]["session_date"], DATES[2]);
    assert_eq!(rows[0]["equity"], "10000");
    assert_eq!(rows[1]["session_date"], DATES[3]);
    assert_eq!(rows[1]["cash"], "90");
    assert_eq!(rows[1]["positions"]["510300.XSHG"]["quantity"], "500");
    assert_eq!(rows[1]["positions"]["510880.XSHG"]["quantity"], "490");
    assert_eq!(rows[1]["equity"], "9990");
    let summary = &benchmark["data"]["runs"][0]["summary"];
    assert_eq!(
        summary["corporate_action_completeness"]["is_complete"],
        false
    );
    assert!(
        summary["corporate_action_completeness"]["reasons"]
            .as_array()
            .is_some_and(|reasons| !reasons.is_empty())
    );
    assert_eq!(strategy_run["failed_count"], 0);
    assert_eq!(benchmark["data"]["runs"][0]["accounting_verified"], true);
}
