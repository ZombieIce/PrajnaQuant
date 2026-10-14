//! Synthetic vendor-format originals only; no real market data or network.

use std::{fs, io::Write, path::Path};

use arrow_array::{Array, StringArray};
use prajna_data::{
    FrozenSourceMeta, Manifest, NormalizerRegistry, PublishError, RawStore, RebuildError,
    SourceKind, SourceRecordInput, freeze_raw, publish_dataset, read_parquet, rebuild_dataset,
};
use serde_json::{Value, json};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const ID: &str = "szse-etf-frozen";
const OBSERVED: &str = "2026-10-14T10:00:00+08:00";

fn meta(source: &str, content_type: &str) -> FrozenSourceMeta {
    FrozenSourceMeta {
        content_type: content_type.into(),
        source_id: source.into(),
        endpoint: format!("synthetic://{source}"),
        observed_at: OBSERVED.into(),
        terms_conclusion: "unresolved".into(),
        coverage: json!({"sessions": 4}),
        gaps: json!([]),
        ingested_by: "szse-etf-frozen-test".into(),
    }
}

/// SZSE `monthList` response for one natural month.
fn calendar_month(year: i32, month: u32, open_days: &[u32]) -> Vec<u8> {
    let first = chrono::NaiveDate::from_ymd_opt(year, month, 1).unwrap();
    let data: Vec<Value> = first
        .iter_days()
        .take_while(|day| chrono::Datelike::month(day) == month)
        .map(|day| {
            let open = open_days.contains(&chrono::Datelike::day(&day));
            json!({"jyrq": day.to_string(), "jybz": if open { "1" } else { "0" }, "zrxh": 1})
        })
        .collect();
    serde_json::to_vec(&json!({"data": data, "nowdate": "2026-10-14"})).unwrap()
}

fn calendar() -> Vec<u8> {
    calendar_month(2026, 1, &[5, 6, 7, 8])
}

