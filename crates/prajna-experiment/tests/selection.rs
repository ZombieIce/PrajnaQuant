use prajna_experiment::{
    Experiment,
    selection::{Selection, freeze},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
#[path = "support/selection_fixture.rs"]
mod fixture;

fn request(dsv: &str, instrument: &str) -> Value {
    json!({"dsv":dsv,"instrument":instrument,"strategy":"buy_and_hold"})
}

fn cli(root: &Path, input: &Value) -> (bool, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_prajna-freeze-selection"))
        .arg("--lake")
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (
        output.status.success(),
        serde_json::from_slice(&output.stdout).unwrap(),
    )
}

#[test]
fn domestic_etf_stock_cli_freezes_real_parseable_defaults_and_reuses_universe() {
    let temp = tempfile::tempdir().unwrap();
    let dsv = fixture::prepare(temp.path(), true);
    for id in ["510300.XSHG", "159952.XSHE", "600519.XSHG", "000001.XSHE"] {
        let input = request(&dsv, id);
        let (success, first) = cli(temp.path(), &input);
        assert!(success, "{first}");
        let definition = &first["data"]["definition"];
        Experiment::parse(&definition.to_string(), temp.path()).unwrap();
        assert_eq!(definition["fast_event"]["initial_cash"], "1000000");
        assert_eq!(definition["fast_event"]["minimum_commission"], "5");
        assert_eq!(
            definition["fast_event"]["lot_size_source"]["kind"],
            "instrument"
        );
        assert_eq!(definition["fast_event"]["rebalance"], "entry_only");
        assert_eq!(definition["engine"], "fast_event");
        assert_eq!(
            definition["costs"],
            json!({"commission_rate":0.0003,"buy_slippage_bps":2.0,"sell_slippage_bps":2.0,"buy_tax_rate":0.0,"sell_tax_rate":0.0})
        );
        assert_eq!(first["data"]["defaults_source"], "mvp7-d7-simulation@1");
        let files = || {
            fs::read_dir(temp.path().join("universes"))
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (
                        entry.file_name(),
                        fs::read(entry.path()).unwrap(),
                        entry.metadata().unwrap().modified().unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let before = files();
        assert_eq!(cli(temp.path(), &input).1, first);
        assert_eq!(files(), before);
        let mut edited = input;
        edited["settings"] =
            json!({"initial_cash":"2000000","minimum_commission":"8","buy_slippage_bps":4.0});
        let (ok, changed) = cli(temp.path(), &edited);
        assert!(ok, "{changed}");
        let changed_definition = &changed["data"]["definition"];
        assert_eq!(changed_definition["universe"], definition["universe"]);
        assert_eq!(changed_definition["fast_event"]["initial_cash"], "2000000");
        assert_eq!(
            changed_definition["fast_event"]["portfolios"][0]["capital"],
            "2000000"
        );
        assert_eq!(changed_definition["fast_event"]["minimum_commission"], "8");
        assert_eq!(changed_definition["costs"]["buy_slippage_bps"], 4.0);
        assert_eq!(changed_definition["costs"]["sell_slippage_bps"], 2.0);
        assert_eq!(changed_definition["costs"]["commission_rate"], 0.0003);
    }
}

#[test]
fn cli_rejects_invalid_unknown_nonmember_roots_evidence_and_definition() {
    let temp = tempfile::tempdir().unwrap();
    let dsv = fixture::prepare(temp.path(), true);
    let base = request(&dsv, "510300.XSHG");
    for (key, value, code) in [
        (
            "instrument",
            json!("999999.XSHG"),
            "instrument_not_in_dataset",
        ),
        ("instrument", json!("510300.SYNTH"), "unsupported_venue"),
        ("instrument", json!(""), "invalid_selection"),
        ("strategy", json!("rotation"), "invalid_selection"),
        (
            "dsv",
            json!("dsv:sha256:0000000000000000000000000000000000000000000000000000000000000000"),
            "dataset_not_found",
        ),
        (
            "settings",
            json!({"initial_cash":"0"}),
            "invalid_definition",
        ),
        (
            "settings",
            json!({"commission_rate":-1.0}),
            "invalid_definition",
        ),
        ("settings", json!({"invented":1}), "invalid_selection"),
    ] {
        let mut input = base.clone();
        input[key] = value;
        let (ok, result) = cli(temp.path(), &input);
        assert!(!ok, "{key}");
        assert_eq!(result["error"]["code"], code, "{result}");
    }
    for key in ["dsv", "instrument", "strategy"] {
        let mut input = base.clone();
        input.as_object_mut().unwrap().remove(key);
        assert!(!cli(temp.path(), &input).0);
    }
    assert_eq!(
        cli(&temp.path().join("missing"), &base).1["error"]["code"],
        "invalid_dataset_root"
    );
    assert!(!temp.path().join("missing").exists());
    let unknown = tempfile::tempdir().unwrap();
    let dsv = fixture::prepare(unknown.path(), false);
    let selection: Selection = serde_json::from_value(request(&dsv, "510300.XSHG")).unwrap();
    assert_eq!(
        freeze(unknown.path(), &selection).unwrap_err().code,
        "selection_evidence_missing"
    );
    assert!(!unknown.path().join("universes").exists());

    // A committed Dataset cannot manufacture missing frozen provenance or Bars.
    let manifest = prajna_data::read_manifest(temp.path(), base["dsv"].as_str().unwrap()).unwrap();
    let snapshot = manifest.provenance.coverage["security_snapshot"]
        .as_str()
        .unwrap();
    let hex = snapshot.strip_prefix("sha256:").unwrap();
    let path = temp.path().join("raw/sha256").join(&hex[..2]).join(hex);
    let bytes = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert_eq!(
        cli(temp.path(), &base).1["error"]["code"],
        "selection_evidence_missing"
    );
    fs::write(path, bytes).unwrap();
    let bars = temp
        .path()
        .join("normalized/bars")
        .join(manifest.dsv.trim_start_matches("dsv:sha256:"))
        .join("part-00000.parquet");
    fs::remove_file(bars).unwrap();
    assert_eq!(
        cli(temp.path(), &base).1["error"]["code"],
        "invalid_dataset_table"
    );
}

fn preflight(root: &Path, input: &Value) -> (bool, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_prajna-freeze-selection"))
        .args(["--lake", root.to_str().unwrap(), "--plan"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (
        output.status.success(),
        serde_json::from_slice(&output.stdout).unwrap(),
    )
}

#[test]
fn production_preflight_plans_native_combinations_without_persisting_or_compressing_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (dsv, adjusted) = fixture::prepare_with_adjusted(root, true);
    let mut input = request(&dsv, "510300.XSHG");
    input["adjusted"] = json!({"510300.XSHG":adjusted["510300.XSHG"]});
    let (ok, planned) = preflight(root, &input);
    assert!(ok && planned["data"]["runnable"] == true, "{planned}");
    assert!(!root.join("universes").exists());
    assert!(!root.join("tasks").exists());
    let frozen = cli(root, &input).1;
    assert_eq!(planned["data"]["window"], frozen["data"]["window"]);
    assert_eq!(
        planned["data"]["adjusted"],
        frozen["data"]["definition"]["adjusted_versions"]
    );
    input["strategy"] = json!("ma_crossover");
    input["parameter_space"] =
        json!({"grid":{"params_version":[1],"short_window":[1],"long_window":[2,4]}});
    let (ok, insufficient) = preflight(root, &input);
    assert!(ok, "{insufficient}");
    let plan = &insufficient["data"];
    assert_eq!(plan["runnable"], false);
    assert_eq!(plan["window"]["warmup_sessions"], 3);
    assert_eq!(plan["window"]["parameters"][0]["runnable"], true);
    assert_eq!(plan["window"]["parameters"][1]["runnable"], false);
    assert_eq!(plan["blocking"][0]["code"], "insufficient_warmup");
    assert_eq!(
        cli(root, &input).1["error"]["code"],
        "evaluation_window_not_runnable"
    );
    input["parameter_space"] =
        json!({"list":[{"params_version":1,"short_window":1,"long_window":2}]});
    let ma = preflight(root, &input).1;
    assert_eq!(ma["data"]["window"]["start"], "2026-01-06");
    let (ok, frozen) = cli(root, &input);
    assert!(ok, "{frozen}");
    assert_eq!(frozen["data"]["window"], ma["data"]["window"]);
    assert_eq!(
        frozen["data"]["definition"]["fast_event"]["rebalance"],
        "full_target"
    );
    input["strategy"] = json!("s2_rotation");
    input["parameter_space"] = json!({"list":[{"short":1,"long":2,"vol":2,"w_s":0.5,"w_l":0.5,"w_v":0.0,"trend":null,"top_k":1,"rebalance_every":1}]});
    let (ok, s2) = preflight(root, &input);
    assert!(ok, "{s2}");
    assert_eq!(s2["data"]["window"]["warmup_sessions"], 2);
    assert_eq!(s2["data"]["runnable"], false); // Only one evaluation Session remains.
    input["parameter_space"]["list"][0]["top_k"] = json!(2);
    assert!(!preflight(root, &input).0);
    input = request(&dsv, "510300.XSHG");
    let gap = fixture::import_adjusted(
        root,
        &dsv,
        "510300.XSHG",
        "2026-01-05",
        "2026-01-07",
        &["2026-01-05", "2026-01-07"],
    );
    input["adjusted"] = json!({"510300.XSHG":gap});
    let gap = preflight(root, &input).1;
    assert_eq!(gap["data"]["window"]["common_end"], "2026-01-07");
    assert_eq!(gap["data"]["window"]["members"][0]["research_sessions"], 2);
    assert!(
        !gap["data"]["window"]["warnings"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    input["adjusted"] = json!({});
    let missing = preflight(root, &input).1;
    assert_eq!(missing["data"]["runnable"], false);
    assert_eq!(
        missing["data"]["blocking"][0]["code"],
        "adjusted_version_missing"
    );
}
