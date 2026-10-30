//! T06c: a published XSHG+XSHE research pool is frozen and submitted through the
//! production selection/CLI path and persists a hand-calculated buy-and-hold Run.
//! All inputs are synthetic; no real market data, calendar or identity is read.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use prajna_experiment::{
    Experiment,
    pools::{Command as PoolCommand, command},
    selection::{Selection, freeze, plan},
};
use serde_json::{Value, json};
use tempfile::TempDir;

#[path = "support/selection_fixture.rs"]
mod fixture;

/// Two Venue members in one published pool version; both exist in the executable
/// synthetic Dataset with a full adjusted research binding.
fn mixed_selection(dsv: &str, version: usize, adjusted: &BTreeMap<String, String>) -> Selection {
    serde_json::from_value(json!({
        "dsv": dsv,
        "pool": {"id": "cross-asset", "version": version},
        "strategy": "buy_and_hold",
        "adjusted": {
            "510300.XSHG": adjusted["510300.XSHG"],
            "159952.XSHE": adjusted["159952.XSHE"],
        },
    }))
    .expect("synthetic mixed Venue selection")
}

fn publish_mixed_pool(root: &Path) -> usize {
    let registry = command(root, PoolCommand::List {}).unwrap();
    let pool = registry["data"]["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pool| pool["id"] == "cross-asset")
        .unwrap()
        .clone();
    command(
        root,
        PoolCommand::Save {
            id: "cross-asset".into(),
            expected: serde_json::from_value(pool["draft"].clone()).unwrap(),
            members: vec!["510300.XSHG".to_string(), "159952.XSHE".to_string()],
        },
    )
    .unwrap();
    let saved = command(root, PoolCommand::List {}).unwrap();
    let draft = saved["data"]["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pool| pool["id"] == "cross-asset")
        .unwrap()["draft"]
        .clone();
    let published = command(
        root,
        PoolCommand::Publish {
            id: "cross-asset".into(),
            expected: serde_json::from_value(draft).unwrap(),
        },
    )
    .unwrap();
    published["data"]["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pool| pool["id"] == "cross-asset")
        .unwrap()["versions"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["version"]
        .as_u64()
        .unwrap() as usize
}

struct Repo {
    _temp: TempDir,
    lake: PathBuf,
    code: PathBuf,
    definition: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let lake = temp.path().join("lake");
        let code = temp.path().join("repository");
        fs::create_dir(&code).unwrap();
        let definition = temp.path().join("experiment.json");
        git(&code, &["init", "--quiet"]);
        fs::write(code.join("code.txt"), "synthetic code\n").unwrap();
        git(&code, &["add", "code.txt"]);
        git(
            &code,
            &[
                "-c",
                "user.name=Cross Asset Test",
                "-c",
                "user.email=cross-asset@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "synthetic provenance",
            ],
        );
        Self {
            _temp: temp,
            lake,
            code,
            definition,
        }
    }

    fn run(&self) -> Value {
        let output: Output = isolated(
            Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
                .arg("run")
                .arg(&self.definition)
                .arg("--lake")
                .arg(&self.lake)
                .arg("--level")
                .arg("standard")
                .current_dir(&self.code),
        )
        .env("RAYON_NUM_THREADS", "2")
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn result(&self, run: &Value) -> Value {
        let output: Output = isolated(
            Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
                .arg("result")
                .arg(run["exp"].as_str().unwrap())
                .arg(run["exe"].as_str().unwrap())
                .arg("--lake")
                .arg(&self.lake)
                .current_dir(&self.code),
        )
        .env("RAYON_NUM_THREADS", "2")
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
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

fn git(directory: &Path, args: &[&str]) {
    let output = isolated(Command::new("git").args(args).current_dir(directory))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn mixed_venue_pool_freezes_runner_identity_and_persists_the_hand_calculated_baseline() {
    let repo = Repo::new();
    let (dsv, adjusted) = fixture::prepare_executable_multi_venue(&repo.lake, true, None);
    let version = publish_mixed_pool(&repo.lake);
    let selection = mixed_selection(&dsv, version, &adjusted);

    let planned = plan(&repo.lake, &selection).unwrap();
    assert_eq!(planned["data"]["runnable"], true, "{planned}");
    assert_eq!(planned["data"]["venue"], Value::Null);
    assert_eq!(planned["data"]["venues"], json!(["XSHE", "XSHG"]));
    assert_eq!(planned["data"]["window"]["start"], "2026-01-05");
    assert_eq!(planned["data"]["window"]["end"], "2026-01-07");
    assert_eq!(
        planned["data"]["pool"]["members"],
        json!(["159952.XSHE", "510300.XSHG"])
    );

    let frozen = freeze(&repo.lake, &selection).unwrap();
    let data = &frozen["data"];
    let definition = &data["definition"];
    assert_eq!(data["venues"], json!(["XSHE", "XSHG"]));
    assert_eq!(
        data["engine"],
        json!({"kind": "fast_event", "semantic_version": "fast_event@14"})
    );
    assert_eq!(definition.get("venue"), None);
    assert_eq!(definition["venues"], json!(["XSHE", "XSHG"]));
    assert_eq!(definition["fast_event"]["rebalance"], "full_target");
    assert_eq!(definition["fast_event"]["sizing"], "lot");
    assert_eq!(definition["fast_event"].get("entry_budget"), None);
    assert_eq!(definition["unfilled_entry"], "retry");
    assert_eq!(
        definition["evaluation"]["semantic_version"],
        "common-window@1"
    );
    assert_eq!(definition["evaluation"]["start"], "2026-01-05");
    assert_eq!(definition["evaluation"]["end"], "2026-01-07");

    let experiment = Experiment::parse(&definition.to_string(), &repo.lake).unwrap();
    assert_eq!(
        experiment.engine_identity(),
        json!({"kind": "fast_event", "semantic_version": "fast_event@14"})
    );
    let runs = experiment.run_definitions(&repo.lake).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_spec["venues"], json!(["XSHE", "XSHG"]));
    assert_eq!(
        runs[0].run_spec["engine"]["semantic_version"],
        "fast_event@14"
    );

    fs::write(&repo.definition, serde_json::to_vec(definition).unwrap()).unwrap();
    let run = repo.run();
    assert_eq!(run["action"], "created");
    assert_eq!(run["failed_count"], 0);
    let result = repo.result(&run);
    assert_eq!(result["data"]["runs"][0]["run_id"], runs[0].run_id);
    let valuations = result["data"]["runs"][0]["series"]["account_valuations"]
        .as_array()
        .expect("persisted account valuations");
    let row = |date: &str, point: &str| {
        valuations
            .iter()
            .find(|row| row["session_date"] == date && row["valuation_point"] == point)
            .unwrap_or_else(|| panic!("missing {date} {point}"))
    };
    // Independent hand calculation on the synthetic 10.00 open, lot 100:
    // 159952.XSHE buys floor(0.5 x 1,000,000 / 10) = 50,000 shares first
    // (50,000 x 10.002 + 150.03 = 500,250.03); 510300.XSHG then buys the largest
    // cash-limited lot, 49,900 (49,900 x 10.002 + 149.72994 = 499,249.52994).
    let entered = row("2026-01-06", "close");
    assert_eq!(entered["cash"], "500.44006");
    assert_eq!(entered["positions"]["159952.XSHE"]["quantity"], "50000");
    assert_eq!(entered["positions"]["510300.XSHG"]["quantity"], "49900");
    assert_eq!(entered["equity"], "999500.44006");
    let opened = row("2026-01-05", "close");
    assert_eq!(opened["positions"], json!({}));
    assert_eq!(opened["cash"], "1000000");
    assert_eq!(opened["equity"], "1000000");
    let last = row("2026-01-07", "close");
    assert_eq!(last["equity"], "999500.44006");
    assert_eq!(last["dividend_receivable"], "0");
}

#[test]
fn shared_axis_disagreement_is_a_structural_selection_error_without_writes() {
    let repo = Repo::new();
    let (dsv, adjusted) =
        fixture::prepare_executable_multi_venue(&repo.lake, true, Some("2026-01-06"));
    let version = publish_mixed_pool(&repo.lake);
    let selection = mixed_selection(&dsv, version, &adjusted);
    for failure in [
        plan(&repo.lake, &selection).unwrap_err(),
        freeze(&repo.lake, &selection).unwrap_err(),
    ] {
        assert_eq!(failure.code, "inconsistent_venue_calendar");
        assert!(
            failure.message.contains("inconsistent shared Session axis"),
            "{}",
            failure.message
        );
    }
    assert!(
        !repo.lake.join("experiments").exists(),
        "a structural failure must not write an Execution"
    );
}
