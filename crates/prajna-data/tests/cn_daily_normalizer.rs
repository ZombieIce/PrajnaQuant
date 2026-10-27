use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::NaiveDate;
use prajna_data::{
    EvidenceSource, FieldEvidence, Manifest, NormalizerRegistry, PublishError, RawStore,
    SecurityEvidenceRecord, SecurityEvidenceStore, SecuritySnapshotId, SourceKind,
    SourceRecordInput, bar_sources_from_record_batch, bars_from_record_batch,
    instruments_from_record_batch, publish_dataset, read_dataset_table, read_manifest,
    rebuild_dataset,
    session_calendar::{CalendarStore, CalendarVersionId},
    sessions_from_record_batch,
};
use prajna_domain::{InstrumentId, InstrumentKind, Price, Quantity, TimestampNs, VenueId};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const CREATED_AT: &str = "2026-10-27T00:00:00Z";

fn date(value: &str) -> NaiveDate {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
}

fn sha256_id(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn calendar_document() -> Value {
    let venue = |venue: &str, source: &str, open: &str, close: &str| {
        let sessions = ["2026-01-05", "2026-01-06", "2026-01-07"]
            .into_iter()
            .map(|day| {
                json!({
                    "session_date": day,
                    "ts_open": format!("{day}T{open}+08:00"),
                    "ts_close": format!("{day}T{close}+08:00")
                })
            })
            .collect::<Vec<_>>();
        json!({
            "venue_id": venue,
            "source": {"source_id": source, "source_version": "1", "source_ref": "synthetic-fixture"},
            "coverage_start": "2026-01-05",
            "coverage_end": "2026-01-08",
            "sessions": sessions
        })
    };
    json!({
        "schema_version": 1,
        "venues": [
            venue("XSHG", "synthetic-xshg-calendar", "09:30:00", "15:00:00"),
            venue("XSHE", "synthetic-xshe-calendar", "09:15:00", "14:57:00")
        ]
    })
}

fn evidence_source() -> EvidenceSource {
    EvidenceSource {
        source_id: "synthetic-security-evidence".to_owned(),
        evidence_ref: "synthetic-fixture".to_owned(),
        source_version: "fixture-v1".to_owned(),
        observed_at: "2026-10-27T09:15:00+08:00".to_owned(),
        effective_from: Some(date("2000-01-01")),
        effective_to_exclusive: None,
    }
}

fn record(
    symbol: &str,
    venue: &str,
    kind: InstrumentKind,
    increment: &str,
) -> SecurityEvidenceRecord {
    SecurityEvidenceRecord {
        instrument_id: InstrumentId::new(symbol.to_owned(), VenueId::new(venue).unwrap()).unwrap(),
        kind,
        established_date: FieldEvidence::Known {
            value: date("2010-01-04"),
            source: evidence_source(),
        },
        listing_date: FieldEvidence::Known {
            value: date("2020-01-02"),
            source: evidence_source(),
        },
        lot_size: FieldEvidence::Known {
            value: Quantity::parse("100").unwrap(),
            source: evidence_source(),
        },
        price_increment: FieldEvidence::Known {
            value: Price::parse(increment).unwrap(),
            source: evidence_source(),
        },
        size_increment: FieldEvidence::Known {
            value: Quantity::parse("1").unwrap(),
            source: evidence_source(),
        },
    }
}

fn four_records() -> Vec<SecurityEvidenceRecord> {
    vec![
        record("600519", "XSHG", InstrumentKind::Equity, "0.01"),
        record("510300", "XSHG", InstrumentKind::Etf, "0.001"),
        record("000001", "XSHE", InstrumentKind::Equity, "0.01"),
        record("159952", "XSHE", InstrumentKind::Etf, "0.001"),
    ]
}

fn pinned_inputs(
    root: &Path,
    records: Vec<SecurityEvidenceRecord>,
) -> (Vec<u8>, CalendarVersionId, Vec<u8>, SecuritySnapshotId) {
    let calendars = CalendarStore::open(root).unwrap();
    let version = calendars
        .import_json(&serde_json::to_vec(&calendar_document()).unwrap())
        .unwrap();
    let calendar = calendars.get(&version).unwrap();
    let calendar_bytes = serde_json::to_vec(&calendar).unwrap();
    assert_eq!(sha256_id(&calendar_bytes), version.as_str());

    let evidence = SecurityEvidenceStore::open(root).unwrap();
    let selections = records
        .into_iter()
        .map(|record| {
            let version = evidence.import(&record).unwrap();
            (record.instrument_id, version)
        })
        .collect::<Vec<_>>();
    let (snapshot_id, snapshot) = evidence.select_snapshot(selections).unwrap();
    let snapshot_bytes = serde_json::to_vec(&snapshot).unwrap();
    assert_eq!(sha256_id(&snapshot_bytes), snapshot_id.as_str());
    (calendar_bytes, version, snapshot_bytes, snapshot_id)
}

fn put(
    store: &RawStore,
    bytes: &[u8],
    source_kind: SourceKind,
    source_id: &str,
    request: Value,
) -> String {
    store
        .put(
            bytes,
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind,
                source_id: source_id.into(),
                request,
                observed_at: "synthetic".into(),
                ingested_by: "cn-daily-test".into(),
            },
        )
        .unwrap()
}

