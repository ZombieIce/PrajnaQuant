use serde_json::Value;
use std::{fs, path::PathBuf, process::Command};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> PathBuf {
    repo_root().join("poc/poc0-benchmark/fixtures").join(name)
}

fn run_benchmark(expected: &std::path::Path, output: &std::path::Path) -> std::process::Output {
    run_benchmark_for_dataset(&fixture("dataset-v1.json"), expected, output)
}

fn run_benchmark_for_dataset(
    dataset: &std::path::Path,
    expected: &std::path::Path,
    output: &std::path::Path,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_quant-research"))
        .current_dir(repo_root())
        .args([
            "benchmark-poc0",
            "--candidate",
            "soa",
            "--dataset",
            dataset.to_str().unwrap(),
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
    let output_dir = tempfile::tempdir().unwrap();
    let report_path = output_dir.path().join("report.json");
    let result = run_benchmark(&fixture("expected-v1.json"), &report_path);

    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(
        report["b2_fast_event_buy_hold"]["status"],
        "correctness_passed_and_measured"
    );
    assert!(
        report["b2_fast_event_buy_hold"]["correctness_checks"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        report["b2_fast_event_buy_hold"]["projection"]["fills"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        report["b2_fast_event_buy_hold"]["projection"]["orders"]
            .as_array()
            .unwrap()
            .iter()
            .find(|order| order["symbol"] == "B" && order["attempt_date"] == "2026-01-13")
            .unwrap()["reason"],
        "HALTED"
    );
    assert_eq!(
        report["b2_fast_event_buy_hold"]["projection"]["ledger"][9]["holdings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|holding| holding["symbol"] == "B")
            .unwrap()["mark_price"],
        99.0
    );
    assert_eq!(
        report["b2_fast_event_buy_hold"]["raw_samples_ns"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    for timing_phase in [
        "initialization_samples_ns",
        "event_processing_samples_ns",
        "end_to_end_samples_ns",
    ] {
        assert_eq!(
            report["b2_fast_event_buy_hold"][timing_phase]
                .as_array()
                .unwrap()
                .len(),
            5
        );
    }
    assert!(
        (report["b2_fast_event_buy_hold"]["projection"]["summary"]["total_cost"]
            .as_f64()
            .unwrap()
            - 390.0)
            .abs()
            < 1e-8
    );
    assert!(
        (report["b2_fast_event_buy_hold"]["projection"]["summary"]["final_equity"]
            .as_f64()
            .unwrap()
            - 101710.0)
            .abs()
            < 1e-8
    );
    let b2_ledger = report["b2_fast_event_buy_hold"]["projection"]["ledger"]
        .as_array()
        .unwrap();
    let expected_b2_cash = [
        100000.0, 100000.0, 100000.0, 100000.0, 100000.0, 100000.0, 39740.0, 9610.0, 9610.0, 9610.0,
    ];
    let expected_b2_nav = [
        100000.0, 100000.0, 100000.0, 100000.0, 100000.0, 100000.0, 100640.0, 101110.0, 100810.0,
        101710.0,
    ];
    assert_eq!(b2_ledger.len(), expected_b2_nav.len());
    for ((row, expected_cash), expected_nav) in
        b2_ledger.iter().zip(expected_b2_cash).zip(expected_b2_nav)
    {
        assert!((row["cash"].as_f64().unwrap() - expected_cash).abs() < 1e-8);
        assert!((row["nav"].as_f64().unwrap() - expected_nav).abs() < 1e-8);
    }
    let b2_fills = report["b2_fast_event_buy_hold"]["projection"]["fills"]
        .as_array()
        .unwrap();
    assert_eq!(b2_fills[0]["date"], "2026-01-13");
    assert_eq!(b2_fills[0]["quantity"], 300);
    assert_eq!(b2_fills[0]["fill_price"], 100.1);
    assert_eq!(b2_fills[2]["date"], "2026-01-14");
    assert_eq!(b2_fills[2]["symbol"], "B");
    let second_report_path = output_dir.path().join("report-second.json");
    let second_result = run_benchmark(&fixture("expected-v1.json"), &second_report_path);
    assert!(second_result.status.success());
    let second_report: Value =
        serde_json::from_slice(&fs::read(second_report_path).unwrap()).unwrap();
    assert_eq!(
        report["b2_fast_event_buy_hold"]["checksum_sha256"],
        second_report["b2_fast_event_buy_hold"]["checksum_sha256"]
    );
    assert_eq!(report["correctness"]["status"], "passed");
    assert_eq!(report["dataset"]["version"], "poc0.synthetic.etf-daily.v1");
    assert_eq!(report["dataset"]["instrument_count"], 3);
    assert_eq!(report["dataset"]["bar_count"], 29);
    assert_eq!(report["candidate"], "custom-soa-momentum-rotation");
    assert_eq!(report["measurements"]["status"], "not_selected");
    assert_eq!(
        report["measurements"]["raw_samples_ns"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        report["result"]["projection"]["fills"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        report["result"]["projection"]["summary"]["final_equity"],
        102_130.0
    );
    assert!(report["dataset"]["content_sha256"].as_str().unwrap().len() == 64);
    assert_eq!(report["b1_soa"]["correctness_status"], "passed");
    assert_eq!(report["b1_soa"]["warmup_runs"], 1);
    assert_eq!(
        report["b1_soa"]["raw_samples_ns"].as_array().unwrap().len(),
        5
    );
    for phase in [
        "factor_ns",
        "ranking_topk_weights_ns",
        "return_projection_ns",
    ] {
        assert_eq!(
            report["b1_soa"]["phase_samples_ns"][phase]
                .as_array()
                .unwrap()
                .len(),
            5
        );
    }
    assert_eq!(
        report["b1_soa"]["projection"]["target_weights"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    assert_eq!(
        report["b1_soa"]["projection"]["target_weights"][0]["weight"],
        1.0
    );
    // Independent hand calculation: C moves 101 -> 100 from Jan 7 to Jan 8.
    let first_return = report["b1_soa"]["projection"]["portfolio_returns"][0]["return_pct"]
        .as_f64()
        .unwrap();
    assert!((first_return - (100.0 / 101.0 - 1.0)).abs() < 1e-12);
    let expected_returns = [
        102.0 / 99.0 - 1.0,
        103.0 / 102.0 - 1.0,
        103.0 / 102.0 - 1.0,
        101.0 / 100.0 - 1.0,
        102.0 / 101.0 - 1.0,
        103.0 / 102.0 - 1.0,
    ];
    let actual_returns = report["b1_soa"]["projection"]["portfolio_returns"]
        .as_array()
        .unwrap();
    assert_eq!(actual_returns.len(), expected_returns.len() + 1);
    for (row, expected) in actual_returns[1..].iter().zip(expected_returns) {
        assert!((row["return_pct"].as_f64().unwrap() - expected).abs() < 1e-12);
    }
    assert_eq!(
        report["b1_soa"]["checksum_sha256"],
        "9d355cb1a9daf1698318699141ea4ddab8a64495b7ec53ac4502310614e0179c"
    );
    assert!(
        report["b1_soa"]["model_boundary"]
            .as_str()
            .unwrap()
            .contains("no cash, fees, fills")
    );
}

#[test]
fn benchmark_cli_skips_timing_when_the_independent_golden_mismatches() {
    let output_dir = tempfile::tempdir().unwrap();
    let expected_path = output_dir.path().join("wrong-expected.json");
    let mut expected: Value =
        serde_json::from_slice(&fs::read(fixture("expected-v1.json")).unwrap()).unwrap();
    expected["projection"]["summary"]["final_equity"] = Value::from(1.0);
    fs::write(
        &expected_path,
        serde_json::to_vec_pretty(&expected).unwrap(),
    )
    .unwrap();
    let report_path = output_dir.path().join("failed-report.json");

    let result = run_benchmark(&expected_path, &report_path);

    assert!(!result.status.success());
    let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(report["correctness"]["status"], "failed");
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
    assert!(
        report["b1_soa"]["raw_samples_ns"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !report["correctness"]["differences"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn fast_event_reports_final_unfilled_target_with_nonzero_buy_tax() {
    let output_dir = tempfile::tempdir().unwrap();
    let dataset_path = output_dir.path().join("pending-dataset.json");
    let mut dataset: Value =
        serde_json::from_slice(&fs::read(fixture("dataset-v1.json")).unwrap()).unwrap();
    dataset["costs"]["buy_tax_rate"] = Value::from(0.02);
    for date in ["2026-01-13", "2026-01-14", "2026-01-15", "2026-01-16"] {
        dataset["missing_bars"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "symbol": "A",
                "date": date,
                "reason": "no_open_before_dataset_end"
            }));
    }
    fs::write(&dataset_path, serde_json::to_vec_pretty(&dataset).unwrap()).unwrap();
    let report_path = output_dir.path().join("pending-report.json");
    let result =
        run_benchmark_for_dataset(&dataset_path, &fixture("expected-v1.json"), &report_path);
    assert!(
        !result.status.success(),
        "S2 golden must report the modified input"
    );
    let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(
        report["b2_fast_event_buy_hold"]["status"],
        "correctness_passed_and_measured"
    );
    assert_eq!(
        report["b2_fast_event_buy_hold"]["projection"]["unexecuted_targets"][0]["symbol"],
        "A"
    );
    assert_eq!(
        report["b2_fast_event_buy_hold"]["projection"]["unexecuted_targets"][0]["attempt_date"],
        "2026-01-16"
    );
    assert_eq!(
        report["b2_fast_event_buy_hold"]["projection"]["unexecuted_targets"][0]["reason"],
        "no_successful_open_fill_before_dataset_end"
    );
    let taxable_projection = &report["b2_fast_event_buy_hold"]["projection"];
    assert_eq!(taxable_projection["fills"].as_array().unwrap().len(), 2);
    assert!(
        taxable_projection["fills"]
            .as_array()
            .unwrap()
            .iter()
            .all(|fill| fill["quantity"] == 300 && fill["tax"].as_f64().unwrap() > 600.0)
    );
    assert!((taxable_projection["summary"]["tax"].as_f64().unwrap() - 1201.2).abs() < 1e-8);
    assert!(
        (taxable_projection["summary"]["final_equity"]
            .as_f64()
            .unwrap()
            - 99738.8)
            .abs()
            < 1e-8
    );
    assert!(
        report["b2_fast_event_buy_hold"]["projection"]["ledger"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["cash"].as_f64().unwrap() >= 0.0)
    );
}
