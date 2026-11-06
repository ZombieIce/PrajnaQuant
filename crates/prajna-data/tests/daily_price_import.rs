use chrono::{Datelike, NaiveDate};
use prajna_data::{
    EvidenceSource, FieldEvidence, SecurityEvidenceRecord, SecurityEvidenceStore, adjusted_series,
    dataset_preview::{self, BarsRequest},
    session_calendar::CalendarStore,
};
use prajna_domain::{InstrumentKind, Price, Quantity};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, process::Command};

const START: &str = "2026-01-05";
const END: &str = "2026-01-20";
const MEMBER: &str = "510300.XSHG";

fn date(value: &str) -> NaiveDate {
    value.parse().unwrap()
}

fn evidence_source() -> EvidenceSource {
    EvidenceSource {
        source_id: "synthetic".into(),
        source_version: "1".into(),
        evidence_ref: "D12 fixture".into(),
        observed_at: "2026-01-21T00:00:00Z".into(),
        effective_from: None,
        effective_to_exclusive: None,
    }
}

fn replay_path(root: &Path, symbol: &str, mode: &str) -> std::path::PathBuf {
    root.join(format!("replay/sh{symbol}-{mode}-{START}-{END}.json"))
}

fn json_read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn json_write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap()
}

fn fixture(root: &Path) {
    fs::create_dir_all(root.join("replay")).unwrap();
    let days = date(START)
        .iter_days()
        .take_while(|d| *d <= date(END))
        .filter(|d| d.weekday().num_days_from_monday() < 5)
        .collect::<Vec<_>>();
    let sessions = std::iter::once(date("2026-01-02"))
        .chain(days.iter().copied())
        .chain(std::iter::once(date("2026-01-21")))
        .map(|day| {
            json!({
                "session_date": day,
                "ts_open": format!("{day}T09:30:00+08:00"),
                "ts_close": format!("{day}T15:00:00+08:00"),
            })
        })
        .collect::<Vec<_>>();
    let calendar = CalendarStore::open(root).unwrap();
    let version = calendar.import_json(&serde_json::to_vec(&json!({
        "schema_version": 1,
        "venues": (["XSHG", "XSHE"].map(|venue| json!({
            "venue_id": venue,
            "source": {"source_id":"synthetic", "source_version":"1", "source_ref":"D12 fixture"},
            "coverage_start": "2026-01-02", "coverage_end": "2026-01-21", "sessions": sessions,
        }))),
    })).unwrap()).unwrap();
    let evidence = SecurityEvidenceStore::open(root).unwrap();
    let mut members = BTreeMap::new();
    for (symbol, step) in [("510300", 10), ("510880", 5)] {
        let id = format!("{symbol}.XSHG");
        let record = SecurityEvidenceRecord {
            instrument_id: id.parse().unwrap(),
            kind: InstrumentKind::Etf,
            established_date: FieldEvidence::Known {
                value: date("2010-01-01"),
                source: evidence_source(),
            },
            listing_date: FieldEvidence::Known {
                value: date("2010-01-05"),
                source: evidence_source(),
            },
            lot_size: FieldEvidence::Known {
                value: Quantity::parse("100").unwrap(),
                source: evidence_source(),
            },
            price_increment: FieldEvidence::Known {
                value: Price::parse("0.001").unwrap(),
                source: evidence_source(),
            },
            size_increment: FieldEvidence::Known {
                value: Quantity::parse("100").unwrap(),
                source: evidence_source(),
            },
        };
        let version = evidence.import(&record).unwrap();
        let (snapshot, _) = evidence
            .select_snapshot([(record.instrument_id, version)])
            .unwrap();
        members.insert(id, snapshot.as_str().to_owned());
        let unadjusted = days
            .iter()
            .map(|day| json!([day, "1.000", "1.010", "1.010", "1.000", "100"]))
            .collect::<Vec<_>>();
        let adjusted = days
            .iter()
            .enumerate()
            .map(|(index, day)| {
                let close = format!("1.{:03}", index * step);
                json!([day, close, close, close, close, "100"])
            })
            .collect::<Vec<_>>();
        let native = format!("sh{symbol}");
        json_write(
            &replay_path(root, symbol, "raw"),
            &json!({"code":0,"data":{&native:{"day":unadjusted}}}),
        );
        json_write(
            &replay_path(root, symbol, "qfq"),
            &json!({"code":0,"data":{&native:{"qfqday":adjusted}}}),
        );
    }
    json_write(
        &root.join("config.json"),
        &json!({
            "calendar_root": root,
            "calendar_version": version.as_str(),
            "evidence_root": root,
            "start": START, "end": END, "members": members,
        }),
    );
}

#[test]
#[ignore = "explicit fixture generator for production browser tests"]
fn generate_e2e_input() {
    let root = std::env::var_os("PRAJNA_DAILY_PRICE_FIXTURE_ROOT")
        .expect("set an independent fixture root");
    fixture(Path::new(&root));
}

fn cli(root: &Path, expect_success: bool) -> Option<Value> {
    cli_replay(root, "replay", expect_success)
}