fn put_daily(store: &RawStore, bytes: &[u8], instrument_id: &InstrumentId) -> String {
    put(
        store,
        bytes,
        SourceKind::VendorApi,
        "tencent_fqkline_daily",
        json!({
            "method_version": "tencent-fqkline-day/1",
            "instrument_id": instrument_id.to_string()
        }),
    )
}

fn row(date: &str, open: &str, close: &str, high: &str, low: &str, lots: &str) -> [String; 6] {
    [date, open, close, high, low, lots].map(str::to_owned)
}

fn good_row(day: &str) -> [String; 6] {
    row(day, "10.00", "10.10", "10.20", "9.90", "100")
}

fn response(native_symbol: &str, rows: &[[String; 6]]) -> Vec<u8> {
    let day = rows.iter().map(|row| json!(row)).collect::<Vec<_>>();
    serde_json::to_vec(&json!({"code": 0, "data": {native_symbol: {"day": day}}})).unwrap()
}

struct Scenario {
    temp: tempfile::TempDir,
    root: PathBuf,
    store: RawStore,
    calendar_version: CalendarVersionId,
    snapshot_id: SecuritySnapshotId,
    registry: NormalizerRegistry,
    pinned: Vec<String>,
}

impl Scenario {
    fn with_records(records: Vec<SecurityEvidenceRecord>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let (calendar_bytes, calendar_version, snapshot_bytes, snapshot_id) =
            pinned_inputs(&root, records);
        let store = RawStore::open(&root).unwrap();
        let calendar_hash = put(
            &store,
            &calendar_bytes,
            SourceKind::FileImport,
            "synthetic-calendar",
            json!({}),
        );
        let snapshot_hash = put(
            &store,
            &snapshot_bytes,
            SourceKind::FileImport,
            "synthetic-security-snapshot",
            json!({}),
        );
        let registry = NormalizerRegistry::with_cn_stock_etf_daily_v1(
            calendar_version.clone(),
            snapshot_id.clone(),
        )
        .unwrap();
        Self {
            temp,
            root,
            store,
            calendar_version,
            snapshot_id,
            registry,
            pinned: vec![calendar_hash, snapshot_hash],
        }
    }

    fn new() -> Self {
        Self::with_records(four_records())
    }

    fn daily(&self, symbol: &str, venue: &str, rows: &[[String; 6]]) -> String {
        let instrument_id = InstrumentId::new(symbol, VenueId::new(venue).unwrap()).unwrap();
        let native = format!("{}{symbol}", if venue == "XSHG" { "sh" } else { "sz" });
        put_daily(&self.store, &response(&native, rows), &instrument_id)
    }

    fn all_four(&self) -> Vec<String> {
        let dates = ["2026-01-05", "2026-01-06", "2026-01-07"];
        let rows = dates.map(good_row);
        vec![
            self.daily("600519", "XSHG", &rows),
            self.daily("510300", "XSHG", &rows),
            self.daily("000001", "XSHE", &rows),
            self.daily("159952", "XSHE", &rows),
        ]
    }

    fn publish(&self, market_hashes: &[String]) -> Result<Manifest, PublishError> {
        publish_with(
            &self.root,
            &self.store,
            &self.registry,
            &self.pinned,
            market_hashes,
        )
    }

    fn assert_no_dataset(&self) {
        assert!(!self.root.join("normalized").exists());
        assert!(!self.root.join("manifests").exists());
    }
}

