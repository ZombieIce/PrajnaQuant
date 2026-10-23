use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use prajna_adapter::{AdapterError, Backend, FakeBackend, RunPacket};
use prajna_data::{NormalizerRegistry, RawStore, SourceKind, SourceRecordInput, publish_dataset};
use prajna_experiment::{
    BackendDiagnostics, BackendFactory, BackendIdentity, Experiment, ExperimentBackend,
    ResultLevel, RunStatus, StoreAction, WorkerCommand,
};
use prajna_research::StaticUniverse;
use prajna_research::fast_event::{FastEventConfig, FastEventCosts};
use serde_json::{Value, json};
use tempfile::TempDir;

fn fixture(root: &Path) -> (String, String) {
    let store = RawStore::open(root).unwrap();
    let raw = store
        .put(
            include_bytes!("../../../poc/poc0-benchmark/fixtures/dataset-v1.json"),
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::Fixture,
                source_id: "poc0-dataset-v1".into(),
                request: json!({}),
                observed_at: "synthetic".into(),
                ingested_by: "accurate-experiment-tests".into(),
            },
        )
        .unwrap();
    let dataset = publish_dataset(
        root,
        &store,
        &NormalizerRegistry::with_builtins(),
        "synthetic-etf-daily",
        "3",
        &[raw],
        "2026-10-03T00:00:00Z",
    )
    .unwrap();
    let universe = StaticUniverse::new(
        "fixture",
        ["A.SYNTH", "B.SYNTH"]
            .map(|id| id.parse().unwrap())
            .to_vec(),
    )
    .unwrap();
    universe.store(root).unwrap();
    (dataset.dsv, universe.id().unwrap())
}

fn python_version_error(path: &str, version: &str) -> Option<String> {
    let version_parts = version
        .split('.')
        .take(2)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok();
    let supported = version_parts
        .filter(|parts| parts.len() == 2)
        .is_some_and(|parts| (parts[0], parts[1]) >= (3, 12));
    (!supported).then(|| {
        format!(
            "accurate_event tests require Python >= 3.12; current interpreter `{path}` is {version}. Set PRAJNA_TEST_PYTHON to a Python 3.12+ executable."
        )
    })
}

fn test_python() -> &'static Path {
    static PYTHON: OnceLock<PathBuf> = OnceLock::new();
    PYTHON
        .get_or_init(|| {
            let python = std::env::var_os("PRAJNA_TEST_PYTHON")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("python3"));
            let output = std::process::Command::new(&python)
                .args([
                    "-c",
                    "import sys; print(f'{sys.executable}\\t{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}')",
                ])
                .output()
                .unwrap_or_else(|error| {
                    panic!(
                        "could not start accurate_event test Python `{}`: {error}. Set PRAJNA_TEST_PYTHON to a Python 3.12+ executable.",
                        python.display()
                    )
                });
            if !output.status.success() {
                panic!(
                    "could not inspect accurate_event test Python `{}` ({}): {}. Set PRAJNA_TEST_PYTHON to a Python 3.12+ executable.",
                    python.display(),
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            let report = String::from_utf8(output.stdout)
                .expect("Python interpreter path/version output must be UTF-8");
            let (path, version) = report
                .trim()
                .split_once('\t')
                .unwrap_or_else(|| panic!("unexpected test Python version response: {report:?}"));
            if let Some(error) = python_version_error(path, version) {
                panic!("{error}");
            }
            python
        })
        .as_path()
}

#[test]
fn old_test_python_reports_path_version_and_configuration_hint() {
    assert_eq!(
        python_version_error("/opt/python3.12/bin/python", "3.12.0"),
        None
    );
    assert_eq!(
        python_version_error("/usr/bin/python3", "3.11.9"),
        Some(
            "accurate_event tests require Python >= 3.12; current interpreter `/usr/bin/python3` is 3.11.9. Set PRAJNA_TEST_PYTHON to a Python 3.12+ executable.".into()
        )
    );
}

