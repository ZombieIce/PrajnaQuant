//! T08a: frozen adjusted research binding, shared evaluation window and the
//! Rust execution boundary. All inputs and expected values are synthetic and
//! hand calculated; no real market data is read.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use prajna_data::daily_publish::{DailyPublicationSelection, publish_daily};
use prajna_domain::InstrumentKind;
use prajna_experiment::{
    Experiment,
    pools::{Command as PoolCommand, command},
    selection::{Selection, freeze, plan},
};
use prajna_research::StaticUniverse;
use serde_json::{Value, json};
use tempfile::TempDir;

#[path = "support/selection_fixture.rs"]
mod fixture;

#[path = "../../prajna-data/tests/support/opening_fixture.rs"]
mod opening;

fn binding(pairs: &[(&str, String)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(instrument, version)| (instrument.to_string(), version.clone()))
        .collect()
}

fn single_selection(
    dsv: &str,
    instrument: &str,
    adjusted: Option<BTreeMap<String, String>>,
) -> Selection {
    let mut value = json!({
        "dsv": dsv,
        "instrument": instrument,
        "strategy": "buy_and_hold"
    });
    if let Some(adjusted) = adjusted {
        value["adjusted"] = json!(adjusted);
    }
    serde_json::from_value(value).unwrap()
}

fn pool_selection(dsv: &str, version: usize, adjusted: BTreeMap<String, String>) -> Selection {
    serde_json::from_value(json!({
        "dsv": dsv,
        "pool": {"id":"dividend","version":version},
        "strategy": "buy_and_hold",
        "adjusted": adjusted
    }))
    .unwrap()
}

/// Publishes one new `dividend` pool version and returns its ordinal.
fn publish_dividend(root: &Path, members: &[&str]) -> usize {
    let registry = command(root, PoolCommand::List {}).unwrap();
    let expected: Vec<String> =
        serde_json::from_value(registry["data"]["pools"][1]["draft"].clone()).unwrap();
    command(
        root,
        PoolCommand::Save {
            id: "dividend".into(),
            expected,
            members: members.iter().map(ToString::to_string).collect(),
        },
    )
    .unwrap();
    let registry = command(root, PoolCommand::List {}).unwrap();
    let expected: Vec<String> =
        serde_json::from_value(registry["data"]["pools"][1]["draft"].clone()).unwrap();
    command(
        root,
        PoolCommand::Publish {
            id: "dividend".into(),
            expected,
        },
    )
    .unwrap();
    let registry = command(root, PoolCommand::List {}).unwrap();
    registry["data"]["pools"][1]["versions"]
        .as_array()
        .unwrap()
        .len()
}