fn publish_with(
    root: &Path,
    store: &RawStore,
    registry: &NormalizerRegistry,
    pinned: &[String],
    market_hashes: &[String],
) -> Result<Manifest, PublishError> {
    let mut hashes = pinned.to_vec();
    hashes.extend_from_slice(market_hashes);
    publish_dataset(
        root,
        store,
        registry,
        "cn-stock-etf-daily",
        "1",
        &hashes,
        CREATED_AT,
    )
}

fn read_bars(root: &Path, manifest: &Manifest) -> Vec<prajna_domain::Bar> {
    let batches = read_dataset_table(root, manifest, "sessions").unwrap();
    let sessions = batches
        .iter()
        .flat_map(|batch| sessions_from_record_batch(batch).unwrap())
        .collect::<Vec<_>>();
    read_dataset_table(root, manifest, "bars")
        .unwrap()
        .iter()
        .flat_map(|batch| bars_from_record_batch(batch, &sessions).unwrap())
        .collect()
}

#[test]
fn four_venue_kind_combinations_publish_read_and_rebuild() {
    let scenario = Scenario::new();
    let rows = [
        row(
            "2026-01-05",
            "10.123456789012345678",
            "10.223456789012345678",
            "10.323456789012345678",
            "10.023456789012345678",
            "123",
        ),
        row("2026-01-06", "10.20", "10.30", "10.40", "10.10", "124"),
        row("2026-01-07", "10.30", "10.40", "10.50", "10.20", "125"),
    ];
    let mut hashes = vec![scenario.daily("600519", "XSHG", &rows)];
    hashes.extend(scenario.all_four().into_iter().skip(1));

    assert!(scenario.registry.contains("cn-stock-etf-daily", "1"));
    let manifest = scenario.publish(&hashes).unwrap();
    assert_eq!(manifest.core.normalizer.id, "cn-stock-etf-daily");
    assert_eq!(manifest.core.normalizer.version, "1");
    assert_eq!(manifest.core.inputs.len(), 6);
    assert_eq!(
        read_manifest(&scenario.root, &manifest.dsv).unwrap(),
        manifest
    );
    assert_eq!(scenario.publish(&hashes).unwrap().dsv, manifest.dsv);

    let sessions = read_dataset_table(&scenario.root, &manifest, "sessions")
        .unwrap()
        .iter()
        .flat_map(|batch| sessions_from_record_batch(batch).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(sessions.len(), 6);
    let sh = sessions
        .iter()
        .find(|session| {
            session.venue_id().as_str() == "XSHG" && session.session_date() == date("2026-01-05")
        })
        .unwrap();
    let sz = sessions
        .iter()
        .find(|session| {
            session.venue_id().as_str() == "XSHE" && session.session_date() == date("2026-01-05")
        })
        .unwrap();
    assert_eq!(
        sh.ts_open(),
        TimestampNs::parse("2026-01-05T09:30:00+08:00").unwrap()
    );
    assert_eq!(
        sz.ts_open(),
        TimestampNs::parse("2026-01-05T09:15:00+08:00").unwrap()
    );
    assert_ne!(sh.ts_close(), sz.ts_close());

    let instruments = read_dataset_table(&scenario.root, &manifest, "instruments")
        .unwrap()
        .iter()
        .flat_map(|batch| instruments_from_record_batch(batch).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(instruments.len(), 4);
    let etf = instruments
        .iter()
        .find(|instrument| instrument.id().to_string() == "510300.XSHG")
        .unwrap();
    assert_eq!(etf.kind(), InstrumentKind::Etf);
    assert_eq!(etf.price_increment(), Price::parse("0.001").unwrap());
    assert_eq!(etf.lot_size(), Some(Quantity::parse("100").unwrap()));
    let stock = instruments
        .iter()
        .find(|instrument| instrument.id().to_string() == "000001.XSHE")
        .unwrap();
    assert_eq!(stock.kind(), InstrumentKind::Equity);
    assert_eq!(stock.price_increment(), Price::parse("0.01").unwrap());

    let bars = read_bars(&scenario.root, &manifest);
    assert_eq!(bars.len(), 12);
    assert!(
        bars.iter()
            .all(|bar| bar.available_at().is_none() && bar.amount().is_none())
    );
    let precise = bars
        .iter()
        .find(|bar| {
            bar.instrument_id().to_string() == "600519.XSHG"
                && bar.session_date() == date("2026-01-05")
        })
        .unwrap();
    assert_eq!(
        precise.open(),
        Price::parse("10.123456789012345678").unwrap()
    );
    assert_eq!(
        precise.close(),
        Price::parse("10.223456789012345678").unwrap()
    );
    assert_eq!(
        precise.high(),
        Price::parse("10.323456789012345678").unwrap()
    );
    assert_eq!(
        precise.low(),
        Price::parse("10.023456789012345678").unwrap()
    );
    assert_eq!(precise.volume(), Quantity::parse("12300").unwrap());
    assert_eq!(precise.ts_open(), sh.ts_open());
    let other_venue = bars
        .iter()
        .find(|bar| {
            bar.instrument_id().to_string() == "159952.XSHE"
                && bar.session_date() == date("2026-01-05")
        })
        .unwrap();
    assert_eq!(other_venue.ts_open(), sz.ts_open());

    let sources = read_dataset_table(&scenario.root, &manifest, "bar_sources")
        .unwrap()
        .iter()
        .flat_map(|batch| bar_sources_from_record_batch(batch).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 12);
    assert!(sources.iter().all(|source| {
        source.source == "tencent_fqkline_daily" && hashes.contains(&source.source_raw_sha256)
    }));
    assert_eq!(
        manifest.provenance.coverage["available_at"],
        json!("unknown_null")
    );
    assert_eq!(
        manifest.provenance.coverage["amount"],
        json!("absent_from_source_null")
    );

    fs::remove_dir_all(scenario.root.join("normalized")).unwrap();
    assert_eq!(
        rebuild_dataset(&scenario.store, &scenario.registry, &manifest).unwrap(),
        manifest.dsv
    );
    assert!(NormalizerRegistry::with_builtins().contains("szse-etf-frozen", "1"));
    assert!(!NormalizerRegistry::with_builtins().contains("cn-stock-etf-daily", "1"));
    drop(scenario.temp);
}

#[test]
fn missing_quotes_stay_absent_and_identical_duplicates_are_deduplicated() {
    let scenario = Scenario::new();
    let mut rows = vec![
        good_row("2026-01-05"),
        good_row("2026-01-05"),
        good_row("2026-01-07"),
    ];
    rows[1] = rows[0].clone();
    let h1 = scenario.daily("600519", "XSHG", &rows);
    let mut hashes = scenario.all_four();
    hashes[0] = h1;

    let manifest = scenario.publish(&hashes).unwrap();
    let bars = read_bars(&scenario.root, &manifest);
    assert_eq!(bars.len(), 11);
    assert!(!bars.iter().any(|bar| {
        bar.instrument_id().symbol() == "600519" && bar.session_date() == date("2026-01-06")
    }));
    assert!(bars.iter().all(|bar| bar.volume() > Quantity::ZERO));
    assert_eq!(
        read_dataset_table(&scenario.root, &manifest, "sessions")
            .unwrap()
            .iter()
            .flat_map(|batch| sessions_from_record_batch(batch).unwrap())
            .count(),
        6
    );
    assert_eq!(
        manifest.provenance.coverage["identical_duplicate_bars"],
        json!(1)
    );

    assert_eq!(
        manifest.provenance.coverage["missing_instrument_sessions_unknown"],
        json!(1)
    );
}

#[test]
fn identical_facts_in_distinct_raws_deduplicate_to_the_minimum_hash() {
    let scenario = Scenario::new();
    let h1 = scenario.daily(
        "600519",
        "XSHG",
        &[good_row("2026-01-05"), good_row("2026-01-07")],
    );
    let h2 = scenario.daily(
        "600519",
        "XSHG",
        &[good_row("2026-01-07"), good_row("2026-01-05")],
    );
    let mut market = scenario.all_four();
    market[0] = h1.clone();
    market.push(h2.clone());
    let manifest = scenario.publish(&market).unwrap();
    let bars = read_bars(&scenario.root, &manifest);
    assert_eq!(bars.len(), 11);
    assert_eq!(
        manifest.provenance.coverage["identical_duplicate_bars"],
        json!(2)
    );
    let minimum = if h1 < h2 { h1 } else { h2 };
    let sources = read_dataset_table(&scenario.root, &manifest, "bar_sources")
        .unwrap()
        .iter()
        .flat_map(|batch| bar_sources_from_record_batch(batch).unwrap())
        .filter(|source| source.instrument_id.symbol() == "600519")
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 2);
    assert!(
        sources
            .iter()
            .all(|source| source.source_raw_sha256 == minimum)
    );
}

#[test]
fn invalid_values_and_conflicting_duplicates_are_rejected_without_publishing() {
    let reject = |rows: Vec<[String; 6]>, code: &str| {
        let scenario = Scenario::new();
        let hash = scenario.daily("600519", "XSHG", &rows);
        let error = scenario.publish(&[hash]).unwrap_err();
        let PublishError::Validation(issues) = error else {
            panic!("expected structured validation error, got {error}");
        };
        assert!(
            issues.iter().any(|issue| issue.code == code),
            "expected {code}, got {issues:?}"
        );
        scenario.assert_no_dataset();
    };
    let day = "2026-01-05";
    reject(
        vec![row(
            day,
            "1.0000000000000000001",
            "10.10",
            "10.20",
            "9.90",
            "100",
        )],
        "inexact_decimal",
    );
    reject(
        vec![row(day, "1e40", "10.10", "10.20", "9.90", "100")],
        "decimal_out_of_range",
    );
    reject(
        vec![row(day, "10.00", "10.10", "9.00", "9.90", "100")],
        "invalid_ohlc",
    );
    reject(
        vec![row(day, "10.00", "10.10", "10.20", "9.90", "-1")],
        "negative_volume",
    );
    reject(
        vec![row("2026-01-08", "10.00", "10.10", "10.20", "9.90", "100")],
        "bar_on_non_session_date",
    );
    reject(
        vec![row("2026-01-09", "10.00", "10.10", "10.20", "9.90", "100")],
        "bar_outside_calendar_coverage",
    );
    reject(
        vec![
            good_row(day),
            row(day, "10.00", "10.20", "10.30", "9.90", "100"),
        ],
        "conflicting_duplicate_bar",
    );
}

#[test]
fn pinned_inputs_and_blocked_evidence_fail_closed() {
    let scenario = Scenario::new();
    let hash = scenario.daily("600519", "XSHG", &[good_row("2026-01-05")]);
    let error = publish_with(
        &scenario.root,
        &scenario.store,
        &scenario.registry,
        &[],
        std::slice::from_ref(&hash),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        PublishError::Validation(issues)
            if issues.iter().any(|issue| issue.code == "missing_pinned_calendar")
                && issues.iter().any(|issue| issue.code == "missing_pinned_security_snapshot")
    ));
    scenario.assert_no_dataset();

    let error = scenario.publish(&[]).unwrap_err();
    assert!(matches!(
        error,
        PublishError::Validation(issues)
            if issues.iter().any(|issue| issue.code == "missing_daily_raw")
    ));
    scenario.assert_no_dataset();

    let pinned_calendar = scenario.pinned[0].clone();
    let other_snapshot = SecuritySnapshotId::parse(format!("sha256:{}", "0".repeat(64))).unwrap();
    let registry = NormalizerRegistry::with_cn_stock_etf_daily_v1(
        scenario.calendar_version.clone(),
        other_snapshot,
    )
    .unwrap();
    let error = publish_with(
        &scenario.root,
        &scenario.store,
        &registry,
        &[pinned_calendar, scenario.pinned[1].clone()],
        &[hash],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        PublishError::Validation(issues)
            if issues.iter().any(|issue| issue.code == "missing_pinned_security_snapshot")
    ));
    scenario.assert_no_dataset();

    let unknown = response("sh600519", &[good_row("2026-01-06")]);
    let unknown = put(
        &scenario.store,
        &unknown,
        SourceKind::FileImport,
        "unpinned-daily-source",
        json!({}),
    );
    let error = scenario.publish(&[unknown]).unwrap_err();
    assert!(matches!(
        error,
        PublishError::Validation(issues)
            if issues.iter().any(|issue| issue.code == "source_method_mismatch")
    ));
    scenario.assert_no_dataset();

    let mut records = four_records();
    records[1].lot_size = FieldEvidence::Unknown {
        reason: "synthetic unknown lot".to_owned(),
        source: evidence_source(),
    };
    let blocked = Scenario::with_records(records);
    let error = blocked.publish(&blocked.all_four()).unwrap_err();
    assert!(matches!(
        error,
        PublishError::Validation(issues)
            if issues.iter().any(|issue| issue.code == "instrument_not_publishable")
    ));
    blocked.assert_no_dataset();
}

