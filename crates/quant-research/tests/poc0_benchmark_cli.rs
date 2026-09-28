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
fn parquet_cli_round_trips_fixture_and_prunes_groups_and_columns() {
    let output_dir = tempfile::tempdir().unwrap();
    for (name, args, expected_groups) in [
        ("full", vec![], 6),
        (
            "filtered",
            vec![
                "--symbol",
                "A",
                "--start",
                "2026-01-06",
                "--end",
                "2026-01-07",
            ],
            1,
        ),
        ("filler", vec!["--filler-rows", "8192", "--symbol", "A"], 2),
    ] {
        let parquet = output_dir.path().join(format!("{name}.parquet"));
        let report = output_dir.path().join(format!("{name}.json"));
        let result = Command::new(env!("CARGO_BIN_EXE_quant-research"))
            .current_dir(repo_root())
            .args([
                "benchmark-poc0-parquet",
                "--dataset",
                fixture("dataset-v1.json").to_str().unwrap(),
                "--expected",
                fixture("expected-v1.json").to_str().unwrap(),
                "--parquet",
                parquet.to_str().unwrap(),
                "--output",
                report.to_str().unwrap(),
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let value: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
        assert_eq!(value["status"], "passed");
        assert_eq!(value["pruning"]["scanned_groups"], expected_groups);
        assert_eq!(value["pruning"]["decoded_columns"], 3);
        assert_eq!(value["pruning"]["source_columns"], 7);
        if name == "filler" {
            assert!(value["pruning"]["source_rows"].as_u64().unwrap() > 8192);
            assert!(value["pruning"]["source_groups"].as_u64().unwrap() > 6);
        }
        if name == "full" {
            let replay_path = output_dir.path().join("replay.json");
            let replay = Command::new(env!("CARGO_BIN_EXE_quant-research"))
                .current_dir(repo_root())
                .args([
                    "benchmark-poc0-parquet",
                    "--reuse",
                    "--parquet",
                    parquet.to_str().unwrap(),
                    "--output",
                    replay_path.to_str().unwrap(),
                ])
                .output()
                .unwrap();
            assert!(
                replay.status.success(),
                "{}",
                String::from_utf8_lossy(&replay.stderr)
            );
            let replay_value: Value =
                serde_json::from_slice(&fs::read(replay_path).unwrap()).unwrap();
            assert_eq!(
                replay_value["projection_sha256"],
                value["projection_sha256"]
            );
            assert_eq!(replay_value["timing_ns"]["generation"], 0);
        }
    }
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
        report["b2_fast_event_momentum_rotation"]["status"],
        "correctness_passed_and_measured"
    );
    assert_eq!(
        report["b2_fast_event_momentum_rotation"]["signals"][0]["date"],
        "2026-01-07"
    );
    assert_eq!(
        report["b2_fast_event_momentum_rotation"]["signals"][0]["target_symbols"],
        serde_json::json!(["C"])
    );
    let rotation = &report["b2_fast_event_momentum_rotation"]["projection"];
    assert_eq!(
        rotation["orders"]
            .as_array()
            .unwrap()
            .iter()
            .map(|order| (
                order["attempt_date"].as_str().unwrap(),
                order["symbol"].as_str().unwrap(),
                order["side"].as_str().unwrap(),
                order["quantity"].as_i64().unwrap(),
                order["reason"].as_str(),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("2026-01-08", "C", "BUY", 0, Some("UNKNOWN")),
            ("2026-01-09", "B", "BUY", 900, None),
            ("2026-01-13", "B", "SELL", 0, Some("HALTED")),
            ("2026-01-14", "B", "SELL", 900, None),
            ("2026-01-14", "A", "BUY", 900, None),
        ]
    );
    assert_eq!(
        rotation["ledger"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| (row["cash"].as_f64().unwrap(), row["nav"].as_f64().unwrap(),))
            .collect::<Vec<_>>(),
        vec![
            (100000.0, 100000.0),
            (100000.0, 100000.0),
            (100000.0, 100000.0),
            (100000.0, 100000.0),
            (9810.0, 101610.0),
            (9810.0, 102510.0),
            (9810.0, 100710.0),
            (9430.0, 100330.0),
            (9430.0, 101230.0),
            (9430.0, 102130.0),
        ]
    );
    assert_eq!(rotation["summary"]["commission"], 300.0);
    assert!((rotation["summary"]["total_cost"].as_f64().unwrap() - 570.0).abs() < 1e-8);
    let ma = &report["b2_fast_event_ma20_60"];
    assert_eq!(ma["status"], "correctness_passed_and_measured");
    assert_eq!(ma["signals"].as_array().unwrap().len(), 2);
    assert_eq!(ma["signals"][0]["target_symbols"], serde_json::json!(["A"]));
    assert_eq!(ma["signals"][1]["target_symbols"], serde_json::json!([]));
    assert_eq!(ma["projection"]["orders"][0]["quantity"], 900);
    assert_eq!(ma["projection"]["orders"][1]["side"], "SELL");
    assert_eq!(ma["projection"]["ledger"][61]["cash"], 9810.0);
    assert_eq!(ma["projection"]["ledger"][86]["cash"], 99620.0);
    assert!((ma["projection"]["summary"]["total_cost"].as_f64().unwrap() - 380.0).abs() < 1e-8);
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
fn b2_isolated_cli_measures_fixed_parallel_runs_after_correctness_gate() {
    let output_dir = tempfile::tempdir().unwrap();
    let output = output_dir.path().join("b2.json");
    let result = Command::new(env!("CARGO_BIN_EXE_quant-research"))
        .current_dir(repo_root())
        .args([
            "benchmark-poc0-b2",
            "--strategy",
            "s2",
            "--mode",
            "serial",
            "--workers",
            "2",
            "--runs",
            "6",
            "--output",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(report["status"], "passed");
    assert_eq!(report["mode"], "serial");
    assert_eq!(report["workers"], 1);
    assert_eq!(report["warmup_runs"], 2);
    assert_eq!(report["warmup_runs_per_worker"], 2);
    assert_eq!(report["raw_samples_ns"].as_array().unwrap().len(), 6);
    assert_eq!(report["run_checksum_sha256"].as_str().unwrap().len(), 64);
    assert!(report["serial_runs_per_second"].as_f64().unwrap() > 0.0);
    assert!(report["parallel_runs_per_second"].is_null());

    let skipped_output = output_dir.path().join("b2-skipped-preflight.json");
    let skipped = Command::new(env!("CARGO_BIN_EXE_quant-research"))
        .current_dir(repo_root())
        .args([
            "benchmark-poc0-b2",
            "--strategy",
            "s2",
            "--mode",
            "serial",
            "--runs",
            "1",
            "--skip-golden-preflight",
            "--expected-checksum",
            report["run_checksum_sha256"].as_str().unwrap(),
            "--output",
            skipped_output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        skipped.status.success(),
        "{}",
        String::from_utf8_lossy(&skipped.stderr)
    );
    let skipped_report: Value = serde_json::from_slice(&fs::read(skipped_output).unwrap()).unwrap();
    assert_eq!(skipped_report["golden_preflight"], "external_passed");
    assert_eq!(
        skipped_report["run_checksum_sha256"],
        report["run_checksum_sha256"]
    );
}

#[test]
fn b2_parallel_warmup_mismatch_returns_instead_of_stranding_workers() {
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("b2-warmup-mismatch.json");
    let wrong_checksum = "0".repeat(64);
    let result = Command::new(env!("CARGO_BIN_EXE_quant-research"))
        .current_dir(repo_root())
        .args([
            "benchmark-poc0-b2",
            "--strategy",
            "s2",
            "--mode",
            "parallel",
            "--workers",
            "2",
            "--runs",
            "2",
            "--skip-golden-preflight",
            "--expected-checksum",
            wrong_checksum.as_str(),
            "--output",
        ])
        .arg(output)
        .output()
        .unwrap();

    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("B2 worker warmup mismatch"));
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
fn benchmark_poc0_sweep_reports_reusable_cache_and_layout_measurements() {
    let output_dir = tempfile::tempdir().unwrap();
    let report_path = output_dir.path().join("sweep-report.json");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_quant-research"))
        .args([
            "benchmark-poc0-sweep",
            "--dataset",
            fixture("dataset-v1.json").to_str().unwrap(),
            "--expected",
            fixture("expected-v1.json").to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--instruments",
            "3",
            "--sessions",
            "10",
        ])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(report["format"], "poc0-b1-sweep.v1");
    assert_eq!(report["status"], "passed");
    assert!(
        report["target_load"]["kind"]
            .as_str()
            .unwrap()
            .contains("synthetic")
    );
    assert_eq!(report["target_load"]["instrument_count"], 3);
    assert_eq!(report["target_load"]["session_count"], 10);
    assert!(report["decision_threshold"]["minimum_throughput_gain_pct"].is_number());

    let parameter_runs = report["parameter_runs"].as_array().unwrap();
    assert!(!parameter_runs.is_empty());
    for run in parameter_runs {
        for layout in ["soa", "arrow", "polars"] {
            assert_eq!(
                run["checksums"][layout]["cache_miss"], run["checksums"][layout]["cache_hit"],
                "{layout} cache hit changed the projection for {:?}",
                run["parameters"]
            );
        }
    }

    for layout in ["soa", "arrow", "polars"] {
        for condition in ["cache_miss", "cache_hit"] {
            let measurement = &report["measurements"][layout][condition];
            assert!(!measurement["raw_samples_ns"].as_array().unwrap().is_empty());
            assert_eq!(
                measurement["scan_samples_ns"].as_array().unwrap().len(),
                report["repetitions"].as_u64().unwrap() as usize
            );
            assert!(measurement["median_ns"].is_number());
            assert!(measurement["p95_ns"].is_number());
            assert!(measurement["parallel_runs_per_second"].is_number());
            assert!(measurement.get("peak_rss_bytes").is_some());
            let per_parameter = measurement["per_parameter_statistics"].as_object().unwrap();
            assert_eq!(per_parameter.len(), parameter_runs.len());
            assert!(per_parameter.values().all(|row| {
                row["raw_sample_count"] == report["repetitions"]
                    && row["median_ns"].is_number()
                    && row["p95_ns"].is_number()
            }));
        }
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