#[test]
fn pool_selection_binds_adjusted_versions_and_freezes_the_shared_window() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let (dsv, adjusted) = fixture::prepare_with_adjusted(root, true);
    let version = publish_dividend(root, &["510300.XSHG", "510880.XSHG"]);
    let selection = pool_selection(
        &dsv,
        version,
        binding(&[
            ("510300.XSHG", adjusted["510300.XSHG"].clone()),
            ("510880.XSHG", adjusted["510880.XSHG"].clone()),
        ]),
    );
    let frozen = freeze(root, &selection).unwrap();
    let definition = &frozen["data"]["definition"];
    assert_eq!(
        definition["adjusted_versions"],
        json!({
            "510300.XSHG": adjusted["510300.XSHG"],
            "510880.XSHG": adjusted["510880.XSHG"]
        })
    );
    assert_eq!(
        definition["evaluation"]["semantic_version"],
        "common-window@1"
    );
    assert_eq!(definition["evaluation"]["start"], "2026-01-05");
    assert_eq!(definition["evaluation"]["end"], "2026-01-07");
    assert_eq!(definition["evaluation"]["common_start"], "2026-01-05");
    assert_eq!(definition["evaluation"]["common_end"], "2026-01-07");
    assert_eq!(definition["evaluation"]["warmup_sessions"], 0);
    assert_eq!(frozen["data"]["window"]["runnable"], true);
    assert_eq!(
        frozen["data"]["window"]["members"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // The same frozen inputs produce the same envelope and definition.
    assert_eq!(freeze(root, &selection).unwrap(), frozen);

    let experiment = Experiment::parse(&definition.to_string(), root).unwrap();
    assert_eq!(experiment.adjusted_versions().len(), 2);
    assert_eq!(
        experiment.evaluation().unwrap().start().to_string(),
        "2026-01-05"
    );
    assert_eq!(
        experiment.evaluation().unwrap().end().to_string(),
        "2026-01-07"
    );
    let mut unwindowed = definition.clone();
    unwindowed
        .as_object_mut()
        .unwrap()
        .remove("adjusted_versions");
    unwindowed.as_object_mut().unwrap().remove("evaluation");
    let unwindowed = Experiment::parse(&unwindowed.to_string(), root).unwrap();
    assert_ne!(
        experiment.id().unwrap(),
        unwindowed.id().unwrap(),
        "a frozen window and bound research inputs change the Experiment identity"
    );
    let planned = experiment.plan(root).unwrap();
    assert_eq!(planned.start.as_deref(), Some("2026-01-05"));
    assert_eq!(planned.end.as_deref(), Some("2026-01-07"));
    let runs = experiment.run_definitions(root).unwrap();
    assert_eq!(runs[0].run_spec["evaluation"]["start"], "2026-01-05");
    assert_eq!(runs[0].run_spec["evaluation"]["warmup_sessions"], 0);
    assert!(runs[0].run_spec["adjusted_versions"].is_object());
}

#[test]
fn adjusted_binding_validation_fails_closed_without_latest_or_fallback() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let (dsv, adjusted) = fixture::prepare_with_adjusted(root, true);
    let version = publish_dividend(root, &["510300.XSHG", "510880.XSHG"]);
    let full = binding(&[
        ("510300.XSHG", adjusted["510300.XSHG"].clone()),
        ("510880.XSHG", adjusted["510880.XSHG"].clone()),
    ]);
    // A research-pool submission must bind every member.
    let mut missing = full.clone();
    missing.remove("510880.XSHG");
    assert_eq!(
        freeze(root, &pool_selection(&dsv, version, missing))
            .unwrap_err()
            .code,
        "adjusted_version_missing"
    );
    // No binding at all is not a raw-price fallback.
    assert_eq!(
        freeze(root, &pool_selection(&dsv, version, BTreeMap::new()))
            .unwrap_err()
            .code,
        "adjusted_version_missing"
    );
    // A version that names another security is rejected.
    assert_eq!(
        freeze(
            root,
            &pool_selection(
                &dsv,
                version,
                binding(&[
                    ("510300.XSHG", adjusted["510880.XSHG"].clone()),
                    ("510880.XSHG", adjusted["510880.XSHG"].clone()),
                ])
            )
        )
        .unwrap_err()
        .code,
        "adjusted_instrument_mismatch"
    );
    // Syntax and integrity failures stay explicit.
    let malformed = binding(&[
        ("510300.XSHG", "adj:sha256:nothex".into()),
        ("510880.XSHG", adjusted["510880.XSHG"].clone()),
    ]);
    assert_eq!(
        freeze(root, &pool_selection(&dsv, version, malformed))
            .unwrap_err()
            .code,
        "adjusted_series_invalid"
    );
    let absent = binding(&[
        ("510300.XSHG", format!("adj:sha256:{}", "0".repeat(64))),
        ("510880.XSHG", adjusted["510880.XSHG"].clone()),
    ]);
    assert_eq!(
        freeze(root, &pool_selection(&dsv, version, absent))
            .unwrap_err()
            .code,
        "adjusted_version_missing"
    );
    // An imported version with no research rows cannot supply a window.
    let empty =
        fixture::import_adjusted(root, &dsv, "510300.XSHG", "2026-01-05", "2026-01-07", &[]);
    assert_eq!(
        freeze(
            root,
            &pool_selection(
                &dsv,
                version,
                binding(&[
                    ("510300.XSHG", empty),
                    ("510880.XSHG", adjusted["510880.XSHG"].clone()),
                ])
            )
        )
        .unwrap_err()
        .code,
        "adjusted_series_empty"
    );
}

