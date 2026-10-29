//! Synthetic source files only; exercise the shipping binary and persisted reads.
use prajna_data::corporate_facts;
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};
fn source() -> Value {
    json!({"source_id":"synthetic","source_version":"1","source_ref":"fixture://actions"})
}
fn fixture() -> Value {
    let scope = json!({"instrument_id":"510300.XSHG","kind":"etf",
        "start":"2026-01-01","end":"2026-01-31","state":"covered","reason":"supplied structured events",
        "source":source(),"available_at":"2025-12-31T12:00:00+08:00",
        "facts":[{"event_id":"dividend-1","kind":"dividend","record_date":"2026-01-05",
        "ex_date":"2026-01-06","pay_date":"2026-02-01","effective_date":null,
        "cash_per_share":"0.123456789012345678","share_ratio":null,"source":source(),
        "available_at":"2025-12-31T12:00:00+08:00"}]});
    let mut no_events = scope.clone();
    no_events["instrument_id"] = json!("000001.XSHE");
    no_events["kind"] = json!("stock");
    no_events["state"] = json!("no_events");
    no_events["facts"] = json!([]);
    let mut missing = scope.clone();
    missing["instrument_id"] = json!("159915.XSHE");
    missing["state"] = json!("missing_facts");
    missing["facts"][0]["cash_per_share"] = Value::Null;
    missing["facts"][0]["available_at"] = Value::Null;
    let mut unknown = no_events.clone();
    unknown["instrument_id"] = json!("600000.XSHG");
    unknown["state"] = json!("unknown");
    unknown["source"] = Value::Null;
    unknown["available_at"] = Value::Null;
    json!({"schema_version":1,"observed_at":"2026-03-01T00:00:00Z",
        "coverage":[scope,no_events,missing,unknown]})
}
fn cli(args: &[&str], success: bool) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_prajna-corporate-facts"))
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        success,
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn publish(root: &Path, input: &Value) -> String {
    let file = root.join("input.json");
    fs::write(&file, serde_json::to_vec(input).unwrap()).unwrap();
    cli(
        &[
            "import",
            "--root",
            root.to_str().unwrap(),
            "--input",
            file.to_str().unwrap(),
        ],
        true,
    )["version"]
        .as_str()
        .unwrap()
        .into()
}
fn query(root: &Path, id: &str, instrument: &str) -> Value {
    cli(
        &[
            "read",
            "--root",
            root.to_str().unwrap(),
            "--version",
            id,
            "--instrument",
            instrument,
            "--start",
            "2026-01-01",
            "--end",
            "2026-01-31",
        ],
        true,
    )
}
#[test]
fn production_publish_read_revision_and_four_scoped_states() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut input = fixture();
    let first = publish(root, &input);
    assert_eq!(first, publish(root, &input));
    for (security, expected) in [
        ("510300.XSHG", "covered"),
        ("000001.XSHE", "no_events"),
        ("159915.XSHE", "missing_facts"),
        ("600000.XSHG", "unknown"),
        ("000001.XSHG", "unknown"),
    ] {
        let result = query(root, &first, security);
        assert_eq!(result["state"], expected);
        assert_eq!(result["instrument_id"], security);
    }
    let missing = query(root, &first, "159915.XSHE");
    assert_eq!(
        missing["missing"][0]["fields"],
        json!(["available_at", "cash_per_share"])
    );
    let old = query(root, &first, "510300.XSHG");
    assert_eq!(
        old["coverage"]["facts"][0]["cash_per_share"],
        "0.123456789012345678"
    );
    input["coverage"][0]["facts"][0]["cash_per_share"] = json!("0.2");
    let second = publish(root, &input);
    assert_ne!(first, second);
    assert_eq!(query(root, &first, "510300.XSHG"), old);
    assert_eq!(
        query(root, &second, "510300.XSHG")["coverage"]["facts"][0]["cash_per_share"],
        "0.2"
    );
    let out = corporate_facts::query(
        root,
        &first,
        "510300.XSHG",
        "2025-12-31".parse().unwrap(),
        "2026-01-01".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(out["state"], "unknown");
    assert!(out["coverage"].is_null());
    // Versioned bytes are immutable; corruption fails both reading and reimport.
    let object = root.join(format!(
        "corporate-facts/{}.json",
        first.strip_prefix("caf:sha256:").unwrap()
    ));
    fs::write(object, b"{}").unwrap();
    assert!(corporate_facts::read(root, &first).is_err());
    assert!(corporate_facts::import(root, &serde_json::to_vec(&fixture()).unwrap()).is_err());
    let error = cli(
        &[
            "read",
            "--root",
            root.to_str().unwrap(),
            "--version",
            &first,
            "--instrument",
            "510300.XSHG",
            "--start",
            "2026-01-01",
            "--end",
            "2026-01-31",
        ],
        false,
    );
    assert!(error["error"]["code"].is_string());
}
#[test]
fn split_fields_and_partial_events_preserve_unknowns() {
    let temp = tempfile::tempdir().unwrap();
    let mut input = fixture();
    let fact = &mut input["coverage"][0]["facts"][0];
    fact["kind"] = json!("share_conversion");
    fact["cash_per_share"] = Value::Null;
    fact["share_ratio"] = json!("1.234567890123456789");
    fact["effective_date"] = json!("2026-01-06");
    let id = publish(temp.path(), &input);
    assert_eq!(
        query(temp.path(), &id, "510300.XSHG")["coverage"]["facts"][0]["share_ratio"],
        "1.234567890123456789"
    );
    for field in [
        "source",
        "available_at",
        "record_date",
        "ex_date",
        "pay_date",
        "cash_per_share",
    ] {
        let mut partial = fixture();
        partial["coverage"][0]["facts"][0][field] = Value::Null;
        assert!(
            corporate_facts::import(temp.path(), &serde_json::to_vec(&partial).unwrap()).is_err(),
            "{field}"
        );
        partial["coverage"][0]["state"] = json!("missing_facts");
        let id = publish(temp.path(), &partial);
        let result = query(temp.path(), &id, "510300.XSHG");
        assert_eq!(result["state"], "missing_facts");
        assert!(
            result["missing"][0]["fields"]
                .as_array()
                .unwrap()
                .contains(&json!(field))
        );
    }
}
#[test]
fn malformed_conflicting_and_unsupported_inputs_fail_without_publication() {
    let temp = tempfile::tempdir().unwrap();
    let original = fixture();
    let mut cases = Vec::new();
    for (pointer, value) in [
        ("/schema_version", json!(2)),
        ("/unexpected", json!(true)),
        ("/observed_at", json!("yesterday")),
        ("/coverage/0/end", json!("2025-01-01")),
        ("/coverage/0/instrument_id", json!("510300.SYNTH")),
        ("/coverage/0/kind", json!("future")),
        ("/coverage/0/reason", json!("")),
        ("/coverage/0/source", Value::Null),
        ("/coverage/1/available_at", Value::Null),
        ("/coverage/0/facts/0/source/source_ref", json!("")),
        ("/coverage/0/facts/0/available_at", json!("date-only")),
        ("/coverage/0/facts/0/cash_per_share", json!("NaN")),
        (
            "/coverage/0/facts/0/cash_per_share",
            json!("0.0000000000000000001"),
        ),
        ("/coverage/0/facts/0/cash_per_share", json!("-1")),
        ("/coverage/0/facts/0/share_ratio", json!("1")),
        ("/coverage/0/facts/0/ex_date", json!("2026-02-01")),
        ("/coverage/0/facts/0/record_date", json!("2026-01-07")),
        ("/coverage/0/facts/0/pay_date", json!("2026-01-05")),
        ("/coverage/0/facts/0/event_id", json!("")),
        ("/coverage/0/facts", json!([])),
        ("/coverage/0/state", json!("no_events")),
        ("/coverage/0/state", json!("unknown")),
    ] {
        let mut case = original.clone();
        if pointer == "/unexpected" {
            case["unexpected"] = value;
        } else {
            *case.pointer_mut(pointer).unwrap() = value;
        }
        cases.push(case);
    }
    let mut overlap = original.clone();
    overlap["coverage"]
        .as_array_mut()
        .unwrap()
        .push(original["coverage"][0].clone());
    cases.push(overlap);
    let mut duplicate = original.clone();
    duplicate["coverage"][0]["facts"]
        .as_array_mut()
        .unwrap()
        .push(original["coverage"][0]["facts"][0].clone());
    cases.push(duplicate);
    for case in cases {
        assert!(corporate_facts::import(temp.path(), &serde_json::to_vec(&case).unwrap()).is_err());
    }
    assert!(!temp.path().join("corporate-facts").exists());
    let file = temp.path().join("bad.json");
    fs::write(&file, b"not JSON").unwrap();
    cli(
        &[
            "import",
            "--root",
            temp.path().to_str().unwrap(),
            "--input",
            file.to_str().unwrap(),
        ],
        false,
    );
    for id in ["../bad", "caf:sha256:abc"] {
        assert!(corporate_facts::read(temp.path(), id).is_err());
    }
    assert!(corporate_facts::read(temp.path(), &format!("caf:sha256:{}", "a".repeat(64))).is_err());
    assert!(
        corporate_facts::import(
            &prajna_data::default_mvp6_root(),
            &serde_json::to_vec(&original).unwrap()
        )
        .is_err()
    );
}
#[cfg(unix)]
#[test]
fn publication_and_reads_reject_symlink_destinations() {
    let temp = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), temp.path().join("corporate-facts")).unwrap();
    assert!(
        corporate_facts::import(temp.path(), &serde_json::to_vec(&fixture()).unwrap()).is_err()
    );
    assert!(fs::read_dir(elsewhere.path()).unwrap().next().is_none());
}

