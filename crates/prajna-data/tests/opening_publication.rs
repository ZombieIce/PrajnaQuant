#[path = "support/opening_fixture.rs"]
mod fixture;
use prajna_data::{
    daily_publish::DailyPublicationReceipt, execution_status_from_record_batch, read_dataset_table,
    read_manifest,
};
use prajna_domain::InstrumentKind;
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn cli(root: &Path, selection: Option<&Value>, receipt: &str) -> bool {
    fs::create_dir_all(root).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_prajna-publish-daily"));
    command
        .arg(if selection.is_some() {
            "publish"
        } else {
            "rebuild"
        })
        .arg("--root")
        .arg(root)
        .arg("--receipt")
        .arg(receipt)
        .arg("--diagnostics")
        .arg(root.join("diagnostics"));
    if let Some(selection) = selection {
        fs::write(
            root.join("selection.json"),
            serde_json::to_vec(selection).unwrap(),
        )
        .unwrap();
        command.arg("--selection").arg(root.join("selection.json"));
    }
    let output = command.output().unwrap();
    assert!(!String::from_utf8_lossy(&output.stderr).contains("sha256:"));
    output.status.success()
}
fn receipt(root: &Path, name: &str) -> DailyPublicationReceipt {
    serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap()
}
fn rows(root: &Path, name: &str) -> Vec<prajna_data::ExecutionStatusRow> {
    let manifest = read_manifest(root, &receipt(root, name).dsv).unwrap();
    let batches = read_dataset_table(root, &manifest, "execution_status").unwrap();
    batches
        .iter()
        .flat_map(|batch| execution_status_from_record_batch(batch).unwrap())
        .collect()
}

#[test]
fn v2_fixed_inputs_repeat_revise_rebuild_and_preserve_v1_for_four_security_kinds() {
    for venue in ["XSHG", "XSHE"] {
        for kind in [InstrumentKind::Etf, InstrumentKind::Equity] {
            let temp = tempfile::tempdir().unwrap();
            let input = temp.path().join("input");
            let lake = temp.path().join("lake");
            let (mut selection, mut snapshot) = fixture::prepare(&input, venue, kind, false, None);
            let mut legacy = selection.clone();
            legacy["normalizer_version"] = json!("1");
            legacy.as_object_mut().unwrap().remove("status_snapshot");
            assert!(cli(&lake, Some(&legacy), "v1.json"));
            let old = receipt(&lake, "v1.json").dsv;
            assert!(
                read_dataset_table(
                    &lake,
                    &read_manifest(&lake, &old).unwrap(),
                    "execution_status"
                )
                .is_err()
            );
            assert!(cli(&lake, Some(&selection), "v2.json"));
            let dsv = receipt(&lake, "v2.json").dsv;
            assert!(rows(&lake, "v2.json").iter().all(|row| row.is_tradable));
            assert!(cli(&lake, Some(&selection), "v2.json"));
            assert_eq!(dsv, receipt(&lake, "v2.json").dsv);
            snapshot["facts"][1]["state"] = json!("HALTED");
            selection["status_snapshot"] = json!(fixture::put(&input, &snapshot));
            assert!(cli(&lake, Some(&selection), "revised.json"));
            assert_ne!(dsv, receipt(&lake, "revised.json").dsv);
            assert!(!rows(&lake, "revised.json")[1].is_tradable);
            assert!(cli(&lake, Some(&legacy), "v1.json"));
            assert_eq!(old, receipt(&lake, "v1.json").dsv);
            fs::remove_dir_all(&input).unwrap();
            for name in ["v1.json", "v2.json", "revised.json"] {
                assert!(cli(&lake, None, name));
            }
        }
    }
}

#[test]
fn missing_unknown_null_late_conflict_and_identical_duplicate_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let lake = temp.path().join("lake");
    let (selection, snapshot) = fixture::prepare(&input, "XSHG", InstrumentKind::Etf, true, None);
    for variant in [
        "empty",
        "missing",
        "unknown",
        "null",
        "reference_null",
        "necessary_null",
        "late",
        "halted",
        "conflict",
        "duplicate",
    ] {
        let mut snapshot = snapshot.clone();
        let mut selection = selection.clone();
        match variant {
            "necessary_null" => {
                let mut reference = snapshot["evidence"][0].clone();
                reference["available_at"] = Value::Null;
                snapshot["evidence"].as_array_mut().unwrap().push(reference);
                snapshot["facts"][1]["evidence_indices"] = json!([0, 1]);
            }
            "empty" => snapshot["facts"] = json!([]),
            "missing" => {
                snapshot["facts"].as_array_mut().unwrap().remove(1);
            }
            "unknown" => snapshot["facts"][1]["state"] = json!("UNKNOWN"),
            "null" => snapshot["facts"][1]["available_at"] = Value::Null,
            "reference_null" => snapshot["evidence"][0]["available_at"] = Value::Null,
            "late" => {
                snapshot["facts"][1]["available_at"] = json!("2026-01-06T09:30:00.000000001+08:00")
            }
            "halted" => snapshot["facts"][1]["state"] = json!("HALTED"),
            "conflict" => snapshot["facts"][1]["state"] = json!("CONFLICT"),
            "duplicate" => {
                let fact = snapshot["facts"][1].clone();
                snapshot["facts"].as_array_mut().unwrap().push(fact);
            }
            _ => unreachable!(),
        }
        selection["status_snapshot"] = json!(fixture::put(&input, &snapshot));
        let name = format!("{variant}.json");
        assert!(cli(&lake, Some(&selection), &name));
        let rows = rows(&lake, &name);
        assert_eq!(rows.len(), 3);
        if !matches!(variant, "late" | "duplicate") {
            assert!(!rows[1].is_tradable);
        }
        if variant == "late" {
            assert!(
                rows[1].available_at.unwrap()
                    > prajna_domain::TimestampNs::parse("2026-01-06T09:30:00+08:00").unwrap()
            );
        }
    }
}