fn definition(dsv: &str, universe: &str) -> Value {
    json!({
        "experiment_version":1, "dsv":dsv, "universe":universe, "venue":"SYNTH",
        "strategy":"buy_and_hold", "engine":"accurate_event",
        "backend":{"name":"nautilus_trader","version":"2.0.0rc6"},
        "availability_assumption":"none", "sessions_per_year":252,
        "costs":{"commission_rate":0.001,"buy_slippage_bps":10.0,
            "sell_slippage_bps":10.0,"buy_tax_rate":0.0,"sell_tax_rate":0.0},
        "parameter_space":{"list":[{"params_version":1}]},
        "fast_event":{"currency":"CNY","initial_cash":"1000000","sizing":"lot",
            "rebalance":"full_target","minimum_commission":"5",
            "portfolios":[{"id":1,"capital":"800000"}],
            "lot_size_source":{"kind":"instrument"}}
    })
}

#[test]
fn accurate_run_identity_includes_backend_without_changing_legacy_specs() {
    let temp = TempDir::new().unwrap();
    let (dsv, universe) = fixture(temp.path());
    let base = definition(&dsv, &universe);
    let expand = |value: &Value| {
        Experiment::parse(&value.to_string(), temp.path())
            .unwrap()
            .expand(temp.path())
            .unwrap()
            .remove(0)
    };
    let accurate = expand(&base);
    assert_eq!(
        accurate.run_spec["engine"],
        json!({"kind":"accurate_event","semantic_version":"accurate_event@1"})
    );
    assert_eq!(accurate.run_spec["backend"], base["backend"]);
    for (field, value) in [("version", "2.0.0rc7"), ("name", "different_backend")] {
        let mut changed = base.clone();
        changed["backend"][field] = value.into();
        assert_ne!(accurate.run_id, expand(&changed).run_id);
    }
    for engine in ["vector", "fast_event"] {
        let mut legacy = base.clone();
        legacy["engine"] = engine.into();
        legacy.as_object_mut().unwrap().remove("backend");
        if engine == "vector" {
            legacy.as_object_mut().unwrap().remove("fast_event");
        }
        let run = expand(&legacy);
        assert!(run.run_spec.get("backend").is_none());
        assert_eq!(run.run_spec["engine"]["kind"], engine);
        assert_eq!(
            run.run_spec["engine"]["semantic_version"],
            if engine == "fast_event" {
                "fast_event@10".to_string()
            } else {
                format!("{engine}@1")
            }
        );
        // Frozen normalized inputs from the #138 branch base.
        let mut expected = json!({
            "dsv":dsv,"universe":universe,"venue":"SYNTH",
            "strategy":{"kind":"buy_and_hold","parameters":{"params_version":1},"unfilled_entry":"retry"},
            "availability_assumption":"none",
            "costs":{"commission_rate":"3f50624dd2f1a9fc","buy_slippage_bps":"4024000000000000",
                "sell_slippage_bps":"4024000000000000","buy_tax_rate":"0000000000000000",
                "sell_tax_rate":"0000000000000000"},
            "engine":{"kind":engine,"semantic_version":if engine == "fast_event" { "fast_event@10".to_string() } else { format!("{engine}@1") }},"seed":null
        });
        if engine == "fast_event" {
            expected["fast_event"] = json!({
                "currency":"CNY","initial_cash":"1000000","sizing":"lot",
                "rebalance":"full_target","entry_budget":null,"minimum_commission":"5",
                "portfolios":[{"id":1,"capital":"800000"}],
                "lot_size_source":{"kind":"instrument","resolved":{
                    "A.SYNTH":"100","B.SYNTH":"100","C.SYNTH":"100"
                }}
            });
        }
        assert_eq!(run.run_spec, expected);
    }
}

#[derive(Debug)]
struct FakeFactory;

#[derive(Debug)]
struct RunDeadlineFactory(WorkerCommand);

impl BackendFactory for RunDeadlineFactory {
    fn validate(&self, identity: &BackendIdentity) -> Result<(), AdapterError> {
        let mut preflight = self.0.clone();
        preflight.timeout = std::time::Duration::from_secs(60);
        preflight.validate(identity)
    }

