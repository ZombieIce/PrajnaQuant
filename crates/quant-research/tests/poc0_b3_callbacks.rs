#![cfg(feature = "b3-pyo3")]

use quant_research::poc0_benchmark::run_b3;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn pyo3_callbacks_match_rust_s1_and_share_rust_portfolio_accounting() {
    let root = repo_root();
    let report = run_b3(
        &root.join("poc/poc0-benchmark/fixtures/dataset-v1.json"),
        &root.join("poc/poc0-benchmark/fixtures/expected-v1.json"),
    )
    .expect("B3 fixture should run");

    assert_eq!(report["status"], "correctness_passed_and_measured");
    assert_eq!(
        report["correctness"]["rust_native_python_s1_decisions_match"],
        true
    );
    assert_eq!(
        report["correctness"]["python_s1_decision_matches_expected_event"],
        true
    );
    assert_eq!(
        report["correctness"]["rust_s1_decision_matches_expected_event"],
        true
    );
    assert_eq!(
        report["correctness"]["python_decision_account_projection_matches_rust"],
        true
    );
    assert_eq!(
        report["correctness"]["empty_callbacks_emit_no_decisions"],
        true
    );
    assert_eq!(
        report["measurements"]["python_empty_callback"]["callbacks_per_run"],
        report["event_count_per_run"]
    );
    assert_eq!(
        report["measurements"]["python_s1_on_bar"]["raw_samples_ns"]
            .as_array()
            .expect("raw samples")
            .len(),
        5
    );
    assert_eq!(
        report["measurements"]["rust_native_empty_callback"]["raw_end_to_end_samples_ns"]
            .as_array()
            .expect("end-to-end samples")
            .len(),
        5
    );
    assert_eq!(
        report["portfolio_result"]["summary"]["final_equity"],
        101710.0
    );
    let total_cost = report["portfolio_result"]["summary"]["total_cost"]
        .as_f64()
        .expect("numeric total cost");
    assert!((total_cost - 390.0).abs() < 1e-8);
    assert!(report["correctness"]["account_projection_checksum_sha256"].is_string());
}