#[test]
fn plan_reports_structured_blocking_and_the_frozen_window() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let (dsv, adjusted) = fixture::prepare_with_adjusted(root, true);
    let version = publish_dividend(root, &["510300.XSHG", "510880.XSHG"]);
    let unbound = plan(root, &pool_selection(&dsv, version, BTreeMap::new())).unwrap();
    assert_eq!(unbound["data"]["runnable"], false);
    assert_eq!(
        unbound["data"]["blocking"][0]["code"],
        "adjusted_version_missing"
    );
    assert!(unbound["data"]["window"].is_null());
    assert_eq!(unbound["data"]["pool"]["version"], json!(version));

    let window = plan(
        root,
        &pool_selection(
            &dsv,
            version,
            binding(&[
                ("510300.XSHG", adjusted["510300.XSHG"].clone()),
                ("510880.XSHG", adjusted["510880.XSHG"].clone()),
            ]),
        ),
    )
    .unwrap();
    assert_eq!(window["data"]["runnable"], true);
    assert_eq!(window["data"]["blocking"], json!([]));
    assert_eq!(window["data"]["window"]["start"], "2026-01-05");
    assert_eq!(window["data"]["window"]["end"], "2026-01-07");
    assert_eq!(
        window["data"]["window"]["parameters"][0]["warmup_sessions"],
        0
    );
    assert!(
        window["data"]["universe"]
            .as_str()
            .unwrap()
            .starts_with("uni:sha256:")
    );
}

#[test]
fn first_single_security_precheck_plans_from_in_memory_inputs_without_writing_a_universe() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let (dsv, adjusted) = fixture::prepare_with_adjusted(root, true);
    let selection = single_selection(
        &dsv,
        "510300.XSHG",
        Some(binding(&[("510300.XSHG", adjusted["510300.XSHG"].clone())])),
    );
    // No pool was published and no Universe was frozen: the precheck must work
    // from the validated in-memory Universe and the bound adjusted rows.
    let planned = plan(root, &selection).unwrap();
    assert_eq!(planned["data"]["runnable"], true);
    assert_eq!(planned["data"]["window"]["start"], "2026-01-05");
    assert_eq!(planned["data"]["window"]["end"], "2026-01-07");
    let universe = planned["data"]["universe"].as_str().unwrap();
    let hex = universe
        .strip_prefix("uni:sha256:")
        .expect("content-addressed Universe");
    assert!(
        !root.join("universes").join(format!("{hex}.json")).exists(),
        "a read-only precheck must not store a Universe"
    );
    assert_eq!(plan(root, &selection).unwrap(), planned);
}

#[test]
fn definition_parse_rejects_incomplete_or_foreign_research_inputs() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let (dsv, adjusted) = fixture::prepare_with_adjusted(root, true);
    let version = publish_dividend(root, &["510300.XSHG", "510880.XSHG"]);
    let selection = pool_selection(
        &dsv,
        version,
        binding(&[
            ("510300.XSHG", adjusted["510300.XSHG"].clone()),
            ("510880.XSHG", adjusted["510880.XSHG"].clone()),
        ]),
    );
    let frozen = freeze(root, &selection).unwrap();
    let definition = frozen["data"]["definition"].clone();
    let parse = |value: &Value| Experiment::parse(&value.to_string(), root);
    assert!(parse(&definition).is_ok());
    // An adjusted-only object is not an executable definition: the research
    // binding and its frozen window are written together by the selection.
    let mut seed = definition.clone();
    seed.as_object_mut().unwrap().remove("evaluation");
    let failure = parse(&seed).unwrap_err().to_string();
    assert!(
        failure.contains("provided together") || failure.contains("planning-only"),
        "{failure}"
    );
    let changes: [fn(&mut Value); 5] = [
        |value: &mut Value| {
            value.as_object_mut().unwrap().remove("adjusted_versions");
        },
        |value: &mut Value| {
            value.as_object_mut().unwrap().remove("evaluation");
        },
        |value: &mut Value| {
            value["evaluation"]["semantic_version"] = json!("common-window@2");
        },
        |value: &mut Value| {
            value["evaluation"]["start"] = json!("2026-01-07");
            value["evaluation"]["end"] = json!("2026-01-06");
        },
        |value: &mut Value| {
            value["adjusted_versions"]["510300.XSHG"] = json!("adj:sha256:nothex");
        },
    ];
    for change in changes {
        let mut mutated = definition.clone();
        change(&mut mutated);
        assert!(
            parse(&mutated).is_err(),
            "mutated definition parsed: {mutated}"
        );
    }
    // A version frozen against another raw DSV is rejected at parse time.
    let foreign_dsv = fixture::prepare(root, false);
    let mut foreign = definition.clone();
    foreign["dsv"] = json!(foreign_dsv);
    assert!(parse(&foreign).is_err());
}

