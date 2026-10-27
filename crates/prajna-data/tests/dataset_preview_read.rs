//! Production read entry: synthetic stock/ETF publication followed by the
//! read-only Dataset preview. Everything here is synthetic and local.

use arrow_array::RecordBatch;
use chrono::NaiveDate;
use prajna_data::{
    EvidenceSource, FieldEvidence, Manifest, ManifestCore, ManifestProvenance, ManifestTable,
    NormalizerIdentity, SecurityEvidenceRecord, SecurityEvidenceStore, bars_to_record_batch,
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
    let output = preview(root, args);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
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

    // A corrupt Dataset table is isolated: the other Dataset still lists.
    fs::remove_dir_all(lake.join("normalized/sessions").join(dsv_hex(&stock))).unwrap();
    let catalog = preview_json(&lake, &["catalog"]);
    assert_eq!(catalog["datasets"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["invalid_manifests"].as_array().unwrap().len(), 1);
    assert_eq!(
        catalog["invalid_manifests"][0]["manifest"],
        format!("{}.json", dsv_hex(&stock))
    );
    assert_eq!(catalog["datasets"][0]["dsv"], etf);
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

#[test]
fn empty_committed_dataset_is_readable_with_explicit_empty_coverage() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("lake");
    let instruments = instruments_to_record_batch(&[]).unwrap();
    let sessions = sessions_to_record_batch(&[]).unwrap();
    let bars = bars_to_record_batch(&[]).unwrap();
    let table = |name: &str, batch: &RecordBatch, keys: &[&str]| ManifestTable {
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
        row_count: 0,
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
                table("instruments", &instruments, &["instrument_id"]),
                table("sessions", &sessions, &["venue_id", "session_date"]),
                table("bars", &bars, &["instrument_id", "session_date"]),
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
    write_manifest(&root, &manifest).unwrap();

    let catalog = preview_json(&root, &["catalog"]);
    assert_eq!(catalog["datasets"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["datasets"][0]["instruments"], 0);
    assert_eq!(catalog["datasets"][0]["sessions"], 0);
    assert_eq!(catalog["datasets"][0]["bars"], 0);
    assert!(catalog["datasets"][0]["first_session"].is_null());
    assert!(catalog["invalid_manifests"].as_array().unwrap().is_empty());

    let show = preview_json(&root, &["show", "--dsv", &manifest.dsv]);
    assert!(show["instruments"].as_array().unwrap().is_empty());
    assert!(show["venues"].as_array().unwrap().is_empty());
    assert_eq!(show["coverage"], json!({}));
    let (_, error) = preview_error(
        &root,
        &[
            "bars",
            "--dsv",
            &manifest.dsv,
            "--instrument",
            "510300.XSHG",
        ],
    );
    assert_eq!(error["error"]["code"], "dataset_not_found");
}
