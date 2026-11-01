#[path = "support/opening_fixture.rs"]
mod fixture;
use prajna_data::{
    daily_publish::{DailyPublicationReceipt, publish_daily},
    pool_publish::{self, PoolSelection},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, process::Command};

fn prepare(root: &Path) -> PoolSelection {
    let mut members = BTreeMap::new();
    for (venue, id) in [("XSHG", "510300.XSHG"), ("XSHE", "159952.XSHE")] {
        let input = root.join(venue);
        fs::create_dir_all(&input).unwrap();
        let (selection, _) = fixture::prepare(
            &input,
            venue,
            prajna_domain::InstrumentKind::Etf,
            false,
            None,
        );
        let name = format!("{venue}.json");
        publish_daily(
            root,
            serde_json::from_value(selection).unwrap(),
            Path::new(&name),
        )
        .unwrap();
        let receipt = serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap();
        members.insert(id.into(), receipt);
    }
    PoolSelection {
        schema_version: 1,
        members,
    }
}
#[test]
fn production_pool_cli_publish_rebuild_keeps_single_security_inputs_and_exact_prices() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let selection = prepare(root);
    fs::write(
        root.join("selection.json"),
        serde_json::to_vec(&selection).unwrap(),
    )
    .unwrap();
    for mode in ["pool", "pool", "rebuild-pool"] {
        let mut cli = Command::new(env!("CARGO_BIN_EXE_prajna-publish-daily"));
        cli.args([mode, "--root"])
            .arg(root)
            .args(["--receipt", "pool.json", "--diagnostics"])
            .arg(root.join("diagnostics"));
        if mode == "pool" {
            cli.arg("--selection").arg(root.join("selection.json"));
        }
        let output = cli.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let receipt: DailyPublicationReceipt =
        serde_json::from_slice(&fs::read(root.join("pool.json")).unwrap()).unwrap();
    let show = prajna_data::dataset_preview::show(root, &receipt.dsv).unwrap();
    assert_eq!(show["instruments"].as_array().unwrap().len(), 2);
    for instrument in show["instruments"].as_array().unwrap() {
        assert_eq!(instrument["coverage"]["bars"], 3);
        assert_eq!(instrument["lifecycle"]["listing_date"]["state"], "known");
    }
    let manifest = prajna_data::read_manifest(root, &receipt.dsv).unwrap();
    assert_eq!(manifest.core.normalizer.version, "3");
    for member in selection.members.values() {
        assert_eq!(
            prajna_data::read_manifest(root, &member.dsv)
                .unwrap()
                .core
                .normalizer
                .version,
            "2"
        );
    }
    let statuses = prajna_data::read_dataset_table(root, &manifest, "execution_status").unwrap();
    assert_eq!(statuses.iter().map(|b| b.num_rows()).sum::<usize>(), 6);
    // Independent price/volume constants from the three explicit supplier rows per member.
    for id in selection.members.keys() {
        let rows = prajna_data::dataset_preview::bars(
            root,
            &prajna_data::dataset_preview::BarsRequest {
                dsv: receipt.dsv.clone(),
                instrument_id: id.clone(),
                start: None,
                end: None,
                limit: 10,
                offset: 0,
            },
        )
        .unwrap();
        assert_eq!(rows["items"][0]["close"], "10.1");
        assert_eq!(rows["items"][0]["volume"], "10000");
    }
}
#[test]
fn pool_rejects_member_mismatch_and_changed_frozen_source() {
    let temp = tempfile::tempdir().unwrap();
    let mut selection = prepare(temp.path());
    let one = selection.members.remove("159952.XSHE").unwrap();
    selection.members.insert("159999.XSHE".into(), one);
    assert!(pool_publish::publish(temp.path(), &selection, Path::new("bad.json")).is_err());
    assert!(!temp.path().join("bad.json").exists());
    let member = selection.members.get_mut("510300.XSHG").unwrap();
    member.recipe_sha256 = format!("sha256:{}", "0".repeat(64));
    assert!(pool_publish::publish(temp.path(), &selection, Path::new("bad.json")).is_err());
}