struct Golden {
    _temp: TempDir,
    lake: PathBuf,
    repo: PathBuf,
    definition: PathBuf,
    dsv: String,
    adjusted: String,
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

impl Golden {
    /// Synthetic v2 dataset with a frozen execution-status table (required by the
    /// real execution entry) plus one full-coverage adjusted research version.
    fn new() -> Self {
        Self::with_series(
            &["2026-01-05", "2026-01-06", "2026-01-07"],
            &[("10", "10.1", "11", "9"); 3],
        )
    }

    /// Synthetic v2 dataset with an explicit Session axis and close series.
    fn with_series(dates: &[&str], bars: &[(&str, &str, &str, &str)]) -> Self {
        let temp = TempDir::new().unwrap();
        let lake = temp.path().join("lake");
        let repo = temp.path().join("repository");
        let definition = temp.path().join("experiment.json");
        fs::create_dir(&repo).unwrap();
        let input = lake.join("input").join("XSHG");
        let (selection, _) = opening::prepare(
            &input,
            "XSHG",
            InstrumentKind::Etf,
            false,
            Some((dates, bars)),
        );
        let selection: DailyPublicationSelection = serde_json::from_value(selection).unwrap();
        let manifest = publish_daily(&lake, selection, Path::new("receipt.json")).unwrap();
        let dsv = manifest.dsv;
        let adjusted = fixture::import_adjusted(
            &lake,
            &dsv,
            "510300.XSHG",
            dates[0],
            dates[dates.len() - 1],
            dates,
        );
        let golden = Self {
            _temp: temp,
            lake,
            repo,
            definition,
            dsv,
            adjusted,
        };
        golden.git(&["init", "--quiet"]);
        fs::write(golden.repo.join("code.txt"), "synthetic code\n").unwrap();
        golden.git(&["add", "code.txt"]);
        golden.git(&[
            "-c",
            "user.name=Window Test",
            "-c",
            "user.email=window@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "synthetic provenance",
        ]);
        golden
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

    fn write(&self, definition: &Value) {
        fs::write(&self.definition, serde_json::to_vec(definition).unwrap()).unwrap();
    }

    fn run_output(&self) -> Output {
        isolated(
            Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
                .arg("run")
                .arg(&self.definition)
                .arg("--lake")
                .arg(&self.lake)
                .arg("--level")
                .arg("standard")
                .current_dir(&self.repo),
        )
        .env("RAYON_NUM_THREADS", "2")
        .output()
        .unwrap()
    }

    fn run(&self) -> Value {
        let output = self.run_output();
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn result(&self, run: &Value) -> Value {
        let exp = run["exp"].as_str().unwrap();
        let exe = run["exe"].as_str().unwrap();
        let output: Output = isolated(
            Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
                .arg("result")
                .arg(exp)
                .arg(exe)
                .arg("--lake")
                .arg(&self.lake)
                .current_dir(&self.repo),
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

    fn close_rows<'a>(&self, result: &'a Value) -> Vec<&'a Value> {
        result["data"]["runs"][0]["series"]["account_valuations"]
            .as_array()
            .expect("saved account valuations")
            .iter()
            .filter(|row| row["valuation_point"] == "close")
            .collect()
    }
}

#[test]
fn production_cli_runs_only_the_frozen_window_with_hand_calculated_fast_event_ledger() {
    let golden = Golden::new();
    // The supplier series ends on 2026-01-06 even though raw Bars continue on
    // 2026-01-07: the shared window must end on the last research Session.
    let truncated = fixture::import_adjusted(
        &golden.lake,
        &golden.dsv,
        "510300.XSHG",
        "2026-01-05",
        "2026-01-07",
        &["2026-01-05", "2026-01-06"],
    );
    let selection = single_selection(
        &golden.dsv,
        "510300.XSHG",
        Some(binding(&[("510300.XSHG", truncated)])),
    );
    let frozen = freeze(&golden.lake, &selection).unwrap();
    let definition = frozen["data"]["definition"].clone();
    assert_eq!(definition["evaluation"]["start"], "2026-01-05");
    assert_eq!(definition["evaluation"]["end"], "2026-01-06");
    golden.write(&definition);

    let run = golden.run();
    let result = golden.result(&run);
    let summary = &result["data"]["runs"][0]["summary"];
    assert_eq!(summary["session_count"], 2);
    assert_eq!(summary["n_returns"], 1);
    assert_eq!(summary["executed_count"], 1);
    let total_return = summary["total_return"].as_f64().unwrap();
    assert!(
        (total_return - 0.009_490_440_06).abs() < 1e-9,
        "{total_return}"
    );
    let rows = golden.close_rows(&result);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["session_date"], "2026-01-05");
    assert_eq!(rows[0]["cash"], "1000000");
    assert_eq!(rows[0]["equity"], "1000000");
    assert_eq!(rows[0]["positions"], json!({}));
    assert_eq!(rows[1]["session_date"], "2026-01-06");
    // Independent hand calculation: 99,900 at 10.002, commission 299.75994,
    // cash 500.44006 + 99,900 * 10.1 = 1,009,490.44006.
    assert_eq!(rows[1]["cash"], "500.44006");
    assert_eq!(rows[1]["equity"], "1009490.44006");
    assert_eq!(rows[1]["positions"]["510300.XSHG"]["quantity"], "99900");
    assert_eq!(rows[1]["positions"]["510300.XSHG"]["price"], "10.1");
    assert_eq!(rows[1]["dividend_receivable"], "0");
    assert!(
        rows.iter().all(|row| row["session_date"] != "2026-01-07"),
        "the supplier-truncated research window must not evaluate 2026-01-07"
    );
}

#[test]
fn production_cli_excludes_warmup_sessions_from_decisions_fills_and_performance() {
    let golden = Golden::new();
    let legacy = single_selection(&golden.dsv, "510300.XSHG", None);
    let frozen = freeze(&golden.lake, &legacy).unwrap();
    let mut definition = frozen["data"]["definition"].clone();
    definition["adjusted_versions"] = json!({"510300.XSHG": golden.adjusted});
    definition["evaluation"] = json!({
        "semantic_version": "common-window@1",
        "warmup_sessions": 1,
        "common_start": "2026-01-05",
        "common_end": "2026-01-07",
        "start": "2026-01-06",
        "end": "2026-01-07"
    });
    Experiment::parse(&definition.to_string(), &golden.lake).unwrap();
    golden.write(&definition);

    let run = golden.run();
    let result = golden.result(&run);
    let summary = &result["data"]["runs"][0]["summary"];
    assert_eq!(summary["session_count"], 2);
    assert_eq!(summary["n_returns"], 1);
    let rows = golden.close_rows(&result);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["session_date"], "2026-01-06");
    assert_eq!(rows[0]["equity"], "1000000");
    assert_eq!(rows[0]["positions"], json!({}));
    assert_eq!(rows[1]["session_date"], "2026-01-07");
    assert_eq!(rows[1]["equity"], "1009490.44006");
    assert_eq!(rows[1]["positions"]["510300.XSHG"]["quantity"], "99900");
    assert!(
        rows.iter().all(|row| row["session_date"] != "2026-01-05"),
        "the warm-up Session must not appear as performance"
    );
}

#[test]
fn vector_event_honors_the_same_frozen_window() {
    let golden = Golden::new();
    let legacy = single_selection(&golden.dsv, "510300.XSHG", None);
    let frozen = freeze(&golden.lake, &legacy).unwrap();
    let mut definition = frozen["data"]["definition"].clone();
    definition["engine"] = json!("vector");
    definition.as_object_mut().unwrap().remove("fast_event");
    definition["adjusted_versions"] = json!({"510300.XSHG": golden.adjusted});
    definition["evaluation"] = json!({
        "semantic_version": "common-window@1",
        "warmup_sessions": 1,
        "common_start": "2026-01-05",
        "common_end": "2026-01-07",
        "start": "2026-01-06",
        "end": "2026-01-07"
    });
    Experiment::parse(&definition.to_string(), &golden.lake).unwrap();
    golden.write(&definition);

    let run = golden.run();
    let result = golden.result(&run);
    assert_eq!(result["data"]["runs"][0]["summary"]["session_count"], 2);
    let sessions = result["data"]["runs"][0]["series"]["sessions"]
        .as_array()
        .expect("saved Vector sessions");
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["session_date"], "2026-01-06");
    assert_eq!(sessions[1]["session_date"], "2026-01-07");
    assert!(
        sessions
            .iter()
            .all(|session| session["session_date"] != "2026-01-05")
    );
}

