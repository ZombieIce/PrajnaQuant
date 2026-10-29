#[allow(dead_code)]
#[path = "support/selection_fixture.rs"]
mod fixture;
use prajna_experiment::momentum::{Command, Request, command};
use serde_json::{Value, json};
use std::path::Path;
fn dates() -> Vec<String> {
    use chrono::{Datelike, Duration, NaiveDate, Weekday};
    (0..210)
        .map(|i| NaiveDate::from_ymd_opt(2026, 1, 5).unwrap() + Duration::days(i))
        .filter(|d| !matches!(d.weekday(), Weekday::Sat | Weekday::Sun))
        .take(130)
        .map(|d| d.to_string())
        .collect()
}
fn inputs(root: &Path) -> Value {
    let days = dates();
    let refs = days.iter().map(String::as_str).collect::<Vec<_>>();
    let dsv = fixture::prepare_days(root, true, &refs);
    let version = fixture::import_adjusted(
        root,
        &dsv,
        "510300.XSHG",
        &days[0],
        days.last().unwrap(),
        &refs,
    );
    // Independently chosen linear prices: p_t = 10+t, not derived by Factor.
    let mut input =
        serde_json::to_value(prajna_data::adjusted_series::read(root, &version).unwrap()).unwrap();
    for (i, row) in input["rows"].as_array_mut().unwrap().iter_mut().enumerate() {
        for field in ["open", "high", "low", "close"] {
            row[field] = json!((10 + i).to_string());
        }
    }
    let version =
        prajna_data::adjusted_series::import(root, &serde_json::to_vec(&input).unwrap()).unwrap();
    json!({"dsv":dsv,"instrument":"510300.XSHG","adjusted":{"510300.XSHG":version},"windows":[20,60,120,1]})
}
fn submit(
    root: &Path,
    request: Value,
) -> Result<Value, prajna_experiment::momentum::ResearchError> {
    command(
        root,
        Command::Submit {
            request: serde_json::from_value::<Request>(request).unwrap(),
        },
    )
}
#[test]
fn production_cli_persists_independent_golden_and_reads_identical_values() {
    use std::{
        io::Write,
        process::{Command as Process, Stdio},
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let request = inputs(root);
    let mut child = Process::new(env!("CARGO_BIN_EXE_prajna-momentum"))
        .args(["--lake", root.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            json!({"op":"submit","request":request})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let data = &value["data"];
    let id = data["id"].as_str().unwrap();
    let read = command(root, Command::Read { id: id.into() }).unwrap();
    assert_eq!(value, read);
    let factors = data["report"]["factors"].as_array().unwrap();
    for (factor, n) in factors.iter().zip([1, 20, 60, 120]) {
        let rows = factor["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 130);
        for (i, row) in rows.iter().enumerate() {
            if i < n {
                assert_eq!(row["status"], "insufficient_window");
                assert!(row["value"].is_null());
            } else {
                assert_eq!(row["status"], "ok");
                let expected = (10 + i) as f64 / (10 + i - n) as f64 - 1.0;
                assert!((row["value"].as_f64().unwrap() - expected).abs() < 1e-12);
                assert!(
                    row["available_at_ns"]
                        .as_str()
                        .is_some_and(|s| s.parse::<i64>().is_ok())
                );
            }
        }
        assert_eq!(
            factor["distribution"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b["count"].as_u64().unwrap())
                .sum::<u64>(),
            (130 - n) as u64
        );
    }
    assert_eq!(
        data["report"]["sources"]["510300.XSHG"]["historical_publication_time"],
        "Unknown"
    );
    assert_eq!(submit(root, request.clone()).unwrap(), value); // cache reuse and immutable report
    std::fs::write(root.join(format!("momentum-research/{id}.json")), b"{}").unwrap();
    assert_eq!(
        command(root, Command::Read { id: id.into() })
            .unwrap_err()
            .code,
        "momentum_store_failed"
    );
    assert!(submit(root, request).is_err()); // corruption never silently overwritten
}
#[test]
fn future_changes_preserve_past_missing_and_binding_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut request = inputs(root);
    let original = submit(root, request.clone()).unwrap();
    let version = request["adjusted"]["510300.XSHG"].as_str().unwrap();
    let mut input =
        serde_json::to_value(prajna_data::adjusted_series::read(root, version).unwrap()).unwrap();
    for field in ["open", "high", "low", "close"] {
        input["rows"][129][field] = json!("999");
    }
    input["rows"].as_array_mut().unwrap().remove(100);
    let next =
        prajna_data::adjusted_series::import(root, &serde_json::to_vec(&input).unwrap()).unwrap();
    request["adjusted"]["510300.XSHG"] = json!(next);
    let changed = submit(root, request.clone()).unwrap();
    assert_ne!(changed["data"]["id"], original["data"]["id"]);
    for (a, b) in original["data"]["report"]["factors"]
        .as_array()
        .unwrap()
        .iter()
        .zip(changed["data"]["report"]["factors"].as_array().unwrap())
    {
        assert_eq!(
            &a["rows"].as_array().unwrap()[..100],
            &b["rows"].as_array().unwrap()[..100]
        );
        assert_eq!(
            b["rows"][100]["status"],
            if b["definition"]["params"]["n"].as_u64().unwrap() > 100 {
                "insufficient_window"
            } else {
                "missing_input"
            }
        );
        assert_eq!(b["rows"].as_array().unwrap().len(), 130);
    }
    request["windows"] = json!([0]);
    assert!(submit(root, request.clone()).is_err());
    request["windows"] = json!([]);
    assert!(submit(root, request.clone()).is_err());
    request["windows"] = json!([1]);
    request["adjusted"] = json!({});
    assert!(submit(root, request.clone()).is_err());
    request["adjusted"] = json!({"510300.XSHG":"adj:sha256:bad"});
    assert!(submit(root, request).is_err());
}
#[test]
#[ignore = "explicit synthetic browser fixture export"]
fn export_momentum_fixture() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::env::var_os("PRAJNA_MOMENTUM_FIXTURE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    let request = inputs(&root);
    std::fs::write(
        root.join("synthetic-momentum-request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
}

#[test]
fn published_pool_binding_cross_venue_and_rejected_inputs() {
    use prajna_experiment::pools::{self, Command as PoolCommand};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut request = inputs(root);
    let registry = pools::command(root, PoolCommand::List {}).unwrap();
    let expected = serde_json::from_value(registry["data"]["pools"][0]["draft"].clone()).unwrap();
    let members = vec!["159952.XSHE".to_owned(), "510300.XSHG".to_owned()];
    pools::command(
        root,
        PoolCommand::Save {
            id: "cross-asset".into(),
            members: members.clone(),
            expected,
        },
    )
    .unwrap();
    let registry = pools::command(
        root,
        PoolCommand::Publish {
            id: "cross-asset".into(),
            expected: members,
        },
    )
    .unwrap();
    let version = registry["data"]["pools"][0]["versions"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["version"]
        .as_u64()
        .unwrap();
    let days = dates();
    let refs = days.iter().map(String::as_str).collect::<Vec<_>>();
    let adjusted = fixture::import_adjusted(
        root,
        request["dsv"].as_str().unwrap(),
        "159952.XSHE",
        &days[0],
        days.last().unwrap(),
        &refs,
    );
    request.as_object_mut().unwrap().remove("instrument");
    request["pool"] = json!({"id":"cross-asset","version":version});
    request["adjusted"]["159952.XSHE"] = json!(adjusted);
    let result = submit(root, request.clone()).unwrap();
    assert_eq!(
        result["data"]["report"]["members"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        result["data"]["report"]["factors"][0]["rows"]
            .as_array()
            .unwrap()
            .len(),
        260
    );
    request["adjusted"]["510300.XSHG"] = request["adjusted"]["159952.XSHE"].clone();
    assert!(submit(root, request.clone()).is_err());
    request["instrument"] = json!("510300.XSHG");
    assert!(submit(root, request).is_err());
    let mut stock = inputs(root);
    stock["instrument"] = json!("600519.XSHG");
    assert!(submit(root, stock).is_err());
    let missing = root.join("not-a-lake");
    assert!(command(&missing, Command::List {}).is_err());
}

#[cfg(unix)]
#[test]
fn research_store_rejects_symbolic_links() {
    let dir = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let request = inputs(dir.path());
    std::os::unix::fs::symlink(external.path(), dir.path().join("momentum-research")).unwrap();
    assert!(submit(dir.path(), request).is_err());
    assert_eq!(std::fs::read_dir(external.path()).unwrap().count(), 0);
}

#[test]
fn mixed_sign_and_constant_histograms_count_every_valid_sample_once() {
    for prices in [[1, 5, 2], [2, 2, 2], [5, 1, 2]] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let dsv = fixture::prepare(root, true);
        let version = fixture::import_adjusted(
            root,
            &dsv,
            "510300.XSHG",
            "2026-01-05",
            "2026-01-07",
            &["2026-01-05", "2026-01-06", "2026-01-07"],
        );
        let mut input =
            serde_json::to_value(prajna_data::adjusted_series::read(root, &version).unwrap())
                .unwrap();
        for (row, price) in input["rows"].as_array_mut().unwrap().iter_mut().zip(prices) {
            for field in ["open", "high", "low", "close"] {
                row[field] = json!(price.to_string());
            }
        }
        let version =
            prajna_data::adjusted_series::import(root, &serde_json::to_vec(&input).unwrap())
                .unwrap();
        let result=submit(root,json!({"dsv":dsv,"instrument":"510300.XSHG","adjusted":{"510300.XSHG":version},"windows":[1]})).unwrap();
        let factor = &result["data"]["report"]["factors"][0];
        let bins = factor["distribution"].as_array().unwrap();
        let expected = [
            f64::from(prices[1]) / f64::from(prices[0]) - 1.0,
            f64::from(prices[2]) / f64::from(prices[1]) - 1.0,
        ];
        assert_eq!(
            bins.iter()
                .map(|b| b["count"].as_u64().unwrap())
                .sum::<u64>(),
            2
        );
        for (row, value) in factor["rows"].as_array().unwrap()[1..].iter().zip(expected) {
            assert!((row["value"].as_f64().unwrap() - value).abs() < 1e-12);
            assert_eq!(
                bins.iter()
                    .enumerate()
                    .filter(|(i, b)| value >= b["low"].as_f64().unwrap()
                        && if *i == 9 {
                            value <= b["high"].as_f64().unwrap()
                        } else {
                            value < b["high"].as_f64().unwrap()
                        })
                    .count(),
                1
            );
        }
        assert_eq!(
            bins[9]["high"].as_f64().unwrap(),
            expected[0].max(expected[1])
        );
    }
}