#[test]
fn pool_revision_keeps_previous_rebuild_and_rejects_receipt_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let first = prepare(temp.path());
    let receipt = pool_publish::publish(temp.path(), &first, Path::new("pool.json")).unwrap();
    let (mut second, _) = fixture::prepare(
        &temp.path().join("revision"),
        "XSHG",
        prajna_domain::InstrumentKind::Etf,
        true,
        None,
    );
    // Fixture creates its stores before the report; publication freezes the partial coverage honestly.
    second["normalizer_version"] = json!("2");
    publish_daily(
        temp.path(),
        serde_json::from_value(second).unwrap(),
        Path::new("revision.json"),
    )
    .unwrap();
    let changed =
        serde_json::from_slice(&fs::read(temp.path().join("revision.json")).unwrap()).unwrap();
    let mut selection = first;
    selection.members.insert("510300.XSHG".into(), changed);
    assert!(pool_publish::publish(temp.path(), &selection, Path::new("pool.json")).is_err());
    let next = pool_publish::publish(temp.path(), &selection, Path::new("next.json")).unwrap();
    assert_ne!(receipt.dsv, next.dsv);
    pool_publish::rebuild(temp.path(), &receipt).unwrap();
    pool_publish::rebuild(temp.path(), &next).unwrap();
    let show: Value = prajna_data::dataset_preview::show(temp.path(), &next.dsv).unwrap();
    let revised = show["instruments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["instrument_id"] == "510300.XSHG")
        .unwrap();
    assert_eq!(revised["coverage"]["bars"], 2);
}

#[test]
fn mismatched_real_venue_session_times_reject_pool_without_publishing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut members = BTreeMap::new();
    for (venue, id) in [("XSHG", "510300.XSHG"), ("XSHE", "159952.XSHE")] {
        let input = root.join(venue);
        let (mut selection, mut status) = fixture::prepare(
            &input,
            venue,
            prajna_domain::InstrumentKind::Etf,
            false,
            None,
        );
        let store = prajna_data::session_calendar::CalendarStore::open(&input).unwrap();
        let old = store
            .get(
                &prajna_data::session_calendar::CalendarVersionId::parse(
                    selection["calendar_version"].as_str().unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        let mut calendar = serde_json::to_value(old).unwrap();
        for selected in calendar["venues"].as_array_mut().unwrap() {
            if selected["venue_id"] == "XSHE" {
                selected["sessions"][0]["ts_open"] = json!("2026-01-05T09:31:00+08:00");
            }
        }
        let version = store
            .import_json(&serde_json::to_vec(&calendar).unwrap())
            .unwrap();
        selection["calendar_version"] = json!(version.as_str());
        status["calendar_version"] = json!(version.as_str());
        if venue == "XSHE" {
            status["facts"][0]["open_from"] = json!("2026-01-05T09:31:00+08:00");
            status["facts"][0]["open_to_exclusive"] = json!("2026-01-05T09:32:00+08:00");
        }
        selection["status_snapshot"] = json!(fixture::put(&input, &status));
        let receipt = format!("{venue}.json");
        publish_daily(
            root,
            serde_json::from_value(selection).unwrap(),
            Path::new(&receipt),
        )
        .unwrap();
        members.insert(
            id.into(),
            serde_json::from_slice(&fs::read(root.join(receipt)).unwrap()).unwrap(),
        );
    }
    let result = pool_publish::publish(
        root,
        &PoolSelection {
            schema_version: 1,
            members,
        },
        Path::new("bad-pool.json"),
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("inconsistent_venue_calendar")
    );
    assert!(!root.join("bad-pool.json").exists());
    assert_eq!(fs::read_dir(root.join("manifests")).unwrap().count(), 2);
}