    fn create(
        &self,
        identity: &BackendIdentity,
        config: &FastEventConfig,
        dsv: &str,
    ) -> Result<Box<dyn ExperimentBackend>, AdapterError> {
        self.0.create(identity, config, dsv)
    }
}

struct FakeRun(FakeBackend);

impl BackendFactory for FakeFactory {
    fn validate(&self, _identity: &BackendIdentity) -> Result<(), AdapterError> {
        Ok(())
    }

    fn create(
        &self,
        _identity: &BackendIdentity,
        _config: &FastEventConfig,
        _dsv: &str,
    ) -> Result<Box<dyn ExperimentBackend>, AdapterError> {
        Ok(Box::new(FakeRun(FakeBackend::default())))
    }
}

impl Backend for FakeRun {
    fn begin_run(&mut self, packet: &RunPacket, costs: FastEventCosts) -> Result<(), AdapterError> {
        self.0.begin_run(packet, costs)
    }

    fn exchange(&mut self, request: &str) -> Result<String, AdapterError> {
        self.0.exchange(request)
    }
}

impl ExperimentBackend for FakeRun {
    fn finish(self: Box<Self>) -> Result<BackendDiagnostics, AdapterError> {
        Ok(BackendDiagnostics::fake())
    }
}

#[test]
fn accurate_fake_replay_is_deterministic_and_never_runs_fast_as_a_fallback() {
    let temp = TempDir::new().unwrap();
    let (dsv, universe) = fixture(temp.path());
    let value = definition(&dsv, &universe);
    let parse = || Experiment::parse(&value.to_string(), temp.path()).unwrap();
    assert!(
        parse()
            .execute(temp.path(), 1)
            .unwrap_err()
            .to_string()
            .contains("worker")
    );
    let experiment = parse().with_backend_factory(Arc::new(FakeFactory)).unwrap();
    let a = experiment.execute(temp.path(), 1).unwrap();
    let b = experiment.execute(temp.path(), 4).unwrap();
    assert!(
        a.runs.iter().all(|r| r.status == RunStatus::Completed),
        "{:?}",
        a.runs
    );
    assert_eq!(
        serde_json::to_vec(&a.runs).unwrap(),
        serde_json::to_vec(&b.runs).unwrap()
    );
    assert!(
        a.runs[0]
            .summary
            .as_ref()
            .unwrap()
            .assumptions
            .iter()
            .any(|a| a.contains("fake"))
    );
}

#[test]
fn accurate_result_levels_promote_and_replay_without_conflating_omitted_tables() {
    let temp = TempDir::new().unwrap();
    let (dsv, universe) = fixture(temp.path());
    let experiment = Experiment::parse(&definition(&dsv, &universe).to_string(), temp.path())
        .unwrap()
        .with_backend_factory(Arc::new(FakeFactory))
        .unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let summary = experiment
        .execute_and_store(temp.path(), &repo, ResultLevel::Summary, 2)
        .unwrap();
    let run = &summary.manifest.runs[0];
    assert_eq!(run.accounting_verified, Some(true));
    assert!(run.table_hashes.is_empty());
    assert!(run.not_produced_by_engine.is_empty());
    assert!(run.not_saved.contains(&"backend_comparisons".to_string()));
    let run_id = run.run_id.clone();
    let standard = experiment
        .promote(
            temp.path(),
            &repo,
            &summary.manifest.execution.id,
            std::slice::from_ref(&run_id),
            ResultLevel::Standard,
            3,
        )
        .unwrap();
    assert_eq!(
        standard.manifest.runs[0]
            .table_hashes
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["account_valuations", "portfolio_valuations"]
    );
    let full = experiment
        .promote(
            temp.path(),
            &repo,
            &summary.manifest.execution.id,
            &[run_id],
            ResultLevel::Full,
            1,
        )
        .unwrap();
    assert_eq!(
        full.manifest.summary_logical_hash,
        summary.manifest.summary_logical_hash
    );
    assert!(full.manifest.runs[0].not_saved.is_empty());
    assert_eq!(full.manifest.runs[0].table_hashes.len(), 8);
    for table in ["adapter_events", "backend_events", "backend_comparisons"] {
        assert!(full.manifest.runs[0].table_hashes.contains_key(table));
    }
    let replay = experiment
        .execute_and_store(temp.path(), &repo, ResultLevel::Summary, 4)
        .unwrap();
    assert_eq!(replay.action, StoreAction::Replayed);
    assert_eq!(
        replay.manifest.runs[0].table_hashes,
        full.manifest.runs[0].table_hashes
    );
}

