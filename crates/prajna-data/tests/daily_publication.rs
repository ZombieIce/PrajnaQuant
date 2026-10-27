use chrono::NaiveDate;
use prajna_data::{
    EvidenceSource, FieldEvidence, SecurityEvidenceRecord, SecurityEvidenceStore,
    bars_from_record_batch,
    daily_publish::DailyPublicationReceipt,
    daily_raw::{DailyRawCollector, DailyRawRequest, SecurityKind, Venue},
    read_dataset_table, read_manifest,
    session_calendar::CalendarStore,
    sessions_from_record_batch,
};
use prajna_domain::{InstrumentId, InstrumentKind, Price, Quantity};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};
use tencent_transport::{HttpReply, HttpRequest, HttpTransport, TransportFailure};

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
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
fn selection(input: &Path, symbol: &str, venue: &str, kind: InstrumentKind, close: &str) -> Value {
    let calendar = json!({"schema_version": 1, "venues": (["XSHG", "XSHE"].map(|v| json!({
        "venue_id": v, "source": {"source_id":"synthetic", "source_version":"1", "source_ref":"fixture"},
        "coverage_start":"2026-01-05", "coverage_end":"2026-01-06",
        "sessions":(["2026-01-05", "2026-01-06"].map(|d| json!({"session_date":d,
            "ts_open":format!("{d}T09:30:00+08:00"), "ts_close":format!("{d}T15:00:00+08:00")})))
    })))});
    let calendar_id = CalendarStore::open(input)
        .unwrap()
        .import_json(&serde_json::to_vec(&calendar).unwrap())
        .unwrap();
    let record = SecurityEvidenceRecord {
        instrument_id: format!("{symbol}.{venue}").parse::<InstrumentId>().unwrap(),
        kind,
        established_date: FieldEvidence::Known {
            value: date("2010-01-01"),
            source: source(),
        },
        listing_date: FieldEvidence::Known {
            value: date("2020-01-01"),
            source: source(),
        },
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
    // Two identical facts on day one; day two is deliberately absent.
    let row = json!(["2026-01-05", "10.000", close, "12.000", "9.000", "1.25"]);
    let code = if kind == InstrumentKind::Equity {
        json!("0")
    } else {
        json!(0)
    };
    let body = serde_json::to_vec(&json!({"code":code, "data":{native:{"day":[row.clone(),row]}}}))
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
            end: date("2026-01-06"),
        })
        .unwrap();
    json!({"raw_root":input, "calendar_root":input, "evidence_root":input,
        "report":run.report_path, "calendar_version":calendar_id.as_str(),
        "security_snapshot":snapshot.as_str(), "normalizer_id":"cn-stock-etf-daily",
        "normalizer_version":"1", "raw_sha256s":[run.report.chunks[0].raw_objects[0].raw_sha256]})
}
fn cli(root: &Path, selection: Option<&Value>, receipt: &str, success: bool) {
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
    if let Some(value) = selection {
        let path = root.join("selection.json");
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        command.arg("--selection").arg(path);
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.success(),
        success,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(
            !text.contains("sha256:")
                && !text.contains("2026-01")
                && !text.contains("synthetic.invalid")
        );
    }
    if !success {
        assert!(fs::read_dir(root.join("diagnostics")).unwrap().count() > 0);
    }
}
fn receipt(root: &Path, name: &str) -> DailyPublicationReceipt {
    serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap()
}

#[test]
fn repeated_publication_does_not_duplicate_selected_raw_provenance() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("lake");
    let mut selected = selection(&root, "510300", "XSHG", InstrumentKind::Etf, "11.123");
    let hash = selected["raw_sha256s"][0].as_str().unwrap().to_owned();
    let raw = prajna_data::RawStore::open(&root).unwrap();
    let original = raw.sources(&hash).unwrap();
    for _ in 0..4 {
        cli(&root, Some(&selected), "same.json", true);
        assert_eq!(raw.sources(&hash).unwrap(), original);
    }
    #[cfg(unix)]
    {
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        selected["raw_root"] = json!(alias);
        cli(&root, Some(&selected), "same.json", true);
        assert_eq!(raw.sources(&hash).unwrap(), original);
    }
    let other = temp.path().join("other");
    for _ in 0..2 {
        cli(&other, Some(&selected), "copied.json", true);
        assert_eq!(
            prajna_data::RawStore::open(&other)
                .unwrap()
                .sources(&hash)
                .unwrap(),
            original
        );
    }
    let refreshed = selection(&root, "510300", "XSHG", InstrumentKind::Etf, "11.123");
    let expected = raw.sources(&hash).unwrap();
    assert_eq!(expected.len(), original.len() + 1);
    for _ in 0..2 {
        cli(&other, Some(&refreshed), "refreshed.json", true);
        assert_eq!(
            prajna_data::RawStore::open(&other)
                .unwrap()
                .sources(&hash)
                .unwrap(),
            expected
        );
    }
}

