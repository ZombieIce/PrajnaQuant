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
         "coverage_start":"2026-01-05", "coverage_end":"2026-01-10", "sessions":[
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

fn version_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let Ok(prefixes) = fs::read_dir(root.join("session-calendars/sha256")) else {
        return files;
    };
    for prefix in prefixes {
        let Ok(entries) = fs::read_dir(prefix.unwrap().path()) else {
            continue;
        };
        for entry in entries {
            files.push(entry.unwrap().path());
        }
    }
    files.sort();
    files
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
    // Each venue keeps its own independently declared coverage.
    assert_eq!(document.venues[0].coverage_start, day("2026-01-05"));
    assert_eq!(document.venues[0].coverage_end, day("2026-01-10"));
    assert_eq!(document.venues[1].coverage_start, day("2026-01-01"));
    assert_eq!(document.venues[1].coverage_end, day("2026-01-10"));
    let sessions = |id: &CalendarVersionId, v: &str, start: &str, end: &str| {
        store.sessions(id, &venue(v), day(start), day(end)).unwrap()
    };
    let sh = sessions(&first, "XSHG", "2026-01-05", "2026-01-10");
    let sz = sessions(&first, "XSHE", "2026-01-01", "2026-01-10");
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
    assert_eq!(sessions(&first, "XSHG", "2026-01-05", "2026-01-10"), sh);
    assert_eq!(sessions(&revised, "XSHE", "2026-01-01", "2026-01-10"), sz);
    assert_ne!(
        sessions(&revised, "XSHG", "2026-01-05", "2026-01-10")[1].ts_close(),
        sh[1].ts_close()
    );
    let conflict = import_cli(&root, &source, "first");
    assert_eq!(conflict.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("already pins a different version"));
    assert_eq!(store.read_receipt("first").unwrap(), first);

    let rebuilt = CalendarStore::open(tmp.path().join("rebuilt")).unwrap();
    let original = fixture();
    assert_eq!(rebuilt.import_json(&bytes(&original)).unwrap(), first);
    assert_eq!(
        rebuilt
            .sessions(&first, &venue("XSHG"), day("2026-01-05"), day("2026-01-10"))
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
    // One venue's declared coverage does not gate reads for the other venue.
    assert_eq!(
        store
            .sessions(&id, &venue("XSHE"), day("2026-01-01"), day("2026-01-10"))
            .unwrap()
            .len(),
        2
    );
    assert!(matches!(
        store.sessions(&id, &venue("XSHG"), day("2026-01-01"), day("2026-01-10")),
        Err(CalendarError::OutsideCoverage)
    ));
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
    store.write_receipt("pinned", &id).unwrap();
    // The same pin is a no-op; another version is a conflict, not an overwrite.
    store.write_receipt("pinned", &id).unwrap();
    assert_eq!(store.read_receipt("pinned").unwrap(), id);
    let mut revised = fixture();
    revised["venues"][0]["sessions"][0]["ts_close"] = json!("2026-01-06T14:59:00+08:00");
    let revised = store.import_json(&bytes(&revised)).unwrap();
    assert_ne!(id, revised);
    assert!(matches!(
        store.write_receipt("pinned", &revised),
        Err(CalendarError::ReceiptConflict)
    ));
    assert_eq!(store.read_receipt("pinned").unwrap(), id);
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

#[test]
fn cli_same_receipt_repeat_is_idempotent_and_conflict_is_explicit() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("lake");
    let source = tmp.path().join("calendar.json");
    fs::write(&source, bytes(&fixture())).unwrap();
    let first = import_cli(&root, &source, "pinned");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let store = CalendarStore::open(&root).unwrap();
    let pinned = store.read_receipt("pinned").unwrap();
    let receipt_path = root.join("session-calendars/receipts/pinned.txt");
    let receipt = fs::read(&receipt_path).unwrap();
    assert_eq!(receipt, format!("{}\n", pinned.as_str()).as_bytes());
    assert_eq!(version_files(&root).len(), 1);

    // The exact production command can be repeated; the receipt is untouched.
    let repeat = import_cli(&root, &source, "pinned");
    assert!(
        repeat.status.success(),
        "{}",
        String::from_utf8_lossy(&repeat.stderr)
    );
    assert_eq!(fs::read(&receipt_path).unwrap(), receipt);
    assert_eq!(store.read_receipt("pinned").unwrap(), pinned);
    assert_eq!(version_files(&root).len(), 1);
    assert!(!String::from_utf8_lossy(&repeat.stdout).contains("sha256:"));

    // Canonically equivalent content also repeats under the same receipt.
    let mut equivalent = fixture();
    equivalent["venues"].as_array_mut().unwrap().reverse();
    equivalent["venues"][0]["sessions"]
        .as_array_mut()
        .unwrap()
        .reverse();
    equivalent["venues"][0]["sessions"][1]["ts_open"] = json!("2026-01-07T01:30:00Z");
    fs::write(&source, bytes(&equivalent)).unwrap();
    let repeat = import_cli(&root, &source, "pinned");
    assert!(
        repeat.status.success(),
        "{}",
        String::from_utf8_lossy(&repeat.stderr)
    );
    assert_eq!(fs::read(&receipt_path).unwrap(), receipt);
    assert_eq!(version_files(&root).len(), 1);

    // A revision under the existing receipt fails specifically without
    // revealing a version identity, and the pin is not rewritten.
    let mut revised = fixture();
    revised["venues"][0]["sessions"][0]["ts_close"] = json!("2026-01-06T14:59:00+08:00");
    fs::write(&source, bytes(&revised)).unwrap();
    let conflict = import_cli(&root, &source, "pinned");
    assert_eq!(conflict.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&conflict.stderr);
    assert!(
        stderr.contains("already pins a different version"),
        "{stderr}"
    );
    assert!(!stderr.contains("sha256:"), "{stderr}");
    assert_eq!(fs::read(&receipt_path).unwrap(), receipt);
    assert_eq!(store.read_receipt("pinned").unwrap(), pinned);

    // The revision stays an immutable version and can be pinned under a new name.
    let revision = import_cli(&root, &source, "revision");
    assert!(
        revision.status.success(),
        "{}",
        String::from_utf8_lossy(&revision.stderr)
    );
    assert_ne!(store.read_receipt("revision").unwrap(), pinned);
    assert_eq!(store.read_receipt("pinned").unwrap(), pinned);
}

#[test]
fn cli_rejects_invalid_receipt_before_persisting_a_version() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("lake");
    let source = tmp.path().join("calendar.json");
    fs::write(&source, bytes(&fixture())).unwrap();
    let output = import_cli(&root, &source, "../escape");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid calendar receipt name"));
    assert!(version_files(&root).is_empty());
}

#[test]
fn help_prints_usage_and_succeeds() {
    let output = Command::new(env!("CARGO_BIN_EXE_prajna-session-calendar"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage:"));
    assert!(stdout.contains("--receipt"));
    assert!(!stdout.contains("Calendar version imported"));
    assert!(!stdout.contains("sha256:"));
}

#[test]
fn concurrent_same_receipt_imports_converge_on_one_version() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("lake");
    let store = CalendarStore::open(&root).unwrap();
    let payload = bytes(&fixture());
    let handles = (0..8)
        .map(|_| {
            let store = store.clone();
            let payload = payload.clone();
            std::thread::spawn(move || {
                let id = store.import_json(&payload).unwrap();
                store.write_receipt("shared", &id).unwrap();
                id
            })
        })
        .collect::<Vec<_>>();
    let mut ids = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    assert!(ids.windows(2).all(|pair| pair[0] == pair[1]));
    let reopened = CalendarStore::open(&root).unwrap();
    assert_eq!(reopened.read_receipt("shared").unwrap(), ids[0]);
    assert_eq!(version_files(&root).len(), 1);
}