fn cli_replay(root: &Path, replay: &str, expect_success: bool) -> Option<Value> {
    let input = root.join("input");
    let output = Command::new(env!("CARGO_BIN_EXE_prajna-import-daily-price"))
        .arg("--root")
        .arg(root.join("lake"))
        .arg("--input-root")
        .arg(&input)
        .arg("--config")
        .arg(input.join("config.json"))
        .args(["--replay", replay, "--receipt", "receipt.json"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        expect_success,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for bytes in [output.stdout, output.stderr] {
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("sha256:") && !text.contains(START) && !text.contains(MEMBER));
    }
    if expect_success {
        Some(json_read(&input.join("receipt.json")))
    } else {
        assert!(
            fs::read_dir(input.join("daily-price/diagnostics"))
                .unwrap()
                .count()
                > 0
        );
        None
    }
}

fn dataset_bars(root: &Path, receipt: &Value) -> Value {
    dataset_preview::bars(
        &root.join("lake"),
        &BarsRequest {
            dsv: receipt["dsv"].as_str().unwrap().into(),
            instrument_id: MEMBER.into(),
            start: None,
            end: None,
            limit: 100,
            offset: 0,
        },
    )
    .unwrap()
}

#[test]
fn replay_publishes_production_prices_and_retains_fixed_versions_across_revisions() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let input = root.join("input");
    fixture(&input);
    let initial = cli(root, true).unwrap();
    let replay_dir = initial["replay_dir"].as_str().unwrap();
    let raw_name = format!("sh510300-raw-{START}-{END}.json");
    assert_eq!(
        fs::read(input.join(replay_dir).join(&raw_name)).unwrap(),
        fs::read(input.join("replay").join(raw_name)).unwrap(),
    );
    let repeated = cli_replay(root, replay_dir, true).unwrap();
    assert_eq!(initial["dsv"], repeated["dsv"]);
    assert_eq!(initial["adjustments"], repeated["adjustments"]);
    assert_eq!(initial["coverage"][MEMBER]["raw_rows"], 12);
    assert_ne!(
        initial["calendar_version"],
        initial["parent_calendar_version"]
    );
    let lake = root.join("lake");
    let manifest = prajna_data::read_manifest(&lake, initial["dsv"].as_str().unwrap()).unwrap();
    let sessions = prajna_data::read_dataset_table(&lake, &manifest, "sessions")
        .unwrap()
        .iter()
        .flat_map(|batch| prajna_data::sessions_from_record_batch(batch).unwrap())
        .filter(|session| session.venue_id().as_str() == "XSHG")
        .map(|session| session.session_date())
        .collect::<Vec<_>>();
    assert_eq!(sessions.len(), 12);
    assert_eq!(sessions[0], date(START));
    assert_eq!(sessions[11], date(END));
    assert_eq!(initial["coverage"][MEMBER]["qfq_missing"]["count"], 0);
    let bars = dataset_bars(root, &initial);
    assert_eq!(bars["items"][0]["open"], "1");
    assert_eq!(bars["items"][0]["close"], "1.01");
    assert_eq!(bars["items"][0]["volume"], "10000");
    let adjusted = adjusted_series::read(
        &root.join("lake"),
        initial["adjustments"][MEMBER].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(adjusted.rows[0].close, "1");
    assert_eq!(adjusted.rows[1].close, "1.01");
    assert_eq!(adjusted.rows.len(), 12);

    let raw = replay_path(&input, "510300", "raw");
    let mut revised_raw = json_read(&raw);
    revised_raw["data"]["sh510300"]["day"]
        .as_array_mut()
        .unwrap()
        .remove(3);
    json_write(&raw, &revised_raw);
    let adjusted_path = replay_path(&input, "510300", "qfq");
    let mut revised_qfq = json_read(&adjusted_path);
    revised_qfq["data"]["sh510300"]["qfqday"]
        .as_array_mut()
        .unwrap()
        .remove(2);
    json_write(&adjusted_path, &revised_qfq);
    let revised = cli(root, true).unwrap();
    assert_ne!(initial["dsv"], revised["dsv"]);
    assert_ne!(
        initial["adjustments"][MEMBER],
        revised["adjustments"][MEMBER]
    );
    assert_eq!(
        revised["coverage"][MEMBER]["raw_missing"]["first_dates"],
        json!(["2026-01-08"])
    );
    assert_eq!(
        revised["coverage"][MEMBER]["qfq_missing"]["first_dates"],
        json!(["2026-01-07"])
    );
    assert_eq!(
        revised["coverage"][MEMBER]["qfq_without_raw"]["first_dates"],
        json!(["2026-01-08"])
    );
    assert_eq!(revised["coverage"][MEMBER]["qfq_usable_rows"], 10);
    assert_eq!(dataset_bars(root, &initial), bars);
    let old = adjusted_series::read(
        &root.join("lake"),
        initial["adjustments"][MEMBER].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(old.rows.len(), 12);
}

#[test]
fn replay_rejects_duplicate_session_and_explicit_empty_qfq_with_raw_rows() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let input = root.join("input");
    fixture(&input);
    let path = replay_path(&input, "510300", "qfq");
    let original = json_read(&path);
    let mut duplicate = original.clone();
    let rows = duplicate["data"]["sh510300"]["qfqday"]
        .as_array_mut()
        .unwrap();
    rows.push(rows[0].clone());
    json_write(&path, &duplicate);
    cli(root, false);
    assert!(!input.join("receipt.json").exists());
    for (field, value) in [(0, json!("2026-01-04")), (5, json!(-1)), (1, json!(false))] {
        let mut invalid = original.clone();
        invalid["data"]["sh510300"]["qfqday"][0][field] = value;
        json_write(&path, &invalid);
        cli(root, false);
        assert!(!input.join("receipt.json").exists());
    }
    let raw = json_read(&replay_path(&input, "510300", "raw"));
    let mut explicit_empty = raw.clone();
    explicit_empty["data"]["sh510300"]["qfqday"] = json!([]);
    json_write(&path, &explicit_empty);
    cli(root, false);
    assert!(!input.join("receipt.json").exists());
    json_write(&path, &raw);
    let fallback = cli(root, true).unwrap();
    assert_eq!(fallback["coverage"][MEMBER]["qfq_day_fallback_chunks"], 1);
}
