//! Synthetic vendor-format originals only; no real market data or network.

use std::{fs, io::Write, path::Path};

use arrow_array::{Array, StringArray};
use prajna_data::{
    CorporateActionKind, FrozenSourceMeta, Manifest, NormalizerRegistry, PublishError, RawStore,
    RebuildError, SourceKind, SourceRecordInput, corporate_actions_from_record_batch, freeze_raw,
    publish_dataset, read_parquet, rebuild_dataset,
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
    // Workspace feature unification may enable zip's current-time default.
    // Synthetic originals must stay byte-identical regardless of wall clock.
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());
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
fn corporate_action_dataset_v2_normalizes_late_announcement_and_binds_facts() {
    let dir = tempfile::tempdir().unwrap();
    let cninfo = serde_json::to_vec(&json!({"announcements":[{
        "announcementId":"CA-1", "secCode":"159901", "secName":"合成ETF",
        "announcementTitle":"合成ETF收益分配公告",
        "announcementTime":millis("2026-01-06T00:00:00+08:00"),
        "corporateActions":[
            {"type":"dividend", "recordDate":"2026-01-05", "exDate":"2026-01-06", "payDate":"2026-01-08", "cashPerShare":"0.0728"},
            {"type":"share_conversion", "effectiveDate":"2026-01-07", "shareRatio":"2"},
            {"type":"liquidation", "effectiveDate":"2026-01-08", "liquidationCashPerShare":"0.85"}
        ]
    }], "totalRecordNum":1, "hasMore":false})).unwrap();
    let frozen = freeze_all(
        dir.path(),
        &[
            ("szse-calendar", calendar()),
            ("tdx", tdx_0105(1.05, None)),
            ("tdx", tdx_package("260106", &[rec("159901", 1.06)], None)),
            ("tdx", tdx_package("260108", &[rec("159901", 1.07)], None)),
            ("tencent", tencent()),
            ("cninfo", cninfo),
        ],
    );
    let manifest = publish_dataset(
        dir.path(),
        &frozen.store,
        &NormalizerRegistry::with_builtins(),
        ID,
        "2",
        &frozen.hashes,
        OBSERVED,
    )
    .unwrap();
    assert!(
        manifest
            .core
            .tables
            .iter()
            .any(|table| table.table == "corporate_actions")
    );
    let batch = read_parquet(
        table_path(dir.path(), &manifest.dsv, "corporate_actions"),
        "corporate_actions",
        "1",
    )
    .unwrap();
    let rows = corporate_actions_from_record_batch(batch.first().unwrap()).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].kind, CorporateActionKind::Dividend);
    assert_eq!(rows[0].cash_per_share.as_deref(), Some("0.0728"));
    assert_eq!(
        rows[0].announced_at,
        "2026-01-05T16:00:00Z".parse().unwrap()
    );
    assert_eq!(rows[1].kind, CorporateActionKind::ShareConversion);
    assert_eq!(rows[1].share_ratio.as_deref(), Some("2"));
    assert_eq!(rows[2].kind, CorporateActionKind::Liquidation);
    assert_eq!(rows[2].liquidation_cash_per_share.as_deref(), Some("0.85"));
    // Jan 7 is the first session after the Jan 6 publication date.
    assert_eq!(
        rows[0].available_at,
        "2026-01-07T15:00:00+08:00".parse().unwrap()
    );
    assert_eq!(
        rebuild_dataset(
            &frozen.store,
            &NormalizerRegistry::with_builtins(),
            &manifest
        )
        .unwrap(),
        manifest.dsv
    );
    let price_only = publish_dataset(
        dir.path(),
        &frozen.store,
        &NormalizerRegistry::with_builtins(),
        ID,
        "1",
        &frozen.hashes,
        OBSERVED,
    )
    .unwrap();
    assert_ne!(price_only.dsv, manifest.dsv);
}

#[test]
fn action_category_without_structured_facts_fails_closed_in_v2() {
    let dir = tempfile::tempdir().unwrap();
    let frozen = standard(dir.path());
    let error = publish_dataset(
        dir.path(),
        &frozen.store,
        &NormalizerRegistry::with_builtins(),
        ID,
        "2",
        &frozen.hashes,
        OBSERVED,
    )
    .unwrap_err();
    assert!(format!("{error:?}").contains("unparsed_corporate_action"));
}