#[test]
fn production_cli_rejects_an_adjusted_only_planning_seed() {
    let golden = Golden::new();
    let legacy = single_selection(&golden.dsv, "510300.XSHG", None);
    let frozen = freeze(&golden.lake, &legacy).unwrap();
    let mut seed = frozen["data"]["definition"].clone();
    seed["adjusted_versions"] = json!({"510300.XSHG": golden.adjusted});
    assert!(seed.get("evaluation").is_none());
    golden.write(&seed);

    let output = golden.run_output();
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("provided together") || stderr.contains("planning-only"),
        "stderr={stderr}"
    );
    assert!(
        !golden.lake.join("experiments").exists(),
        "a rejected seed must not execute or persist a result"
    );
}

#[test]
fn legacy_single_security_selection_keeps_the_unwindowed_definition_and_identity() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let (dsv, _) = fixture::prepare_with_adjusted(root, true);
    let selection = single_selection(&dsv, "510300.XSHG", None);
    let frozen = freeze(root, &selection).unwrap();
    let definition = &frozen["data"]["definition"];
    assert!(definition.get("adjusted_versions").is_none());
    assert!(definition.get("evaluation").is_none());
    assert!(frozen["data"].get("window").is_none());
    let experiment = Experiment::parse(&definition.to_string(), root).unwrap();
    assert!(experiment.adjusted_versions().is_empty());
    assert!(experiment.evaluation().is_none());
    assert!(
        experiment.run_definitions(root).unwrap()[0]
            .run_spec
            .get("evaluation")
            .is_none()
    );
}

