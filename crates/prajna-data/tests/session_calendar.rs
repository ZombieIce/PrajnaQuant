use std::{fs, process::Command};

use chrono::NaiveDate;
use prajna_data::session_calendar::{CalendarError, CalendarStore, CalendarVersionId};
use prajna_domain::VenueId;
use serde_json::{Value, json};

fn fixture() -> Value {
    json!({"schema_version": 1, "venues": [
        {"venue_id":"XSHE", "source":{"source_id":"synthetic-sz", "source_version":"1", "source_ref":"fixture-sz"},
         "coverage_start":"2026-01-01", "coverage_end":"2026-01-10", "sessions":[
            {"session_date":"2026-01-06", "ts_open":"2026-01-06T09:30:00+08:00", "ts_close":"2026-01-06T15:00:00+08:00"},
            {"session_date":"2026-01-07", "ts_open":"2026-01-07T09:35:00+08:00", "ts_close":"2026-01-07T15:00:00+08:00"}]},
        {"venue_id":"XSHG", "source":{"source_id":"synthetic-sh", "source_version":"1", "source_ref":"fixture-sh"},
         "coverage_start":"2026-01-01", "coverage_end":"2026-01-10", "sessions":[
            {"session_date":"2026-01-07", "ts_open":"2026-01-07T09:30:00+08:00", "ts_close":"2026-01-07T15:00:00+08:00"},
            {"session_date":"2026-01-05", "ts_open":"2026-01-05T09:30:00+08:00", "ts_close":"2026-01-05T15:00:00+08:00"}]}
    ]})
}
fn day(value: &str) -> NaiveDate {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
}
fn venue(value: &str) -> VenueId {
    VenueId::new(value).unwrap()
}
fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn import_cli(
    root: &std::path::Path,
    input: &std::path::Path,
    receipt: &str,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_prajna-session-calendar"))
        .args(["import", "--root"])
        .arg(root)
        .arg("--input")
        .arg(input)
        .args(["--receipt", receipt])
        .output()
        .unwrap()
}

#[test]
fn cli_import_pins_isolated_venues_revisions_and_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("lake");
    let source = tmp.path().join("calendar.json");
    let mut input = fixture();
    fs::write(&source, bytes(&input)).unwrap();
    let output = import_cli(&root, &source, "first");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("sha256:"));
    let store = CalendarStore::open(&root).unwrap();
    let first = store.read_receipt("first").unwrap();
    let document = store.get(&first).unwrap();
    assert_eq!(document.venues[0].venue_id, venue("XSHG"));
    assert_eq!(document.venues[0].source.source_id, "synthetic-sh");
    assert_eq!(document.venues[1].source.source_id, "synthetic-sz");
    assert_eq!(document.venues[0].coverage_start, day("2026-01-01"));
    let sessions = |id: &CalendarVersionId, v: &str| {
        store
            .sessions(id, &venue(v), day("2026-01-01"), day("2026-01-10"))
            .unwrap()
    };
    let sh = sessions(&first, "XSHG");
    let sz = sessions(&first, "XSHE");
    assert_eq!(
        sh.iter().map(|s| s.session_date()).collect::<Vec<_>>(),
        [day("2026-01-05"), day("2026-01-07")]
    );
    assert_eq!(
        sz.iter().map(|s| s.session_date()).collect::<Vec<_>>(),
        [day("2026-01-06"), day("2026-01-07")]
    );
    assert_eq!(sh[0].venue_id(), &venue("XSHG"));
    assert_eq!(sz[0].venue_id(), &venue("XSHE"));
    assert_ne!(sh[1].ts_open(), sz[1].ts_open());
    assert!(
        store
            .sessions(&first, &venue("XSHG"), day("2026-01-06"), day("2026-01-06"))
            .unwrap()
            .is_empty()
    );

    // Ordering is canonical, but genuine revisions receive a different version.
    input["venues"].as_array_mut().unwrap().reverse();
    input["venues"][0]["sessions"]
        .as_array_mut()
        .unwrap()
        .reverse();
    input["venues"][0]["sessions"][1]["ts_open"] = json!("2026-01-07T01:30:00Z");
    fs::write(&source, bytes(&input)).unwrap();
    assert!(import_cli(&root, &source, "repeat").status.success());
    assert_eq!(first, store.read_receipt("repeat").unwrap());
    input["venues"][0]["sessions"][1]["ts_close"] = json!("2026-01-07T14:59:00+08:00");
    fs::write(&source, bytes(&input)).unwrap();
    assert!(import_cli(&root, &source, "revision").status.success());
    let revised = store.read_receipt("revision").unwrap();
    assert_ne!(first, revised);
    assert_eq!(sessions(&first, "XSHG"), sh);
    assert_eq!(sessions(&revised, "XSHE"), sz);
    assert_ne!(sessions(&revised, "XSHG")[1].ts_close(), sh[1].ts_close());
    assert!(!import_cli(&root, &source, "first").status.success());
    assert_eq!(store.read_receipt("first").unwrap(), first);

    let rebuilt = CalendarStore::open(tmp.path().join("rebuilt")).unwrap();
    let original = fixture();
    assert_eq!(rebuilt.import_json(&bytes(&original)).unwrap(), first);
    assert_eq!(
        rebuilt
            .sessions(&first, &venue("XSHG"), day("2026-01-01"), day("2026-01-10"))
            .unwrap(),
        sh
    );
}