/// Committed synthetic inputs for the production browser E2E. Only synthetic
/// identities and values; the E2E publishes them through the real import CLI.
fn page_scope(state: &str, instrument: &str, kind: &str) -> Value {
    let mut scope = json!({"instrument_id":instrument,"kind":kind,
        "start":"2026-01-01","end":"2026-01-31","state":state,
        "reason":"synthetic scoped source assertion",
        "source":{"source_id":"synthetic-facts","source_version":"1","source_ref":"fixture://corporate-actions"},
        "available_at":"2025-12-31T12:00:00+08:00","facts":[]});
    if state == "unknown" {
        scope["source"] = Value::Null;
        scope["available_at"] = Value::Null;
    }
    scope
}
fn page_fixture() -> Value {
    let mut covered = page_scope("covered", "510300.XSHG", "etf");
    covered["facts"] = json!([
        {"event_id":"dividend-1","kind":"dividend","record_date":"2026-01-05",
         "ex_date":"2026-01-06","pay_date":"2026-02-01","effective_date":null,
         "cash_per_share":"0.123456789012345678","share_ratio":null,
         "source":{"source_id":"synthetic-facts","source_version":"1","source_ref":"fixture://corporate-actions"},
         "available_at":"2025-12-31T12:00:00+08:00"},
        {"event_id":"split-1","kind":"share_conversion","record_date":null,
         "ex_date":null,"pay_date":null,"effective_date":"2026-01-20",
         "cash_per_share":null,"share_ratio":"1.5",
         "source":{"source_id":"synthetic-facts","source_version":"1","source_ref":"fixture://corporate-actions"},
         "available_at":"2025-12-31T12:00:00+08:00"}]);
    let mut missing = page_scope("missing_facts", "159915.XSHE", "etf");
    missing["facts"] = json!([
        {"event_id":"dividend-2","kind":"dividend","record_date":"2026-01-05",
         "ex_date":"2026-01-06","pay_date":"2026-01-30","effective_date":null,
         "cash_per_share":null,"share_ratio":null,
         "source":{"source_id":"synthetic-facts","source_version":"1","source_ref":"fixture://corporate-actions"},
         "available_at":null}]);
    json!({"schema_version":1,"observed_at":"2026-03-01T00:00:00Z","coverage":[
        covered,
        page_scope("no_events", "000001.XSHE", "stock"),
        missing,
        page_scope("unknown", "600000.XSHG", "stock")]})
}

