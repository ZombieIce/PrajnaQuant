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