fn scripted_worker(root: &Path) -> WorkerCommand {
    std::fs::write(
        root.join("worker.py"),
        include_str!("support/fake_worker.py"),
    )
    .unwrap();
    let metadata = root.join("nautilus_trader-2.0.0rc6.dist-info");
    std::fs::create_dir(&metadata).unwrap();
    std::fs::write(
        metadata.join("METADATA"),
        "Metadata-Version: 2.1\nName: nautilus_trader\nVersion: 2.0.0rc6\n",
    )
    .unwrap();
    WorkerCommand::new(test_python(), root.join("worker.py"))
}

#[test]
fn worker_transport_preserves_native_account_comparisons_and_rejects_wrong_version() {
    let temp = TempDir::new().unwrap();
    let (dsv, universe) = fixture(temp.path());
    let worker = scripted_worker(temp.path());
    let experiment = Experiment::parse(&definition(&dsv, &universe).to_string(), temp.path())
        .unwrap()
        .with_backend_factory(Arc::new(worker.clone()))
        .unwrap();
    let report = experiment.execute(temp.path(), 2).unwrap();
    let run = &report.runs[0];
    assert_eq!(run.status, RunStatus::Completed, "{:?}", run.error);
    let comparisons = &run.backend_diagnostics.as_ref().unwrap().comparisons;
    let accounts = comparisons
        .iter()
        .filter(|v| v["event"] == "account_balance")
        .collect::<Vec<_>>();
    assert_eq!(accounts.len(), run.summary.as_ref().unwrap().session_count);
    assert!(accounts.iter().all(|a| a["platform_cash"].is_string()));
    assert!(accounts.iter().all(|a| {
        a["position_deltas"]
            .as_object()
            .unwrap()
            .values()
            .all(|v| v == "0")
    }));
    assert!(accounts.iter().all(|a| a["authority"] == "prajna-account"));
    let mut changed = definition(&dsv, &universe);
    changed["backend"]["version"] = "2.0.0rc7".into();
    let changed = Experiment::parse(&changed.to_string(), temp.path())
        .unwrap()
        .with_backend_factory(Arc::new(worker))
        .unwrap();
    assert!(
        changed
            .execute(temp.path(), 1)
            .unwrap_err()
            .to_string()
            .contains("version mismatch")
    );
}

#[test]
fn worker_failures_are_failed_runs_not_empty_successful_results() {
    use std::time::Duration;
    let original = include_str!("support/fake_worker.py");
    for (script, expected) in [
        ("import sys\nsys.exit(0)\n".to_string(), "EOF"),
        (
            "import json,sys\njson.loads(sys.stdin.readline())\nprint(json.dumps({'type':'error','reason':'test failure'}),flush=True)\n".to_string(),
            "terminal error",
        ),
        (original.replace("        main()", "        main()\n        sys.exit(7)"), "exited"),
        (original.replace("backend-diagnostics.jsonl", "ignored.jsonl"), "No such file"),
        (original.replace("\"account_balance\"", "\"missing_account\""), "missing native account"),
        ("import time\ntime.sleep(60)\n".to_string(), "timeout"),
    ] {
        let temp = TempDir::new().unwrap();
        let (dsv, universe) = fixture(temp.path());
        let mut worker = scripted_worker(temp.path());
        worker.timeout = Duration::from_secs(2);
        std::fs::write(&worker.worker, script).unwrap();
        let experiment = Experiment::parse(&definition(&dsv, &universe).to_string(), temp.path())
            .unwrap()
            .with_backend_factory(Arc::new(RunDeadlineFactory(worker)))
            .unwrap();
        let report = experiment.execute(temp.path(), 1).unwrap();
        let run = &report.runs[0];
        assert_eq!(run.status, RunStatus::Failed, "{expected}: {run:?}");
        assert!(run.error.as_ref().unwrap().contains(expected), "{:?}", run.error);
        assert!(run.summary.is_none());
        assert!(run.fast_event_result.is_none());
        assert!(run.backend_diagnostics.is_none());
        assert!(report.run_time_ms < 5000);
    }
}

