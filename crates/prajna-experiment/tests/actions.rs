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
                    .filter(|r| r.announcement_id == "dividend")
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
        .find(|r| r.announcement_id == "split")
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
