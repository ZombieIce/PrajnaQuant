use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
};

use arrow_array::{Array, Float64Array, RecordBatch, StringArray};
use parquet::arrow::{ArrowWriter, arrow_reader::ParquetRecordBatchReaderBuilder};
use serde_json::{Value, json};
use tempfile::TempDir;

#[path = "../examples/support/fixture.rs"]
mod fixture;

#[path = "support/multi_venue.rs"]
mod multi_venue;

struct Example {
    _temp: TempDir,
    lake: PathBuf,
    definition: PathBuf,
    repo: PathBuf,
}

fn isolated(command: &mut Command) -> &mut Command {
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
}

impl Example {
    fn new() -> Self {
        Self::build(|lake, definition| {
            fixture::prepare(lake, definition).unwrap();
        })
    }

    /// Synthetic XSHG+XSHE pool; `missing_xshe_session` breaks the shared axis.
    fn multi_venue(missing_xshe_session: Option<&str>) -> Self {
        Self::build(|lake, definition| {
            multi_venue::prepare(lake, definition, missing_xshe_session);
        })
    }

    fn build(prepare: impl FnOnce(&Path, &Path)) -> Self {
        let temp = TempDir::new().unwrap();
        let lake = temp.path().join("lake");
        let definition = temp.path().join("experiment.json");
        let repo = temp.path().join("repository");
        fs::create_dir(&repo).unwrap();
        prepare(&lake, &definition);
        let example = Self {
            _temp: temp,
            lake,
            definition,
            repo,
        };
        example.git(&["init", "--quiet"]);
        example.commit("initial code");
        example
    }