#[test]
#[ignore = "explicit synthetic fixture export for apps/web/e2e"]
fn export_synthetic_corporate_facts_fixture() {
    let root = std::path::PathBuf::from(
        std::env::var_os("PRAJNA_CORPORATE_FACTS_FIXTURE_ROOT").expect("explicit fixture root"),
    );
    assert!(!root.exists(), "fixture root must not already exist");
    fs::create_dir_all(&root).unwrap();
    let first = page_fixture();
    fs::write(
        root.join("corporate-actions.json"),
        serde_json::to_vec(&first).unwrap(),
    )
    .unwrap();
    let mut revision = first.clone();
    revision["observed_at"] = json!("2026-03-02T00:00:00Z");
    revision["coverage"][0]["facts"][0]["cash_per_share"] = json!("0.2");
    revision["coverage"][0]["reason"] = json!("synthetic scoped source assertion, revised");
    fs::write(
        root.join("corporate-actions-revision.json"),
        serde_json::to_vec(&revision).unwrap(),
    )
    .unwrap();
}

#[test]
fn production_cli_checks_both_dates_against_payment() {
    for kind in ["liquidation", "share_conversion"] {
        for (ex, effective, pay, valid) in [
            (
                Some("2026-01-06"),
                Some("2026-01-20"),
                Some("2026-01-10"),
                false,
            ),
            (
                Some("2026-01-20"),
                Some("2026-01-06"),
                Some("2026-01-10"),
                false,
            ),
            (
                Some("2026-01-10"),
                Some("2026-01-10"),
                Some("2026-01-10"),
                true,
            ),
            (
                Some("2026-01-06"),
                Some("2026-01-07"),
                Some("2026-01-10"),
                true,
            ),
            (None, Some("2026-01-07"), Some("2026-01-10"), true),
            (Some("2026-01-06"), None, Some("2026-01-10"), false),
            (Some("2026-01-06"), Some("2026-01-07"), None, true),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let mut input = fixture();
            input["coverage"].as_array_mut().unwrap().truncate(1);
            let fact = &mut input["coverage"][0]["facts"][0];
            fact["kind"] = json!(kind);
            fact["ex_date"] = json!(ex);
            fact["effective_date"] = json!(effective);
            fact["pay_date"] = json!(pay);
            if kind == "share_conversion" {
                fact["cash_per_share"] = Value::Null;
                fact["share_ratio"] = json!("1.5");
            }
            let valid = valid && !(kind == "liquidation" && pay.is_none());
            let file = root.join("input.json");
            fs::write(&file, serde_json::to_vec(&input).unwrap()).unwrap();
            let result = cli(
                &[
                    "import",
                    "--root",
                    root.to_str().unwrap(),
                    "--input",
                    file.to_str().unwrap(),
                ],
                valid,
            );
            if valid {
                let read = query(root, result["version"].as_str().unwrap(), "510300.XSHG");
                assert_eq!(read["state"], "covered");
                assert_eq!(read["missing"][0]["fields"], json!([]));
            } else {
                assert!(result["error"]["code"].is_string());
                assert!(!root.join("corporate-facts").exists());
            }
        }
    }
}