#[test]
fn worker_process_grid_replays_all_full_tables_across_pool_sizes() {
    let temp = TempDir::new().unwrap();
    let (dsv, universe) = fixture(temp.path());
    let worker = scripted_worker(temp.path());
    let mut value = definition(&dsv, &universe);
    value["strategy"] = "ma_crossover".into();
    value["parameter_space"] = json!({"grid":{
        "params_version":[1],"short_window":[1,2],"long_window":[3,5]
    }});
    let experiment = Experiment::parse(&value.to_string(), temp.path())
        .unwrap()
        .with_backend_factory(Arc::new(worker.clone()))
        .unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let full = experiment
        .execute_and_store(temp.path(), &repo, ResultLevel::Full, 4)
        .unwrap();
    assert_eq!(full.manifest.runs.len(), 4);
    assert!(
        full.manifest
            .runs
            .iter()
            .all(|r| r.status == RunStatus::Completed)
    );
    let replay = experiment
        .execute_and_store(temp.path(), &repo, ResultLevel::Summary, 1)
        .unwrap();
    assert_eq!(replay.action, StoreAction::Replayed);
    assert_eq!(
        replay.manifest.summary_logical_hash,
        full.manifest.summary_logical_hash
    );
    for (left, right) in full.manifest.runs.iter().zip(&replay.manifest.runs) {
        assert_eq!(left.run_id, right.run_id);
        assert_eq!(left.table_hashes, right.table_hashes);
    }
    let changed = include_str!("support/fake_worker.py").replace(
        "record(\"adapter_construction\", \"transport_test_double\")",
        "record(\"adapter_construction\", \"transport_test_double\", diagnostic_probe=\"changed\")",
    );
    std::fs::write(&worker.worker, changed).unwrap();
    let mismatch = experiment
        .execute_and_store(temp.path(), &repo, ResultLevel::Full, 2)
        .unwrap_err();
    assert!(matches!(
        mismatch,
        prajna_experiment::ExperimentError::ReplayMismatch(_)
    ));
    assert!(mismatch.to_string().contains("adapter_events"));
}

#[test]
fn accurate_resolved_explicit_lots_are_used_by_both_order_sizing_and_packet() {
    let temp = TempDir::new().unwrap();
    let (dsv, universe) = fixture(temp.path());
    let mut value = definition(&dsv, &universe);
    value["fast_event"]["lot_size_source"] = json!({
        "kind":"explicit","values":{"A.SYNTH":"200","B.SYNTH":"200","C.SYNTH":"200"}
    });
    let experiment = Experiment::parse(&value.to_string(), temp.path())
        .unwrap()
        .with_backend_factory(Arc::new(FakeFactory))
        .unwrap();
    let report = experiment.execute(temp.path(), 1).unwrap();
    assert_eq!(
        report.runs[0].status,
        RunStatus::Completed,
        "{:?}",
        report.runs[0].error
    );
}