/// One `.cod` record / `.md1` block: code, open, high, low, close, shares, amount.
type TdxRecord = (&'static str, f64, f64, f64, f64, u64, f64);

fn rec(code: &'static str, close: f64) -> TdxRecord {
    (code, 1.0, 1.1, 0.9, close, 20_000, 21_000.5)
}

/// TDX `g4day` package holding `szYYMMDD.cod/.md1` plus an unrelated SH member.
fn tdx_package(day_key: &str, records: &[TdxRecord], comment: Option<&str>) -> Vec<u8> {
    let mut cod = Vec::new();
    let mut md1 = vec![0_u8; records.len() * 512];
    for (i, (code, open, high, low, close, shares, amount)) in records.iter().enumerate() {
        // Price blocks are stored in reverse so decoding must follow the sequence.
        let sequence = (records.len() - 1 - i) as u16;
        let mut record = [0_u8; 150];
        record[..6].copy_from_slice(code.as_bytes());
        record[32..34].copy_from_slice(&sequence.to_le_bytes());
        cod.extend_from_slice(&record);
        let block = &mut md1[usize::from(sequence) * 512..][..512];
        for (offset, value) in [(12, open), (20, high), (28, low), (36, close), (72, amount)] {
            block[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        block[56..64].copy_from_slice(&shares.to_le_bytes());
    }
    zip_members(
        &[
            (format!("sz{day_key}.cod"), cod),
            (format!("sz{day_key}.md1"), md1),
            (format!("sh{day_key}.cod"), Vec::new()),
        ],
        comment,
    )
}

fn zip_members(members: &[(String, Vec<u8>)], comment: Option<&str>) -> Vec<u8> {
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, bytes) in members {
        writer.start_file(name.as_str(), options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    if let Some(comment) = comment {
        writer.set_comment(comment);
    }
    writer.finish().unwrap().into_inner()
}

fn tdx_0105(close_159901: f64, comment: Option<&str>) -> Vec<u8> {
    tdx_package(
        "260105",
        &[
            rec("159901", close_159901),
            rec("159902", 0.95),
            rec("000001", 9.99),
            rec("159903", 0.0),
            ("159904", 0.0, 0.0, 0.0, 1.2, 0, 0.0),
        ],
        comment,
    )
}

/// Tencent `fqkline` rows: date, open, close, high, low, lots.
fn tencent_doc(key: &str, array: &str, rows: &[[&str; 6]]) -> Vec<u8> {
    let mut node = json!({"qt": {}, "version": "16"});
    node[array] = json!(rows);
    serde_json::to_vec(&json!({"code": 0, "msg": "", "data": {key: node}})).unwrap()
}

fn tencent() -> Vec<u8> {
    tencent_doc(
        "sz159901",
        "day",
        &[
            ["2026-01-05", "1.000", "1.050", "1.100", "0.900", "200.000"],
            ["2026-01-06", "1.000", "1.061", "1.100", "0.900", "200.000"],
            ["2026-01-07", "1.000", "1.000", "1.100", "0.900", "200.000"],
        ],
    )
}

fn millis(text: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(text)
        .unwrap()
        .timestamp_millis()
}

/// cninfo `hisAnnouncement/query` response.
fn cninfo_doc(items: &[(&str, &str, &str, &str)]) -> Vec<u8> {
    let announcements: Vec<Value> = items
        .iter()
        .map(|(id, code, title, day)| {
            json!({"announcementId": id, "secCode": code, "secName": "合成ETF",
                "announcementTitle": title,
                "announcementTime": millis(&format!("{day}T00:00:00+08:00")),
                "adjunctUrl": format!("finalpage/{day}/{id}.PDF"), "adjunctType": "PDF"})
        })
        .collect();
    serde_json::to_vec(&json!({"announcements": announcements,
        "totalRecordNum": items.len(), "hasMore": false}))
    .unwrap()
}

fn cninfo() -> Vec<u8> {
    cninfo_doc(&[
        (
            "A-1",
            "159901",
            "合成ETF<em>收益分配</em>公告",
            "2026-01-06",
        ),
        ("A-2", "159903", "合成ETF上市交易公告书", "2026-01-03"),
        ("A-3", "000001", "某股票停牌公告", "2026-01-05"),
        ("A-4", "159901", "合成ETF提示性公告", "2026-01-05"),
    ])
}

struct Frozen {
    store: RawStore,
    hashes: Vec<String>,
}

fn freeze_all(root: &Path, docs: &[(&str, Vec<u8>)]) -> Frozen {
    let store = RawStore::open(root).unwrap();
    let hashes = docs
        .iter()
        .map(|(source, bytes)| {
            let content_type = if bytes.starts_with(b"PK") {
                "application/zip"
            } else {
                "application/json"
            };
            freeze_raw(&store, bytes, meta(source, content_type)).unwrap()
        })
        .collect();
    Frozen { store, hashes }
}

/// Order: calendar, TDX 01-05, 01-06, 01-08, Tencent, cninfo.
fn standard(root: &Path) -> Frozen {
    freeze_all(
        root,
        &[
            ("szse-calendar", calendar()),
            ("tdx", tdx_0105(1.05, None)),
            ("tdx", tdx_package("260106", &[rec("159901", 1.06)], None)),
            ("tdx", tdx_package("260108", &[rec("159901", 1.07)], None)),
            ("tencent", tencent()),
            ("cninfo", cninfo()),
        ],
    )
}

fn with_hashes(frozen: &Frozen, hashes: Vec<String>) -> Frozen {
    Frozen {
        store: frozen.store.clone(),
        hashes,
    }
}

fn publish(root: &Path, frozen: &Frozen) -> Result<Manifest, PublishError> {
    publish_dataset(
        root,
        &frozen.store,
        &NormalizerRegistry::with_builtins(),
        ID,
        "1",
        &frozen.hashes,
        OBSERVED,
    )
}

fn table_path(root: &Path, dsv: &str, table: &str) -> std::path::PathBuf {
    let hex = dsv.strip_prefix("dsv:sha256:").unwrap();
    root.join("normalized")
        .join(table)
        .join(hex)
        .join("part-00000.parquet")
}

fn read_strings(root: &Path, dsv: &str, table: &str, column: usize) -> Vec<String> {
    read_parquet(table_path(root, dsv, table), table, "1")
        .unwrap()
        .iter()
        .flat_map(|batch| {
            let array = batch
                .column(column)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            (0..array.len())
                .map(|i| array.value(i).to_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn timestamps(root: &Path, dsv: &str, table: &str, column: usize) -> Vec<Option<i64>> {
    read_parquet(table_path(root, dsv, table), table, "1")
        .unwrap()
        .iter()
        .flat_map(|batch| {
            let array = batch
                .column(column)
                .as_any()
                .downcast_ref::<arrow_array::TimestampNanosecondArray>()
                .unwrap();
            (0..array.len())
                .map(|i| (!array.is_null(i)).then(|| array.value(i)))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn ts(text: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(text)
        .unwrap()
        .timestamp_nanos_opt()
        .unwrap()
}

fn issue_codes(error: PublishError) -> Vec<String> {
    match error {
        PublishError::Validation(issues) => issues.into_iter().map(|issue| issue.code).collect(),
        other => panic!("expected validation error, got {other}"),
    }
}

fn rejects(root: &Path, docs: &[(&str, Vec<u8>)], code: &str) {
    let codes = issue_codes(publish(root, &freeze_all(root, docs)).unwrap_err());
    assert!(codes.contains(&code.to_owned()), "{code} not in {codes:?}");
}

fn raw_object(root: &Path, hash: &str) -> std::path::PathBuf {
    let hex = hash.strip_prefix("sha256:").unwrap();
    root.join("raw/sha256").join(&hex[..2]).join(hex)
}

#[test]
fn same_frozen_originals_rebuild_identical_dsv_in_independent_lakes() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (frozen_a, frozen_b) = (standard(a.path()), standard(b.path()));
    assert_eq!(frozen_a.hashes, frozen_b.hashes);

    let first = publish(a.path(), &frozen_a).unwrap();
    let second = publish(b.path(), &frozen_b).unwrap();
    assert_eq!(first.dsv, second.dsv);
    assert_eq!(first.core.tables, second.core.tables);

    let registry = NormalizerRegistry::with_builtins();
    assert_eq!(
        rebuild_dataset(&frozen_a.store, &registry, &first).unwrap(),
        first.dsv
    );

    let names: Vec<_> = first.core.tables.iter().map(|t| t.table.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "announcements",
            "bars",
            "instruments",
            "sessions",
            "source_crosscheck"
        ]
    );
}

#[test]
fn vendor_originals_decode_to_bars_gaps_crosscheck_and_announcements() {
    let dir = tempfile::tempdir().unwrap();
    let frozen = standard(dir.path());
    let manifest = publish(dir.path(), &frozen).unwrap();
    let dsv = &manifest.dsv;

    // Stock 000001 is out of scope; 159903 (close 0) and 159904 (carried close,
    // empty range) did not trade: none is a Bar.
    assert_eq!(read_strings(dir.path(), dsv, "bars", 0).len(), 4);
    assert_eq!(
        read_strings(dir.path(), dsv, "instruments", 0),
        vec!["159901.SZSE", "159902.SZSE"]
    );
    assert_eq!(read_strings(dir.path(), dsv, "sessions", 0).len(), 4);
    // 159903 has announcements but no Bars: it is not an Instrument here.
    assert_eq!(
        read_strings(dir.path(), dsv, "announcements", 1),
        vec!["159901.SZSE", "159903.SZSE", "159901.SZSE"]
    );
    assert_eq!(
        read_strings(dir.path(), dsv, "announcements", 2),
        vec!["dividend", "listing_notice", "unclassified"]
    );
    let status = read_strings(dir.path(), dsv, "source_crosscheck", 2);
    let fields = read_strings(dir.path(), dsv, "source_crosscheck", 3);
    assert_eq!(
        status,
        vec![
            "match",
            "mismatch",
            "secondary_only",
            "primary_only",
            "primary_only"
        ]
    );
    assert_eq!(fields, vec!["", "close", "", "", ""]);
    // Crosscheck rows point at the original package/response objects.
    let primary_raw = read_strings(dir.path(), dsv, "source_crosscheck", 4);
    assert_eq!(primary_raw[0], frozen.hashes[1]);

    let coverage = &manifest.provenance.coverage;
    // 2026-01-07 is inside 159901's span but has no Bar: a gap, not a halt.
    assert_eq!(coverage["primary_gap_sessions_unknown"], 1);
    assert_eq!(coverage["execution_status"], "not_derived");
    assert_eq!(coverage["bars"], 4);
    assert_eq!(coverage["calendar_days"], 31);
    assert_eq!(coverage["raw_objects_by_kind"]["tdx_daily"], 3);
    assert_eq!(
        coverage["excluded_records"],
        json!({"announcements_out_of_scope": 1, "tdx_no_trade_records": 2,
            "tdx_out_of_scope_records": 1})
    );
    let serialized = coverage.to_string();
    assert!(!serialized.contains("1.05") && !serialized.contains("21000"));
}

#[test]
fn announcement_is_available_only_from_the_next_session_close() {
    let dir = tempfile::tempdir().unwrap();
    let frozen = standard(dir.path());
    let manifest = publish(dir.path(), &frozen).unwrap();
    let available = timestamps(dir.path(), &manifest.dsv, "announcements", 4);
    // A-1 announced 2026-01-06 (a session): usable from 01-07 close, not 01-06 close.
    // A-2 announced Saturday 2026-01-03: first later session is Monday 01-05.
    // A-4 announced 2026-01-05: usable from 01-06 close.
    assert_eq!(
        available,
        vec![
            Some(ts("2026-01-07T15:00:00+08:00")),
            Some(ts("2026-01-05T15:00:00+08:00")),
            Some(ts("2026-01-06T15:00:00+08:00")),
        ]
    );
}

#[test]
fn announcement_without_a_known_next_session_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let base = || vec![("szse-calendar", calendar()), ("tdx", tdx_0105(1.05, None))];
    // After 01-08 the rest of January is closed and February is not frozen.
    let mut late = base();
    late.push((
        "cninfo",
        cninfo_doc(&[("A-9", "159901", "分红公告", "2026-01-08")]),
    ));
    rejects(dir.path(), &late, "announcement_next_session_unknown");
    // December 2025 is not frozen, so an earlier unknown session could exist.
    let mut uncovered = base();
    uncovered.push((
        "cninfo",
        cninfo_doc(&[("A-8", "159901", "分红公告", "2025-11-30")]),
    ));
    rejects(dir.path(), &uncovered, "announcement_next_session_unknown");
}

#[test]
fn real_source_bars_keep_availability_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let frozen = standard(dir.path());
    let manifest = publish(dir.path(), &frozen).unwrap();
    let available = timestamps(dir.path(), &manifest.dsv, "bars", 11);
    assert_eq!(available.len(), 4);
    assert!(available.iter().all(Option::is_none));
}

#[test]
fn replay_requires_the_vendor_original_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let frozen = standard(dir.path());
    let manifest = publish(dir.path(), &frozen).unwrap();
    let inputs: Vec<_> = manifest.core.inputs.iter().map(|i| &i.raw_sha256).collect();
    assert_eq!(inputs.len(), frozen.hashes.len());
    assert!(frozen.hashes.iter().all(|hash| inputs.contains(&hash)));

    fs::remove_file(raw_object(dir.path(), &frozen.hashes[2])).unwrap();
    assert!(matches!(
        rebuild_dataset(
            &frozen.store,
            &NormalizerRegistry::with_builtins(),
            &manifest
        ),
        Err(RebuildError::MissingRaw(_))
    ));
}

#[test]
fn vendor_revision_creates_a_new_dsv_and_conflicting_revisions_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let frozen = standard(dir.path());
    let first = publish(dir.path(), &frozen).unwrap();

    let revised_hash = freeze_raw(
        &frozen.store,
        &tdx_0105(1.051, None),
        FrozenSourceMeta {
            observed_at: "2026-10-15T10:00:00+08:00".into(),
            ..meta("tdx", "application/zip")
        },
    )
    .unwrap();
    assert_ne!(revised_hash, frozen.hashes[1]);
    let mut hashes = frozen.hashes.clone();
    hashes[1] = revised_hash.clone();
    let second = publish(dir.path(), &with_hashes(&frozen, hashes)).unwrap();
    assert_ne!(first.dsv, second.dsv);
    // The earlier version still rebuilds from its own frozen Raw set.
    let registry = NormalizerRegistry::with_builtins();
    assert_eq!(
        rebuild_dataset(&frozen.store, &registry, &first).unwrap(),
        first.dsv
    );

    let mut both = frozen.hashes.clone();
    both.push(revised_hash);
    let error = publish(dir.path(), &with_hashes(&frozen, both)).unwrap_err();
    assert!(issue_codes(error).contains(&"source_revision_conflict".to_owned()));
}

#[test]
fn re_download_with_identical_facts_deduplicates_deterministically() {
    let dir = tempfile::tempdir().unwrap();
    let frozen = standard(dir.path());
    let again = freeze_raw(
        &frozen.store,
        &tdx_0105(1.05, Some("re-download")),
        meta("tdx", "application/zip"),
    )
    .unwrap();
    assert_ne!(again, frozen.hashes[1]);
    let mut forward = frozen.hashes.clone();
    forward.push(again.clone());
    let mut reversed = forward.clone();
    reversed.reverse();
    let a = publish(dir.path(), &with_hashes(&frozen, forward)).unwrap();
    let b = publish(dir.path(), &with_hashes(&frozen, reversed)).unwrap();
    assert_eq!(a.core.tables, b.core.tables);
    // 159901 and 159902 on 2026-01-05 are restated identically.
    assert_eq!(a.provenance.coverage["identical_duplicate_facts"], 2);
    let primary_raw = read_strings(dir.path(), &a.dsv, "source_crosscheck", 4);
    assert_eq!(primary_raw[0], again.clone().min(frozen.hashes[1].clone()));
}

#[test]
fn missing_and_corrupt_raw_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let frozen = standard(dir.path());
    let manifest = publish(dir.path(), &frozen).unwrap();
    let object = raw_object(dir.path(), &frozen.hashes[1]);

    fs::write(&object, b"tampered").unwrap();
    assert!(matches!(
        publish(dir.path(), &frozen),
        Err(PublishError::Raw(_))
    ));
    let registry = NormalizerRegistry::with_builtins();
    assert!(matches!(
        rebuild_dataset(&frozen.store, &registry, &manifest),
        Err(RebuildError::Raw(_))
    ));

    fs::remove_file(&object).unwrap();
    assert!(matches!(
        rebuild_dataset(&frozen.store, &registry, &manifest),
        Err(RebuildError::MissingRaw(_))
    ));
}

#[test]
fn malformed_or_misplaced_originals_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let cal = || ("szse-calendar", calendar());

    rejects(root, &[("tdx", tdx_0105(1.05, None))], "missing_source");
    // 2026-01-10 is a frozen non-trading day; February is not frozen at all.
    let saturday = tdx_package("260110", &[rec("159901", 1.0)], None);
    rejects(root, &[cal(), ("tdx", saturday)], "bar_outside_calendar");
    let february = tdx_package("260202", &[rec("159901", 1.0)], None);
    rejects(root, &[cal(), ("tdx", february)], "calendar_not_covered");
    let off_tick = tdx_package("260105", &[rec("159901", 1.0505)], None);
    rejects(root, &[cal(), ("tdx", off_tick)], "invalid_price");
    let bad_range = tdx_package("260105", &[("159901", 1.0, 0.95, 0.9, 1.0, 100, 1.0)], None);
    rejects(root, &[cal(), ("tdx", bad_range)], "invalid_ohlc");
    let no_md1 = zip_members(&[("sz260105.cod".into(), vec![0; 150])], None);
    rejects(root, &[cal(), ("tdx", no_md1)], "invalid_tdx_package");
    let misaligned = zip_members(
        &[
            ("sz260105.cod".into(), vec![0; 149]),
            ("sz260105.md1".into(), vec![0; 512]),
        ],
        None,
    );
    rejects(root, &[cal(), ("tdx", misaligned)], "invalid_tdx_package");
    rejects(
        root,
        &[
            ("szse-calendar", calendar()),
            ("tdx", tdx_0105(1.05, None)),
            ("odd", b"not a vendor response".to_vec()),
        ],
        "unrecognized_original",
    );
    let mut partial: Value = serde_json::from_slice(&calendar()).unwrap();
    partial["data"].as_array_mut().unwrap().pop();
    rejects(
        root,
        &[
            ("szse-calendar", serde_json::to_vec(&partial).unwrap()),
            ("tdx", tdx_0105(1.05, None)),
        ],
        "invalid_calendar",
    );
    // A forward-adjusted-only response is never compared as unadjusted prices.
    let adjusted = tencent_doc(
        "sz159901",
        "qfqday",
        &[["2026-01-05", "1.000", "1.050", "1.100", "0.900", "200"]],
    );
    rejects(
        root,
        &[cal(), ("tdx", tdx_0105(1.05, None)), ("tencent", adjusted)],
        "invalid_tencent",
    );
    let stock = tencent_doc("sz000001", "day", &[]);
    rejects(
        root,
        &[cal(), ("tdx", tdx_0105(1.05, None)), ("tencent", stock)],
        "invalid_symbol",
    );
}

#[test]
fn fixture_raw_and_blank_provenance_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = RawStore::open(dir.path()).unwrap();
    let hash = store
        .put(
            &calendar(),
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::Fixture,
                source_id: "x".into(),
                request: json!({}),
                observed_at: "synthetic".into(),
                ingested_by: "test".into(),
            },
        )
        .unwrap();
    let frozen = Frozen {
        store: store.clone(),
        hashes: vec![hash],
    };
    assert!(
        issue_codes(publish(dir.path(), &frozen).unwrap_err())
            .contains(&"invalid_raw_source".to_owned())
    );

    for broken in [
        FrozenSourceMeta {
            terms_conclusion: " ".into(),
            ..meta("s", "application/json")
        },
        FrozenSourceMeta {
            content_type: "".into(),
            ..meta("s", "application/json")
        },
        FrozenSourceMeta {
            observed_at: "synthetic".into(),
            ..meta("s", "application/json")
        },
    ] {
        assert!(freeze_raw(&store, b"{}", broken).is_err());
    }
}

#[test]
fn source_record_keeps_media_type_endpoint_observation_terms_coverage_and_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let store = RawStore::open(dir.path()).unwrap();
    let hash = freeze_raw(
        &store,
        &tdx_0105(1.05, None),
        meta("tdx", "application/zip"),
    )
    .unwrap();
    let records = store.sources(&hash).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].content_type, "application/zip");
    assert_eq!(records[0].source_kind, SourceKind::FileImport);
    assert_eq!(records[0].observed_at, OBSERVED);
    assert_eq!(records[0].request["terms_conclusion"], "unresolved");
    assert_eq!(records[0].request["coverage"], json!({"sessions": 4}));
    assert_eq!(records[0].request["gaps"], json!([]));
    assert_eq!(records[0].request["endpoint"], "synthetic://tdx");
}