/// Single-security `ma_crossover` (MA 2/3) definition over the custom series.
fn ma_window_definition(golden: &Golden, windowed: bool) -> Value {
    let universe =
        StaticUniverse::new("ma-window@1", vec!["510300.XSHG".parse().unwrap()]).unwrap();
    universe.store(&golden.lake).unwrap();
    let mut definition = json!({
        "experiment_version": 1,
        "dsv": golden.dsv,
        "universe": universe.id().unwrap(),
        "venue": "XSHG",
        "strategy": "ma_crossover",
        "engine": "fast_event",
        "availability_assumption": "treat_unknown_as_ts_close",
        "sessions_per_year": 252,
        "unfilled_entry": "skip",
        "parameter_space": {"list": [{"params_version": 1, "short_window": 2, "long_window": 3}]},
        "costs": {"commission_rate": 0.0003, "buy_slippage_bps": 2.0,
            "sell_slippage_bps": 2.0, "buy_tax_rate": 0.0, "sell_tax_rate": 0.0},
        "fast_event": {"currency": "CNY", "initial_cash": "1000000", "sizing": "lot",
            "rebalance": "full_target", "minimum_commission": "5",
            "portfolios": [{"id": 1, "capital": "1000000"}],
            "lot_size_source": {"kind": "instrument"}}
    });
    if windowed {
        definition["adjusted_versions"] = json!({"510300.XSHG": golden.adjusted});
        definition["evaluation"] = json!({
            "semantic_version": "common-window@1",
            "warmup_sessions": 2,
            "common_start": "2026-01-05",
            "common_end": "2026-01-15",
            "start": "2026-01-12",
            "end": "2026-01-14"
        });
    }
    definition
}