#[test]
fn accurate_cli_uses_the_same_run_replay_promotion_and_routing_commands() {
    use std::process::Command;
    let temp = TempDir::new().unwrap();
    let (dsv, universe) = fixture(temp.path());
    let worker = scripted_worker(temp.path());
    let repo = temp.path().join("repository");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "--quiet"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let path = temp.path().join("experiment.json");
    let value = definition(&dsv, &universe);
    std::fs::write(&path, value.to_string()).unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
            .arg("run")
            .arg(&path)
            .arg("--lake")
            .arg(temp.path())
            .args(args)
            .current_dir(&repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .output()
            .unwrap()
    };
    let missing = invoke(&[]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("worker"));
    assert!(missing.stdout.is_empty());
    let worker_path = worker.worker.to_str().unwrap();
    let python = test_python().to_string_lossy().into_owned();
    let options = [
        "--backend-python",
        python.as_str(),
        "--backend-worker",
        worker_path,
    ];
    let created = invoke(&options);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let created: Value = serde_json::from_slice(&created.stdout).unwrap();
    assert_eq!(created["action"], "created");
    assert_eq!(created["failed_count"], 0);
    let run_id = Experiment::parse(&value.to_string(), temp.path())
        .unwrap()
        .expand(temp.path())
        .unwrap()
        .remove(0)
        .run_id;
    let mut promotion = options.to_vec();
    promotion.extend(["--level", "full", "--promote", &run_id]);
    let promoted = invoke(&promotion);
    assert!(
        promoted.status.success(),
        "{}",
        String::from_utf8_lossy(&promoted.stderr)
    );
    let promoted: Value = serde_json::from_slice(&promoted.stdout).unwrap();
    assert_eq!(promoted["action"], "promoted");
    assert_eq!(promoted["exe"], created["exe"]);
    let replay = invoke(&options);
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let replay: Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay["action"], "replayed");
    assert_eq!(replay["written_files"], 0);
    let mut unsupported = value;
    unsupported["fast_event"]["sizing"] = "vector_parity".into();
    std::fs::write(&path, unsupported.to_string()).unwrap();
    let rejected = invoke(&options);
    assert_eq!(rejected.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("vector_parity"));
    assert!(rejected.stdout.is_empty());
}

#[test]
fn invalid_backend_identity_is_rejected_before_data_or_worker_execution() {
    let temp = TempDir::new().unwrap();
    for backend in [
        Value::Null,
        json!({"name":"","version":"2.0.0rc6"}),
        json!({"name":"nautilus_trader","version":" "}),
        json!({"name":" nautilus_trader","version":"2.0.0rc6"}),
    ] {
        let mut value = definition("missing-dataset", "missing-universe");
        value["backend"] = backend;
        let error = Experiment::parse(&value.to_string(), temp.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("Backend identity"), "{error}");
    }
    for engine in ["vector", "fast_event"] {
        let mut value = definition("missing-dataset", "missing-universe");
        value["engine"] = engine.into();
        let error = Experiment::parse(&value.to_string(), temp.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("only valid for accurate_event"), "{error}");
    }
}

#[cfg(unix)]
#[ignore = "requires permission to terminate a worker process group; lightweight CI runs with --include-ignored"]
#[test]
fn timed_out_worker_descendants_cannot_hold_pipes_or_outlive_the_run() {
    use std::time::Duration;
    let temp = TempDir::new().unwrap();
    let (dsv, universe) = fixture(temp.path());
    let mut worker = scripted_worker(temp.path());
    worker.timeout = Duration::from_secs(2);
    std::fs::write(
        &worker.worker,
        "import subprocess,sys,time\nsubprocess.Popen([sys.executable,'-c',\"import time,pathlib;time.sleep(5);pathlib.Path('leaked').write_text('descendant survived');time.sleep(1)\"])\ntime.sleep(60)\n",
    )
    .unwrap();
    let experiment = Experiment::parse(&definition(&dsv, &universe).to_string(), temp.path())
        .unwrap()
        .with_backend_factory(Arc::new(RunDeadlineFactory(worker)))
        .unwrap();
    let report = experiment.execute(temp.path(), 1).unwrap();
    assert_eq!(report.runs[0].status, RunStatus::Failed);
    assert!(report.runs[0].error.as_ref().unwrap().contains("timeout"));
    assert!(report.run_time_ms < 5000, "pipe cleanup exceeded deadline");
    std::thread::sleep(Duration::from_millis(5200));
    assert!(
        !temp.path().join("leaked").exists(),
        "owned descendant survived cleanup"
    );
}

