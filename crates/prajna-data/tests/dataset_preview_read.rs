//! Production read entry: synthetic stock/ETF publication followed by the
//! read-only Dataset preview. Everything here is synthetic and local.

use arrow_array::RecordBatch;
use chrono::NaiveDate;
use prajna_data::{
    EvidenceSource, FieldEvidence, Manifest, ManifestCore, ManifestProvenance, ManifestTable,
    NormalizerIdentity, SecurityEvidenceRecord, SecurityEvidenceStore, bars_to_record_batch,
    corporate_facts,
    daily_publish::DailyPublicationReceipt,
    daily_raw::{DailyRawCollector, DailyRawRequest, SecurityKind, Venue},
    instruments_to_record_batch, logical_hash, schema_fingerprint,
    session_calendar::CalendarStore,
    sessions_to_record_batch, write_manifest, write_parquet,
};
use prajna_domain::{InstrumentId, InstrumentKind, Price, Quantity};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tencent_transport::{HttpReply, HttpRequest, HttpTransport, TransportFailure};

fn date(value: &str) -> NaiveDate {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
}

struct Transport(Vec<u8>);

impl HttpTransport for Transport {
    fn get(&mut self, _: &HttpRequest) -> Result<HttpReply, TransportFailure> {
        Ok(HttpReply {
            source: "https://synthetic.invalid/daily".into(),
            bytes: self.0.clone(),
        })
    }
}

#[derive(Clone, Copy)]
enum Lifecycle {
    Known,
    Unknown,
}

fn source() -> EvidenceSource {
    EvidenceSource {
        source_id: "synthetic".into(),
        source_version: "1".into(),
        evidence_ref: "fixture".into(),
        observed_at: "2026-10-27T00:00:00Z".into(),
        effective_from: None,
        effective_to_exclusive: None,
    }
}

fn lifecycle_field(lifecycle: Lifecycle, value: &str) -> FieldEvidence<NaiveDate> {
    match lifecycle {
        Lifecycle::Known => FieldEvidence::Known {
            value: date(value),
            source: source(),
        },
        Lifecycle::Unknown => FieldEvidence::Unknown {
            reason: "synthetic source has no lifecycle date".into(),
            source: source(),
        },
    }
}

struct Publication<'a> {
    receipt: &'a str,
    symbol: &'a str,
    venue: &'a str,
    kind: InstrumentKind,
    close: &'a str,
    lifecycle: Lifecycle,
}