#[test]
fn rejects_missing_conflicting_invalid_and_out_of_coverage_without_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let store = CalendarStore::open(tmp.path()).unwrap();
    let mut input = fixture();
    input["venues"].as_array_mut().unwrap().pop();
    assert!(matches!(
        store.import_json(&bytes(&input)),
        Err(CalendarError::Invalid(_))
    ));
    input = fixture();
    input["venues"][1]["venue_id"] = json!("XSHE");
    assert!(matches!(
        store.import_json(&bytes(&input)),
        Err(CalendarError::Invalid(_))
    ));
    for conflicting in [false, true] {
        input = fixture();
        let mut duplicate = input["venues"][0]["sessions"][0].clone();
        if conflicting {
            duplicate["ts_close"] = json!("2026-01-06T14:00:00+08:00");
        }
        input["venues"][0]["sessions"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(matches!(
            store.import_json(&bytes(&input)),
            Err(CalendarError::Invalid(_))
        ));
    }
    for (field, value) in [
        ("ts_open", "2026-01-06T15:00:00+08:00"),
        ("ts_close", "2026-01-06T09:30:00+08:00"),
    ] {
        input = fixture();
        input["venues"][0]["sessions"][0][field] = json!(value);
        assert!(matches!(
            store.import_json(&bytes(&input)),
            Err(CalendarError::Invalid(_))
        ));
    }
    input = fixture();
    input["venues"][0]["sessions"][0]["ts_open"] = json!("2026-01-05T09:30:00+08:00");
    assert!(matches!(
        store.import_json(&bytes(&input)),
        Err(CalendarError::Invalid(_))
    ));
    input = fixture();
    input["venues"][0]["sessions"][0]["ts_open"] = json!("2026-01-06T09:30:00");
    assert!(matches!(
        store.import_json(&bytes(&input)),
        Err(CalendarError::Json(_))
    ));
    input = fixture();
    input["venues"][0]["coverage_end"] = json!("2026-01-05");
    assert!(matches!(
        store.import_json(&bytes(&input)),
        Err(CalendarError::Invalid(_))
    ));
    input = fixture();
    input["venues"][0]["source"]["source_ref"] = json!("");
    assert!(matches!(
        store.import_json(&bytes(&input)),
        Err(CalendarError::Invalid(_))
    ));
    let id = store.import_json(&bytes(&fixture())).unwrap();
    assert!(matches!(
        store.sessions(&id, &venue("XSHG"), day("2025-12-31"), day("2026-01-01")),
        Err(CalendarError::OutsideCoverage)
    ));
    assert!(matches!(
        store.sessions(&id, &venue("XSHG"), day("2026-01-07"), day("2026-01-06")),
        Err(CalendarError::OutsideCoverage)
    ));
    assert!(matches!(
        store.sessions(&id, &venue("BJSE"), day("2026-01-01"), day("2026-01-10")),
        Err(CalendarError::MissingVenue)
    ));
    assert!(CalendarVersionId::parse("sha256:../bad").is_err());

    // A source may explicitly declare an all-closed covered range.
    let mut closed = fixture();
    closed["venues"][0]["sessions"] = json!([]);
    let closed_id = store.import_json(&bytes(&closed)).unwrap();
    assert!(
        store
            .sessions(
                &closed_id,
                &venue("XSHE"),
                day("2026-01-01"),
                day("2026-01-10")
            )
            .unwrap()
            .is_empty()
    );
    assert!(CalendarStore::open(prajna_data::default_mvp6_root()).is_err());
}

#[test]
fn corruption_and_receipt_path_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = CalendarStore::open(tmp.path()).unwrap();
    let id = store.import_json(&bytes(&fixture())).unwrap();
    assert!(matches!(
        store.write_receipt("../escape", &id),
        Err(CalendarError::InvalidReceipt)
    ));
    let hex = &id.as_str()[7..];
    let path = tmp
        .path()
        .join("session-calendars/sha256")
        .join(&hex[..2])
        .join(format!("{hex}.json"));
    fs::write(path, b"corrupt").unwrap();
    assert!(matches!(store.get(&id), Err(CalendarError::CorruptVersion)));
    assert!(matches!(
        store.import_json(&bytes(&fixture())),
        Err(CalendarError::CorruptVersion)
    ));
}

#[cfg(unix)]
#[test]
fn output_symlink_cannot_redirect_calendar_writes() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("lake");
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();
    symlink(&elsewhere, root.join("session-calendars")).unwrap();
    let store = CalendarStore::open(root).unwrap();
    assert!(matches!(
        store.import_json(&bytes(&fixture())),
        Err(CalendarError::Path(_))
    ));
    assert_eq!(fs::read_dir(elsewhere).unwrap().count(), 0);
}
