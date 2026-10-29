use prajna_experiment::{
    Experiment,
    pools::{Command as PoolCommand, Reference, command, resolve},
    selection::{Selection, freeze},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
};
#[path = "support/selection_fixture.rs"]
mod fixture;

fn cli(root: &Path, request: Value) -> (bool, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_prajna-pools"))
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
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (
        output.status.success(),
        serde_json::from_slice(&output.stdout).unwrap(),
    )
}
fn pool(registry: &Value) -> Value {
    registry["data"]["pools"][1].clone()
}
fn change(root: &Path, op: &str, expected: Value, values: Value) -> (bool, Value) {
    let mut input = json!({"op":op,"id":"dividend","expected":expected});
    if op == "save" {
        input["members"] = values;
    }
    cli(root, input)
}

#[test]
fn production_cli_defaults_edits_publish_old_versions_conflicts_and_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (ok, registry) = cli(root, json!({"op":"list"}));
    assert!(ok);
    let presets = registry["data"]["pools"].as_array().unwrap();
    assert_eq!(
        presets
            .iter()
            .map(|p| p["draft"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [5, 4, 4]
    );
    assert_eq!(
        presets[0]["draft"],
        json!([
            "159612.XSHE",
            "159925.XSHE",
            "159952.XSHE",
            "513300.XSHG",
            "518880.XSHG"
        ])
    );
    assert_eq!(
        presets[1]["draft"],
        json!(["510880.XSHG", "512890.XSHG", "515100.XSHG", "515180.XSHG"])
    );
    assert_eq!(
        presets[2]["draft"],
        json!(["511010.XSHG", "511090.XSHG", "511260.XSHG", "511520.XSHG"])
    );
    let original = pool(&registry);
    let path = root.join("research-pools/registry.json");
    let before = fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(cli(root, json!({"op":"list"})).1, registry);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before);
    for values in [
        json!([]),
        json!(["510300.XSHG", "510300.XSHG"]),
        json!(["bad"]),
        json!(["510300.SZSE"]),
    ] {
        let (ok, failure) = change(root, "save", original["draft"].clone(), values);
        assert!(!ok);
        assert_eq!(failure["error"]["code"], "invalid_pool");
    }
    assert_eq!(
        cli(
            root,
            json!({"op":"save","id":"missing","expected":[],"members":[]})
        )
        .1["error"]["code"],
        "pool_not_found"
    );
    assert!(!cli(root, json!({"op":"list","extra":true})).0);
    let (ok, saved) = change(
        root,
        "save",
        original["draft"].clone(),
        json!(["510300.XSHG"]),
    );
    assert!(ok);
    assert_eq!(pool(&saved)["versions"], original["versions"]);
    assert_eq!(
        change(root, "publish", original["draft"].clone(), Value::Null).1["error"]["code"],
        "pool_conflict"
    );
    let (_, published) = change(root, "publish", json!(["510300.XSHG"]), Value::Null);
    assert_eq!(pool(&published)["versions"].as_array().unwrap().len(), 2);
    assert_eq!(pool(&published)["versions"][0], original["versions"][0]);
    assert_eq!(
        change(root, "publish", json!(["510300.XSHG"]), Value::Null).1,
        published
    );
    let reference = Reference {
        id: "dividend".into(),
        version: 1,
    };
    assert_eq!(resolve(root, &reference).unwrap().members.len(), 4);
    assert!(
        resolve(
            root,
            &Reference {
                id: "dividend".into(),
                version: 999
            }
        )
        .is_err()
    );
    let immutable = root.join("universes").join(format!(
        "{}.json",
        original["versions"][0]["universe"]
            .as_str()
            .unwrap()
            .trim_start_matches("uni:sha256:")
    ));
    let bytes = fs::read(&immutable).unwrap();
    fs::write(&immutable, b"corrupt synthetic universe").unwrap();
    assert_eq!(
        cli(root, json!({"op":"list"})).1["error"]["code"],
        "pool_store_failed"
    );
    fs::write(immutable, bytes).unwrap();
    fs::write(path, b"corrupt synthetic registry").unwrap();
    assert_eq!(
        cli(root, json!({"op":"list"})).1["error"]["code"],
        "pool_store_failed"
    );
}

