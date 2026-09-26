use serde_json::Value;
use std::{fs, path::PathBuf, process::Command};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> PathBuf {
    root().join("poc/poc0-benchmark/fixtures").join(name)
}

fn run(expected: &std::path::Path, output: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_quant-research"))
        .current_dir(root())
        .args([
            "benchmark-poc0",
            "--dataset",
            fixture("dataset-v1.json").to_str().unwrap(),
            "--expected",
            expected.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap()
}

#[test]
fn benchmark_cli_emits_reproducible_report_after_golden_passes() {
    let dir = tempfile::tempdir().unwrap();
    let report_path = dir.path().join("report.json");
    let result = run(&fixture("expected-v1.json"), &report_path);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["correctness"]["status"], "passed");
    for field in ["differences", "accounting_checks", "time_checks"] {
        assert!(report["correctness"][field].as_array().unwrap().is_empty());
    }
    assert_eq!(report["dataset"]["version"], "poc0.synthetic.etf-daily.v1");
    assert_eq!(report["dataset"]["instrument_count"], 3);
    assert_eq!(report["dataset"]["bar_count"], 29);
    assert_eq!(
        report["dataset"]["content_sha256"],
        "8c16742a2031cab19e08456dbbe009769b02f0331276618831cefdb6827b5a6f"
    );
    assert_eq!(report["strategy"]["version"], "poc0-golden-v1");
    assert_eq!(
        report["strategy"]["future_returns_used_for_decisions"],
        false
    );
    assert_eq!(
        report["result"]["projection"]["summary"]["final_equity"],
        102130.0
    );
    assert_eq!(
        report["result"]["projection"]["fills"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        report["result"]["projection"]["ledger"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    assert_eq!(
        report["result"]["projection"]["unexecuted_orders"][2]["reason"],
        "no_future_execution_session"
    );
    assert_eq!(report["measurements"]["status"], "measured");
    assert_eq!(
        report["measurements"]["raw_samples_ns"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        report["measurements"]["resource_measurement_status"],
        "not_measured_in_ticket_01"
    );
    assert_eq!(report["provenance"]["build_profile"], "debug");
    assert_eq!(
        report["result"]["checksum_sha256"].as_str().unwrap().len(),
        64
    );
}

#[test]
fn benchmark_cli_skips_timing_when_the_independent_golden_mismatches() {
    let dir = tempfile::tempdir().unwrap();
    let expected_path = dir.path().join("wrong-expected.json");
    let mut expected: Value =
        serde_json::from_slice(&fs::read(fixture("expected-v1.json")).unwrap()).unwrap();
    expected["projection"]["summary"]["final_equity"] = Value::from(1.0);
    fs::write(
        &expected_path,
        serde_json::to_vec_pretty(&expected).unwrap(),
    )
    .unwrap();
    let report_path = dir.path().join("failed-report.json");
    let result = run(&expected_path, &report_path);
    assert_eq!(result.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["correctness"]["status"], "failed");
    assert!(
        !report["correctness"]["differences"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        report["measurements"]["status"],
        "skipped_correctness_failure"
    );
    assert!(
        report["measurements"]["raw_samples_ns"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn fast_event_buy_and_hold_has_an_independent_l1_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let report_path = dir.path().join("report.json");
    let result = run(&fixture("expected-v1.json"), &report_path);
    assert!(result.status.success());
    let report: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    let event = &report["b2_fast_event_buy_hold"];
    assert_eq!(event["status"], "correctness_passed_and_measured");
    assert!(event["correctness_checks"].as_array().unwrap().is_empty());
    assert_eq!(event["projection"]["fills"].as_array().unwrap().len(), 3);
    assert_eq!(event["projection"]["fills"][0]["date"], "2026-01-13");
    assert_eq!(event["projection"]["fills"][2]["symbol"], "B");
    assert_eq!(event["projection"]["fills"][2]["date"], "2026-01-14");
    assert_eq!(event["projection"]["summary"]["total_cost"], 390.0);
    assert_eq!(event["projection"]["summary"]["final_equity"], 101710.0);
    let rejected = event["projection"]["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|order| order["symbol"] == "B" && order["attempt_date"] == "2026-01-13")
        .unwrap();
    assert_eq!(rejected["reason"], "HALTED");
    let expected_cash = [
        100000.0, 100000.0, 100000.0, 100000.0, 100000.0, 100000.0, 39740.0, 9610.0, 9610.0, 9610.0,
    ];
    let expected_nav = [
        100000.0, 100000.0, 100000.0, 100000.0, 100000.0, 100000.0, 100640.0, 101110.0, 100810.0,
        101710.0,
    ];
    let ledger = event["projection"]["ledger"].as_array().unwrap();
    for ((row, cash), nav) in ledger.iter().zip(expected_cash).zip(expected_nav) {
        assert!((row["cash"].as_f64().unwrap() - cash).abs() < 1e-8);
        assert!((row["nav"].as_f64().unwrap() - nav).abs() < 1e-8);
    }
    let last_b = ledger[9]["holdings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|holding| holding["symbol"] == "B")
        .unwrap();
    assert_eq!(last_b["mark_price"], 99.0);
    for phase in [
        "raw_samples_ns",
        "initialization_samples_ns",
        "event_processing_samples_ns",
        "end_to_end_samples_ns",
    ] {
        assert_eq!(event[phase].as_array().unwrap().len(), 5);
    }
    let second_path = dir.path().join("second.json");
    assert!(
        run(&fixture("expected-v1.json"), &second_path)
            .status
            .success()
    );
    let second: Value = serde_json::from_slice(&fs::read(second_path).unwrap()).unwrap();
    assert_eq!(
        event["checksum_sha256"],
        second["b2_fast_event_buy_hold"]["checksum_sha256"]
    );
}

#[test]
fn fast_event_keeps_unfilled_target_and_buy_tax_traceable() {
    let dir = tempfile::tempdir().unwrap();
    let dataset_path = dir.path().join("pending-dataset.json");
    let mut dataset: Value =
        serde_json::from_slice(&fs::read(fixture("dataset-v1.json")).unwrap()).unwrap();
    dataset["costs"]["buy_tax_rate"] = Value::from(0.02);
    for date in ["2026-01-13", "2026-01-14", "2026-01-15", "2026-01-16"] {
        dataset["missing_bars"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "symbol": "A", "date": date, "reason": "no_open_before_dataset_end"
            }));
    }
    fs::write(&dataset_path, serde_json::to_vec_pretty(&dataset).unwrap()).unwrap();
    let report_path = dir.path().join("pending-report.json");
    let output = Command::new(env!("CARGO_BIN_EXE_quant-research"))
        .current_dir(root())
        .args([
            "benchmark-poc0",
            "--dataset",
            dataset_path.to_str().unwrap(),
            "--expected",
            fixture("expected-v1.json").to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "changed input must fail the S2 golden"
    );
    let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    let event = &report["b2_fast_event_buy_hold"];
    assert_eq!(event["status"], "correctness_passed_and_measured");
    assert_eq!(event["projection"]["unexecuted_targets"][0]["symbol"], "A");
    assert_eq!(
        event["projection"]["unexecuted_targets"][0]["reason"],
        "no_successful_open_fill_before_dataset_end"
    );
    assert!((event["projection"]["summary"]["tax"].as_f64().unwrap() - 1201.2).abs() < 1e-8);
    assert!(
        event["projection"]["ledger"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["cash"].as_f64().unwrap() >= 0.0)
    );
}