#[test]
fn termination_announcement_without_liquidation_facts_fails_closed_in_v2() {
    let dir = tempfile::tempdir().unwrap();
    let frozen = freeze_all(
        dir.path(),
        &[
            ("szse-calendar", calendar()),
            ("tdx", tdx_0105(1.05, None)),
            ("tdx", tdx_package("260106", &[rec("159901", 1.06)], None)),
            ("tdx", tdx_package("260108", &[rec("159901", 1.07)], None)),
            ("tencent", tencent()),
            (
                "cninfo",
                cninfo_doc(&[("T-1", "159901", "合成ETF基金合同终止公告", "2026-01-06")]),
            ),
        ],
    );
    let error = publish_dataset(
        dir.path(),
        &frozen.store,
        &NormalizerRegistry::with_builtins(),
        ID,
        "2",
        &frozen.hashes,
        OBSERVED,
    )
    .unwrap_err();
    assert!(format!("{error:?}").contains("unparsed_corporate_action"));
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
fn trade_to_no_trade_revisions_fail_closed_in_both_orders() {
    // Both documented no-trade encodings must participate in revision checks.
    for revised_record in [rec("159901", 0.0), ("159901", 0.0, 0.0, 0.0, 1.05, 0, 0.0)] {
        let dir = tempfile::tempdir().unwrap();
        let frozen = standard(dir.path());
        let first = publish(dir.path(), &frozen).unwrap();
        let revised_hash = freeze_raw(
            &frozen.store,
            &tdx_package("260105", &[revised_record], None),
            meta("tdx", "application/zip"),
        )
        .unwrap();
        let mut combined = frozen.hashes.clone();
        combined.push(revised_hash.clone());
        for hashes in [combined.clone(), combined.into_iter().rev().collect()] {
            let error = publish(dir.path(), &with_hashes(&frozen, hashes)).unwrap_err();
            assert!(issue_codes(error).contains(&"source_revision_conflict".to_owned()));
        }

        // Replacing the old package is a new version, with no stale Jan-05 Bar.
        let mut replacement = frozen.hashes.clone();
        replacement[1] = revised_hash;
        let revised = publish(dir.path(), &with_hashes(&frozen, replacement)).unwrap();
        assert_ne!(first.dsv, revised.dsv);
        assert_eq!(read_strings(dir.path(), &revised.dsv, "bars", 0).len(), 2);
        assert_eq!(
            read_strings(dir.path(), &revised.dsv, "source_crosscheck", 2),
            [
                "secondary_only",
                "mismatch",
                "secondary_only",
                "primary_only"
            ]
        );
        let registry = NormalizerRegistry::with_builtins();
        for manifest in [&first, &revised] {
            assert_eq!(
                rebuild_dataset(&frozen.store, &registry, manifest).unwrap(),
                manifest.dsv
            );
        }
    }
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
    // Two traded and two no-trade observations on Jan-05 are restated identically.
    assert_eq!(a.provenance.coverage["identical_duplicate_facts"], 4);
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

/// Replace only offset 4 in synthetic price blocks; no real vendor bytes.
fn with_reference(bytes: Vec<u8>, reference: f64) -> Vec<u8> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut members = Vec::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).unwrap();
        let name = file.name().to_owned();
        let mut data = Vec::new();
        file.read_to_end(&mut data).unwrap();
        if name.ends_with(".md1") {
            for block in data.chunks_exact_mut(512) {
                block[4..12].copy_from_slice(&reference.to_le_bytes());
            }
        }
        members.push((name, data));
    }
    zip_members(&members, None)
}