#[test]
fn concurrent_processes_do_not_lose_updates_or_overwrite_stale_drafts() {
    let temp = Arc::new(tempfile::tempdir().unwrap());
    let registry = command(temp.path(), PoolCommand::List {}).unwrap();
    let expected: Vec<String> = serde_json::from_value(pool(&registry)["draft"].clone()).unwrap();
    let handles: Vec<_> = ["510300.XSHG", "510880.XSHG"]
        .into_iter()
        .map(|member| {
            let temp = temp.clone();
            let expected = expected.clone();
            std::thread::spawn(move || {
                command(
                    temp.path(),
                    PoolCommand::Save {
                        id: "dividend".into(),
                        expected,
                        members: vec![member.into()],
                    },
                )
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results.into_iter().find_map(Result::err).unwrap().code,
        "pool_conflict"
    );
}

#[test]
fn pool_selection_freezes_old_members_and_dataset_without_changing_legacy_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let dsv = fixture::prepare(root, true);
    let registry = command(root, PoolCommand::List {}).unwrap();
    let legacy: Selection = serde_json::from_value(
        json!({"dsv":dsv,"instrument":"510300.XSHG","strategy":"buy_and_hold"}),
    )
    .unwrap();
    let original_legacy = freeze(root, &legacy).unwrap();
    change(
        root,
        "save",
        pool(&registry)["draft"].clone(),
        json!(["510300.XSHG", "510880.XSHG"]),
    );
    change(
        root,
        "publish",
        json!(["510300.XSHG", "510880.XSHG"]),
        Value::Null,
    );
    let selection: Selection = serde_json::from_value(
        json!({"dsv":dsv,"pool":{"id":"dividend","version":2},"strategy":"buy_and_hold"}),
    )
    .unwrap();
    let first = freeze(root, &selection).unwrap();
    Experiment::parse(&first["data"]["definition"].to_string(), root).unwrap();
    assert_eq!(first["data"]["definition"]["venue"], json!("XSHG"));
    assert_eq!(
        first["data"]["pool"]["members"].as_array().unwrap().len(),
        2
    );
    change(
        root,
        "save",
        json!(["510300.XSHG", "510880.XSHG"]),
        json!(["159952.XSHE"]),
    );
    change(root, "publish", json!(["159952.XSHE"]), Value::Null);
    assert_eq!(freeze(root, &selection).unwrap(), first);
    assert_eq!(freeze(root, &legacy).unwrap(), original_legacy);
    let unknown_dsv = fixture::prepare(root, false);
    let mut unknown = serde_json::to_value(&selection).unwrap();
    unknown["dsv"] = json!(unknown_dsv);
    assert_eq!(
        freeze(root, &serde_json::from_value(unknown).unwrap())
            .unwrap_err()
            .code,
        "selection_evidence_missing"
    );
    change(
        root,
        "save",
        json!(["159952.XSHE"]),
        json!(["159952.XSHE", "510300.XSHG"]),
    );
    change(
        root,
        "publish",
        json!(["159952.XSHE", "510300.XSHG"]),
        Value::Null,
    );
    let mut mixed = serde_json::to_value(&selection).unwrap();
    mixed["pool"]["version"] = json!(4);
    assert_eq!(
        freeze(root, &serde_json::from_value(mixed).unwrap())
            .unwrap_err()
            .code,
        "unsupported_venue"
    );
    let mut missing: Value = serde_json::to_value(&selection).unwrap();
    missing["pool"]["version"] = json!(1);
    assert_eq!(
        freeze(root, &serde_json::from_value(missing).unwrap())
            .unwrap_err()
            .code,
        "instrument_not_in_dataset"
    );
    let mut both = serde_json::to_value(&selection).unwrap();
    both["instrument"] = json!("510300.XSHG");
    assert_eq!(
        freeze(root, &serde_json::from_value(both).unwrap())
            .unwrap_err()
            .code,
        "invalid_selection"
    );
}

#[test]
#[ignore = "exports only a synthetic production Dataset for browser tests"]
fn export_synthetic_pool_fixture() {
    if let Some(root) = std::env::var_os("PRAJNA_POOL_FIXTURE_ROOT") {
        fixture::prepare(Path::new(&root), true);
    }
}
