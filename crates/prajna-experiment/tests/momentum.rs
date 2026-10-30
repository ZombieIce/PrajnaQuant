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
    let dsv = fixture::prepare_executable_days(root, true, &refs);
    let version = fixture::import_adjusted(
        root,
        &dsv,
        "510300.XSHG",
        &days[0],
        days.last().unwrap(),
        &refs,
    );
    // The same executable XSHG panel carries a second ETF for the comparison E2E.
    fixture::import_adjusted(
        root,
        &dsv,
        "510880.XSHG",
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
    // Missing action evidence must not gate this independent price-factor workflow.
    let facts = json!({"schema_version":1,"observed_at":"2026-08-01T00:00:00Z",
        "coverage":[{"instrument_id":"510300.XSHG","kind":"etf",
        "start":"2026-01-01","end":"2026-12-31","state":"missing_facts",
        "reason":"synthetic action source and event terms unavailable",
        "source":null,"available_at":null,"facts":[]}]});
    let fact_version =
        prajna_data::corporate_facts::import(root, &serde_json::to_vec(&facts).unwrap()).unwrap();
    let coverage = prajna_data::corporate_facts::query(
        root,
        &fact_version,
        "510300.XSHG",
        "2026-01-01".parse().unwrap(),
        "2026-12-31".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(coverage["state"], "missing_facts");
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
    // Legacy momentum requests keep their existing report semantics and direction.
    assert_eq!(data["report"]["semantics"], "momentum-research@2");
    assert_eq!(data["report"]["direction"], "higher_is_stronger");
    assert!(data["report"]["request"].get("factor").is_none());
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

/// Independent hand calculation over the linear adjusted closes `p_i = 10 + i`.
fn expected_return(prices: &[f64], i: usize) -> f64 {
    prices[i] / prices[i - 1] - 1.0
}

fn expected_sample_volatility(prices: &[f64], i: usize, n: usize) -> f64 {
    let returns = (i + 1 - n..=i)
        .map(|k| expected_return(prices, k))
        .collect::<Vec<_>>();
    let mean = returns.iter().sum::<f64>() / n as f64;
    let squares = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>();
    (squares / (n - 1) as f64).sqrt()
}

#[test]
fn volatility_research_reuses_v1_and_reports_lower_is_stronger() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut request = inputs(root);
    let legacy = submit(root, request.clone()).unwrap();
    assert_eq!(legacy["data"]["report"]["semantics"], "momentum-research@2");

    request["factor"] = json!("volatility");
    request["windows"] = json!([2, 20, 60]);
    let value = submit(root, request.clone()).unwrap();
    let report = &value["data"]["report"];
    assert_eq!(report["semantics"], "factor-research@1");
    assert_eq!(report["direction"], "lower_is_stronger");
    assert_eq!(report["request"]["factor"], "volatility");
    assert_eq!(report["request"]["windows"], json!([2, 20, 60]));
    let prices = (0..130).map(|i| f64::from(10 + i)).collect::<Vec<_>>();
    let factors = report["factors"].as_array().unwrap();
    assert_eq!(factors.len(), 3);
    for (factor, n) in factors.iter().zip([2_usize, 20, 60]) {
        assert_eq!(
            factor["definition"],
            json!({"kind": "volatility", "version": "1", "params": {"n": n}})
        );
        let rows = factor["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 130);
        for (i, row) in rows.iter().enumerate() {
            if i < n {
                assert_eq!(row["status"], "insufficient_window");
                assert!(row["value"].is_null());
            } else {
                assert_eq!(row["status"], "ok");
                let expected = expected_sample_volatility(&prices, i, n);
                assert!(
                    (row["value"].as_f64().unwrap() - expected).abs() < 1e-12,
                    "window {n} row {i}"
                );
                assert!(
                    row["available_at_ns"]
                        .as_str()
                        .is_some_and(|s| s.parse::<i64>().is_ok())
                );
            }
        }
    }

    // Volatility v1 requires n >= 2; the request must fail rather than fall back.
    request["windows"] = json!([1]);
    assert!(submit(root, request.clone()).is_err());

    let mut unknown = inputs(root);
    unknown["factor"] = json!("not_a_factor");
    assert!(serde_json::from_value::<Request>(unknown.clone()).is_err());
    // Explicit momentum is not a second encoding of the legacy identity.
    unknown["factor"] = json!("momentum");
    assert!(serde_json::from_value::<Request>(unknown).is_err());
}

#[test]
fn strict_trend_equality_warmup_missing_and_adjusted_binding_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut request = inputs(root);
    request["factor"] = json!("trend_filter_strict");
    request["windows"] = json!([1, 20, 60, 120]);
    let value = submit(root, request.clone()).unwrap();
    let report = &value["data"]["report"];
    assert_eq!(report["semantics"], "factor-research@1");
    assert_eq!(report["direction"], "binary_close_above_ma");
    assert_eq!(report["request"]["factor"], "trend_filter_strict");
    let factors = report["factors"].as_array().unwrap();
    assert_eq!(factors.len(), 4);
    for (factor, n) in factors.iter().zip([1_usize, 20, 60, 120]) {
        assert_eq!(
            factor["definition"],
            json!({"kind": "trend_filter_strict", "version": "1", "params": {"n": n}})
        );
        let rows = factor["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 130);
        for (i, row) in rows.iter().enumerate() {
            if i + 1 < n {
                assert_eq!(row["status"], "insufficient_window");
                assert!(row["value"].is_null());
            } else {
                assert_eq!(row["status"], "ok");
                let expected = if n == 1 {
                    // `close == mean(close)` for a one-session mean, and the Spec's
                    // strict definition must not count equality as above.
                    0.0
                } else {
                    // Linear increasing adjusted closes; equality never occurs here.
                    1.0
                };
                assert_eq!(
                    row["value"].as_f64().unwrap(),
                    expected,
                    "window {n} row {i}"
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
            if n == 1 { 130 } else { 130 - (n as u64) + 1 }
        );
    }

    // Revise the adjusted series: drop an interior Session and change the last
    // close. The report must keep past values and mark only affected windows.
    let version = request["adjusted"]["510300.XSHG"].as_str().unwrap();
    let mut input =
        serde_json::to_value(prajna_data::adjusted_series::read(root, version).unwrap()).unwrap();
    for field in ["open", "high", "low", "close"] {
        input["rows"][129][field] = json!("999");
    }
    input["rows"].as_array_mut().unwrap().remove(100);
    let revised =
        prajna_data::adjusted_series::import(root, &serde_json::to_vec(&input).unwrap()).unwrap();
    request["adjusted"]["510300.XSHG"] = json!(revised);
    let changed = submit(root, request).unwrap();
    assert_ne!(changed["data"]["id"], value["data"]["id"]);
    for (original, revised) in value["data"]["report"]["factors"]
        .as_array()
        .unwrap()
        .iter()
        .zip(changed["data"]["report"]["factors"].as_array().unwrap())
    {
        let n = original["definition"]["params"]["n"].as_u64().unwrap() as usize;
        let original_rows = original["rows"].as_array().unwrap();
        let revised_rows = revised["rows"].as_array().unwrap();
        assert_eq!(
            &original_rows[..100],
            &revised_rows[..100],
            "revision must not rewrite the first 100 Sessions"
        );
        for (index, row) in revised_rows[100..100 + n.min(20)].iter().enumerate() {
            // Insufficient history keeps precedence over a missing interior close.
            let expected = if 100 + index < n - 1 {
                "insufficient_window"
            } else {
                "missing_input"
            };
            assert_eq!(row["status"], expected, "window {n} row {}", 100 + index);
            assert!(row["value"].is_null());
        }
        if n == 20 {
            assert_eq!(revised_rows[120]["status"], "ok");
            assert_eq!(revised_rows[120]["value"].as_f64().unwrap(), 1.0);
        }
    }
}

/// Submits through the production `prajna-momentum` binary (JSON stdin/stdout).
fn run_production_cli(root: &Path, request: Value) -> Value {
    use std::{
        io::Write,
        process::{Command as Process, Stdio},
    };
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
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn strict_trend_non_representable_equal_means_are_zero_through_production_cli() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut request = inputs(root);
    let version = request["adjusted"]["510300.XSHG"].as_str().unwrap();
    let mut input =
        serde_json::to_value(prajna_data::adjusted_series::read(root, version).unwrap()).unwrap();
    // 3.826 is not exactly representable in binary64; every Session's decimal
    // close equals the decimal mean, so the strict kind must persist 0 at every
    // window (Issue #299 review B2).
    for row in input["rows"].as_array_mut().unwrap() {
        for field in ["open", "high", "low", "close"] {
            row[field] = json!("3.826");
        }
    }
    let constant =
        prajna_data::adjusted_series::import(root, &serde_json::to_vec(&input).unwrap()).unwrap();
    request["adjusted"]["510300.XSHG"] = json!(constant);
    request["factor"] = json!("trend_filter_strict");
    request["windows"] = json!([1, 2, 3, 20, 60, 120]);
    let value = run_production_cli(root, request);
    let report = &value["data"]["report"];
    assert_eq!(report["direction"], "binary_close_above_ma");
    let factors = report["factors"].as_array().unwrap();
    assert_eq!(factors.len(), 6);
    for (factor, n) in factors.iter().zip([1_usize, 2, 3, 20, 60, 120]) {
        let rows = factor["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 130);
        for (i, row) in rows.iter().enumerate() {
            if i + 1 < n {
                assert_eq!(row["status"], "insufficient_window", "window {n} row {i}");
                assert!(row["value"].is_null());
            } else {
                assert_eq!(row["status"], "ok", "window {n} row {i}");
                assert_eq!(
                    row["value"].as_f64().unwrap(),
                    0.0,
                    "window {n} row {i} must be equal, not above"
                );
            }
        }
    }
}

fn evaluation_cli(root: &Path, request: Value) -> Value {
    use std::{
        io::Write,
        process::{Command as Process, Stdio},
    };
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
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn evaluation_cli_fixed_labels_persist_and_future_opens_never_change_factors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut request = inputs(root);
    let legacy = submit(root, request.clone()).unwrap();
    assert!(legacy["data"]["report"].get("evaluation").is_none());
    request["evaluation"] = json!("next_open_to_forward_open_v1");
    let original = evaluation_cli(root, request.clone());
    let data = &original["data"];
    assert_eq!(data["report"]["semantics"], "factor-evaluation@1");
    assert_eq!(
        data["report"]["evaluation"]["version"],
        "next_open_to_forward_open@1"
    );
    let labels = data["report"]["evaluation"]["labels"].as_array().unwrap();
    assert_eq!(labels.len(), 390);
    for (offset, expected) in [
        (0, 12.0 / 11.0 - 1.0),
        (1, 16.0 / 11.0 - 1.0),
        (2, 31.0 / 11.0 - 1.0),
    ] {
        assert!((labels[offset]["value"].as_f64().unwrap() - expected).abs() < 1e-12);
    }
    assert!(labels[389]["value"].is_null());
    assert_eq!(
        command(
            root,
            Command::Read {
                id: data["id"].as_str().unwrap().into()
            }
        )
        .unwrap(),
        original
    );
    let persisted: Value = serde_json::from_slice(
        &std::fs::read(root.join(format!(
            "momentum-research/{}.json",
            data["id"].as_str().unwrap()
        )))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(persisted, *data);
    for factor in data["report"]["factors"].as_array().unwrap() {
        for relation in factor["relationships"].as_array().unwrap() {
            let n = relation["effective_count"].as_u64().unwrap();
            assert_eq!(relation["points"].as_array().unwrap().len() as u64, n);
            assert_eq!(n + relation["missing_count"].as_u64().unwrap(), 130);
            for point in relation["points"].as_array().unwrap() {
                assert!(point["factor_value"].is_number() && point["forward_return"].is_number());
            }
        }
    }
    // Change only future adjusted open/high; factor close inputs stay byte-identical.
    let version = request["adjusted"]["510300.XSHG"].as_str().unwrap();
    let mut input =
        serde_json::to_value(prajna_data::adjusted_series::read(root, version).unwrap()).unwrap();
    input["rows"][100]["open"] = json!("999");
    input["rows"][100]["high"] = json!("999");
    let changed_version =
        prajna_data::adjusted_series::import(root, &serde_json::to_vec(&input).unwrap()).unwrap();
    request["adjusted"]["510300.XSHG"] = json!(changed_version);
    let changed = evaluation_cli(root, request.clone());
    assert_ne!(changed["data"]["id"], data["id"]);
    for (a, b) in data["report"]["factors"]
        .as_array()
        .unwrap()
        .iter()
        .zip(changed["data"]["report"]["factors"].as_array().unwrap())
    {
        assert_eq!(a["rows"], b["rows"]);
        assert_eq!(a["distribution"], b["distribution"]);
        assert_eq!(a["status_counts"], b["status_counts"]);
    }
    let changed_labels = changed["data"]["report"]["evaluation"]["labels"]
        .as_array()
        .unwrap();
    assert_ne!(labels[98 * 3]["value"], changed_labels[98 * 3]["value"]);
    assert_eq!(labels[..79 * 3], changed_labels[..79 * 3]);
    // Missing adjusted endpoint: never use the later available day instead.
    input["rows"].as_array_mut().unwrap().remove(100);
    let missing_version =
        prajna_data::adjusted_series::import(root, &serde_json::to_vec(&input).unwrap()).unwrap();
    request["adjusted"]["510300.XSHG"] = json!(missing_version);
    let missing = evaluation_cli(root, request.clone());
    let missing_labels = &missing["data"]["report"]["evaluation"]["labels"];
    assert_eq!(missing_labels[98 * 3]["status"], "missing_exit_open");
    assert!(missing_labels[98 * 3]["value"].is_null());
    assert_eq!(missing_labels[99 * 3]["status"], "missing_entry_open");
    assert_eq!(
        command(
            root,
            Command::Read {
                id: data["id"].as_str().unwrap().into()
            }
        )
        .unwrap(),
        original
    );
    let mut unknown_version = request.clone();
    unknown_version["evaluation"] = json!("unknown");
    assert!(serde_json::from_value::<Request>(unknown_version).is_err());
    request["forward_return"] = json!(1.0);
    assert!(serde_json::from_value::<Request>(request).is_err());
}
