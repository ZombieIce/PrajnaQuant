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
        let temp = TempDir::new().unwrap();
        let lake = temp.path().join("lake");
        let definition = temp.path().join("experiment.json");
        let repo = temp.path().join("repository");
        fs::create_dir(&repo).unwrap();
        fixture::prepare(&lake, &definition).unwrap();
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
    assert_eq!(promoted["written_files"], 7); // six tables and replaced manifest.
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