/// Publishes one security through the production CLI. Sessions 2026-01-05..09
/// exist in the pinned calendar; only 01-05 and 01-08 have a Bar.
fn publish(input: &Path, lake: &Path, publication: &Publication<'_>) -> String {
    let Publication {
        receipt: receipt_name,
        symbol,
        venue,
        kind,
        close,
        lifecycle,
    } = *publication;
    let calendar = json!({"schema_version": 1, "venues": (["XSHG", "XSHE"].map(|v| json!({
        "venue_id": v, "source": {"source_id":"synthetic", "source_version":"1", "source_ref":"fixture"},
        "coverage_start":"2026-01-05", "coverage_end":"2026-01-09",
        "sessions":(["2026-01-05", "2026-01-06", "2026-01-07", "2026-01-08", "2026-01-09"]
            .map(|d| json!({"session_date":d,
                "ts_open":format!("{d}T09:30:00+08:00"), "ts_close":format!("{d}T15:00:00+08:00")})))
    })))});
    let calendar_id = CalendarStore::open(input)
        .unwrap()
        .import_json(&serde_json::to_vec(&calendar).unwrap())
        .unwrap();
    let record = SecurityEvidenceRecord {
        instrument_id: format!("{symbol}.{venue}").parse::<InstrumentId>().unwrap(),
        kind,
        established_date: lifecycle_field(lifecycle, "2010-01-01"),
        listing_date: lifecycle_field(lifecycle, "2020-01-01"),
        lot_size: FieldEvidence::Known {
            value: Quantity::parse("100").unwrap(),
            source: source(),
        },
        price_increment: FieldEvidence::Known {
            value: Price::parse("0.001").unwrap(),
            source: source(),
        },
        size_increment: FieldEvidence::Known {
            value: Quantity::parse("1").unwrap(),
            source: source(),
        },
    };
    let evidence = SecurityEvidenceStore::open(input).unwrap();
    let version = evidence.import(&record).unwrap();
    let (snapshot, _) = evidence
        .select_snapshot([(record.instrument_id, version)])
        .unwrap();
    let native = format!("{}{symbol}", if venue == "XSHG" { "sh" } else { "sz" });
    let code = if kind == InstrumentKind::Equity {
        json!("0")
    } else {
        json!(0)
    };
    let value = close.parse::<f64>().unwrap();
    let row = |open: f64, high: f64, low: f64, lots: &str, day: &str| {
        json!([
            day,
            format!("{open:.3}"),
            close,
            format!("{high:.3}"),
            format!("{low:.3}"),
            lots
        ])
    };
    let body = serde_json::to_vec(&json!({"code":code, "data":{native:{"day":[
        row(value - 0.1, value + 0.2, value - 0.2, "1.25", "2026-01-05"),
        row(value - 0.1, value + 0.2, value - 0.2, "1.25", "2026-01-05"),
        row(value - 0.05, value + 0.15, value - 0.15, "2.00", "2026-01-08")
    ]}}}))
    .unwrap();
    let run = DailyRawCollector::new(Transport(body), input)
        .unwrap()
        .collect(DailyRawRequest {
            symbol: symbol.into(),
            venue: if venue == "XSHG" {
                Venue::Xshg
            } else {
                Venue::Xshe
            },
            kind: if kind == InstrumentKind::Equity {
                SecurityKind::Stock
            } else {
                SecurityKind::Etf
            },
            start: date("2026-01-05"),
            end: date("2026-01-09"),
        })
        .unwrap();
    let selection = json!({
        "raw_root":input, "calendar_root":input, "evidence_root":input,
        "report":run.report_path, "calendar_version":calendar_id.as_str(),
        "security_snapshot":snapshot.as_str(), "normalizer_id":"cn-stock-etf-daily",
        "normalizer_version":"1", "raw_sha256s":[run.report.chunks[0].raw_objects[0].raw_sha256]
    });
    fs::create_dir_all(lake).unwrap();
    let selection_path = lake.join(format!("{receipt_name}-selection.json"));
    fs::write(&selection_path, serde_json::to_vec(&selection).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_prajna-publish-daily"))
        .arg("publish")
        .arg("--root")
        .arg(lake)
        .arg("--receipt")
        .arg(receipt_name)
        .arg("--diagnostics")
        .arg(lake.join("diagnostics"))
        .arg("--selection")
        .arg(&selection_path)
        .output()
        .unwrap();
    if !output.status.success() {
        let diagnostics = fs::read_dir(lake.join("diagnostics"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| fs::read_to_string(entry.path()).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        panic!(
            "publish failed: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            diagnostics
        );
    }
    let receipt: DailyPublicationReceipt =
        serde_json::from_slice(&fs::read(lake.join(receipt_name)).unwrap()).unwrap();
    receipt.dsv
}

fn preview(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_prajna-preview-dataset"));
    command.arg(args[0]).arg("--root").arg(root);
    for pair in args[1..].chunks(2) {
        command.arg(pair[0]).arg(pair[1]);
    }
    command.output().unwrap()
}

fn preview_json(root: &Path, args: &[&str]) -> Value {
    let output = preview(root, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    value["data"].clone()
}

fn preview_error(root: &Path, args: &[&str]) -> (i32, Value) {
    preview_failure(root, args, 2)
}

fn preview_failure(root: &Path, args: &[&str], expected: i32) -> (i32, Value) {
    let output = preview(root, args);
    assert_eq!(output.status.code(), Some(expected), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {output:?}"));
    (output.status.code().unwrap(), value)
}

fn tree_hash(root: &Path) -> String {
    fn walk(root: &Path, directory: &Path, entries: &mut Vec<(String, u64, String)>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, entries);
            } else if path.is_file() {
                let relative = path.strip_prefix(root).unwrap().display().to_string();
                let bytes = fs::read(&path).unwrap();
                entries.push((
                    relative,
                    bytes.len() as u64,
                    format!("{:x}", Sha256::digest(&bytes)),
                ));
            }
        }
    }
    let mut entries = Vec::new();
    walk(root, root, &mut entries);
    entries.sort();
    let mut hasher = Sha256::new();
    for (path, length, digest) in entries {
        hasher.update(path.as_bytes());
        hasher.update(length.to_le_bytes());
        hasher.update(digest.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn dsv_hex(dsv: &str) -> &str {
    dsv.strip_prefix("dsv:sha256:").unwrap()
}

#[test]
fn synthetic_stock_and_etf_preview_exposes_coverage_gaps_and_unknown_fields() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let lake = temp.path().join("lake");
    let etf = publish(
        &input,
        &lake,
        &Publication {
            receipt: "etf.json",
            symbol: "510300",
            venue: "XSHG",
            kind: InstrumentKind::Etf,
            close: "11.123",
            lifecycle: Lifecycle::Known,
        },
    );
    let stock = publish(
        &input,
        &lake,
        &Publication {
            receipt: "stock.json",
            symbol: "000001",
            venue: "XSHE",
            kind: InstrumentKind::Equity,
            close: "9.875",
            lifecycle: Lifecycle::Known,
        },
    );

    let before = tree_hash(&lake);
    let catalog = preview_json(&lake, &["catalog"]);
    assert_eq!(catalog["datasets"].as_array().unwrap().len(), 2);
    assert!(catalog["invalid_manifests"].as_array().unwrap().is_empty());
    let venues = catalog["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|dataset| dataset["venues"].as_array().unwrap())
        .map(|venue| venue.as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        venues,
        ["XSHG", "XSHE"].into_iter().map(str::to_owned).collect()
    );
    for dataset in catalog["datasets"].as_array().unwrap() {
        assert_eq!(dataset["normalizer_id"], "cn-stock-etf-daily");
        assert_eq!(dataset["normalizer_version"], "1");
        assert_eq!(dataset["sessions"], 10);
        assert_eq!(dataset["bars"], 2);
        assert_eq!(dataset["instruments"], 1);
        assert_eq!(dataset["first_session"], "2026-01-05");
        assert_eq!(dataset["last_session"], "2026-01-09");
    }

    let show = preview_json(&lake, &["show", "--dsv", &etf]);
    assert_eq!(show["dsv"], etf);
    assert_eq!(show["venues"].as_array().unwrap().len(), 2);
    let instrument = &show["instruments"][0];
    assert_eq!(instrument["instrument_id"], "510300.XSHG");
    assert_eq!(instrument["venue"], "XSHG");
    assert_eq!(instrument["kind"], "etf");
    assert_eq!(instrument["native_symbol"], "510300");
    assert_eq!(instrument["lot_size"], "100");
    assert_eq!(instrument["price_increment"], "0.001");
    assert_eq!(instrument["size_increment"], "1");
    assert_eq!(
        instrument["lifecycle"]["established_date"]["state"],
        "known"
    );
    assert_eq!(
        instrument["lifecycle"]["established_date"]["value"],
        "2010-01-01"
    );
    assert_eq!(
        instrument["lifecycle"]["listing_date"]["value"],
        "2020-01-01"
    );
    assert_eq!(instrument["coverage"]["bars"], 2);
    assert_eq!(instrument["coverage"]["first_bar"], "2026-01-05");
    assert_eq!(instrument["coverage"]["last_bar"], "2026-01-08");
    assert_eq!(instrument["coverage"]["venue_sessions"], 5);
    assert_eq!(instrument["coverage"]["sessions_in_bar_range"], 4);
    assert_eq!(
        instrument["coverage"]["sessions_without_bar_in_observed_range"],
        2
    );
    assert_eq!(
        instrument["coverage"]["session_gaps"],
        json!(["2026-01-06", "2026-01-07"])
    );
    assert_eq!(instrument["coverage"]["session_gaps_truncated"], false);
    assert_eq!(instrument["coverage"]["sessions_before_first_bar"], 0);
    assert_eq!(instrument["coverage"]["sessions_after_last_bar"], 1);
    assert_eq!(
        instrument["coverage"]["sources"],
        json!(["tencent_fqkline_daily"])
    );
    assert_eq!(instrument["coverage"]["bars_without_source"], 0);
    assert_eq!(show["coverage"]["amount"], "absent_from_source_null");
    assert_eq!(show["coverage"]["available_at"], "unknown_null");
    assert!(
        show["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("not inferred as a halt"))
    );

    let first = preview_json(
        &lake,
        &[
            "bars",
            "--dsv",
            &etf,
            "--instrument",
            "510300.XSHG",
            "--limit",
            "1",
        ],
    );
    assert_eq!(first["total"], 2);
    assert_eq!(first["amount_unit"], "unknown; source has no amount field");
    assert_eq!(first["price_adjustment"], "none");
    assert_eq!(first["items"][0]["trade_date"], "2026-01-05");
    assert_eq!(first["items"][0]["open"], "11.023");
    assert_eq!(first["items"][0]["close"], "11.123");
    assert_eq!(first["items"][0]["volume"], "125");
    assert!(first["items"][0]["amount"].is_null());
    assert!(first["items"][0]["available_at"].is_null());
    assert_eq!(first["items"][0]["source"], "tencent_fqkline_daily");
    assert!(
        first["items"][0]["source_raw_sha256"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(first["next_offset"], 1);
    let second = preview_json(
        &lake,
        &[
            "bars",
            "--dsv",
            &etf,
            "--instrument",
            "510300.XSHG",
            "--limit",
            "1",
            "--offset",
            "1",
        ],
    );
    assert_eq!(second["items"][0]["trade_date"], "2026-01-08");
    assert!(second["next_offset"].is_null());

    let (_, error) = preview_error(&lake, &["show", "--dsv", &"f".repeat(64)]);
    assert_eq!(error["error"]["code"], "dataset_not_found");
    let (_, error) = preview_error(
        &lake,
        &["bars", "--dsv", &etf, "--instrument", "600000.XSHG"],
    );
    assert_eq!(error["error"]["code"], "dataset_not_found");
    let (_, error) = preview_error(
        &lake,
        &[
            "bars",
            "--dsv",
            &etf,
            "--instrument",
            "510300.XSHG",
            "--offset",
            "3",
        ],
    );
    assert_eq!(error["error"]["code"], "invalid_request");
    assert_eq!(
        before,
        tree_hash(&lake),
        "preview must not write to the lake"
    );

    // Each required table is read before a Dataset is offered. A missing table
    // is isolated in invalid_manifests and cannot keep a normal catalog entry.
    for table in ["instruments", "sessions", "bars"] {
        let source = lake.join("normalized").join(table).join(dsv_hex(&stock));
        let backup = lake.join("normalized").join(format!("{table}-missing"));
        fs::rename(&source, &backup).unwrap();

        let catalog = preview_json(&lake, &["catalog"]);
        assert_eq!(catalog["datasets"].as_array().unwrap().len(), 1, "{table}");
        assert_eq!(
            catalog["invalid_manifests"].as_array().unwrap().len(),
            1,
            "{table}"
        );
        assert_eq!(
            catalog["invalid_manifests"][0]["manifest"],
            format!("{}.json", dsv_hex(&stock)),
            "{table}"
        );
        assert_eq!(catalog["datasets"][0]["dsv"], etf, "{table}");
        let (_, error) = preview_failure(&lake, &["show", "--dsv", &stock], 1);
        assert_eq!(error["error"]["code"], "invalid_dataset_table", "{table}");

        fs::rename(&backup, &source).unwrap();
    }
    let catalog = preview_json(&lake, &["catalog"]);
    assert_eq!(catalog["datasets"].as_array().unwrap().len(), 2);
    assert!(catalog["invalid_manifests"].as_array().unwrap().is_empty());
}

#[test]
fn unknown_lifecycle_and_invalid_manifests_are_not_shown_as_covered() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let lake = temp.path().join("lake");
    let dsv = publish(
        &input,
        &lake,
        &Publication {
            receipt: "etf.json",
            symbol: "159952",
            venue: "XSHE",
            kind: InstrumentKind::Etf,
            close: "3.210",
            lifecycle: Lifecycle::Unknown,
        },
    );
    fs::write(
        lake.join("manifests")
            .join(format!("{}.json", "a".repeat(64))),
        b"{}",
    )
    .unwrap();
    let catalog = preview_json(&lake, &["catalog"]);
    assert_eq!(catalog["datasets"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["invalid_manifests"].as_array().unwrap().len(), 1);
    assert_eq!(
        catalog["invalid_manifests"][0]["manifest"],
        format!("{}.json", "a".repeat(64))
    );
    let show = preview_json(&lake, &["show", "--dsv", &dsv]);
    let lifecycle = &show["instruments"][0]["lifecycle"];
    assert_eq!(lifecycle["established_date"]["state"], "unknown");
    assert!(lifecycle["established_date"]["value"].is_null());
    assert!(
        lifecycle["established_date"]["reason"]
            .as_str()
            .unwrap()
            .contains("no lifecycle date")
    );
    assert_eq!(lifecycle["listing_date"]["state"], "unknown");
    assert!(lifecycle["listing_date"]["value"].is_null());
    assert!(
        show["instruments"][0]["coverage"]["first_bar"]
            .as_str()
            .is_some()
    );
    assert!(
        show["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("not inferred as a halt"))
    );
}

#[test]
fn a_revision_adds_a_version_and_keeps_the_old_one_readable() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let lake = temp.path().join("lake");
    let first_dsv = publish(
        &input,
        &lake,
        &Publication {
            receipt: "first.json",
            symbol: "510300",
            venue: "XSHG",
            kind: InstrumentKind::Etf,
            close: "11.123",
            lifecycle: Lifecycle::Known,
        },
    );
    let second_dsv = publish(
        &input,
        &lake,
        &Publication {
            receipt: "second.json",
            symbol: "510300",
            venue: "XSHG",
            kind: InstrumentKind::Etf,
            close: "12.456",
            lifecycle: Lifecycle::Known,
        },
    );
    assert_ne!(first_dsv, second_dsv);
    let catalog = preview_json(&lake, &["catalog"]);
    assert_eq!(catalog["datasets"].as_array().unwrap().len(), 2);
    let first = preview_json(
        &lake,
        &["bars", "--dsv", &first_dsv, "--instrument", "510300.XSHG"],
    );
    let second = preview_json(
        &lake,
        &["bars", "--dsv", &second_dsv, "--instrument", "510300.XSHG"],
    );
    assert_eq!(first["items"][0]["close"], "11.123");
    assert_eq!(second["items"][0]["close"], "12.456");
    assert_eq!(first["dsv"], first_dsv);
    assert_eq!(second["dsv"], second_dsv);
}

fn write_empty_dataset(root: &Path, bars_row_count: u64) -> String {
    let instruments = instruments_to_record_batch(&[]).unwrap();
    let sessions = sessions_to_record_batch(&[]).unwrap();
    let bars = bars_to_record_batch(&[]).unwrap();
    let table = |name: &str, batch: &RecordBatch, keys: &[&str], row_count: u64| ManifestTable {
        table: name.into(),
        schema_version: 1,
        schema_fingerprint: schema_fingerprint(batch.schema().as_ref()).unwrap(),
        logical_hash: logical_hash(
            name,
            batch.schema().as_ref(),
            std::slice::from_ref(batch),
            keys,
        )
        .unwrap(),
        row_count,
    };
    let manifest = Manifest::new(
        ManifestCore {
            manifest_version: 1,
            normalizer: NormalizerIdentity {
                id: "synthetic-empty".into(),
                version: "1".into(),
                config_sha256: format!("sha256:{}", "0".repeat(64)),
            },
            inputs: Vec::new(),
            tables: vec![
                table("instruments", &instruments, &["instrument_id"], 0),
                table("sessions", &sessions, &["venue_id", "session_date"], 0),
                table(
                    "bars",
                    &bars,
                    &["instrument_id", "session_date"],
                    bars_row_count,
                ),
            ],
        },
        ManifestProvenance {
            files: Vec::new(),
            writer: json!({}),
            created_at: "2026-10-27T00:00:00Z".into(),
            git_rev: None,
            git_dirty: None,
            host: None,
            synthetic_assumptions: vec!["empty synthetic Dataset".into()],
            coverage: json!({}),
            ignored_input_fields: Vec::new(),
        },
    )
    .unwrap();
    let hex = dsv_hex(&manifest.dsv);
    for (name, batch) in [
        ("instruments", &instruments),
        ("sessions", &sessions),
        ("bars", &bars),
    ] {
        let directory = root.join("normalized").join(name).join(hex);
        fs::create_dir_all(&directory).unwrap();
        write_parquet(directory.join("part-00000.parquet"), batch, &manifest.dsv).unwrap();
    }
    write_manifest(root, &manifest).unwrap();
    manifest.dsv
}

#[test]
fn empty_committed_dataset_is_readable_with_explicit_empty_coverage() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("lake");
    let dsv = write_empty_dataset(&root, 0);

    let catalog = preview_json(&root, &["catalog"]);
    assert_eq!(catalog["datasets"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["datasets"][0]["instruments"], 0);
    assert_eq!(catalog["datasets"][0]["sessions"], 0);
    assert_eq!(catalog["datasets"][0]["bars"], 0);
    assert!(catalog["datasets"][0]["first_session"].is_null());
    assert!(catalog["invalid_manifests"].as_array().unwrap().is_empty());

    let show = preview_json(&root, &["show", "--dsv", &dsv]);
    assert!(show["instruments"].as_array().unwrap().is_empty());
    assert!(show["venues"].as_array().unwrap().is_empty());
    assert_eq!(show["coverage"], json!({}));
    let (_, error) = preview_error(
        &root,
        &["bars", "--dsv", &dsv, "--instrument", "510300.XSHG"],
    );
    assert_eq!(error["error"]["code"], "dataset_not_found");
}

#[test]
fn committed_row_count_mismatch_is_not_listed_as_usable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("lake");
    let dsv = write_empty_dataset(&root, 1);

    let catalog = preview_json(&root, &["catalog"]);
    assert!(catalog["datasets"].as_array().unwrap().is_empty());
    assert_eq!(catalog["invalid_manifests"].as_array().unwrap().len(), 1);
    assert_eq!(
        catalog["invalid_manifests"][0]["manifest"],
        format!("{}.json", dsv_hex(&dsv))
    );
    let (_, error) = preview_failure(&root, &["show", "--dsv", &dsv], 1);
    assert_eq!(error["error"]["code"], "invalid_dataset_table");
}

/// Explicit opt-in exporter for production HTTP/browser checks; synthetic only.
#[test]
#[ignore = "writes synthetic fixture to an explicitly selected empty local root"]
fn export_synthetic_preview_fixture() {
    let root = std::path::PathBuf::from(
        std::env::var_os("PRAJNA_PREVIEW_FIXTURE_ROOT").expect("explicit fixture root"),
    );
    assert!(!root.exists(), "fixture root must not already exist");
    let input = root.join("input");
    let lake = root.join("lake");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("synthetic-preview-fixture"), "synthetic only\n").unwrap();
    for (receipt, symbol, venue, kind, close, lifecycle) in [
        (
            "etf.json",
            "510300",
            "XSHG",
            InstrumentKind::Etf,
            "11.123",
            Lifecycle::Known,
        ),
        (
            "stock.json",
            "000001",
            "XSHE",
            InstrumentKind::Equity,
            "9.875",
            Lifecycle::Unknown,
        ),
    ] {
        let dsv = publish(
            &input,
            &lake,
            &Publication {
                receipt,
                symbol,
                venue,
                kind,
                close,
                lifecycle,
            },
        );
        if kind == InstrumentKind::Etf {
            // Dedicated adjusted fixture: literals are supplier inputs, no cash-flow inference.
            let mut base = adjusted_input(&dsv, "510300.XSHG");
            base["rows"] = json!([
                {"trade_date":"2026-01-05", "open":"5.5115", "high":"5.6615", "low":"5.4615", "close":"5.5615"},
                {"trade_date":"2026-01-08", "open":"5.5365", "high":"5.6365", "low":"5.4865", "close":"5.5615"}]);
            adjusted_import(&lake, &root.join("adjusted-input.json"), &base).unwrap();
            let mut revision = base.clone();
            revision["method_version"] = json!("synthetic-revision@2");
            revision["rows"][0]["close"] = json!("5.6");
            adjusted_import(&lake, &root.join("adjusted-input.json"), &revision).unwrap();
            let mut empty = base;
            empty["rows"] = json!([]);
            empty["method_version"] = json!("synthetic-empty@1");
            adjusted_import(&lake, &root.join("adjusted-input.json"), &empty).unwrap();
        }
    }
    assert_eq!(
        preview_json(&lake, &["catalog"])["datasets"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

// Independent synthetic oracle: supplier applies 1/2 to every raw OHLC.
// Raw fixture: 01-05 (9.9,10.2,9.8,10); 01-08 (9.95,10.15,9.85,10).
// Expected adjusted values are literal hand calculations, not derived by the code under test.
fn adjusted_input(dsv: &str, instrument: &str) -> Value {
    json!({"schema_version":1, "raw_dsv":dsv, "instrument_id":instrument,
        "method":"forward", "method_version":"synthetic-half@1",
        "source":{"source_id":"synthetic-supplier", "source_version":"1", "source_ref":"synthetic://half-price"},
        "observed_at":"2026-10-28T00:00:00Z",
        "retrospective_assumption":"Supplier history may be revised after later events; not strict PIT or sequential TRI",
        "close_availability_assumption":"Research assumes daily close known after domestic Session close",
        "start":"2026-01-05", "end":"2026-01-09", "rows":[
            {"trade_date":"2026-01-05", "open":"4.95", "high":"5.1", "low":"4.9", "close":"5"},
            {"trade_date":"2026-01-08", "open":"4.975", "high":"5.075", "low":"4.925", "close":"5"}]})
}

fn adjusted_import(lake: &Path, file: &Path, input: &Value) -> std::result::Result<String, Output> {
    fs::write(file, serde_json::to_vec(input).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_prajna-import-adjusted"))
        .arg("--root")
        .arg(lake)
        .arg("--input")
        .arg(file)
        .output()
        .unwrap();
    if !output.status.success() {
        return Err(output);
    }
    Ok(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["version"]
            .as_str()
            .unwrap()
            .into(),
    )
}

fn adjusted_fixture(root: &Path) -> (String, Value) {
    let dsv = publish(
        &root.join("input"),
        &root.join("lake"),
        &Publication {
            receipt: "adjusted-base.json",
            symbol: "510300",
            venue: "XSHG",
            kind: InstrumentKind::Etf,
            close: "10",
            lifecycle: Lifecycle::Known,
        },
    );
    let input = adjusted_input(&dsv, "510300.XSHG");
    (dsv, input)
}

#[test]
fn adjusted_cli_import_fixed_read_revision_and_independent_price_golden() {
    use prajna_data::{adjusted_series, dataset_preview::BarsRequest};
    let temp = tempfile::tempdir().unwrap();
    let (dsv, input) = adjusted_fixture(temp.path());
    let lake = temp.path().join("lake");
    let file = temp.path().join("adjusted.json");
    let raw_args = ["bars", "--dsv", &dsv, "--instrument", "510300.XSHG"];
    let raw_before = preview_json(&lake, &raw_args);
    let manifest_before = fs::read(lake.join(format!("manifests/{}.json", dsv_hex(&dsv)))).unwrap();
    let id = adjusted_import(&lake, &file, &input).unwrap();
    let object = lake.join(format!(
        "adjusted-series/{}.json",
        id.strip_prefix("adj:sha256:").unwrap()
    ));
    assert_eq!(
        fs::read(object).unwrap(),
        fs::read(&file).unwrap(),
        "exact imported source file is preserved"
    );
    let before = tree_hash(&lake);
    assert_eq!(adjusted_import(&lake, &file, &input).unwrap(), id);
    assert_eq!(
        tree_hash(&lake),
        before,
        "idempotent import cannot rewrite input"
    );
    let first = preview_json(
        &lake,
        &[
            "bars",
            "--dsv",
            &dsv,
            "--instrument",
            "510300.XSHG",
            "--adjusted-version",
            &id,
        ],
    );
    for (index, expected) in [
        ["4.95", "5.1", "4.9", "5"],
        ["4.975", "5.075", "4.925", "5"],
    ]
    .iter()
    .enumerate()
    {
        for (key, value) in ["open", "high", "low", "close"].iter().zip(expected) {
            assert_eq!(first["items"][index][key], *value);
        }
    }
    assert_eq!(
        first["adjusted"]["coverage"]["session_gaps"],
        json!(["2026-01-06", "2026-01-07", "2026-01-09"])
    );
    assert_eq!(first["adjusted"]["coverage"]["missing_sessions"], 3);
    assert_eq!(
        first["adjusted"]["coverage"]["missing_sessions_with_raw_bar"],
        0
    );
    assert_eq!(
        first["adjusted"]["coverage"]["missing_sessions_without_raw_bar"],
        3
    );
    let bare_dsv = dsv_hex(&dsv);
    let bare = preview_json(
        &lake,
        &[
            "bars",
            "--dsv",
            bare_dsv,
            "--instrument",
            "510300.XSHG",
            "--adjusted-version",
            &id,
        ],
    );
    assert_eq!(bare["items"][0]["close"], "5");
    assert_eq!(
        adjusted_series::catalog(&lake, bare_dsv).unwrap()["versions"][0]["version"],
        json!(id)
    );
    assert_eq!(first["items"][0]["available_at"], Value::Null);
    assert_eq!(first["items"][0]["volume"], Value::Null);
    let pinned = adjusted_series::read(&lake, &id).unwrap();
    assert_eq!(pinned.raw_dsv, dsv);
    assert_eq!(pinned.rows[0].close, "5");
    assert_eq!(
        tree_hash(&lake),
        before,
        "fixed reads do not write the lake"
    );
    let mut revised = input.clone();
    revised["method_version"] = json!("synthetic-revision@2");
    revised["rows"][0]["close"] = json!("5.05");
    let next = adjusted_import(&lake, &file, &revised).unwrap();
    assert_ne!(next, id);
    assert_eq!(
        adjusted_series::read(&lake, &id).unwrap().rows[0].close,
        "5"
    );
    assert_eq!(
        adjusted_series::read(&lake, &next).unwrap().rows[0].close,
        "5.05"
    );
    assert_eq!(
        preview_json(&lake, &raw_args),
        raw_before,
        "execution/valuation Bars unchanged"
    );
    assert_eq!(
        fs::read(lake.join(format!("manifests/{}.json", dsv_hex(&dsv)))).unwrap(),
        manifest_before
    );
    let tables = prajna_data::read_manifest(&lake, &dsv).unwrap().core.tables;
    assert!(
        !tables.iter().any(|t| t.table == "corporate_actions"),
        "price import cannot fabricate dividend/cash facts"
    );
    let request = BarsRequest {
        dsv: dsv.clone(),
        instrument_id: "510300.XSHG".into(),
        start: None,
        end: None,
        limit: 1,
        offset: 0,
    };
    let page = adjusted_series::bars(&lake, &request, &id).unwrap();
    assert_eq!(page["next_offset"], 1);
    let mut bare_request = request.clone();
    bare_request.dsv = dsv_hex(&dsv).into();
    assert_eq!(
        adjusted_series::bars(&lake, &bare_request, &id).unwrap()["total"],
        2
    );
    let mut second = request.clone();
    second.offset = 1;
    assert_eq!(
        adjusted_series::bars(&lake, &second, &id).unwrap()["items"][0]["trade_date"],
        "2026-01-08"
    );
    second.offset = 3;
    assert!(adjusted_series::bars(&lake, &second, &id).is_err());
    second.offset = 0;
    second.start = Some(date("2026-01-06"));
    second.end = Some(date("2026-01-07"));
    assert_eq!(
        adjusted_series::bars(&lake, &second, &id).unwrap()["total"],
        0
    );
}

#[test]
fn adjusted_input_rejects_invalid_identity_source_method_dates_and_prices() {
    let temp = tempfile::tempdir().unwrap();
    let (_, input) = adjusted_fixture(temp.path());
    let lake = temp.path().join("lake");
    let file = temp.path().join("adjusted.json");
    for (pointer, bad) in [
        ("/schema_version", json!(2)),
        ("/method", json!("none")),
        ("/method_version", json!("")),
        ("/source/source_id", json!(" ")),
        ("/source/source_version", json!("")),
        ("/source/source_ref", json!("")),
        ("/observed_at", json!("not-a-time")),
        ("/retrospective_assumption", json!("")),
        ("/close_availability_assumption", json!("")),
        ("/raw_dsv", json!("dsv:sha256:bad")),
        ("/instrument_id", json!("000001.XSHE")),
        ("/end", json!("2026-01-01")),
        ("/start", json!("2026-01-01")),
        ("/rows/0/trade_date", json!("2026-01-06")),
        ("/rows/0/trade_date", json!("2026-01-08")),
        ("/rows/0/trade_date", json!("2026-01-10")),
        ("/rows/0/open", json!("0")),
        ("/rows/0/open", json!("-1")),
        ("/rows/0/open", json!("NaN")),
        ("/rows/0/open", json!("6")),
        ("/rows/0/low", json!("5.2")),
        ("/rows/0/close", json!("1.0000000000000000001")),
        ("/rows/0/close", json!(5)),
    ] {
        let mut bad_input = input.clone();
        *bad_input.pointer_mut(pointer).unwrap() = bad;
        assert!(
            adjusted_import(&lake, &file, &bad_input).is_err(),
            "{pointer}"
        );
    }
    for bad_input in [
        {
            let mut input = input.clone();
            input["unexpected"] = json!(true);
            input
        },
        {
            let mut input = input.clone();
            input["source"]["unexpected"] = json!(true);
            input
        },
        {
            let mut input = input.clone();
            input["rows"][0]["unexpected"] = json!(true);
            input
        },
    ] {
        assert!(
            adjusted_import(&lake, &file, &bad_input).is_err(),
            "the closed schema rejects unknown fields"
        );
    }
    let mut missing_raw_bar = input.clone();
    missing_raw_bar["rows"][0]["trade_date"] = json!("2026-01-06");
    let output = adjusted_import(&lake, &file, &missing_raw_bar).unwrap_err();
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["code"], "invalid_request");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no raw Bar")
    );
    assert!(
        !lake.join("adjusted-series").exists(),
        "invalid imports leave no published version"
    );
}

#[test]
fn adjusted_empty_partial_updates_identity_isolation_and_corruption_are_explicit() {
    use prajna_data::adjusted_series;
    let temp = tempfile::tempdir().unwrap();
    let (dsv, input) = adjusted_fixture(temp.path());
    let lake = temp.path().join("lake");
    let file = temp.path().join("adjusted.json");
    let mut empty = input.clone();
    empty["rows"] = json!([]);
    let empty_id = adjusted_import(&lake, &file, &empty).unwrap();
    let detail = preview_json(&lake, &["show", "--dsv", &dsv]);
    assert_eq!(
        detail["adjusted_series"]["versions"][0]["coverage"]["rows"],
        0
    );
    assert_eq!(
        detail["adjusted_series"]["versions"][0]["coverage"]["missing_sessions"],
        5
    );
    assert_eq!(
        detail["adjusted_series"]["versions"][0]["coverage"]["missing_sessions_with_raw_bar"],
        2
    );
    assert_eq!(
        detail["adjusted_series"]["versions"][0]["coverage"]["missing_sessions_without_raw_bar"],
        3
    );
    let mut partial = input.clone();
    partial["rows"].as_array_mut().unwrap().pop();
    let partial_id = adjusted_import(&lake, &file, &partial).unwrap();
    assert_ne!(partial_id, empty_id);
    let full_id = adjusted_import(&lake, &file, &input).unwrap();
    let mut backward = input.clone();
    backward["method"] = json!("backward");
    backward["source"]["source_version"] = json!("2");
    let backward_id = adjusted_import(&lake, &file, &backward).unwrap();
    assert_ne!(backward_id, full_id);
    assert_eq!(
        adjusted_series::read(&lake, &backward_id).unwrap().method,
        "backward"
    );
    assert_eq!(
        adjusted_series::read(&lake, &empty_id).unwrap().rows.len(),
        0
    );
    assert_eq!(
        adjusted_series::read(&lake, &partial_id)
            .unwrap()
            .rows
            .len(),
        1
    );
    let foreign = publish(
        &temp.path().join("input"),
        &lake,
        &Publication {
            receipt: "revised-raw.json",
            symbol: "510300",
            venue: "XSHG",
            kind: InstrumentKind::Etf,
            close: "11",
            lifecycle: Lifecycle::Known,
        },
    );
    let (_, error) = preview_failure(
        &lake,
        &[
            "bars",
            "--dsv",
            &foreign,
            "--instrument",
            "510300.XSHG",
            "--adjusted-version",
            &full_id,
        ],
        2,
    );
    assert_eq!(error["error"]["code"], "invalid_request");
    assert_eq!(
        adjusted_series::catalog(&lake, &foreign).unwrap()["versions"],
        json!([])
    );
    assert!(adjusted_series::read(&lake, "../invalid").is_err());
    assert!(adjusted_series::read(&lake, &format!("adj:sha256:{}", "0".repeat(64))).is_err());
    let object = lake.join(format!(
        "adjusted-series/{}.json",
        full_id.strip_prefix("adj:sha256:").unwrap()
    ));
    fs::write(&object, b"{}").unwrap();
    let (_, error) = preview_failure(
        &lake,
        &[
            "bars",
            "--dsv",
            &dsv,
            "--instrument",
            "510300.XSHG",
            "--adjusted-version",
            &full_id,
        ],
        1,
    );
    assert_eq!(error["error"]["code"], "invalid_dataset_table");
    assert!(
        adjusted_import(&lake, &file, &input).is_err(),
        "cannot overwrite corrupt same-id object"
    );
    let catalog = adjusted_series::catalog(&lake, &dsv).unwrap();
    assert_eq!(catalog["versions"].as_array().unwrap().len(), 3);
    assert_eq!(catalog["invalid_versions"].as_array().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn adjusted_import_and_read_reject_symlink_destination() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let (dsv, input) = adjusted_fixture(temp.path());
    let lake = temp.path().join("lake");
    let other = temp.path().join("other");
    fs::create_dir(&other).unwrap();
    symlink(&other, lake.join("adjusted-series")).unwrap();
    assert!(adjusted_import(&lake, &temp.path().join("adjusted.json"), &input).is_err());
    assert!(
        prajna_data::adjusted_series::catalog(&lake, input["raw_dsv"].as_str().unwrap()).is_err()
    );
    let detail = preview_json(&lake, &["show", "--dsv", &dsv]);
    assert_eq!(detail["adjusted_series"]["versions"], json!([]));
    assert!(detail["adjusted_series"]["error"].is_string());
    assert!(
        detail["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap()
                .contains("adjusted series catalog unavailable"))
    );
    assert_eq!(fs::read_dir(other).unwrap().count(), 0);
}

fn facts_scope(state: &str, instrument: &str, kind: &str) -> Value {
    let source =
        json!({"source_id":"synthetic","source_version":"1","source_ref":"fixture://actions"});
    let mut scope = json!({"instrument_id":instrument,"kind":kind,"start":"2026-01-01",
        "end":"2026-01-31","state":state,"reason":"supplied events","source":source,
        "available_at":"2025-12-31T12:00:00+08:00","facts":[]});
    if state == "unknown" {
        scope["source"] = Value::Null;
        scope["available_at"] = Value::Null;
    }
    scope
}

/// Synthetic facts only: the preview CLI lists persisted versions and reads one
/// explicit version/security/range without publishing, choosing a latest or
/// merging partial coverage.
#[test]
fn actions_catalog_and_fixed_read_expose_four_states_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("lake");
    fs::create_dir_all(&root).unwrap();
    let source =
        json!({"source_id":"synthetic","source_version":"1","source_ref":"fixture://actions"});
    let mut covered = facts_scope("covered", "510300.XSHG", "etf");
    covered["facts"] = json!([{"event_id":"dividend-1","kind":"dividend",
        "record_date":"2026-01-05","ex_date":"2026-01-06","pay_date":"2026-02-01",
        "effective_date":null,"cash_per_share":"0.123456789012345678","share_ratio":null,
        "source":source,"available_at":"2025-12-31T12:00:00+08:00"}]);
    let mut missing = facts_scope("missing_facts", "159915.XSHE", "etf");
    missing["facts"] = json!([{"event_id":"dividend-2","kind":"dividend",
        "record_date":"2026-01-05","ex_date":"2026-01-06","pay_date":"2026-01-30",
        "effective_date":null,"cash_per_share":null,"share_ratio":null,
        "source":source,"available_at":null}]);
    let input = json!({"schema_version":1,"observed_at":"2026-03-01T00:00:00Z","coverage":[
        covered, facts_scope("no_events","000001.XSHE","stock"), missing,
        facts_scope("unknown","600000.XSHG","stock")]});
    let first = corporate_facts::import(&root, &serde_json::to_vec(&input).unwrap()).unwrap();

    let empty = temp.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let catalog = preview_json(&empty, &["actions-catalog"]);
    assert_eq!(catalog["versions"], json!([]));
    assert_eq!(catalog["invalid_versions"], json!([]));
    let (code, error) = preview_failure(&root.join("missing"), &["actions-catalog"], 1);
    assert_eq!(code, 1);
    assert_eq!(error["error"]["code"], "invalid_dataset_root");

    let catalog = preview_json(&root, &["actions-catalog"]);
    assert_eq!(catalog["versions"].as_array().unwrap().len(), 1);
    let scopes = catalog["versions"][0]["scopes"].as_array().unwrap();
    assert_eq!(scopes.len(), 4);
    assert_eq!(scopes[0]["state"], "covered");
    assert_eq!(scopes[0]["facts"], 1);
    assert_eq!(scopes[1]["state"], "no_events");
    assert_eq!(scopes[2]["state"], "missing_facts");
    assert_eq!(scopes[2]["incomplete_facts"], 1);
    assert_eq!(scopes[3]["state"], "unknown");

    let read = |id: &str, security: &str, start: &str, end: &str| {
        preview_json(
            &root,
            &[
                "actions",
                "--version",
                id,
                "--instrument",
                security,
                "--start",
                start,
                "--end",
                end,
            ],
        )
    };
    for (security, expected) in [
        ("510300.XSHG", "covered"),
        ("000001.XSHE", "no_events"),
        ("159915.XSHE", "missing_facts"),
        ("600000.XSHG", "unknown"),
        ("510300.XSHE", "unknown"),
    ] {
        let value = read(&first, security, "2026-01-01", "2026-01-31");
        assert_eq!(
            (&value["state"], &value["version"], &value["instrument_id"]),
            (&json!(expected), &json!(first), &json!(security))
        );
    }
    assert_eq!(
        read(&first, "159915.XSHE", "2026-01-01", "2026-01-31")["missing"][0]["fields"],
        json!(["available_at", "cash_per_share"])
    );
    let outside = read(&first, "510300.XSHG", "2025-12-01", "2025-12-31");
    assert_eq!(outside["state"], "unknown");
    assert!(outside["coverage"].is_null());

    let mut revision = input.clone();
    revision["observed_at"] = json!("2026-03-02T00:00:00Z");
    revision["coverage"][0]["facts"][0]["cash_per_share"] = json!("0.2");
    let second = corporate_facts::import(&root, &serde_json::to_vec(&revision).unwrap()).unwrap();
    let catalog = preview_json(&root, &["actions-catalog"]);
    assert_eq!(
        (
            &catalog["versions"][0]["version"],
            &catalog["versions"][1]["version"]
        ),
        (&json!(second), &json!(first))
    );
    assert_eq!(
        read(&first, "510300.XSHG", "2026-01-01", "2026-01-31")["coverage"]["facts"][0]["cash_per_share"],
        "0.123456789012345678"
    );
    assert_eq!(
        read(&second, "510300.XSHG", "2026-01-01", "2026-01-31")["coverage"]["facts"][0]["cash_per_share"],
        "0.2"
    );

    let before = tree_hash(&root);
    for args in [
        vec!["actions-catalog"],
        vec![
            "actions",
            "--version",
            first.as_str(),
            "--instrument",
            "510300.XSHG",
            "--start",
            "2026-01-01",
            "--end",
            "2026-01-31",
        ],
    ] {
        let _ = preview_json(&root, &args);
    }
    assert_eq!(tree_hash(&root), before);

    let object = root.join(format!(
        "corporate-facts/{}.json",
        first.strip_prefix("caf:sha256:").unwrap()
    ));
    let bytes = fs::read(&object).unwrap();
    fs::write(&object, b"{}").unwrap();
    let catalog = preview_json(&root, &["actions-catalog"]);
    assert_eq!(
        (
            catalog["versions"].as_array().unwrap().len(),
            catalog["invalid_versions"].as_array().unwrap().len()
        ),
        (1, 1)
    );
    let (code, error) = preview_failure(
        &root,
        &[
            "actions",
            "--version",
            first.as_str(),
            "--instrument",
            "510300.XSHG",
            "--start",
            "2026-01-01",
            "--end",
            "2026-01-31",
        ],
        1,
    );
    assert_eq!(code, 1);
    assert_eq!(error["error"]["code"], "invalid_dataset_table");
    fs::write(&object, bytes).unwrap();
    assert_eq!(
        read(&first, "510300.XSHG", "2026-01-01", "2026-01-31")["state"],
        "covered"
    );

    for args in [
        vec![
            "actions",
            "--version",
            first.as_str(),
            "--instrument",
            "510300.XSHG",
        ],
        vec![
            "actions",
            "--version",
            first.as_str(),
            "--instrument",
            "510300.XSHG",
            "--start",
            "2026-01-01",
            "--end",
            "2026-01-31",
            "--dsv",
            "x",
        ],
        vec!["actions-catalog", "--dsv", "x"],
    ] {
        assert_eq!(preview(&root, &args).status.code(), Some(2));
    }
}