    fn git(&self, args: &[&str]) {
        let output = isolated(Command::new("git").args(args).current_dir(&self.repo))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn commit(&self, contents: &str) {
        fs::write(self.repo.join("code.txt"), contents).unwrap();
        self.git(&["add", "code.txt"]);
        self.git(&[
            "-c",
            "user.name=CLI Test",
            "-c",
            "user.email=cli@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            contents,
        ]);
    }

    fn command(&self, args: &[&str]) -> Output {
        isolated(
            Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
                .args(args)
                .arg("--lake")
                .arg(&self.lake)
                .current_dir(&self.repo),
        )
        .env("RAYON_NUM_THREADS", "2")
        .output()
        .unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_prajna-experiment"));
        isolated(
            command
                .arg("run")
                .arg(&self.definition)
                .arg("--lake")
                .arg(&self.lake)
                .args(args)
                .current_dir(&self.repo),
        )
        .env("RAYON_NUM_THREADS", "2")
        .output()
        .unwrap()
    }

    fn result(&self, exp: &str, exe: &str, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_prajna-experiment"));
        isolated(
            command
                .arg("result")
                .arg(exp)
                .arg(exe)
                .arg("--lake")
                .arg(&self.lake)
                .args(args)
                .current_dir(&self.repo),
        )
        .env("RAYON_NUM_THREADS", "2")
        .output()
        .unwrap()
    }

    fn promote(&self, level: &str, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_prajna-experiment"));
        isolated(
            command
                .arg("promote")
                .arg(&self.definition)
                .arg("--lake")
                .arg(&self.lake)
                .arg("--level")
                .arg(level)
                .args(args)
                .current_dir(&self.repo),
        )
        .env("RAYON_NUM_THREADS", "2")
        .output()
        .unwrap()
    }

    fn directory(&self, output: &Value) -> PathBuf {
        let hex = |key: &str| output[key].as_str().unwrap().rsplit(':').next().unwrap();
        self.lake
            .join("experiments")
            .join(hex("exp"))
            .join("executions")
            .join(hex("exe"))
    }

    fn write_definition(&self, change: impl FnOnce(&mut Value)) {
        let mut definition: Value =
            serde_json::from_slice(&fs::read(&self.definition).unwrap()).unwrap();
        change(&mut definition);
        fs::write(&self.definition, serde_json::to_vec(&definition).unwrap()).unwrap();
    }
}

fn status(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn json_output(output: Output, expected: i32) -> Value {
    status(&output, expected);
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn python_strategy_definition_is_identified_but_cli_requires_python_host() {
    let example = Example::new();
    example.write_definition(|definition| {
        definition["strategy"] = json!("research.rotation@1");
        definition["parameter_space"] = json!({"list": [{"window": 5, "weight": 0.5}]});
    });
    let definition = fs::read_to_string(&example.definition).unwrap();
    let experiment = prajna_experiment::Experiment::parse(&definition, &example.lake).unwrap();
    assert!(experiment.id().unwrap().starts_with("exp:sha256:"));
    let canonical: Value = serde_json::from_slice(&experiment.canonical_json().unwrap()).unwrap();
    assert_eq!(canonical["strategy"], "research.rotation@1");
    let output = example.run(&[]);
    status(&output, 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires Python host"));
    assert!(!example.lake.join("experiments").exists());
}

#[test]
fn python_parameter_identity_preserves_json_types_and_nested_values() {
    let example = Example::new();
    let mut definition: Value =
        serde_json::from_slice(&fs::read(&example.definition).unwrap()).unwrap();
    definition["strategy"] = json!("research.rotation@1");
    let identify = |parameters: Value| {
        let mut definition = definition.clone();
        definition["parameter_space"] = json!({"list": [parameters]});
        let experiment =
            prajna_experiment::Experiment::parse(&definition.to_string(), &example.lake).unwrap();
        (
            experiment.id().unwrap(),
            experiment.run_definitions(&example.lake).unwrap()[0]
                .run_id
                .clone(),
        )
    };
    let numeric = identify(json!({"nested": [0.5, true, null]}));
    let text = identify(json!({"nested": ["3fe0000000000000", true, null]}));
    assert_ne!(numeric, text);
    assert_ne!(
        identify(json!({"value": "text"})),
        identify(json!({"value": {"string": "text"}})),
    );
    assert_ne!(
        identify(json!({"value": 0.5})),
        identify(json!({"value": {"f64": "3fe0000000000000"}})),
    );
    assert_ne!(
        identify(json!({"value": 0.0})),
        identify(json!({"value": -0.0}))
    );
}

#[test]
fn readme_example_creates_replays_promotes_and_diffs_two_executions() {
    let example = Example::new();
    let first = json_output(example.run(&[]), 0);
    assert_eq!(first["action"], "created");
    assert_eq!(first["reproducible"], true);
    assert_eq!(first["run_count"], 2);
    assert_eq!(first["failed_count"], 0);
    assert_eq!(first["threads"], 2); // Rayon default respects RAYON_NUM_THREADS.
    assert!(first["cache"]["compute_count"].as_u64().unwrap() > 0);
    assert!(first["written_bytes"].as_u64().unwrap() > 0);
    assert_eq!(first["written_files"], 3); // definition, manifest, Summary.
    let directory = example.directory(&first);
    assert!(!directory.join("runs").exists());
    let original_manifest = fs::read(directory.join("execution.json")).unwrap();
    let replay = json_output(example.run(&["--threads", "1"]), 0);
    assert_eq!(replay["action"], "replayed");
    assert_eq!(replay["exe"], first["exe"]);
    assert_eq!(replay["cache"]["compute_count"], 0);
    assert!(replay["cache"]["hit_count"].as_u64().unwrap() > 0);
    assert_eq!(replay["written_bytes"], 0);
    assert_eq!(replay["written_files"], 0);
    assert_eq!(
        fs::read(directory.join("execution.json")).unwrap(),
        original_manifest
    );
    let manifest: Value = serde_json::from_slice(&original_manifest).unwrap();
    let run_id = manifest["runs"][0]["run_id"].as_str().unwrap();
    let promoted = json_output(example.run(&["--level", "full", "--promote", run_id]), 0);
    assert_eq!(promoted["action"], "promoted");
    assert_eq!(promoted["exe"], first["exe"]);
    assert_eq!(promoted["cache"]["compute_count"], 0);
    assert_eq!(promoted["written_files"], 8); // seven tables and replaced manifest.
    let updated: Value =
        serde_json::from_slice(&fs::read(directory.join("execution.json")).unwrap()).unwrap();
    assert_eq!(updated["runs"][0]["result_level"], "full");
    assert_eq!(updated["runs"][1]["result_level"], "summary");
    assert_eq!(updated["threads"], manifest["threads"]);
    json_output(example.run(&[]), 0); // replay verifies promoted tables too.
    example.commit("second code revision");
    let second = json_output(example.run(&[]), 0);
    assert_eq!(second["exp"], first["exp"]);
    assert_ne!(second["exe"], first["exe"]);
    let a = first["exe"].as_str().unwrap();
    let b = second["exe"].as_str().unwrap();
    // Full tables exist on only one side: #102 includes availability differences.
    let availability = json_output(example.command(&["diff", a, b]), 4);
    assert_eq!(availability["summary_changed"], false);
    let run_ids = updated["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|run| run["result_level"] == "full")
        .map(|run| run["run_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    let mut args = vec!["--level", "full", "--promote"];
    args.extend(run_ids);
    json_output(example.run(&args), 0);
    let equal = json_output(example.command(&["diff", a, b]), 0);
    assert!(
        equal["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|run| run["table_differences"] == json!([]))
    );
    assert_eq!(equal["summary_changed"], false);
    // Dirty provenance is saved explicitly and yields another Execution.
    fs::write(example.repo.join("code.txt"), "dirty code").unwrap();
    let dirty = json_output(example.run(&[]), 0);
    assert_eq!(dirty["reproducible"], false);
    assert_ne!(dirty["exe"], second["exe"]);
}

#[test]
fn cli_definition_expansion_provenance_and_promotion_errors_have_documented_codes() {
    let example = Example::new();
    status(&example.run(&["--threads", "0"]), 2);
    status(&example.run(&["--level", "unknown"]), 2);
    let unknown_run = format!("run:sha256:{}", "f".repeat(64));
    status(&example.run(&["--promote", &unknown_run]), 2);
    // A valid promotion request must not create a missing Execution.
    status(
        &example.run(&["--level", "full", "--promote", &unknown_run]),
        1,
    );
    assert!(!example.lake.join("experiments").exists());
    example.write_definition(|value| value["parameter_space"]["grid"]["short"] = json!([40]));
    status(&example.run(&[]), 2);
    assert!(!example.lake.join("experiments").exists());
    example.write_definition(|value| value["parameter_space"]["grid"]["short"] = json!([5]));
    let first = json_output(example.run(&[]), 0);
    status(
        &example.run(&["--level", "full", "--promote", &unknown_run]),
        2,
    );
    let manifest: Value = serde_json::from_slice(
        &fs::read(example.directory(&first).join("execution.json")).unwrap(),
    )
    .unwrap();
    let run_id = manifest["runs"][0]["run_id"].as_str().unwrap();
    status(
        &example.run(&["--level", "full", "--promote", run_id, run_id]),
        2,
    );
    // Multiple Run IDs in one option are promoted together.
    let other = manifest["runs"][1]["run_id"].as_str().unwrap();
    json_output(
        example.run(&["--level", "standard", "--promote", run_id, other]),
        0,
    );
    status(
        &example.run(&["--level", "standard", "--promote", run_id]),
        2,
    );
    example.write_definition(|value| value["sessions_per_year"] = json!(0));
    status(&example.run(&[]), 2);
    fs::write(&example.definition, "{bad json}").unwrap();
    status(&example.run(&[]), 2);
    fixture::prepare(&example.lake, &example.definition).unwrap();
    let output = isolated(
        Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
            .arg("run")
            .arg(&example.definition)
            .arg("--lake")
            .arg(&example.lake)
            .current_dir(example._temp.path()),
    )
    .output()
    .unwrap();
    status(&output, 1); // no git repository for provenance.
}

#[test]
fn replay_corruption_is_code_three_and_leaves_stored_files_untouched() {
    let example = Example::new();
    let output = json_output(example.run(&[]), 0);
    let directory = example.directory(&output);
    let manifest_path = directory.join("execution.json");
    let manifest_bytes = fs::read(&manifest_path).unwrap();
    let summary = directory.join("summary.parquet");
    let manifest: Value = serde_json::from_slice(&manifest_bytes).unwrap();
    let run_id = manifest["runs"][0]["run_id"].as_str().unwrap();
    fs::write(&summary, b"invalid parquet").unwrap();
    let output = example.run(&[]);
    status(&output, 3);
    assert!(output.stdout.is_empty());
    status(&example.run(&["--level", "full", "--promote", run_id]), 3);
    assert_eq!(fs::read(summary).unwrap(), b"invalid parquet");
    assert_eq!(fs::read(manifest_path).unwrap(), manifest_bytes);
    assert!(!directory.join("runs").exists());
}

/// Deliberately create a coherent alternative Summary to exercise diff field reporting.
fn change_summary(directory: &Path, make_null: bool) {
    let path = directory.join("summary.parquet");
    let reader = ParquetRecordBatchReaderBuilder::try_new(fs::File::open(&path).unwrap())
        .unwrap()
        .build()
        .unwrap();
    let batch = reader.into_iter().next().unwrap().unwrap();
    let schema = batch.schema();
    let metric = schema.index_of("total_return").unwrap();
    let payload = schema.index_of("summary_json").unwrap();
    let values = batch
        .column(metric)
        .as_any()
        .downcast_ref::<Float64Array>()
        .unwrap();
    let mut new_values = values.iter().collect::<Vec<_>>();
    new_values[0] = if make_null {
        None
    } else {
        Some(new_values[0].unwrap() + 0.25)
    };
    let summaries = batch
        .column(payload)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let mut new_summaries = summaries
        .iter()
        .map(|value| value.map(str::to_owned))
        .collect::<Vec<_>>();
    let mut summary: Value = serde_json::from_str(new_summaries[0].as_ref().unwrap()).unwrap();
    summary["total_return"] = json!(new_values[0]);
    new_summaries[0] = Some(summary.to_string());
    let mut columns = batch.columns().to_vec();
    columns[metric] = Arc::new(Float64Array::from(new_values));
    columns[payload] = Arc::new(StringArray::from(new_summaries));
    let changed = RecordBatch::try_new(schema.clone(), columns).unwrap();
    let mut writer =
        ArrowWriter::try_new(fs::File::create(&path).unwrap(), schema.clone(), None).unwrap();
    writer.write(&changed).unwrap();
    writer.close().unwrap();
    let hash = prajna_data::logical_hash("summary", &schema, &[changed], &["run_id"]).unwrap();
    let path = directory.join("execution.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["summary_logical_hash"] = json!(hash);
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

#[test]
fn diff_reports_metric_and_null_changes_and_rejects_cross_experiment_comparisons() {
    let example = Example::new();
    let first = json_output(example.run(&[]), 0);
    example.commit("second revision");
    let second = json_output(example.run(&[]), 0);
    let a = first["exe"].as_str().unwrap();
    let b = second["exe"].as_str().unwrap();
    json_output(example.command(&["diff", a, b]), 0);
    let directory = example.directory(&second);
    change_summary(&directory, false);
    let diff = json_output(example.command(&["diff", a, b]), 4);
    assert_eq!(diff["summary_changed"], true);
    assert!(
        diff["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["metric_deltas"]["total_return"] == json!(0.25))
    );
    change_summary(&directory, true);
    assert_eq!(
        json_output(example.command(&["diff", a, b]), 4)["summary_changed"],
        true
    );
    example.write_definition(|definition| definition["sessions_per_year"] = json!(365));
    let other = json_output(example.run(&[]), 0);
    let output = example.command(&["diff", a, other["exe"].as_str().unwrap()]);
    status(&output, 5);
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("different Experiments"));
    status(&example.command(&["diff", a, "exe:sha256:invalid"]), 1);
}

#[test]
fn help_lists_the_exit_code_contract() {
    for args in [
        vec!["--help"],
        vec!["run", "--help"],
        vec!["diff", "--help"],
        vec!["result", "--help"],
        vec!["promote", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
            .args(args)
            .output()
            .unwrap();
        status(&output, 0);
        let help = String::from_utf8(output.stdout).unwrap();
        for code in [
            "0 success",
            "1 I/O",
            "2 CLI",
            "3 replay",
            "4 diff",
            "5 diff",
            "6 promote",
        ] {
            assert!(help.contains(code), "{help}");
        }
    }
}

#[test]
fn full_new_execution_reports_failed_runs_without_failing_the_invocation() {
    let example = Example::new();
    example.write_definition(|definition| definition["costs"]["commission_rate"] = json!(100.0));
    let output = json_output(example.run(&["--level", "full"]), 0);
    assert_eq!(output["action"], "created");
    assert_eq!(output["run_count"], 2);
    assert_eq!(output["failed_count"], 2);
    assert_eq!(output["written_files"], 3);
    json_output(example.run(&["--level", "full"]), 0);
}

#[test]
fn stored_definition_and_promotion_metadata_corruption_use_replay_inconsistency_code() {
    let example = Example::new();
    let result = json_output(example.run(&[]), 0);
    let execution_directory = example.directory(&result);
    let manifest_path = execution_directory.join("execution.json");
    let original_manifest = fs::read(&manifest_path).unwrap();
    let manifest: Value = serde_json::from_slice(&original_manifest).unwrap();
    let run_id = manifest["runs"][0]["run_id"].as_str().unwrap();
    let definition_path = execution_directory
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("experiment.json");
    let original_definition = fs::read(&definition_path).unwrap();
    fs::write(&definition_path, b"{}").unwrap();
    status(&example.run(&[]), 3);
    assert_eq!(fs::read(&definition_path).unwrap(), b"{}");
    assert_eq!(fs::read(&manifest_path).unwrap(), original_manifest);
    fs::write(&definition_path, original_definition).unwrap();
    fs::write(&manifest_path, b"{bad metadata}").unwrap();
    status(&example.run(&["--level", "full", "--promote", run_id]), 3);
    assert_eq!(fs::read(&manifest_path).unwrap(), b"{bad metadata}");
    assert!(!execution_directory.join("runs").exists());
}

#[test]
fn fast_event_cli_summary_promotes_and_verifies_saved_details_on_replay() {
    let example = Example::new();
    example.write_definition(|value| {
        value["engine"]=json!("fast_event");
        value["fast_event"]=json!({"currency":"CNY","initial_cash":"100000","sizing":"lot","rebalance":"full_target","minimum_commission":"100","portfolios":[{"id":1,"capital":"100000"}],"lot_size_source":{"kind":"instrument"}});
    });
    let first = json_output(example.run(&["--threads", "1"]), 0);
    assert_eq!(first["failed_count"], 0);
    let directory = example.directory(&first);
    assert!(!directory.join("runs").exists());
    let manifest: Value =
        serde_json::from_slice(&fs::read(directory.join("execution.json")).unwrap()).unwrap();
    let id = manifest["runs"][0]["run_id"].as_str().unwrap();
    assert!(
        manifest["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["accounting_verified"] == true)
    );
    let promoted = json_output(
        example.run(&["--level", "full", "--promote", id, "--threads", "4"]),
        0,
    );
    assert_eq!(promoted["exe"], first["exe"]);
    let replay = json_output(example.run(&["--threads", "2"]), 0);
    assert_eq!(replay["action"], "replayed");
    let fill_path = directory
        .join("runs")
        .join(id.rsplit(':').next().unwrap())
        .join("fills.parquet");
    let original = fs::read(&fill_path).unwrap();
    fs::write(&fill_path, b"corrupt saved fills").unwrap();
    status(&example.run(&["--threads", "4"]), 3);
    assert_eq!(fs::read(&fill_path).unwrap(), b"corrupt saved fills");
    fs::write(fill_path, original).unwrap();
    json_output(example.run(&[]), 0);
}

fn tree(root: &Path) -> Vec<(PathBuf, u64)> {
    fn walk(path: &Path, items: &mut Vec<(PathBuf, u64)>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(&path, items);
            } else {
                items.push((path.clone(), fs::metadata(&path).unwrap().len()));
            }
        }
    }
    let mut items = Vec::new();
    walk(root, &mut items);
    items.sort();
    items
}

#[test]
fn result_reads_saved_standard_series_without_writing() {
    let example = Example::new();
    let first = json_output(example.run(&["--level", "standard", "--threads", "1"]), 0);
    let exp = first["exp"].as_str().unwrap();
    let exe = first["exe"].as_str().unwrap();
    let directory = example.directory(&first);
    let manifest: Value =
        serde_json::from_slice(&fs::read(directory.join("execution.json")).unwrap()).unwrap();
    let run_ids = manifest["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["run_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(run_ids.len(), 2);
    let before = tree(&example.lake);

    let result = json_output(example.result(exp, exe, &[]), 0);
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["data"]["exp"], exp);
    assert_eq!(result["data"]["exe"], exe);
    assert_eq!(result["data"]["default_result_level"], "standard");
    let runs = result["data"]["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    for (run, run_id) in runs.iter().zip(&run_ids) {
        assert_eq!(run["run_id"], *run_id);
        assert_eq!(run["status"], "completed");
        assert!(run["summary"].is_object());
        assert_eq!(run["series"]["sessions"].as_array().unwrap().len(), 252);
        assert!(
            run["tables"]["saved"]
                .as_array()
                .unwrap()
                .iter()
                .any(|table| table == "sessions")
        );
        assert!(
            run["tables"]["not_produced_by_engine"]
                .as_array()
                .unwrap()
                .iter()
                .any(|table| table == "orders")
        );
    }

    let (_, batches) = prajna_experiment::read_run_table(
        &example.lake,
        exp,
        exe,
        &run_ids[0],
        prajna_experiment::ResultLevel::Standard,
        "sessions",
    )
    .unwrap();
    let payloads = batches[0]
        .column_by_name("payload")
        .unwrap()
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let stored: Value = serde_json::from_str(payloads.value(0)).unwrap();
    assert_eq!(
        runs[0]["series"]["sessions"][0]["session_date"],
        stored["session_date"]
    );
    assert_eq!(runs[0]["series"]["sessions"][0]["nav"], stored["nav"]);

    let filtered = json_output(example.result(exp, exe, &["--run", run_ids[0].as_str()]), 0);
    assert_eq!(filtered["data"]["runs"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["data"]["runs"][0]["run_id"], run_ids[0]);
    assert_eq!(before, tree(&example.lake));
}

#[test]
fn result_reports_summary_series_as_not_saved() {
    let example = Example::new();
    let first = json_output(example.run(&[]), 0);
    let result = json_output(
        example.result(
            first["exp"].as_str().unwrap(),
            first["exe"].as_str().unwrap(),
            &[],
        ),
        0,
    );
    let run = &result["data"]["runs"][0];
    assert_eq!(run["result_level"], "summary");
    assert!(run["series"].as_object().unwrap().is_empty());
    assert!(run["summary"].is_object());
    assert!(
        run["tables"]["not_saved"]
            .as_array()
            .unwrap()
            .iter()
            .any(|table| table == "sessions")
    );
}

#[test]
fn result_rejects_corrupted_saved_series() {
    let example = Example::new();
    let first = json_output(example.run(&["--level", "standard"]), 0);
    let directory = example.directory(&first);
    let manifest: Value =
        serde_json::from_slice(&fs::read(directory.join("execution.json")).unwrap()).unwrap();
    let run_id = manifest["runs"][0]["run_id"].as_str().unwrap();
    let path = directory
        .join("runs")
        .join(run_id.rsplit(':').next().unwrap())
        .join("sessions.parquet");
    fs::write(&path, b"not a parquet file").unwrap();

    let output = example.result(
        first["exp"].as_str().unwrap(),
        first["exe"].as_str().unwrap(),
        &[],
    );
    status(&output, 1);
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[test]
fn result_reads_fast_event_account_valuations() {
    let example = Example::new();
    example.write_definition(|value| {
        value["engine"] = json!("fast_event");
        value["fast_event"] = json!({"currency":"CNY","initial_cash":"100000","sizing":"lot","rebalance":"full_target","minimum_commission":"100","portfolios":[{"id":1,"capital":"100000"}],"lot_size_source":{"kind":"instrument"}});
    });
    let first = json_output(example.run(&["--level", "standard", "--threads", "1"]), 0);
    let result = json_output(
        example.result(
            first["exp"].as_str().unwrap(),
            first["exe"].as_str().unwrap(),
            &[],
        ),
        0,
    );
    let run = &result["data"]["runs"][0];
    assert_eq!(run["status"], "completed");
    assert_eq!(run["result_level"], "standard");
    assert_eq!(
        run["series"]["account_valuations"]
            .as_array()
            .unwrap()
            .len(),
        504
    );
    assert!(run["series"].as_object().unwrap().get("sessions").is_none());
    assert!(
        !run["tables"]["not_saved"]
            .as_array()
            .unwrap()
            .iter()
            .any(|table| table == "account_valuations")
    );
    let directory = example.directory(&first);
    let manifest: Value =
        serde_json::from_slice(&fs::read(directory.join("execution.json")).unwrap()).unwrap();
    let run_id = manifest["runs"][0]["run_id"].as_str().unwrap();
    let (_, batches) = prajna_experiment::read_run_table(
        &example.lake,
        first["exp"].as_str().unwrap(),
        first["exe"].as_str().unwrap(),
        run_id,
        prajna_experiment::ResultLevel::Standard,
        "account_valuations",
    )
    .unwrap();
    let mut stored_rows = Vec::new();
    for batch in &batches {
        let payloads = batch
            .column_by_name("payload")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        for row in 0..batch.num_rows() {
            stored_rows.push(serde_json::from_str::<Value>(payloads.value(row)).unwrap());
        }
    }
    let served_rows = run["series"]["account_valuations"].as_array().unwrap();
    assert_eq!(stored_rows.len(), 504);
    assert_eq!(served_rows.len(), stored_rows.len());
    for (served, stored) in served_rows.iter().zip(&stored_rows) {
        assert_eq!(served["cash"], stored["cash"]);
        assert_eq!(served["equity"], stored["equity"]);
    }
    let (_, summary_batches) = prajna_experiment::read_run_table(
        &example.lake,
        first["exp"].as_str().unwrap(),
        first["exe"].as_str().unwrap(),
        run_id,
        prajna_experiment::ResultLevel::Summary,
        "summary",
    )
    .unwrap();
    let summary_batch = summary_batches
        .iter()
        .find(|batch| batch.num_rows() > 0)
        .unwrap();
    let summary_json = summary_batch
        .column_by_name("summary_json")
        .unwrap()
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let stored_summary: Value = serde_json::from_str(summary_json.value(0)).unwrap();
    assert_eq!(run["summary"], stored_summary);
}

#[test]
fn promote_command_upgrades_an_existing_summary_execution() {
    let example = Example::new();
    let summary = json_output(example.run(&[]), 0);
    let promoted = json_output(example.promote("standard", &[]), 0);
    assert_eq!(promoted["exp"], summary["exp"]);
    assert_eq!(promoted["exe"], summary["exe"]);
    assert_eq!(promoted["action"], "promoted");

    let result = json_output(
        example.result(
            summary["exp"].as_str().unwrap(),
            summary["exe"].as_str().unwrap(),
            &[],
        ),
        0,
    );
    assert_eq!(result["data"]["default_result_level"], "summary");
    assert_eq!(result["data"]["runs"][0]["result_level"], "standard");
    assert!(
        result["data"]["runs"][0]["series"]["sessions"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
    let replay = json_output(example.promote("standard", &[]), 0);
    assert_eq!(replay["exp"], summary["exp"]);
    assert_eq!(replay["exe"], summary["exe"]);
    assert_eq!(replay["action"], "replayed");
}

#[test]
fn promote_requires_an_existing_execution_and_a_higher_level() {
    let example = Example::new();
    status(&example.promote("standard", &[]), 6);
    json_output(example.run(&[]), 0);
    status(&example.promote("summary", &[]), 2);
}

#[test]
fn result_rejects_a_corrupted_non_nav_saved_table() {
    let example = Example::new();
    let first = json_output(example.run(&["--level", "standard"]), 0);
    let directory = example.directory(&first);
    let manifest: Value =
        serde_json::from_slice(&fs::read(directory.join("execution.json")).unwrap()).unwrap();
    let run_id = manifest["runs"][0]["run_id"].as_str().unwrap();
    let run_dir = directory
        .join("runs")
        .join(run_id.rsplit(':').next().unwrap());
    fs::copy(
        run_dir.join("sessions.parquet"),
        run_dir.join("executions.parquet"),
    )
    .unwrap();

    let output = example.result(
        first["exp"].as_str().unwrap(),
        first["exe"].as_str().unwrap(),
        &[],
    );
    status(&output, 1);
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("executions"));
}

#[test]
fn result_reports_missing_and_malformed_identities() {
    let example = Example::new();
    status(&example.result("exp:sha256:xyz", "exe:sha256:abc", &[]), 2);

    let exp = format!("exp:sha256:{}", "a".repeat(64));
    let exe = format!("exe:sha256:{}", "b".repeat(64));
    let output = example.result(&exp, &exe, &[]);
    status(&output, 1);
    assert!(output.stdout.is_empty());

    let first = json_output(example.run(&[]), 0);
    let missing_run = format!("run:sha256:{}", "c".repeat(64));
    let output = example.result(
        first["exp"].as_str().unwrap(),
        first["exe"].as_str().unwrap(),
        &["--run", &missing_run],
    );
    status(&output, 1);
    assert!(output.stdout.is_empty());
}

#[test]
fn result_filters_output_but_still_verifies_every_run() {
    let example = Example::new();
    let first = json_output(example.run(&["--level", "standard"]), 0);
    let exp = first["exp"].as_str().unwrap();
    let exe = first["exe"].as_str().unwrap();
    let directory = example.directory(&first);
    let manifest: Value =
        serde_json::from_slice(&fs::read(directory.join("execution.json")).unwrap()).unwrap();
    let runs = manifest["runs"].as_array().unwrap();
    assert!(runs.len() >= 2);
    let returned = runs[0]["run_id"].as_str().unwrap();
    let damaged = runs[1]["run_id"].as_str().unwrap();
    let damaged_dir = directory
        .join("runs")
        .join(damaged.rsplit(':').next().unwrap());
    fs::copy(
        damaged_dir.join("executions.parquet"),
        damaged_dir.join("sessions.parquet"),
    )
    .unwrap();

    let output = example.result(exp, exe, &["--run", returned]);
    status(&output, 1);
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("sessions"));
}

#[test]
fn result_rejects_a_parsable_nav_table_hash_mismatch() {
    let example = Example::new();
    let first = json_output(example.run(&["--level", "standard"]), 0);
    let directory = example.directory(&first);
    let manifest: Value =
        serde_json::from_slice(&fs::read(directory.join("execution.json")).unwrap()).unwrap();
    let run_id = manifest["runs"][0]["run_id"].as_str().unwrap();
    let run_dir = directory
        .join("runs")
        .join(run_id.rsplit(':').next().unwrap());
    fs::copy(
        run_dir.join("executions.parquet"),
        run_dir.join("sessions.parquet"),
    )
    .unwrap();

    let output = example.result(
        first["exp"].as_str().unwrap(),
        first["exe"].as_str().unwrap(),
        &[],
    );
    status(&output, 1);
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("sessions"));
}

#[test]
fn promote_with_only_failed_runs_returns_replayed_without_writing_tables() {
    let example = Example::new();
    example.write_definition(|definition| definition["costs"]["commission_rate"] = json!(100.0));
    let summary = json_output(example.run(&[]), 0);
    assert_eq!(summary["run_count"], 2);
    assert_eq!(summary["failed_count"], 2);

    let promoted = json_output(example.promote("standard", &[]), 0);
    assert_eq!(promoted["action"], "replayed");
    assert_eq!(promoted["failed_count"], 2);
    let directory = example.directory(&summary);
    assert!(!directory.join("runs").exists());

    let result = json_output(
        example.result(
            summary["exp"].as_str().unwrap(),
            summary["exe"].as_str().unwrap(),
            &[],
        ),
        0,
    );
    let runs = result["data"]["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    for run in runs {
        assert_eq!(run["status"], "failed");
        assert_eq!(run["result_level"], "summary");
        assert_eq!(run["summary"], Value::Null);
        assert!(run["series"].as_object().unwrap().is_empty());
        assert!(
            run["tables"]["not_saved"]
                .as_array()
                .unwrap()
                .iter()
                .any(|table| table == "sessions")
        );
    }
}

#[test]
fn promote_no_op_fails_closed_on_corrupted_saved_tables() {
    let example = Example::new();
    let first = json_output(example.run(&["--level", "standard"]), 0);
    let directory = example.directory(&first);
    let manifest: Value =
        serde_json::from_slice(&fs::read(directory.join("execution.json")).unwrap()).unwrap();
    let run_id = manifest["runs"][0]["run_id"].as_str().unwrap();
    let run_dir = directory
        .join("runs")
        .join(run_id.rsplit(':').next().unwrap());
    fs::copy(
        run_dir.join("executions.parquet"),
        run_dir.join("sessions.parquet"),
    )
    .unwrap();

    let output = example.promote("standard", &[]);
    status(&output, 3);
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("sessions"));
}

#[test]
fn promote_mixed_completed_and_failed_runs_persists_completed_only() {
    let example = Example::new();
    example.write_definition(|definition| {
        definition["costs"]["commission_rate"] = json!(0.5);
        definition["parameter_space"]["grid"]["top_k"] = json!([3, 64]);
    });
    let summary = json_output(example.run(&[]), 0);
    assert_eq!(summary["run_count"], 2);
    assert_eq!(summary["failed_count"], 1);

    let promoted = json_output(example.promote("standard", &[]), 0);
    assert_eq!(promoted["action"], "promoted");
    assert_eq!(promoted["failed_count"], 1);

    let result = json_output(
        example.result(
            summary["exp"].as_str().unwrap(),
            summary["exe"].as_str().unwrap(),
            &[],
        ),
        0,
    );
    assert_eq!(result["data"]["default_result_level"], "summary");
    let runs = result["data"]["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    let completed = runs
        .iter()
        .find(|run| run["status"] == "completed")
        .unwrap();
    assert_eq!(completed["result_level"], "standard");
    assert!(completed["summary"].is_object());
    assert_eq!(
        completed["series"]["sessions"].as_array().unwrap().len(),
        252
    );
    let failed = runs.iter().find(|run| run["status"] == "failed").unwrap();
    assert_eq!(failed["result_level"], "summary");
    assert_eq!(failed["summary"], Value::Null);
    assert!(failed["series"].as_object().unwrap().is_empty());
    for table in ["sessions", "executions", "corporate_actions"] {
        assert!(
            failed["tables"]["not_saved"]
                .as_array()
                .unwrap()
                .iter()
                .any(|saved| saved == table),
            "{failed}"
        );
    }

    let replay = json_output(example.promote("standard", &[]), 0);
    assert_eq!(replay["action"], "replayed");
    assert_eq!(replay["failed_count"], 1);
    assert_eq!(replay["written_files"], 0);
}

#[test]
fn promote_existing_directory_without_manifest_is_not_no_execution() {
    let example = Example::new();
    let first = json_output(example.run(&[]), 0);
    let directory = example.directory(&first);
    fs::remove_file(directory.join("execution.json")).unwrap();

    let output = example.promote("standard", &[]);
    status(&output, 3);
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("execution.json"));
}

fn execution_manifest(example: &Example, output: &Value) -> Value {
    serde_json::from_slice(&fs::read(example.directory(output).join("execution.json")).unwrap())
        .unwrap()
}

#[test]
fn multi_venue_fast_event_cli_matches_independent_hand_calculated_golden() {
    let example = Example::multi_venue(None);
    let first = json_output(example.run(&["--level", "standard", "--threads", "1"]), 0);
    assert_eq!(first["action"], "created");
    assert_eq!(first["run_count"], 1);
    assert_eq!(first["failed_count"], 0);

    let manifest = execution_manifest(&example, &first);
    assert_eq!(manifest["runs"][0]["status"], "completed");
    let definition: Value =
        serde_json::from_slice(&fs::read(&example.definition).unwrap()).unwrap();
    let experiment =
        prajna_experiment::Experiment::parse(&definition.to_string(), &example.lake).unwrap();
    let runs = experiment.run_definitions(&example.lake).unwrap();
    assert_eq!(runs[0].run_spec["venues"], json!(["XSHE", "XSHG"]));
    assert!(runs[0].run_spec.get("venue").is_none());
    assert_eq!(
        runs[0].run_spec["engine"],
        json!({"kind": "fast_event", "semantic_version": "fast_event@13"})
    );
    assert_eq!(manifest["runs"][0]["run_id"], json!(runs[0].run_id));

    let replay = json_output(example.run(&["--threads", "2"]), 0);
    assert_eq!(replay["action"], "replayed");
    assert_eq!(replay["exp"], first["exp"]);
    assert_eq!(replay["exe"], first["exe"]);

    let result = json_output(
        example.result(
            first["exp"].as_str().unwrap(),
            first["exe"].as_str().unwrap(),
            &[],
        ),
        0,
    );
    let valuations = result["data"]["runs"][0]["series"]["account_valuations"]
        .as_array()
        .unwrap();
    // Independent hand calculation for the two-Venue pool: 10,000 initial cash;
    // the first-session equal-weight Decision executes at the next open (10.00).
    // 1% commission on each 5,000 target is 50, so each Venue buys 4,950 / 10.00
    // = 495 shares at zero cash; marks are 10.00 at open and 10.10 at close.
    // Rows are ordered by Session and then by valuation point ("close" first).
    let golden = [
        ("2026-01-05", "close", "10000", "10000", false),
        ("2026-01-05", "open_after", "10000", "10000", false),
        ("2026-01-06", "close", "0", "9999", true),
        ("2026-01-06", "open_after", "0", "9900", true),
        ("2026-01-07", "close", "0", "9999", true),
        ("2026-01-07", "open_after", "0", "9900", true),
    ];
    assert_eq!(valuations.len(), golden.len());
    for (row, (date, point, cash, equity, holds)) in valuations.iter().zip(golden) {
        assert_eq!(row["session_date"], date);
        assert_eq!(row["valuation_point"], point);
        assert_eq!(row["cash"], cash);
        assert_eq!(row["equity"], equity);
        let positions = row["positions"].as_object().unwrap();
        assert_eq!(positions.len(), if holds { 2 } else { 0 }, "{row}");
        for (instrument, quantity) in [("600519.XSHG", "495"), ("000001.XSHE", "495")] {
            match positions.get(instrument) {
                Some(position) => assert_eq!(position["quantity"], quantity),
                None => assert!(!holds),
            }
        }
    }
}

#[test]
fn multi_venue_run_specs_use_the_t06b1_engine_semantics_versions() {
    let example = Example::multi_venue(None);
    example.write_definition(|value| {
        let object = value.as_object_mut().unwrap();
        object.remove("fast_event");
        object.insert("engine".into(), json!("vector"));
    });
    let output = json_output(example.run(&["--threads", "1"]), 0);
    assert_eq!(output["failed_count"], 0);
    let manifest = execution_manifest(&example, &output);
    let vector: Value = serde_json::from_slice(&fs::read(&example.definition).unwrap()).unwrap();
    let experiment =
        prajna_experiment::Experiment::parse(&vector.to_string(), &example.lake).unwrap();
    let runs = experiment.run_definitions(&example.lake).unwrap();
    assert_eq!(
        runs[0].run_spec["engine"],
        json!({"kind": "vector", "semantic_version": "vector@10"})
    );
    assert_eq!(runs[0].run_spec["venues"], json!(["XSHE", "XSHG"]));
    assert_eq!(manifest["runs"][0]["run_id"], json!(runs[0].run_id));

    // The Lot variant of the same multi-Venue Fast Event Run Spec.
    let mut lot = vector;
    lot["engine"] = json!("fast_event");
    lot["fast_event"] = json!({"currency": "CNY", "initial_cash": "10000", "sizing": "lot",
        "rebalance": "full_target", "minimum_commission": "0",
        "portfolios": [{"id": 1, "capital": "10000"}],
        "lot_size_source": {"kind": "instrument"}});
    let experiment = prajna_experiment::Experiment::parse(&lot.to_string(), &example.lake).unwrap();
    let runs = experiment.run_definitions(&example.lake).unwrap();
    assert_eq!(
        runs[0].run_spec["engine"],
        json!({"kind": "fast_event", "semantic_version": "fast_event@14"})
    );
}

#[test]
fn multi_venue_calendar_mismatch_is_a_structural_failure() {
    let example = Example::multi_venue(Some("2026-01-06"));
    let output = example.run(&[]);
    status(&output, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("inconsistent shared Session axis"),
        "{stderr}"
    );
    assert!(!example.lake.join("experiments").exists());
}

#[test]
fn multi_venue_dataset_missing_a_selected_venue_fails_explicitly() {
    let example = Example::multi_venue(None);
    example.write_definition(|value| value["venues"] = json!(["XSHG", "XSHE", "SYNTH"]));
    let output = example.run(&[]);
    status(&output, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no Instruments for Venue SYNTH"),
        "{stderr}"
    );
    assert!(!example.lake.join("experiments").exists());
}

#[test]
fn multi_venue_accurate_event_and_adapter_reject_a_run() {
    let example = Example::multi_venue(None);
    example.write_definition(|value| {
        value["engine"] = json!("accurate_event");
        value["backend"] = json!({"name": "synthetic-backend", "version": "1"});
        value["fast_event"]["sizing"] = json!("lot");
    });
    let output = example.run(&[]);
    status(&output, 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Accurate Event requires exactly one Venue"),
        "{stderr}"
    );

    // The adapter packet boundary rejects the same multi-Venue Dataset.
    let definition: Value =
        serde_json::from_slice(&fs::read(&example.definition).unwrap()).unwrap();
    let error =
        prajna_adapter::RunPacket::from_dataset(&example.lake, definition["dsv"].as_str().unwrap())
            .unwrap_err();
    assert!(error.to_string().contains("requires one Venue"), "{error}");
}

#[test]
fn multi_venue_definition_boundaries_reject_mismatched_scope_and_python_strategy() {
    let example = Example::multi_venue(None);
    // The fixture Universe has an XSHE member, so selecting only XSHG must fail.
    example.write_definition(|value| value["venues"] = json!(["XSHG"]));
    let output = example.run(&[]);
    status(&output, 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not belong to selected Venue(s) XSHG"),
        "{stderr}"
    );

    // Python strategies stay single-Venue; the boundary rejects before host work.
    example.write_definition(|value| {
        value["venues"] = json!(["XSHG", "XSHE"]);
        value["strategy"] = json!("research.rotation@1");
    });
    let output = example.run(&[]);
    status(&output, 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Python strategies require exactly one Venue"),
        "{stderr}"
    );
    assert!(!example.lake.join("experiments").exists());
}