#[test]
fn fixed_selection_rebuilds_old_version_and_revision_creates_a_new_one() {
    let scenario = Scenario::new();
    let hashes = scenario.all_four();
    let first = scenario.publish(&hashes).unwrap();
    let first_bytes = scenario.store.get(&hashes[0]).unwrap();
    put(
        &scenario.store,
        &first_bytes,
        SourceKind::VendorApi,
        "later-appended-source-record",
        json!({"method_version": "unrelated-later-method/9"}),
    );
    assert_eq!(scenario.publish(&hashes).unwrap().dsv, first.dsv);

    let evidence = SecurityEvidenceStore::open(&scenario.root).unwrap();
    let mut revised = record("600519", "XSHG", InstrumentKind::Equity, "0.01");
    revised.lot_size = FieldEvidence::Known {
        value: Quantity::parse("200").unwrap(),
        source: EvidenceSource {
            source_version: "fixture-v2".to_owned(),
            ..evidence_source()
        },
    };
    let mut selections = Vec::new();
    for original in four_records() {
        let selected = if original.instrument_id.symbol() == "600519" {
            revised.clone()
        } else {
            original
        };
        let version = evidence.import(&selected).unwrap();
        selections.push((selected.instrument_id, version));
    }
    let (revised_snapshot_id, revised_snapshot) = evidence.select_snapshot(selections).unwrap();
    assert_ne!(revised_snapshot_id, scenario.snapshot_id);
    let revised_bytes = serde_json::to_vec(&revised_snapshot).unwrap();
    let revised_hash = put(
        &scenario.store,
        &revised_bytes,
        SourceKind::FileImport,
        "synthetic-security-snapshot-v2",
        json!({}),
    );

    let latest = scenario.publish(&hashes).unwrap();
    assert_eq!(latest.dsv, first.dsv);
    fs::remove_dir_all(scenario.root.join("normalized")).unwrap();
    assert_eq!(
        rebuild_dataset(&scenario.store, &scenario.registry, &first).unwrap(),
        first.dsv
    );

    let registry = NormalizerRegistry::with_cn_stock_etf_daily_v1(
        scenario.calendar_version.clone(),
        revised_snapshot_id,
    )
    .unwrap();
    let second = publish_with(
        &scenario.root,
        &scenario.store,
        &registry,
        &[scenario.pinned[0].clone(), revised_hash],
        &hashes,
    )
    .unwrap();
    assert_ne!(second.dsv, first.dsv);
    assert_eq!(read_manifest(&scenario.root, &first.dsv).unwrap(), first);

    let mut revised_calendar = calendar_document();
    revised_calendar["venues"][1]["sessions"][0]["ts_open"] = json!("2026-01-05T09:16:00+08:00");
    let calendars = CalendarStore::open(&scenario.root).unwrap();
    let calendar_version = calendars
        .import_json(&serde_json::to_vec(&revised_calendar).unwrap())
        .unwrap();
    let calendar_bytes = serde_json::to_vec(&calendars.get(&calendar_version).unwrap()).unwrap();
    let calendar_hash = put(
        &scenario.store,
        &calendar_bytes,
        SourceKind::FileImport,
        "synthetic-calendar-v2",
        json!({}),
    );
    let registry = NormalizerRegistry::with_cn_stock_etf_daily_v1(
        calendar_version,
        scenario.snapshot_id.clone(),
    )
    .unwrap();
    let third = publish_with(
        &scenario.root,
        &scenario.store,
        &registry,
        &[calendar_hash, scenario.pinned[1].clone()],
        &hashes,
    )
    .unwrap();
    assert_ne!(third.dsv, first.dsv);
    assert_eq!(read_manifest(&scenario.root, &first.dsv).unwrap(), first);
}

#[test]
fn existing_mvp6_registry_identity_is_unchanged() {
    let builtins = NormalizerRegistry::with_builtins();
    for version in ["1", "2", "3", "4"] {
        assert!(builtins.contains("szse-etf-frozen", version));
    }
    assert!(builtins.contains("synthetic-etf-daily", "1"));
}