#[test]
fn v3_primary_fallback_no_trade_reference_and_replay() {
    use prajna_data::{
        bar_sources_from_record_batch, bars_from_record_batch, sessions_from_record_batch,
    };
    let dir = tempfile::tempdir().unwrap();
    // Hand example: previous close 1.05; dividend 0.05 -> reference 1.00;
    // then 2-for-1 conversion -> reference 0.50. Not a previous-close shift.
    let frozen = freeze_all(
        dir.path(),
        &[
            ("szse-calendar", calendar()),
            (
                "tdx",
                with_reference(
                    tdx_package("260105", &[rec("158001", 1.05), rec("159002", 1.0)], None),
                    1.0,
                ),
            ),
            (
                "tdx",
                with_reference(tdx_package("260106", &[rec("158001", 1.0)], None), 1.0),
            ),
            (
                "tdx",
                with_reference(
                    tdx_package(
                        "260107",
                        &[("158001", 0.5, 0.5, 0.5, 0.5, 20000, 10000.0)],
                        None,
                    ),
                    0.5,
                ),
            ),
            // A keyed no-trade record blocks Tencent, even with positive OHLC.
            (
                "tdx",
                with_reference(
                    tdx_package("260108", &[("158001", 1.0, 1.1, 0.9, 1.0, 0, 0.0)], None),
                    0.5,
                ),
            ),
            (
                "tencent",
                tencent_doc(
                    "sz158001",
                    "day",
                    &[
                        ["2026-01-05", "1.0", "1.06", "1.1", "0.9", "200"],
                        ["2026-01-08", "1.0", "1.0", "1.1", "0.9", "200"],
                    ],
                ),
            ),
            // Existing primary instrument: only its missing Jan-06 cell is filled.
            (
                "tencent",
                tencent_doc(
                    "sz159002",
                    "day",
                    &[
                        ["2026-01-05", "1.0", "1.0", "1.1", "0.9", "200"],
                        ["2026-01-06", "1.0", "1.0", "1.1", "0.9", "200"],
                    ],
                ),
            ),
            (
                "tencent",
                tencent_doc(
                    "sz158002",
                    "day",
                    &[["2026-01-06", "1.0", "1.0", "1.1", "0.9", "200"]],
                ),
            ),
        ],
    );
    let registry = NormalizerRegistry::with_builtins();
    let manifest = publish_dataset(
        dir.path(),
        &frozen.store,
        &registry,
        ID,
        "3",
        &frozen.hashes,
        OBSERVED,
    )
    .unwrap();
    let batches = |table: &str| {
        read_parquet(table_path(dir.path(), &manifest.dsv, table), table, "1").unwrap()
    };
    let sessions = sessions_from_record_batch(&batches("sessions")[0]).unwrap();
    let bars = bars_from_record_batch(&batches("bars")[0], &sessions).unwrap();
    let sources = bar_sources_from_record_batch(&batches("bar_sources")[0]).unwrap();
    assert_eq!(bars.len(), 6);
    assert_eq!(sources.len(), bars.len());
    assert_eq!(bars[0].close().to_string(), "1.05"); // Tencent mismatch never overrides.
    assert_eq!(sources[1].reference_price.unwrap().to_string(), "1");
    assert_eq!(sources[2].reference_price.unwrap().to_string(), "0.5");
    for (bar, source) in bars.iter().zip(&sources) {
        assert_eq!(bar.instrument_id(), &source.instrument_id);
        assert_eq!(bar.session_date(), source.session_date);
        assert_eq!(bar.available_at(), None);
        if source.source == "tencent_fallback" {
            assert_eq!(bar.amount(), None);
            assert_eq!(source.reference_price, None);
        } else {
            assert!(bar.amount().is_some());
            assert_eq!(source.source, "tdx");
        }
    }
    assert_eq!(
        rebuild_dataset(&frozen.store, &registry, &manifest).unwrap(),
        manifest.dsv
    );
    let reversed = frozen.hashes.iter().rev().cloned().collect::<Vec<_>>();
    let replay = publish_dataset(
        dir.path(),
        &frozen.store,
        &registry,
        ID,
        "3",
        &reversed,
        OBSERVED,
    )
    .unwrap();
    assert_eq!(manifest.dsv, replay.dsv);
}

#[test]
fn v3_config_identity_and_legacy_regression() {
    use prajna_data::{RawInput, SzseEtfFrozen};
    let dir = tempfile::tempdir().unwrap();
    let frozen = freeze_all(
        dir.path(),
        &[
            ("szse-calendar", calendar()),
            (
                "tdx",
                with_reference(
                    tdx_package("260105", &[rec("159901", 1.05), rec("158001", 1.0)], None),
                    1.0,
                ),
            ),
        ],
    );
    let registry = NormalizerRegistry::with_builtins();
    // Pinned from main before v3, independently of the new config builder.
    assert_eq!(
        registry.get(ID, "1").unwrap().config_sha256(),
        "sha256:49d7475ca2ac715ae1f8d8c0071b6901f8b26b313b04a43f229dc8b4b7f88810"
    );
    assert_eq!(
        registry.get(ID, "2").unwrap().config_sha256(),
        "sha256:79ea1b64dd4522a24125dbb412a938f86717ea1c3fc1cbdcb742be9190a20a34"
    );

    let publish_version = |registry: &NormalizerRegistry, v: &str| {
        publish_dataset(
            dir.path(),
            &frozen.store,
            registry,
            ID,
            v,
            &frozen.hashes,
            OBSERVED,
        )
        .unwrap()
    };
    let v1 = publish_version(&registry, "1");
    let v2 = publish_version(&registry, "2");
    let v3 = publish_version(&registry, "3");
    assert_ne!(v1.dsv, v2.dsv);
    assert_ne!(v2.dsv, v3.dsv);
    // Legacy table rows and schema stay identical, except v2's action table.
    for table in &v1.core.tables {
        assert_eq!(
            Some(table),
            v2.core.tables.iter().find(|t| t.table == table.table)
        );
    }
    assert_eq!(
        read_strings(dir.path(), &v1.dsv, "instruments", 0),
        ["159901.SZSE"]
    );
    assert_eq!(
        read_strings(dir.path(), &v3.dsv, "instruments", 0),
        ["158001.SZSE", "159901.SZSE"]
    );
    let mut custom = NormalizerRegistry::new();
    custom.register(SzseEtfFrozen::v3(vec![(159000, 159999)]).unwrap());
    let changed = publish_version(&custom, "3");
    assert_ne!(changed.dsv, v3.dsv);
    assert_ne!(
        changed.core.normalizer.config_sha256,
        v3.core.normalizer.config_sha256
    );
    assert!(matches!(
        rebuild_dataset(&frozen.store, &registry, &changed),
        Err(RebuildError::ConfigHashMismatch { .. })
    ));
    assert_eq!(
        rebuild_dataset(&frozen.store, &custom, &changed).unwrap(),
        changed.dsv
    );
    // A reference-only revision is ignored by v1/v2, but conflicts in v3.
    let inputs = [
        RawInput::from_bytes(calendar()),
        RawInput::from_bytes(with_reference(
            tdx_package("260105", &[rec("159901", 1.05)], None),
            1.0,
        )),
        RawInput::from_bytes(with_reference(
            tdx_package("260105", &[rec("159901", 1.05)], None),
            0.5,
        )),
    ];
    for v in ["1", "2"] {
        assert!(registry.get(ID, v).unwrap().normalize(&inputs).is_ok());
    }
    assert!(
        registry
            .get(ID, "3")
            .unwrap()
            .normalize(&inputs)
            .unwrap_err()
            .iter()
            .any(|e| e.code == "source_revision_conflict")
    );
    for ranges in [
        vec![],
        vec![(159999, 158000)],
        vec![(158000, 159000), (159000, 159999)],
    ] {
        assert!(SzseEtfFrozen::v3(ranges).is_err());
    }
}