#[test]
fn four_security_combinations_publish_read_repeat_revise_and_rebuild_from_frozen_inputs() {
    for (symbol, venue, kind) in [
        ("600519", "XSHG", InstrumentKind::Equity),
        ("510300", "XSHG", InstrumentKind::Etf),
        ("000001", "XSHE", InstrumentKind::Equity),
        ("159952", "XSHE", InstrumentKind::Etf),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input");
        let root = temp.path().join("published");
        let first = selection(&input, symbol, venue, kind, "11.123");
        cli(&root, Some(&first), "first.json", true);
        let published = receipt(&root, "first.json");
        let manifest = read_manifest(&root, &published.dsv).unwrap();
        let sessions = read_dataset_table(&root, &manifest, "sessions").unwrap();
        let sessions = sessions_from_record_batch(&sessions[0]).unwrap();
        let batches = read_dataset_table(&root, &manifest, "bars").unwrap();
        let bars = bars_from_record_batch(&batches[0], &sessions).unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].close(), Price::parse("11.123").unwrap());
        assert_eq!(bars[0].volume(), Quantity::parse("125").unwrap());
        assert_eq!(bars[0].available_at(), None);
        assert_eq!(bars[0].amount(), None);
        assert_eq!(sessions.len(), 4);
        cli(&root, Some(&first), "first.json", true);
        assert_eq!(receipt(&root, "first.json").dsv, published.dsv);
        let revised = selection(&input, symbol, venue, kind, "11.124");
        cli(&root, Some(&revised), "revised.json", true);
        assert_ne!(receipt(&root, "revised.json").dsv, published.dsv);
        assert_eq!(
            read_manifest(&root, &published.dsv).unwrap().dsv,
            published.dsv
        );
        fs::remove_dir_all(input).unwrap();
        cli(&root, None, "first.json", true);
        cli(&root, None, "revised.json", true);
    }
}

#[test]
fn mismatched_selection_empty_failed_sources_and_unknown_rules_preserve_old_version() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let root = temp.path().join("published");
    let good = selection(&input, "510300", "XSHG", InstrumentKind::Etf, "11.123");
    cli(&root, Some(&good), "good.json", true);
    let old = receipt(&root, "good.json");
    for mutation in [
        "kind",
        "venue",
        "source",
        "range",
        "empty",
        "failed",
        "dates",
        "raw",
        "rule",
        "calendar",
        "evidence",
        "prelisting",
    ] {
        let mut bad = good.clone();
        let mut report: Value =
            serde_json::from_slice(&fs::read(good["report"].as_str().unwrap()).unwrap()).unwrap();
        match mutation {
            "kind" => report["security_kind"] = json!("stock"),
            "venue" => report["instrument_id"] = json!("510300.XSHE"),
            "source" => {
                report["chunks"][0]["raw_objects"][0]["source"] = json!("unselected-source")
            }
            "range" => report["requested_range"]["end"] = json!("2026-01-07"),
            "empty" => report["chunks"][0]["status"] = json!("empty"),
            "failed" => {
                report["chunks"][0]["raw_objects"][0]["outcome"] =
                    json!("rejected_candidate_response")
            }
            "dates" => report["chunks"][0]["actual_response_dates"] = json!(["2026-01-06"]),
            "raw" => {
                bad["raw_sha256s"] = json!([
                    "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                ])
            }
            "rule" => bad["normalizer_version"] = json!("unknown-unit-version"),
            "calendar" => bad["calendar_version"] = json!("latest"),
            "evidence" | "prelisting" => {
                let store = SecurityEvidenceStore::open(&input).unwrap();
                let snapshot_id = good["security_snapshot"].as_str().unwrap().to_owned();
                let snapshot = store
                    .get_snapshot(&prajna_data::SecuritySnapshotId::parse(snapshot_id).unwrap())
                    .unwrap();
                let mut record = snapshot.instruments[0].record.clone();
                if mutation == "evidence" {
                    record.lot_size = FieldEvidence::Unknown {
                        reason: "synthetic unknown".into(),
                        source: source(),
                    };
                } else {
                    record.listing_date = FieldEvidence::Known {
                        value: date("2026-01-06"),
                        source: source(),
                    };
                }
                let version = store.import(&record).unwrap();
                let (id, _) = store
                    .select_snapshot([(record.instrument_id, version)])
                    .unwrap();
                bad["security_snapshot"] = json!(id.as_str());
            }
            _ => unreachable!(),
        }
        let path = input.join("bad-report.json");
        fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
        bad["report"] = json!(path);
        cli(&root, Some(&bad), "bad.json", false);
        assert!(!root.join("bad.json").exists());
        assert_eq!(fs::read_dir(root.join("manifests")).unwrap().count(), 1);
        assert!(
            read_dataset_table(&root, &read_manifest(&root, &old.dsv).unwrap(), "bars").is_ok()
        );
    }
}

#[test]
fn mid_publication_failure_is_uncommitted_and_retry_preserves_unrelated_security() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let root = temp.path().join("published");
    let good = selection(&input, "510300", "XSHG", InstrumentKind::Etf, "11.123");
    cli(&root, Some(&good), "old.json", true);
    let old = receipt(&root, "old.json");
    let next = selection(&input, "159952", "XSHE", InstrumentKind::Etf, "11.124");
    // Fail a later table rename, after earlier tables have already moved.
    fs::rename(
        root.join("normalized/sessions"),
        root.join("saved-sessions"),
    )
    .unwrap();
    fs::write(
        root.join("normalized/sessions"),
        b"synthetic I/O obstruction",
    )
    .unwrap();
    cli(&root, Some(&next), "next.json", false);
    assert_eq!(fs::read_dir(root.join("manifests")).unwrap().count(), 1);
    assert_eq!(
        fs::read_dir(root.join("normalized/bars")).unwrap().count(),
        1
    );
    assert!(!root.join("next.json").exists());
    fs::remove_file(root.join("normalized/sessions")).unwrap();
    fs::rename(
        root.join("saved-sessions"),
        root.join("normalized/sessions"),
    )
    .unwrap();
    cli(&root, Some(&next), "next.json", true);
    cli(&root, None, "old.json", true);
    assert!(read_dataset_table(&root, &read_manifest(&root, &old.dsv).unwrap(), "bars").is_ok());
}