#[test]
fn invalid_evidence_fails_atomically_without_mutating_old_dataset() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let lake = temp.path().join("lake");
    let (selection, snapshot) = fixture::prepare(&input, "XSHE", InstrumentKind::Etf, false, None);
    assert!(cli(&lake, Some(&selection), "old.json"));
    let dsv = receipt(&lake, "old.json").dsv;
    for variant in [
        "unselected",
        "corrupt",
        "instrument",
        "venue",
        "calendar",
        "security",
        "market",
        "scope",
        "date",
        "non_session",
        "interval",
        "enum",
        "reference",
        "missing_raw",
        "wrong_hash",
        "locator",
        "earlier",
        "necessary_later",
        "source",
        "observed",
        "duplicate",
    ] {
        let mut snapshot = snapshot.clone();
        let mut selection = selection.clone();
        match variant {
            "instrument" => snapshot["instrument_id"] = json!("000001.XSHE"),
            "venue" => snapshot["venue_id"] = json!("XSHG"),
            "calendar" => {
                snapshot["calendar_version"] = json!(format!("sha256:{}", "a".repeat(64)))
            }
            "security" => {
                snapshot["security_snapshot"] = json!(format!("sha256:{}", "b".repeat(64)))
            }
            "market" => snapshot["market_raw_sha256s"] = json!([]),
            "scope" => snapshot["end"] = json!("2026-01-08"),
            "date" => snapshot["facts"][0]["session_date"] = json!("2026-01-08"),
            "non_session" => snapshot["facts"][0]["session_date"] = json!("2026-01-04"),
            "interval" => {
                snapshot["facts"][0]["open_to_exclusive"] = json!("2026-01-05T09:30:00+08:00")
            }
            "enum" => snapshot["facts"][0]["state"] = json!("GUESS"),
            "reference" => snapshot["facts"][0]["evidence_indices"] = json!([99]),
            "missing_raw" => {
                snapshot["evidence"][0]["raw_sha256"] = json!(format!("sha256:{}", "f".repeat(64)))
            }
            "wrong_hash" => snapshot["evidence"][0]["raw_sha256"] = json!("wrong hash"),
            "locator" => snapshot["evidence"][0]["locator"] = json!(""),
            "source" => snapshot["extraction_version"] = json!(""),
            "observed" => snapshot["observed_at"] = json!("unknown"),
            "necessary_later" => {
                let mut reference = snapshot["evidence"][0].clone();
                reference["available_at"] = json!("2026-01-06T09:31:00+08:00");
                snapshot["evidence"].as_array_mut().unwrap().push(reference);
                snapshot["facts"][1]["evidence_indices"] = json!([0, 1]);
            }
            "earlier" => snapshot["facts"][0]["available_at"] = json!("2026-01-03T00:00:00Z"),
            "duplicate" => {
                let mut fact = snapshot["facts"][0].clone();
                fact["reason"] = json!("conflicting evidence");
                snapshot["facts"].as_array_mut().unwrap().push(fact);
            }
            _ => {}
        }
        selection["status_snapshot"] = json!(fixture::put(&input, &snapshot));
        if variant == "unselected" {
            selection.as_object_mut().unwrap().remove("status_snapshot");
        }
        if variant == "corrupt" {
            selection["status_snapshot"] = json!(fixture::put(&input, &json!({"invalid":true})));
        }
        assert!(!cli(&lake, Some(&selection), "bad.json"), "{variant}");
        assert!(!lake.join("bad.json").exists());
        assert_eq!(fs::read_dir(lake.join("manifests")).unwrap().count(), 1);
        assert_eq!(read_manifest(&lake, &dsv).unwrap().dsv, dsv);
    }
}

#[test]
fn later_status_table_io_failure_rolls_back_and_retry_uses_frozen_input() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let lake = temp.path().join("lake");
    let (selection, _) = fixture::prepare(&input, "XSHG", InstrumentKind::Etf, false, None);
    let mut old = selection.clone();
    old["normalizer_version"] = json!("1");
    old.as_object_mut().unwrap().remove("status_snapshot");
    assert!(cli(&lake, Some(&old), "v1.json"));
    fs::write(
        lake.join("normalized/execution_status"),
        b"synthetic obstruction",
    )
    .unwrap();
    assert!(!cli(&lake, Some(&selection), "v2.json"));
    assert_eq!(fs::read_dir(lake.join("manifests")).unwrap().count(), 1);
    assert!(!lake.join("v2.json").exists());
    fs::remove_file(lake.join("normalized/execution_status")).unwrap();
    assert!(cli(&lake, Some(&selection), "v2.json"));
    assert!(cli(&lake, None, "v1.json"));
}

#[test]
fn corrupt_original_bytes_cannot_publish_under_claimed_hash() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let lake = temp.path().join("lake");
    let (selection, snapshot) = fixture::prepare(&input, "XSHE", InstrumentKind::Etf, false, None);
    let hash = snapshot["evidence"][0]["raw_sha256"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    fs::write(
        input.join("raw/sha256").join(&hash[..2]).join(hash),
        b"corrupted synthetic bytes",
    )
    .unwrap();
    assert!(!cli(&lake, Some(&selection), "bad.json"));
    assert!(!lake.join("bad.json").exists());
    assert!(!lake.join("manifests").exists());
}