#[test]
fn production_cli_filters_non_entry_warmup_decisions_and_keeps_the_final_decision_pending() {
    // MA(2,3) closes: gaps cross up at 01-08 (warm-up buy), down at 01-09,
    // up at 01-12 (window start) and down at 01-14 (window end). A session
    // after the window (01-15) exists, so the end Decision would execute in an
    // unwindowed run but must stay pending inside the frozen window.
    let dates = [
        "2026-01-05",
        "2026-01-06",
        "2026-01-07",
        "2026-01-08",
        "2026-01-09",
        "2026-01-12",
        "2026-01-13",
        "2026-01-14",
        "2026-01-15",
    ];
    let bars = [
        ("10", "10", "11", "9"),
        ("10", "10", "11", "9"),
        ("9", "9", "10", "8"),
        ("12", "12", "13", "11"),
        ("5", "5", "6", "4"),
        ("20", "20", "21", "19"),
        ("10", "10", "11", "9"),
        ("10", "10", "11", "9"),
        ("10", "10", "11", "9"),
    ];
    let golden = Golden::with_series(&dates, &bars);
    let windowed = ma_window_definition(&golden, true);
    Experiment::parse(&windowed.to_string(), &golden.lake).unwrap();
    golden.write(&windowed);

    let result = golden.result(&golden.run());
    let summary = &result["data"]["runs"][0]["summary"];
    assert_eq!(summary["session_count"], 3);
    assert_eq!(summary["n_returns"], 2);
    assert_eq!(summary["executed_count"], 1);
    assert_eq!(summary["pending_at_end"], true);
    let total_return = summary["total_return"].as_f64().unwrap();
    assert!(
        (total_return + 0.000_499_559_94).abs() < 1e-9,
        "{total_return}"
    );
    let rows = golden.close_rows(&result);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["session_date"], "2026-01-12");
    assert_eq!(rows[0]["cash"], "1000000");
    assert_eq!(rows[0]["positions"], json!({}));
    assert_eq!(rows[1]["session_date"], "2026-01-13");
    assert_eq!(rows[1]["cash"], "500.44006");
    assert_eq!(rows[1]["equity"], "999500.44006");
    assert_eq!(rows[1]["positions"]["510300.XSHG"]["quantity"], "99900");
    assert_eq!(rows[2]["session_date"], "2026-01-14");
    assert_eq!(rows[2]["positions"]["510300.XSHG"]["quantity"], "99900");
    assert!(rows.iter().all(|row| row["session_date"] != "2026-01-15"));

    // Control: the same prices without a window execute the warm-up sell at the
    // first window Session and the final sell on the Session after the window.
    golden.write(&ma_window_definition(&golden, false));
    let control = golden.result(&golden.run());
    let control_rows = golden.close_rows(&control);
    let first_window = control_rows
        .iter()
        .find(|row| row["session_date"] == "2026-01-12")
        .unwrap();
    assert_ne!(first_window["cash"], "1000000");
    assert!(
        control_rows
            .iter()
            .any(|row| row["session_date"] == "2026-01-15")
    );

    // Vector shares the same filtered Decisions and window.
    let mut vector = windowed.clone();
    vector["engine"] = json!("vector");
    vector.as_object_mut().unwrap().remove("fast_event");
    Experiment::parse(&vector.to_string(), &golden.lake).unwrap();
    golden.write(&vector);
    let vector = golden.result(&golden.run());
    assert_eq!(vector["data"]["runs"][0]["summary"]["session_count"], 3);
    assert_eq!(vector["data"]["runs"][0]["summary"]["pending_at_end"], true);
    let sessions = vector["data"]["runs"][0]["series"]["sessions"]
        .as_array()
        .unwrap();
    assert_eq!(sessions.len(), 3);
    assert_eq!(sessions[0]["session_date"], "2026-01-12");
    assert_eq!(sessions[0]["nav"].as_f64(), Some(1.0));
    assert_eq!(sessions[2]["session_date"], "2026-01-14");
}