#[cfg(unix)]
#[test]
fn worker_python_keeps_its_virtual_environment_when_worker_directory_changes() {
    use std::process::Command;
    let temp = TempDir::new().unwrap();
    let venv = temp.path().join("venv");
    let output = Command::new(test_python())
        .args(["-m", "venv", "--without-pip"])
        .arg(&venv)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let python = venv.join("bin/python");
    let output = Command::new(&python)
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_path('purelib'))",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let packages = String::from_utf8(output.stdout).unwrap();
    let metadata = Path::new(packages.trim()).join("nautilus_trader-fixture.dist-info");
    std::fs::create_dir(&metadata).unwrap();
    std::fs::write(
        metadata.join("METADATA"),
        "Metadata-Version: 2.1\nName: nautilus_trader\nVersion: venv-transport-fixture\n",
    )
    .unwrap();
    let worker = temp.path().join("worker.py");
    std::fs::write(&worker, "# transport path fixture\n").unwrap();
    WorkerCommand::new(python, worker)
        .validate(&BackendIdentity {
            name: "nautilus_trader".into(),
            version: "venv-transport-fixture".into(),
        })
        .unwrap();
}

#[cfg(unix)]
#[ignore = "requires permission to terminate a worker process group; lightweight CI runs with --include-ignored"]
#[test]
fn exited_worker_leader_retains_ownership_until_descendant_pipe_cleanup() {
    use std::time::Duration;
    let temp = TempDir::new().unwrap();
    let (dsv, universe) = fixture(temp.path());
    let mut worker = scripted_worker(temp.path());
    worker.timeout = Duration::from_secs(2);
    let script = include_str!("support/fake_worker.py").replace(
        "                send({\"type\": \"done\"})",
        "                import subprocess\n                subprocess.Popen([sys.executable,'-c',\"import time,pathlib;time.sleep(5);pathlib.Path('leaked').write_text('descendant survived');time.sleep(1)\"])\n                send({\"type\": \"done\"})",
    );
    std::fs::write(&worker.worker, script).unwrap();
    let experiment = Experiment::parse(&definition(&dsv, &universe).to_string(), temp.path())
        .unwrap()
        .with_backend_factory(Arc::new(RunDeadlineFactory(worker)))
        .unwrap();
    let report = experiment.execute(temp.path(), 1).unwrap();
    assert_eq!(report.runs[0].status, RunStatus::Failed);
    assert!(
        report.runs[0]
            .error
            .as_ref()
            .unwrap()
            .contains("stdout completion")
    );
    assert!(report.run_time_ms < 5000);
    std::thread::sleep(Duration::from_millis(5200));
    assert!(
        !temp.path().join("leaked").exists(),
        "exited leader's descendant survived"
    );
}

#[test]
fn accurate_two_vps_keep_independent_fills_and_exact_account_aggregation() {
    let temp = TempDir::new().unwrap();
    let (dsv, universe) = fixture(temp.path());
    let mut value = definition(&dsv, &universe);
    value["fast_event"]["portfolios"] = json!([
        {"id":1,"capital":"400000"}, {"id":2,"capital":"400000"}
    ]);
    let experiment = Experiment::parse(&value.to_string(), temp.path())
        .unwrap()
        .with_backend_factory(Arc::new(FakeFactory))
        .unwrap();
    let report = experiment.execute(temp.path(), 2).unwrap();
    let run = &report.runs[0];
    assert_eq!(run.status, RunStatus::Completed, "{:?}", run.error);
    let ledger = run.fast_event_result.as_ref().unwrap();
    assert!(
        ledger
            .fills
            .iter()
            .any(|f| f.fill.order().portfolio_id().0 == 1)
    );
    assert!(
        ledger
            .fills
            .iter()
            .any(|f| f.fill.order().portfolio_id().0 == 2)
    );
    for session in &ledger.sessions {
        for valuation in [&session.open_after, &session.close] {
            let cash: i128 = valuation
                .portfolios
                .values()
                .map(|p| p.cash.mantissa())
                .sum();
            assert_eq!(
                valuation.account.cash.mantissa(),
                cash + valuation.unallocated_capital.mantissa()
            );
            let equity: i128 = valuation
                .portfolios
                .values()
                .map(|p| p.equity.mantissa())
                .sum();
            assert_eq!(
                valuation.account.equity.mantissa(),
                equity + valuation.unallocated_capital.mantissa()
            );
        }
    }
}