#[test]
fn v3_invalid_tdx_reference_fails_closed_without_changing_legacy_decoding() {
    use prajna_data::RawInput;
    let registry = NormalizerRegistry::with_builtins();
    for reference in [0.0, -1.0, f64::NAN, 1.0005] {
        let inputs = [
            RawInput::from_bytes(calendar()),
            RawInput::from_bytes(with_reference(
                tdx_package("260105", &[rec("159901", 1.05)], None),
                reference,
            )),
        ];
        for v in ["1", "2"] {
            assert!(registry.get(ID, v).unwrap().normalize(&inputs).is_ok());
        }

        assert!(
            registry
                .get(ID, "3")
                .unwrap()
                .normalize(&inputs)
                .unwrap_err()
                .iter()
                .any(|e| e.code == "invalid_reference_price")
        );
    }
}

#[test]
fn v4_publishes_full_configured_tdx_code_tables_including_no_trade_and_empty_sessions() {
    use arrow_array::StringArray;
    use prajna_data::{Normalizer, RawInput, SzseEtfFrozen, tdx_code_table_from_record_batch};
    let dir = tempfile::tempdir().unwrap();
    let frozen = freeze_all(
        dir.path(),
        &[
            ("szse-calendar", calendar()),
            (
                "tdx",
                with_reference(
                    tdx_package(
                        "260105",
                        &[
                            rec("158001", 1.05),
                            ("158002", 1.0, 1.1, 0.9, 1.0, 0, 0.0),
                            rec("159001", 1.05),
                        ],
                        None,
                    ),
                    1.0,
                ),
            ),
            ("tdx", with_reference(tdx_package("260106", &[], None), 1.0)),
        ],
    );
    let mut registry = NormalizerRegistry::new();
    registry.register(SzseEtfFrozen::v4(vec![(158000, 158999)]).unwrap());
    let manifest = publish_dataset(
        dir.path(),
        &frozen.store,
        &registry,
        ID,
        "4",
        &frozen.hashes,
        OBSERVED,
    )
    .unwrap();
    assert_eq!(
        manifest
            .core
            .tables
            .iter()
            .filter(|table| table.table == "tdx_code_table")
            .count(),
        1
    );
    let batches = read_parquet(
        table_path(dir.path(), &manifest.dsv, "tdx_code_table"),
        "tdx_code_table",
        "1",
    )
    .unwrap();
    assert_eq!(batches[0].num_rows(), 2);
    let code_tables = tdx_code_table_from_record_batch(&batches[0]).unwrap();
    assert_eq!(code_tables[0].code_ranges, vec![(158000, 158999)]);
    assert_eq!(code_tables[0].source_raw_sha256.len(), "sha256:".len() + 64);
    let symbols = batches[0]
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<String>>(symbols.value(0)).unwrap(),
        ["158001", "158002"]
    );
    assert_eq!(
        serde_json::from_str::<Vec<String>>(symbols.value(1)).unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        rebuild_dataset(&frozen.store, &registry, &manifest).unwrap(),
        manifest.dsv
    );
    let v3 = SzseEtfFrozen::v3(vec![(158000, 159999)]).unwrap();
    let legacy = v3
        .normalize(
            &frozen
                .hashes
                .iter()
                .map(|hash| RawInput {
                    raw_sha256: hash.clone(),
                    bytes: frozen.store.get(hash).unwrap(),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert!(legacy.tables.tdx_code_table.is_none());
}
