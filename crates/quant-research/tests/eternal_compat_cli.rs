#![cfg(feature = "app")]

use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader},
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const CLI: &str = env!("CARGO_BIN_EXE_quant-research");

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve repository root")
}

fn temp_root_outside_repository(candidate: &Path, repository: &Path) -> Option<PathBuf> {
    let candidate = candidate.canonicalize().ok()?;
    let repository = repository.canonicalize().ok()?;
    if candidate == repository || candidate.starts_with(&repository) {
        None
    } else {
        Some(candidate)
    }
}

fn first_temp_root_outside_repository(
    candidates: impl IntoIterator<Item = PathBuf>,
    repository: &Path,
) -> Option<PathBuf> {
    candidates
        .into_iter()
        .find_map(|candidate| temp_root_outside_repository(&candidate, repository))
}

struct TestArtifacts {
    path: PathBuf,
    temporary: Option<tempfile::TempDir>,
}

impl TestArtifacts {
    fn new() -> Self {
        if let Some(root) = std::env::var_os("PRAJNA_TEST_ARTIFACTS") {
            let directory =
                PathBuf::from(root).join(format!("eternal-compat-cli-{}", Uuid::new_v4()));
            fs::create_dir_all(&directory).expect("create synthetic compatibility test directory");
            return Self {
                path: directory,
                temporary: None,
            };
        }

        let repository = repository_root();
        let root = first_temp_root_outside_repository(
            [std::env::temp_dir(), PathBuf::from("/tmp")],
            &repository,
        )
        .expect("find a temporary directory outside the repository");
        let temporary = tempfile::Builder::new()
            .prefix("eternal-compat-cli-")
            .tempdir_in(root)
            .expect("create synthetic compatibility test directory");
        Self {
            path: temporary.path().to_path_buf(),
            temporary: Some(temporary),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestArtifacts {
    fn drop(&mut self) {
        if self.temporary.is_none() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

struct LocalService {
    child: Child,
    address: SocketAddr,
}

impl LocalService {
    fn start(output: &Path, python: &str) -> Self {
        Self::start_with_search_path(output, python, None)
    }

    fn start_with_search_path(
        output: &Path,
        python: &str,
        search_path: Option<&std::ffi::OsStr>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve loopback port");
        let address = listener.local_addr().expect("read reserved loopback port");
        drop(listener);

        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("resolve repository root");
        let mut command = Command::new(CLI);
        command
            .args([
                "serve",
                "--output",
                output.to_str().expect("UTF-8 test output path"),
                "--market-data-dir",
                output
                    .join("market-data")
                    .to_str()
                    .expect("UTF-8 test market-data path"),
                "--address",
                &address.to_string(),
                "--web-dist",
                repository
                    .join("apps/web/dist")
                    .to_str()
                    .expect("UTF-8 web path"),
            ])
            .env("PRAJNA_ETERNAL_COMPAT_PYTHON", python)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(search_path) = search_path {
            command.env("PATH", search_path);
        }
        let mut child = command.spawn().expect("start production local service");
        let stderr = child.stderr.take().expect("capture service startup marker");
        let expected_marker = format!("Prajna Quant: http://{address}");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                match line {
                    Ok(line) if line.trim() == expected_marker => {
                        let _ = sender.send(true);
                        return;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            let _ = sender.send(false);
        });
        let mut service = Self { child, address };
        service.wait_until_ready(receiver);
        service
    }

    fn wait_until_ready(&mut self, marker: Receiver<bool>) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.assert_alive();
            match marker.recv_timeout(Duration::from_millis(25)) {
                Ok(true) => {
                    self.assert_alive();
                    assert!(
                        Self::health_check(self.address),
                        "production local service failed its health check"
                    );
                    self.assert_alive();
                    return;
                }
                Ok(false) => panic!("production local service exited without its startup marker"),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("production local service startup marker reader stopped")
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        panic!("production local service did not start before timeout");
    }

    fn health_check(address: SocketAddr) -> bool {
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
        {
            Ok(client) => client,
            Err(_) => return false,
        };
        let response = client
            .get(format!("http://{address}/api/v1/health"))
            .send()
            .and_then(|response| response.error_for_status())
            .and_then(|response| response.json::<Value>());
        matches!(
            response,
            Ok(value) if value["status"] == "ok" && value["service"] == "prajna-quant"
        )
    }

    fn assert_alive(&mut self) {
        assert!(
            self.child
                .try_wait()
                .expect("check service process")
                .is_none(),
            "production local service exited before the request"
        );
    }

    fn url(&mut self) -> String {
        self.assert_alive();
        format!("http://{}", self.address)
    }

    fn stop(&mut self) {
        if self
            .child
            .try_wait()
            .expect("check service process")
            .is_none()
        {
            self.child.kill().expect("stop production local service");
            self.child.wait().expect("wait for local service exit");
        }
    }
}

impl Drop for LocalService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_cli(arguments: &[String]) -> Output {
    Command::new(CLI)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .expect("run production compatibility CLI")
}

fn successful_json(arguments: &[String]) -> Value {
    let output = run_cli(arguments);
    assert!(
        output.status.success(),
        "production CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("production CLI returns JSON")
}

fn submit(
    server: &mut LocalService,
    snapshot: &Path,
    config: &Value,
    key: &str,
    artifacts: &Path,
) -> String {
    let config_path = artifacts.join(format!("{key}.json"));
    fs::write(
        &config_path,
        serde_json::to_vec(config).expect("serialize synthetic config"),
    )
    .expect("write synthetic config");
    let response = successful_json(&[
        "eternal-compat".into(),
        "submit".into(),
        "--server".into(),
        server.url(),
        "--snapshot".into(),
        snapshot
            .to_str()
            .expect("UTF-8 synthetic snapshot path")
            .into(),
        "--config".into(),
        config_path.to_str().expect("UTF-8 config path").into(),
        "--idempotency-key".into(),
        key.into(),
    ]);
    response["task"]["task_id"]
        .as_str()
        .expect("submit returns task id")
        .to_owned()
}

fn status(server: &mut LocalService, task_id: &str) -> Value {
    successful_json(&[
        "eternal-compat".into(),
        "status".into(),
        "--server".into(),
        server.url(),
        task_id.into(),
    ])
}

fn terminal_status(server: &mut LocalService, task_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let task = status(server, task_id);
        if matches!(task["status"].as_str(), Some("succeeded" | "failed")) {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "task did not reach a terminal state"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn result(server: &mut LocalService, task_id: &str) -> Value {
    successful_json(&[
        "eternal-compat".into(),
        "result".into(),
        "--server".into(),
        server.url(),
        task_id.into(),
    ])
}

fn task_count(output: &Path) -> usize {
    fs::read_dir(output.join("tasks"))
        .map(|entries| entries.count())
        .unwrap_or(0)
}

fn make_synthetic_snapshot(python: &str, path: &Path, scenario: &str) {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve repository root");
    let script = r#"
import json
import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
sys.path.insert(0, sys.argv[2])
from test_eternal_baseline import make_replay_snapshot
from test_eternal_compat import seal_snapshot
import eternal_baseline as baseline
snapshot = Path(sys.argv[3])
scenario = sys.argv[4]
make_replay_snapshot(snapshot)
if scenario == "threshold":
    for code in baseline.HISTORY_CODES:
        path = snapshot / f"bars/{code}.csv.gz"
        rows, fields = baseline.csv_rows(path)
        for row in rows:
            row.update(open="100", close="100", high="100", low="100")
            if code == "sh511260" and row["date"] == "2026-11-09":
                row["close"] = "106"
            if code == "sh511260" and row["date"] == "2026-11-10":
                row.update(open="106", close="106", high="106")
        baseline.write_csv(path, rows, fields)
    reference_path = snapshot / "reference.json"
    reference = json.loads(reference_path.read_text(encoding="utf-8"))
    reference["history_etf"] = [[day, nav * 100.0] for day, nav in reference["history_etf"]]
    reference_path.unlink()
    baseline.write_json(reference_path, reference)
elif scenario in ("validation-gap", "normalized-underflow"):
    path = snapshot / "bars/sh511260.csv.gz"
    rows, fields = baseline.csv_rows(path)
    if scenario == "validation-gap":
        rows[0]["volume"] = "0"
    else:
        for row in rows:
            if row["date"] == "2026-11-09":
                row.update(close="0.5", low="0.5")
    baseline.write_csv(path, rows, fields)
seal_snapshot(snapshot)
"#;
    let output = Command::new(python)
        .args([
            "-c",
            script,
            repository
                .join("tests")
                .to_str()
                .expect("UTF-8 tests directory"),
            repository
                .join("scripts")
                .to_str()
                .expect("UTF-8 scripts directory"),
            path.to_str().expect("UTF-8 synthetic snapshot path"),
            scenario,
        ])
        .stdin(Stdio::null())
        .output()
        .expect("create synthetic replay snapshot");
    assert!(
        output.status.success(),
        "synthetic snapshot helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn temporary_artifacts_fall_back_when_tmpdir_is_inside_repository() {
    let repository = repository_root();
    let outside = repository
        .parent()
        .expect("repository has an outside parent")
        .canonicalize()
        .expect("canonicalize outside parent");
    let selected =
        first_temp_root_outside_repository([repository.clone(), outside.clone()], &repository)
            .expect("select a root outside the repository");
    assert_eq!(selected, outside);
}

#[test]
fn production_cli_reads_persisted_ledger_thresholds_and_pending_gap_failures() {
    let artifacts = TestArtifacts::new();
    let python = std::env::var("PRAJNA_TEST_PYTHON").unwrap_or_else(|_| "python3.12".into());
    let output_root = artifacts.path().join("service-output");
    let initial_snapshot = artifacts.path().join("initial-snapshot");
    let threshold_snapshot = artifacts.path().join("threshold-snapshot");
    let validation_gap_snapshot = artifacts.path().join("validation-gap-snapshot");
    let normalized_underflow_snapshot = artifacts.path().join("normalized-underflow-snapshot");
    make_synthetic_snapshot(&python, &initial_snapshot, "initial");
    make_synthetic_snapshot(&python, &threshold_snapshot, "threshold");
    make_synthetic_snapshot(&python, &validation_gap_snapshot, "validation-gap");
    make_synthetic_snapshot(
        &python,
        &normalized_underflow_snapshot,
        "normalized-underflow",
    );

    let mut service = LocalService::start(&output_root, &python);

    let validation_gap_id = submit(
        &mut service,
        &validation_gap_snapshot,
        &json!({}),
        "synthetic-validation-gap",
        artifacts.path(),
    );
    let validation_gap_status = terminal_status(&mut service, &validation_gap_id);
    assert_eq!(validation_gap_status["status"], "failed");
    assert_eq!(
        validation_gap_status["error"]["code"],
        "compat_input_invalid"
    );
    let validation_details = &validation_gap_status["error"]["details"];
    assert_eq!(validation_details["runner_error_details_complete"], true);
    let gap = &validation_details["runner_error"]["details"]["input_gaps"][0];
    assert_eq!(gap["asset"], "sh511260");
    assert_eq!(gap["date"], "2026-11-06");
    assert_eq!(gap["field"], "volume");
    assert_eq!(gap["source"], "synthetic");
    assert!(!gap["impact"].as_str().unwrap().is_empty());
    assert_eq!(gap["candidate_measures"].as_array().unwrap().len(), 2);
    assert_eq!(
        validation_details["runner_error"]["details"]["checks_not_run"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    let normalized_underflow_id = submit(
        &mut service,
        &normalized_underflow_snapshot,
        &json!({
            "initial_nav": 1e100,
            "weights": {
                "sh511260": f64::from_bits(1),
                "sh518880": 0.5,
                "sh515080": 0.2,
                "sh513300": 0.25,
                "sz159612": 0.05
            }
        }),
        "synthetic-normalized-underflow",
        artifacts.path(),
    );
    let normalized_underflow_status = terminal_status(&mut service, &normalized_underflow_id);
    assert_eq!(normalized_underflow_status["status"], "failed");
    assert_eq!(
        normalized_underflow_status["error"]["code"],
        "compat_normalized_value_underflow"
    );
    assert_eq!(
        normalized_underflow_status["error"]["details"]["runner_error"]["details"],
        json!({"field": "daily.holdings", "asset": "sh511260", "date": "2026-11-09"})
    );
    for id in [&validation_gap_id, &normalized_underflow_id] {
        assert!(
            !output_root
                .join("tasks")
                .join(id)
                .join("compat-result.json")
                .exists()
        );
        let unavailable = run_cli(&[
            "eternal-compat".into(),
            "result".into(),
            "--server".into(),
            service.url(),
            id.clone(),
        ]);
        assert!(!unavailable.status.success());
        assert!(String::from_utf8_lossy(&unavailable.stderr).contains("compat_result_unavailable"));
    }

    let default_capital_id = submit(
        &mut service,
        &initial_snapshot,
        &json!({}),
        "synthetic-default-capital",
        artifacts.path(),
    );
    let scaled_capital_id = submit(
        &mut service,
        &initial_snapshot,
        &json!({"initial_nav":10.0}),
        "synthetic-scaled-capital",
        artifacts.path(),
    );
    assert_eq!(
        terminal_status(&mut service, &default_capital_id)["status"],
        "succeeded"
    );
    assert_eq!(
        terminal_status(&mut service, &scaled_capital_id)["status"],
        "succeeded"
    );
    let default_capital = result(&mut service, &default_capital_id);
    let scaled_capital = result(&mut service, &scaled_capital_id);
    assert_eq!(
        scaled_capital["frozen_reference_comparison"]["overall_status"],
        "match"
    );
    assert_eq!(
        scaled_capital["frozen_reference_comparison"]["daily_nav"]["status"],
        "match"
    );
    assert_eq!(
        scaled_capital["frozen_reference_comparison"]["evaluation_mode"],
        "informational_variant"
    );
    for (default_day, scaled_day) in default_capital["daily"]
        .as_array()
        .unwrap()
        .iter()
        .zip(scaled_capital["daily"].as_array().unwrap())
    {
        for field in ["nav", "cash", "fees"] {
            let expected = default_day[field].as_f64().unwrap() * 10.0;
            let actual = scaled_day[field].as_f64().unwrap();
            assert!((actual - expected).abs() < 1e-10);
        }
        for field in ["units", "holdings"] {
            for asset in ["sh511260", "sh518880", "sh515080", "sh513300", "sz159612"] {
                let expected = default_day[field][asset].as_f64().unwrap() * 10.0;
                let actual = scaled_day[field][asset].as_f64().unwrap();
                assert!((actual - expected).abs() < 1e-10);
            }
        }
    }

    let underflow_id = submit(
        &mut service,
        &initial_snapshot,
        &json!({
            "initial_nav": f64::MIN_POSITIVE,
            "weights": {
                "sh511260": 0.4999999999999999,
                "sh518880": 1e-16,
                "sh515080": 0.20,
                "sh513300": 0.25,
                "sz159612": 0.05
            }
        }),
        "synthetic-scaled-allocation-underflow",
        artifacts.path(),
    );
    let underflow_status = terminal_status(&mut service, &underflow_id);
    assert_eq!(underflow_status["status"], "failed");
    assert_eq!(
        underflow_status["error"]["code"],
        "compat_scaled_value_underflow"
    );
    assert_eq!(
        underflow_status["error"]["details"]["runner_error"]["details"]["field"],
        "daily.units"
    );
    assert_eq!(
        underflow_status["error"]["details"]["runner_error"]["details"]["asset"],
        "sh518880"
    );
    let underflow_result = run_cli(&[
        "eternal-compat".into(),
        "result".into(),
        "--server".into(),
        service.url(),
        underflow_id.clone(),
    ]);
    assert!(!underflow_result.status.success());
    assert!(
        String::from_utf8_lossy(&underflow_result.stderr).contains("compat_result_unavailable")
    );
    let underflow_task_dir = output_root.join("tasks").join(&underflow_id);
    assert!(!underflow_task_dir.join("compat-result.json").exists());
    assert!(
        fs::metadata(underflow_task_dir.join("compat-failure.json"))
            .expect("underflow failure sidecar exists")
            .len()
            > 16
    );

    let precision_id = submit(
        &mut service,
        &initial_snapshot,
        &json!({"initial_nav": f64::MIN_POSITIVE, "weights": {
            "sh511260":0.4999999999999997, "sh518880":3e-16,
            "sh515080":0.20, "sh513300":0.25, "sz159612":0.05
        }}),
        "synthetic-subnormal-precision",
        artifacts.path(),
    );
    let precision_status = terminal_status(&mut service, &precision_id);
    assert_eq!(precision_status["status"], "failed");
    assert_eq!(
        precision_status["error"]["code"],
        "compat_scaled_value_precision_loss"
    );
    assert_eq!(
        precision_status["error"]["details"]["runner_error"]["details"]["asset"],
        "sh518880"
    );

    let ledger_config = json!({
        "initial_nav": 10.0,
        "weights": {
            "sh511260": 0.25,
            "sh518880": 0.20,
            "sh515080": 0.20,
            "sh513300": 0.25,
            "sz159612": 0.10
        },
        "rebalance_threshold": 0.10,
        "fee_rate_one_way": 0.001
    });
    let ledger_id = submit(
        &mut service,
        &initial_snapshot,
        &ledger_config,
        "synthetic-ledger",
        artifacts.path(),
    );
    let ledger_status = terminal_status(&mut service, &ledger_id);
    assert_eq!(ledger_status["status"], "succeeded");
    let ledger = result(&mut service, &ledger_id);
    let day = &ledger["daily"][0];
    let expected_units = [
        ("sh511260", 2500.0 / 1001.0),
        ("sh518880", 2000.0 / 1001.0),
        ("sh515080", 2000.0 / 1001.0),
        ("sh513300", 2500.0 / 1001.0),
        ("sz159612", 1000.0 / 1001.0),
    ];
    for (asset, expected) in expected_units {
        let units = day["units"][asset].as_f64().expect("persisted model units");
        let holding = day["holdings"][asset]
            .as_f64()
            .expect("persisted initial holding");
        assert!((units - expected).abs() < 1e-12);
        assert!((holding - expected).abs() < 1e-12);
    }
    assert!((day["nav"].as_f64().unwrap() - 10_000.0 / 1001.0).abs() < 1e-12);
    assert!((day["fees"].as_f64().unwrap() - 10.0 / 1001.0).abs() < 1e-12);
    assert_eq!(day["cash"], 0.0);
    assert_eq!(
        ledger["effective_config"]["initial_capital_unit"],
        "model_nav"
    );
    assert!(ledger["effective_config"]["initial_capital_currency_amount"].is_null());

    let threshold_five = json!({"initial_nav":10.0,"rebalance_threshold":0.05});
    let threshold_ten = json!({"initial_nav":10.0,"rebalance_threshold":0.10});
    let five_id = submit(
        &mut service,
        &threshold_snapshot,
        &threshold_five,
        "synthetic-threshold-five",
        artifacts.path(),
    );
    let ten_id = submit(
        &mut service,
        &threshold_snapshot,
        &threshold_ten,
        "synthetic-threshold-ten",
        artifacts.path(),
    );
    assert_eq!(
        terminal_status(&mut service, &five_id)["status"],
        "succeeded"
    );
    assert_eq!(
        terminal_status(&mut service, &ten_id)["status"],
        "succeeded"
    );
    let five = result(&mut service, &five_id);
    let ten = result(&mut service, &ten_id);
    assert!(!five["signals"].as_array().unwrap().is_empty());
    assert_eq!(five["signals"][0]["signal_date"], "2026-11-09");
    assert!(
        five["signals"][0]["trigger_details"]
            .as_str()
            .unwrap()
            .contains("gain:0.06000000")
    );
    assert_eq!(five["trades"][0]["signal_date"], "2026-11-09");
    assert_eq!(five["trades"][0]["intended_date"], "2026-11-10");
    assert_eq!(five["trades"][0]["trade_date"], "2026-11-10");
    assert!(ten["signals"].as_array().unwrap().is_empty());
    assert!(ten["trades"].as_array().unwrap().is_empty());

    let high_fee_id = submit(
        &mut service,
        &threshold_snapshot,
        &json!({
            "initial_nav": 10.0,
            "rebalance_threshold": 0.05,
            "fee_rate_one_way": 0.9999999995
        }),
        "synthetic-high-fee-runtime-limitation",
        artifacts.path(),
    );
    let high_fee = terminal_status(&mut service, &high_fee_id);
    assert_eq!(high_fee["status"], "failed");
    assert_eq!(high_fee["error"]["code"], "compat_runner_failed");
    assert!(
        high_fee["error"]["details"]["runner_error"]["message"]
            .as_str()
            .unwrap()
            .contains("未收敛")
    );

    let pending_snapshot = artifacts.path().join("pending-only-snapshot");
    fs::create_dir_all(&pending_snapshot).expect("create pending-only synthetic snapshot");
    let long_detail = format!("synthetic-gap-{}", "x".repeat(24_000));
    let pending_report = json!({
        "status": "pending_decision",
        "gaps": [
            {
                "asset": "sh511260",
                "date": "2026-11-09",
                "field": "volume",
                "impact": "synthetic first gap",
                "candidate_measures": ["synthetic first option"],
                "source": "synthetic-source-a",
                "extra_detail": long_detail,
                "extra_nested": ["😀", 0.1]
            },
            {
                "asset": "sh518880",
                "date": "2026-11-10",
                "field": "daily_bar",
                "impact": "synthetic second gap",
                "candidate_measures": ["synthetic second option"],
                "source": "synthetic-source-b",
                "extra_detail": "retained second detail"
            }
        ]
    });
    fs::write(
        pending_snapshot.join("pending-gaps.json"),
        serde_json::to_vec(&pending_report).expect("serialize synthetic gap report"),
    )
    .expect("write synthetic gap report");
    let failed_id = submit(
        &mut service,
        &pending_snapshot,
        &json!({}),
        "synthetic-pending-gaps",
        artifacts.path(),
    );
    let failed = terminal_status(&mut service, &failed_id);
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["error"]["code"], "compat_input_gaps_pending");
    assert_eq!(
        failed["error"]["details"]["runner_error_details_complete"],
        true
    );
    assert_eq!(failed["error"]["details"]["stderr_truncated"], false);
    let runner_error = &failed["error"]["details"]["runner_error"];
    assert_eq!(
        runner_error["details"]["pending_gap_report"],
        pending_report
    );
    assert_eq!(
        runner_error["details"]["checks_not_run"]
            .as_array()
            .unwrap()
            .iter()
            .map(|check| check["name"].as_str().expect("check label"))
            .collect::<Vec<_>>(),
        vec![
            "snapshot_manifest_validation",
            "snapshot_file_inventory_validation",
            "snapshot_configuration_validation",
            "frozen_source_validation",
            "bar_and_factor_validation",
            "total_return_price_and_limit_construction",
            "historical_overlap_and_splice_validation",
            "common_session_validation",
            "simulation",
            "frozen_reference_comparison",
        ]
    );
    assert!(
        runner_error["details"]["checks_not_run"]
            .as_array()
            .unwrap()
            .iter()
            .all(|check| check["status"] == "not_run"
                && check["reason"] == "blocked by the pending-gap report")
    );
    let failed_task_dir = output_root.join("tasks").join(&failed_id);
    assert!(
        fs::metadata(failed_task_dir.join("compat-failure.json"))
            .expect("structured failure sidecar exists")
            .len()
            > 16 * 1024
    );
    assert!(!failed_task_dir.join("compat-result.json").exists());
    let result_attempt = run_cli(&[
        "eternal-compat".into(),
        "result".into(),
        "--server".into(),
        service.url(),
        failed_id.clone(),
    ]);
    assert!(!result_attempt.status.success());
    assert!(String::from_utf8_lossy(&result_attempt.stderr).contains("compat_result_unavailable"));

    let nonrepresentable_snapshot = artifacts.path().join("nonrepresentable-pending-snapshot");
    fs::create_dir_all(&nonrepresentable_snapshot)
        .expect("create non-representable synthetic snapshot");
    fs::write(
        nonrepresentable_snapshot.join("pending-gaps.json"),
        br#"{"status":"pending_decision","gaps":[{"asset":"sh511260","metric":1e1000}]}"#,
    )
    .expect("write non-representable synthetic gap report");
    let nonrepresentable_id = submit(
        &mut service,
        &nonrepresentable_snapshot,
        &json!({}),
        "synthetic-nonrepresentable-gap-number",
        artifacts.path(),
    );
    let nonrepresentable_status = terminal_status(&mut service, &nonrepresentable_id);
    assert_eq!(nonrepresentable_status["status"], "failed");
    assert_eq!(
        nonrepresentable_status["error"]["code"],
        "compat_pending_gaps_invalid"
    );
    assert_eq!(
        nonrepresentable_status["error"]["details"]["runner_error_details_complete"],
        false
    );
    assert_eq!(
        nonrepresentable_status["error"]["details"]["runner_error"]["details"]["report_status"],
        "non_representable_number"
    );
    assert_eq!(
        nonrepresentable_status["error"]["details"]["runner_error"]["details"]["report_contents_persisted"],
        false
    );
    let nonrepresentable_task_dir = output_root.join("tasks").join(&nonrepresentable_id);
    assert!(
        nonrepresentable_task_dir
            .join("compat-failure.json")
            .exists()
    );
    assert!(
        !nonrepresentable_task_dir
            .join("compat-result.json")
            .exists()
    );
    let nonrepresentable_result = run_cli(&[
        "eternal-compat".into(),
        "result".into(),
        "--server".into(),
        service.url(),
        nonrepresentable_id.clone(),
    ]);
    assert!(!nonrepresentable_result.status.success());
    assert!(
        String::from_utf8_lossy(&nonrepresentable_result.stderr)
            .contains("compat_result_unavailable")
    );

    let mut invalid_reports = Vec::new();
    for (label, payload, expected_status) in [
        (
            "rounded-decimal",
            r#"{"status":"pending_decision","gaps":[{"asset":"sh511260","metric":9007199254740993.0},{"asset":"sh518880","date":"2026-11-10","field":"daily_bar","source":"synthetic-b","impact":"second gap","candidate_measures":["option"]}]}"#,
            "non_representable_number",
        ),
        (
            "lone-surrogate",
            r#"{"status":"pending_decision","gaps":[{"source":"\ud800"}]}"#,
            "invalid",
        ),
        (
            "duplicate-key",
            r#"{"status":"pending_decision","gaps":[{"source":"a","source":"b"}]}"#,
            "invalid",
        ),
        ("malformed", "{", "invalid"),
    ] {
        let snapshot = artifacts.path().join(label);
        fs::create_dir_all(&snapshot).expect("create invalid synthetic snapshot");
        fs::write(snapshot.join("pending-gaps.json"), payload)
            .expect("write invalid synthetic gap report");
        let id = submit(&mut service, &snapshot, &json!({}), label, artifacts.path());
        let failed = terminal_status(&mut service, &id);
        assert_eq!(failed["status"], "failed", "{label}");
        assert_eq!(
            failed["error"]["code"], "compat_pending_gaps_invalid",
            "{label}"
        );
        let details = &failed["error"]["details"];
        assert_eq!(details["runner_error_details_complete"], false, "{label}");
        assert_eq!(
            details["runner_error"]["details"]["report_status"],
            expected_status
        );
        assert_eq!(
            details["runner_error"]["details"]["report_contents_persisted"],
            false
        );
        assert!(
            details["runner_error"]["details"]
                .get("pending_gap_report")
                .is_none()
        );
        assert_eq!(
            details["runner_error"]["details"]["checks_not_run"],
            runner_error["details"]["checks_not_run"]
        );
        assert!(
            !output_root
                .join("tasks")
                .join(&id)
                .join("compat-result.json")
                .exists()
        );
        if label == "rounded-decimal" {
            let recovered = &details["runner_error"]["details"]["recoverable_pending_gap_report"];
            assert_eq!(recovered["gaps"][0]["asset"], "sh511260");
            assert_eq!(
                recovered["gaps"][0]["metric"],
                json!({"unsupported_json_number":"9007199254740993.0"})
            );
            assert_eq!(
                recovered["gaps"][1],
                json!({
                    "asset":"sh518880", "date":"2026-11-10", "field":"daily_bar",
                    "source":"synthetic-b", "impact":"second gap", "candidate_measures":["option"]
                })
            );
        }
        assert_eq!(
            details["runner_error"]["details"]["pending_gap_report_raw_json"],
            payload
        );
        invalid_reports.push((id, failed["error"].clone()));
    }

    let nested_value = format!("{}null{}", "[".repeat(130), "]".repeat(130));
    let deep_report = format!(
        r#"{{"status":"pending_decision","gaps":[{{"asset":"sh511260","date":"2026-11-09","field":"volume","source":"synthetic-source-a","impact":"first gap remains queryable","candidate_measures":["synthetic first option"],"extra_deep":{nested_value}}},{{"asset":"sh518880","date":"2026-11-10","field":"daily_bar","source":"synthetic-source-b","impact":"second gap remains queryable","candidate_measures":["synthetic second option"]}}]}}"#
    );
    let deep_snapshot = artifacts.path().join("deep-pending-snapshot");
    fs::create_dir_all(&deep_snapshot).expect("create deep pending snapshot");
    fs::write(deep_snapshot.join("pending-gaps.json"), deep_report)
        .expect("write deeply nested synthetic gap report");
    let deep_id = submit(
        &mut service,
        &deep_snapshot,
        &json!({}),
        "synthetic-deep-pending-metadata",
        artifacts.path(),
    );
    let deep_status = terminal_status(&mut service, &deep_id);
    assert_eq!(deep_status["status"], "failed");
    assert_eq!(deep_status["error"]["code"], "compat_pending_gaps_invalid");
    let deep_error = &deep_status["error"]["details"]["runner_error"]["details"];
    assert_eq!(deep_error["report_status"], "depth_limit_exceeded");
    assert_eq!(deep_error["report_contents_persisted"], false);
    assert_eq!(
        deep_error["recoverable_pending_gap_report"]["gaps"][0]["asset"],
        "sh511260"
    );
    assert_eq!(
        deep_error["recoverable_pending_gap_report"]["gaps"][0]["extra_deep"],
        json!({"unsupported_json_depth":"see pending_gap_report_raw_json"})
    );
    assert_eq!(
        deep_error["recoverable_pending_gap_report"]["gaps"][1]["source"],
        "synthetic-source-b"
    );
    assert_eq!(deep_error["checks_not_run"].as_array().unwrap().len(), 10);
    invalid_reports.push((deep_id, deep_status["error"].clone()));

    service.stop();
    service = LocalService::start(&output_root, &python);
    for (id, before) in [
        (&validation_gap_id, &validation_gap_status),
        (&normalized_underflow_id, &normalized_underflow_status),
    ] {
        let after = status(&mut service, id);
        assert_eq!(after["status"], "failed");
        assert_eq!(after["error"], before["error"]);
        let unavailable = run_cli(&[
            "eternal-compat".into(),
            "result".into(),
            "--server".into(),
            service.url(),
            id.clone(),
        ]);
        assert!(!unavailable.status.success());
        assert!(String::from_utf8_lossy(&unavailable.stderr).contains("compat_result_unavailable"));
        assert!(
            !output_root
                .join("tasks")
                .join(id)
                .join("compat-result.json")
                .exists()
        );
    }
    assert_eq!(
        status(&mut service, &default_capital_id)["status"],
        "succeeded"
    );
    let restarted_default_capital = result(&mut service, &default_capital_id);
    let restarted_scaled_capital = result(&mut service, &scaled_capital_id);
    assert_eq!(
        restarted_scaled_capital["frozen_reference_comparison"]["overall_status"],
        "match"
    );
    assert_eq!(
        restarted_scaled_capital["daily"][0]["nav"],
        scaled_capital["daily"][0]["nav"]
    );
    assert_eq!(
        restarted_default_capital["daily"][0]["nav"],
        default_capital["daily"][0]["nav"]
    );
    let restarted_underflow = status(&mut service, &underflow_id);
    assert_eq!(restarted_underflow["status"], "failed");
    assert_eq!(
        restarted_underflow["error"]["code"],
        "compat_scaled_value_underflow"
    );
    assert_eq!(
        restarted_underflow["error"]["details"]["runner_error"]["details"]["asset"],
        "sh518880"
    );
    let underflow_result_after_restart = run_cli(&[
        "eternal-compat".into(),
        "result".into(),
        "--server".into(),
        service.url(),
        underflow_id.clone(),
    ]);
    assert!(!underflow_result_after_restart.status.success());
    assert!(
        String::from_utf8_lossy(&underflow_result_after_restart.stderr)
            .contains("compat_result_unavailable")
    );
    assert!(!underflow_task_dir.join("compat-result.json").exists());
    let restarted_nonrepresentable = status(&mut service, &nonrepresentable_id);
    assert_eq!(restarted_nonrepresentable["status"], "failed");
    assert_eq!(
        restarted_nonrepresentable["error"]["code"],
        "compat_pending_gaps_invalid"
    );
    assert_eq!(
        restarted_nonrepresentable["error"]["details"]["runner_error"]["details"]["report_status"],
        "non_representable_number"
    );
    let nonrepresentable_result_after_restart = run_cli(&[
        "eternal-compat".into(),
        "result".into(),
        "--server".into(),
        service.url(),
        nonrepresentable_id.clone(),
    ]);
    assert!(!nonrepresentable_result_after_restart.status.success());
    assert!(
        String::from_utf8_lossy(&nonrepresentable_result_after_restart.stderr)
            .contains("compat_result_unavailable")
    );
    assert!(
        !nonrepresentable_task_dir
            .join("compat-result.json")
            .exists()
    );
    invalid_reports.push((precision_id, precision_status["error"].clone()));
    for (id, expected_error) in invalid_reports {
        let failed = status(&mut service, &id);
        assert_eq!(failed["status"], "failed");
        assert_eq!(failed["error"], expected_error);
        let unavailable = run_cli(&[
            "eternal-compat".into(),
            "result".into(),
            "--server".into(),
            service.url(),
            id.clone(),
        ]);
        assert!(!unavailable.status.success());
        assert!(String::from_utf8_lossy(&unavailable.stderr).contains("compat_result_unavailable"));
        assert!(
            !output_root
                .join("tasks")
                .join(id)
                .join("compat-result.json")
                .exists()
        );
    }
    let restarted = status(&mut service, &failed_id);
    assert_eq!(restarted["status"], "failed");
    assert_eq!(
        restarted["error"]["details"]["runner_error"]["details"]["pending_gap_report"],
        pending_report
    );
    assert_eq!(
        restarted["error"]["details"]["runner_error"]["details"]["checks_not_run"],
        runner_error["details"]["checks_not_run"]
    );
    assert!(!failed_task_dir.join("compat-result.json").exists());
}

#[cfg(unix)]
#[test]
fn cleanup_failure_preserves_compat_gap_diagnostics_across_restart() {
    use std::os::unix::fs::PermissionsExt;

    let artifacts = TestArtifacts::new();
    let python = std::env::var("PRAJNA_TEST_PYTHON").unwrap_or_else(|_| "python3.12".into());
    let output_root = artifacts.path().join("service-output");
    let inspector_dir = artifacts.path().join("inspector-bin");
    fs::create_dir_all(&inspector_dir).expect("create synthetic inspector directory");
    let inspector = inspector_dir.join("ps");
    fs::write(&inspector, "#!/bin/sh\nexit 1\n").expect("write failing process inspector");
    fs::set_permissions(&inspector, fs::Permissions::from_mode(0o755))
        .expect("make failing process inspector executable");
    let mut search_paths = vec![inspector_dir];
    search_paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("test environment has PATH"),
    ));
    let search_path = std::env::join_paths(search_paths).expect("join synthetic PATH");
    let mut service =
        LocalService::start_with_search_path(&output_root, &python, Some(search_path.as_os_str()));

    let snapshot = artifacts.path().join("pending-only-snapshot");
    fs::create_dir_all(&snapshot).expect("create pending-only snapshot");
    let report = json!({
        "status": "pending_decision",
        "gaps": [
            {
                "asset": "sh511260",
                "date": "2026-11-09",
                "field": "volume",
                "source": "synthetic-source-a",
                "impact": "first gap",
                "metric": 0.24378642307954934,
                "candidate_measures": ["first option"]
            },
            {
                "asset": "sh518880",
                "date": "2026-11-10",
                "field": "daily_bar",
                "source": "synthetic-source-b",
                "impact": "second gap",
                "candidate_measures": ["second option"]
            }
        ]
    });
    fs::write(
        snapshot.join("pending-gaps.json"),
        serde_json::to_vec(&report).expect("serialize synthetic pending report"),
    )
    .expect("write synthetic pending report");

    let task_id = submit(
        &mut service,
        &snapshot,
        &json!({}),
        "synthetic-cleanup-failure-pending-gaps",
        artifacts.path(),
    );
    let failed = terminal_status(&mut service, &task_id);
    assert_eq!(failed["status"], "failed");
    assert_eq!(
        failed["error"]["code"],
        "process_group_ownership_unverified"
    );
    let cli_error = failed["error"]["details"]["cli_error"].clone();
    assert_eq!(cli_error["code"], "compat_input_gaps_pending");
    assert_eq!(
        cli_error["details"]["runner_error"]["details"]["pending_gap_report"],
        report
    );
    assert_eq!(
        cli_error["details"]["runner_error"]["details"]["pending_gap_report"]["gaps"][0]["metric"]
            .as_f64()
            .expect("preserved synthetic gap metric")
            .to_bits(),
        0x3fcf_3464_bd2a_f124
    );
    assert_eq!(
        cli_error["details"]["runner_error"]["details"]["checks_not_run"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    assert!(
        cli_error["details"]["runner_error"]["details"]["checks_not_run"]
            .as_array()
            .unwrap()
            .iter()
            .all(|check| check["status"] == "not_run"
                && check["reason"] == "blocked by the pending-gap report")
    );
    assert_eq!(cli_error["details"]["runner_error_details_complete"], true);
    assert!(
        !output_root
            .join("tasks")
            .join(&task_id)
            .join("compat-result.json")
            .exists()
    );

    service.stop();
    service =
        LocalService::start_with_search_path(&output_root, &python, Some(search_path.as_os_str()));
    let restarted = status(&mut service, &task_id);
    assert_eq!(
        restarted["error"]["code"],
        "process_group_ownership_unverified"
    );
    assert_eq!(restarted["error"]["details"]["cli_error"], cli_error);
    assert_eq!(
        restarted["error"]["details"]["cli_error"]["details"]["runner_error"]["details"]
            ["pending_gap_report"]["gaps"][0]["metric"]
            .as_f64()
            .expect("preserved restarted synthetic gap metric")
            .to_bits(),
        0x3fcf_3464_bd2a_f124
    );
    service.stop();
}

#[test]
fn production_cli_rejects_invalid_configs_without_creating_tasks() {
    let artifacts = TestArtifacts::new();
    let python = std::env::var("PRAJNA_TEST_PYTHON").unwrap_or_else(|_| "python3.12".into());
    let output_root = artifacts.path().join("service-output");
    let snapshot = artifacts.path().join("synthetic-snapshot");
    make_synthetic_snapshot(&python, &snapshot, "initial");
    let mut service = LocalService::start(&output_root, &python);

    let minimum_normal = submit(
        &mut service,
        &snapshot,
        &json!({"initial_nav":f64::MIN_POSITIVE,"fee_rate_one_way":0.9999999995}),
        "synthetic-near-boundary-valid",
        artifacts.path(),
    );
    assert_eq!(
        terminal_status(&mut service, &minimum_normal)["status"],
        "succeeded"
    );

    let existing_tasks = task_count(&output_root);
    for (key, config) in [
        ("zero-nav", json!({"initial_nav":0.0})),
        ("negative-nav", json!({"initial_nav":-1.0})),
        ("subnormal-nav", json!({"initial_nav":f64::from_bits(1)})),
        ("fee-one", json!({"fee_rate_one_way":1.0})),
        ("fee-over-one", json!({"fee_rate_one_way":1.1})),
        ("unsupported-field", json!({"rebalance_threshhold":0.05})),
    ] {
        let config_path = artifacts.path().join(format!("{key}.json"));
        fs::write(
            &config_path,
            serde_json::to_vec(&config).expect("serialize invalid synthetic config"),
        )
        .expect("write invalid synthetic config");
        let output = run_cli(&[
            "eternal-compat".into(),
            "submit".into(),
            "--server".into(),
            service.url(),
            "--snapshot".into(),
            snapshot.to_str().expect("UTF-8 snapshot path").into(),
            "--config".into(),
            config_path.to_str().expect("UTF-8 config path").into(),
            "--idempotency-key".into(),
            format!("synthetic-{key}"),
        ]);
        assert!(
            !output.status.success(),
            "invalid config {key} was accepted"
        );
        if key == "unsupported-field" {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("rebalance_threshhold"),
                "unsupported field was not identified in the rejection"
            );
        }
        assert_eq!(task_count(&output_root), existing_tasks);
    }
}
