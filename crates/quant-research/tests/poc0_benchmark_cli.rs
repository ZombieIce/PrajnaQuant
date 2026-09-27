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
    run_candidate_for_dataset("soa", dataset, expected, output)
}

fn run_candidate_for_dataset(
    candidate: &str,
    dataset: &std::path::Path,
    expected: &std::path::Path,
    output: &std::path::Path,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_quant-research"))
        .current_dir(repo_root())
        .args([
            "benchmark-poc0",
            "--candidate",
            candidate,
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
    assert_eq!(report["b1_arrow"]["correctness_status"], "passed");
    assert_eq!(
        report["b1_arrow"]["checksum_sha256"],
        report["b1_soa"]["checksum_sha256"]
    );
    let first_ranking = report["b1_arrow"]["projection"]["rankings"][0]["candidates"]
        .as_array()
        .unwrap();
    assert_eq!(first_ranking[0]["symbol"], "C");
    assert!((first_ranking[0]["score"].as_f64().unwrap() - 0.02).abs() < 1e-12);
    assert_eq!(first_ranking[1]["symbol"], "A");
    assert!((first_ranking[1]["score"].as_f64().unwrap() - 0.01980198019801982).abs() < 1e-12);
    assert_eq!(first_ranking[2]["symbol"], "B");
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
fn benchmark_arrow_candidate_matches_independent_return_examples_and_records_conversion() {
    let output_dir = tempfile::tempdir().unwrap();
    let report_path = output_dir.path().join("arrow-report.json");
    let result = run_candidate_for_dataset(
        "arrow",
        &fixture("dataset-v1.json"),
        &fixture("expected-v1.json"),
        &report_path,
    );
    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(
        report["candidate"],
        "apache-arrow-column-batch-momentum-rotation"
    );
    assert_eq!(report["b1_arrow"]["correctness_status"], "passed");
    assert_eq!(report["b1_arrow"]["warmup_runs"], 1);
    assert_eq!(
        report["b1_arrow"]["raw_samples_ns"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        report["b1_arrow"]["conversion_samples_ns"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        report["b1_arrow"]["checksum_sha256"],
        report["b1_soa"]["checksum_sha256"]
    );
    // Independent hand calculation: selected C moves 101 -> 100 on the first interval.
    let first_return = report["b1_arrow"]["projection"]["portfolio_returns"][0]["return_pct"]
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
    let actual_returns = report["b1_arrow"]["projection"]["portfolio_returns"]
        .as_array()
        .unwrap();
    assert_eq!(actual_returns.len(), expected_returns.len() + 1);
    for (row, expected) in actual_returns[1..].iter().zip(expected_returns) {
        assert!((row["return_pct"].as_f64().unwrap() - expected).abs() < 1e-12);
    }
}

#[test]
fn benchmark_polars_candidate_matches_soa_and_records_expression_samples() {
    let output_dir = tempfile::tempdir().unwrap();
    let report_path = output_dir.path().join("polars-report.json");
    let result = run_candidate_for_dataset(
        "polars",
        &fixture("dataset-v1.json"),
        &fixture("expected-v1.json"),
        &report_path,
    );
    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(report["candidate"], "polars-expression-momentum-rotation");
    assert_eq!(report["correctness"]["status"], "passed");
    assert_eq!(report["b1_polars"]["correctness_status"], "passed");
    assert_eq!(report["b1_polars"]["warmup_runs"], 1);
    assert_eq!(
        report["b1_polars"]["raw_samples_ns"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        report["b1_polars"]["conversion_samples_ns"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        report["b1_polars"]["checksum_sha256"],
        report["b1_soa"]["checksum_sha256"]
    );
    let polars_returns = report["b1_polars"]["projection"]["portfolio_returns"]
        .as_array()
        .unwrap();
    let soa_returns = report["b1_soa"]["projection"]["portfolio_returns"]
        .as_array()
        .unwrap();
    assert_eq!(polars_returns.len(), soa_returns.len());
    // Hand-check the selected-symbol returns independently of either candidate projection.
    let first_return = polars_returns[0]["return_pct"].as_f64().unwrap();
    assert!((first_return - (100.0 / 101.0 - 1.0)).abs() < 1e-12);
    let expected_later_returns = [
        102.0 / 99.0 - 1.0,
        103.0 / 102.0 - 1.0,
        103.0 / 102.0 - 1.0,
        101.0 / 100.0 - 1.0,
        102.0 / 101.0 - 1.0,
        103.0 / 102.0 - 1.0,
    ];
    assert_eq!(polars_returns.len(), expected_later_returns.len() + 1);
    for (row, expected) in polars_returns[1..].iter().zip(expected_later_returns) {
        assert!((row["return_pct"].as_f64().unwrap() - expected).abs() < 1e-12);
    }
    for (polars, soa) in polars_returns.iter().zip(soa_returns) {
        assert_eq!(polars["from"], soa["from"]);
        assert_eq!(polars["to"], soa["to"]);
        match (polars["return_pct"].as_f64(), soa["return_pct"].as_f64()) {
            (Some(left), Some(right)) => assert!((left - right).abs() < 1e-12),
            (None, None) => {}
            mismatch => panic!("return availability differs: {mismatch:?}"),
        }
    }
}

#[test]
fn benchmark_polars_breaks_score_ties_by_symbol() {
    let output_dir = tempfile::tempdir().unwrap();
    let dataset_path = output_dir.path().join("tied-dataset.json");
    let mut dataset: Value =
        serde_json::from_slice(&fs::read(fixture("dataset-v1.json")).unwrap()).unwrap();
    dataset["strategy"]["short_momentum_weight"] = Value::from(0.0);
    dataset["strategy"]["long_momentum_weight"] = Value::from(0.0);
    dataset["strategy"]["volatility_weight"] = Value::from(0.0);
    fs::write(&dataset_path, serde_json::to_vec_pretty(&dataset).unwrap()).unwrap();
    let report_path = output_dir.path().join("tied-report.json");
    let result = run_candidate_for_dataset(
        "polars",
        &dataset_path,
        &fixture("expected-v1.json"),
        &report_path,
    );
    assert!(
        !result.status.success(),
        "the changed dataset must not reuse the old golden"
    );
    let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(report["b1_polars"]["correctness_status"], "passed");
    assert!(
        report["b1_polars"]["raw_samples_ns"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let rankings = report["b1_polars"]["projection"]["rankings"]
        .as_array()
        .unwrap();
    assert!(!rankings.is_empty());
    for ranking in rankings {
        let candidates = ranking["candidates"].as_array().unwrap();
        for candidate in candidates {
            assert_eq!(candidate["score"], 0.0);
        }
        let soa_candidates = report["b1_soa"]["projection"]["rankings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|soa_ranking| soa_ranking["date"] == ranking["date"])
            .unwrap()["candidates"]
            .as_array()
            .unwrap();
        assert_eq!(candidates, soa_candidates);
    }
}

#[test]
fn benchmark_polars_checks_window_boundary_and_missing_next_bar() {
    let output_dir = tempfile::tempdir().unwrap();
    let window_dataset_path = output_dir.path().join("window-dataset.json");
    let mut window_dataset: Value =
        serde_json::from_slice(&fs::read(fixture("dataset-v1.json")).unwrap()).unwrap();
    window_dataset["strategy"]["momentum_short_days"] = Value::from(4);
    window_dataset["strategy"]["momentum_long_days"] = Value::from(4);
    window_dataset["strategy"]["short_momentum_weight"] = Value::from(1.0);
    window_dataset["strategy"]["long_momentum_weight"] = Value::from(0.0);
    fs::write(
        &window_dataset_path,
        serde_json::to_vec_pretty(&window_dataset).unwrap(),
    )
    .unwrap();
    let window_report_path = output_dir.path().join("window-report.json");
    let window_result = run_candidate_for_dataset(
        "polars",
        &window_dataset_path,
        &fixture("expected-v1.json"),
        &window_report_path,
    );
    assert!(
        !window_result.status.success(),
        "the changed strategy needs its own golden"
    );
    let window_report: Value =
        serde_json::from_slice(&fs::read(window_report_path).unwrap()).unwrap();
    assert_eq!(window_report["b1_polars"]["correctness_status"], "passed");
    assert!(
        window_report["b1_polars"]["raw_samples_ns"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let first_valid = &window_report["b1_polars"]["projection"]["rankings"][0];
    assert_eq!(first_valid["date"], "2026-01-09");
    assert_eq!(first_valid["candidates"][0]["symbol"], "B");
    assert!((first_valid["candidates"][0]["score"].as_f64().unwrap() - 0.02).abs() < 1e-12);

    let missing_dataset_path = output_dir.path().join("missing-next-bar-dataset.json");
    let mut missing_dataset: Value =
        serde_json::from_slice(&fs::read(fixture("dataset-v1.json")).unwrap()).unwrap();
    missing_dataset["missing_bars"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "symbol": "A",
            "date": "2026-01-16",
            "reason": "missing_next_calendar_bar"
        }));
    fs::write(
        &missing_dataset_path,
        serde_json::to_vec_pretty(&missing_dataset).unwrap(),
    )
    .unwrap();
    let missing_report_path = output_dir.path().join("missing-next-bar-report.json");
    let missing_result = run_candidate_for_dataset(
        "polars",
        &missing_dataset_path,
        &fixture("expected-v1.json"),
        &missing_report_path,
    );
    assert!(
        !missing_result.status.success(),
        "the changed input needs its own golden"
    );
    let missing_report: Value =
        serde_json::from_slice(&fs::read(missing_report_path).unwrap()).unwrap();
    assert_eq!(missing_report["b1_polars"]["correctness_status"], "passed");
    let missing_return = missing_report["b1_polars"]["projection"]["portfolio_returns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["from"] == "2026-01-15")
        .unwrap();
    assert_eq!(missing_return["to"], "2026-01-16");
    assert!(missing_return["return_pct"].is_null());
}

#[test]
fn benchmark_polars_equal_weights_use_actual_selected_count() {
    let output_dir = tempfile::tempdir().unwrap();
    let dataset_path = output_dir.path().join("few-candidates-dataset.json");
    let mut dataset: Value =
        serde_json::from_slice(&fs::read(fixture("dataset-v1.json")).unwrap()).unwrap();
    dataset["strategy"]["top_n"] = Value::from(10);
    fs::write(&dataset_path, serde_json::to_vec_pretty(&dataset).unwrap()).unwrap();

    let report_path = output_dir.path().join("few-candidates-report.json");
    let result = run_candidate_for_dataset(
        "polars",
        &dataset_path,
        &fixture("expected-v1.json"),
        &report_path,
    );
    assert!(
        !result.status.success(),
        "changed input needs its own golden"
    );
    let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(report["b1_polars"]["correctness_status"], "passed");
    let target_weights = report["b1_polars"]["projection"]["target_weights"]
        .as_array()
        .unwrap();
    let first_date = target_weights[0]["date"].as_str().unwrap();
    let selected = target_weights
        .iter()
        .filter(|target| target["date"] == first_date)
        .collect::<Vec<_>>();
    assert_eq!(selected.len(), 3);
    assert!(
        selected
            .iter()
            .all(|target| target["weight"] == (1.0 / 3.0))
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
