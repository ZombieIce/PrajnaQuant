use std::{fs, path::Path};

use arrow_array::{Array, StringArray};
use prajna_data::{
    FrozenSourceMeta, Manifest, NormalizerRegistry, PublishError, RawStore, RebuildError,
    SourceKind, SourceRecordInput, freeze_raw, publish_dataset, read_parquet, rebuild_dataset,
};
use serde_json::{Value, json};

const ID: &str = "szse-etf-frozen";

fn meta(source: &str, observed_at: &str) -> FrozenSourceMeta {
    FrozenSourceMeta {
        source_id: source.into(),
        endpoint: format!("synthetic://{source}"),
        observed_at: observed_at.into(),
        terms_conclusion: "unresolved".into(),
        coverage: json!({"sessions": 4}),
        gaps: json!([]),
        ingested_by: "szse-etf-frozen-test".into(),
    }
}

fn bytes(value: Value) -> Vec<u8> {
    serde_json::to_vec(&value).unwrap()
}

fn calendar() -> Value {
    json!({"frozen_format_version": 1, "kind": "szse_calendar",
        "sessions": ["2026-01-05", "2026-01-06", "2026-01-07", "2026-01-08"]})
}

fn tdx_bar(symbol: &str, date: &str, close: &str) -> Value {
    json!({"symbol": symbol, "date": date, "open": 1.000, "high": 1.100, "low": 0.900,
        "close": serde_json::from_str::<Value>(close).unwrap(), "volume": 20000, "amount": 21000.50})
}

fn tdx(bars: Vec<Value>) -> Value {
    json!({"frozen_format_version": 1, "kind": "tdx_daily", "bars": bars})
}

fn default_tdx() -> Value {
    tdx(vec![
        tdx_bar("159901", "2026-01-05", "1.050"),
        tdx_bar("159901", "2026-01-06", "1.060"),
        tdx_bar("159901", "2026-01-08", "1.070"),
        tdx_bar("159902", "2026-01-05", "0.950"),
    ])
}

fn tencent() -> Value {
    json!({"frozen_format_version": 1, "kind": "tencent_kline", "rows": [
        {"symbol": "159901", "date": "2026-01-05", "open": 1.000, "high": 1.100, "low": 0.900,
         "close": 1.050, "volume_lots": 200},
        {"symbol": "159901", "date": "2026-01-06", "open": 1.000, "high": 1.100, "low": 0.900,
         "close": 1.061, "volume_lots": 200},
        {"symbol": "159901", "date": "2026-01-07", "open": 1.000, "high": 1.100, "low": 0.900,
         "close": 1.000, "volume_lots": 200}
    ]})
}

fn cninfo() -> Value {
    json!({"frozen_format_version": 1, "kind": "cninfo_announcements", "items": [
        {"id": "A-1", "symbol": "159901", "category": "dividend", "announced_date": "2026-01-06"},
        {"id": "A-2", "symbol": "159903", "category": "listing_notice", "announced_date": "2025-12-01"}
    ]})
}

struct Frozen {
    store: RawStore,
    hashes: Vec<String>,
}

fn freeze_all(root: &Path, docs: &[(&str, Value)]) -> Frozen {
    let store = RawStore::open(root).unwrap();
    let hashes = docs
        .iter()
        .map(|(source, doc)| {
            freeze_raw(
                &store,
                &bytes(doc.clone()),
                meta(source, "2026-10-14T10:00:00+08:00"),
            )
            .unwrap()
        })
        .collect();
    Frozen { store, hashes }
}

fn standard(root: &Path) -> Frozen {
    freeze_all(
        root,
        &[
            ("szse-calendar", calendar()),
            ("tdx", default_tdx()),
            ("tencent", tencent()),
            ("cninfo", cninfo()),
        ],
    )
}

fn publish(root: &Path, frozen: &Frozen) -> Result<Manifest, PublishError> {
    publish_dataset(
        root,
        &frozen.store,
        &NormalizerRegistry::with_builtins(),
        ID,
        "1",
        &frozen.hashes,
        "2026-10-14T10:00:00+08:00",
    )
}

fn read_strings(root: &Path, dsv: &str, table: &str, column: usize) -> Vec<String> {
    let hex = dsv.strip_prefix("dsv:sha256:").unwrap();
    let path = root
        .join("normalized")
        .join(table)
        .join(hex)
        .join("part-00000.parquet");
    read_parquet(path, table, "1")
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

fn issue_codes(error: PublishError) -> Vec<String> {
    match error {
        PublishError::Validation(issues) => issues.into_iter().map(|issue| issue.code).collect(),
        other => panic!("expected validation error, got {other}"),
    }
}

#[test]
fn same_frozen_raw_set_rebuilds_identical_dsv_in_independent_lakes() {
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
fn bars_gaps_crosscheck_and_announcements_follow_the_frozen_set() {
    let dir = tempfile::tempdir().unwrap();
    let frozen = standard(dir.path());
    let manifest = publish(dir.path(), &frozen).unwrap();
    let dsv = &manifest.dsv;

    assert_eq!(read_strings(dir.path(), dsv, "bars", 0).len(), 4);
    assert_eq!(
        read_strings(dir.path(), dsv, "instruments", 0),
        vec!["159901.SZSE", "159902.SZSE"]
    );
    // 159903 has announcements but no Bars: it is not an Instrument here.
    assert_eq!(
        read_strings(dir.path(), dsv, "announcements", 1),
        vec!["159901.SZSE", "159903.SZSE"]
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

    let coverage = &manifest.provenance.coverage;
    // 2026-01-07 is inside 159901's span but has no Bar: a gap, not a halt.
    assert_eq!(coverage["primary_gap_sessions_unknown"], 1);
    assert_eq!(coverage["execution_status"], "not_derived");
    assert_eq!(coverage["bars"], 4);
    let serialized = coverage.to_string();
    assert!(!serialized.contains("1.05") && !serialized.contains("21000"));
}

#[test]
fn announcement_is_available_only_after_its_close() {
    let dir = tempfile::tempdir().unwrap();
    let frozen = standard(dir.path());
    let manifest = publish(dir.path(), &frozen).unwrap();
    let hex = manifest.dsv.strip_prefix("dsv:sha256:").unwrap();
    let path = dir
        .path()
        .join("normalized/announcements")
        .join(hex)
        .join("part-00000.parquet");
    let batches = read_parquet(path, "announcements", "1").unwrap();
    let available = batches[0]
        .column(4)
        .as_any()
        .downcast_ref::<arrow_array::TimestampNanosecondArray>()
        .unwrap();
    // 2026-01-06T15:00:00+08:00 = 07:00:00Z
    let expected = chrono::DateTime::parse_from_rfc3339("2026-01-06T15:00:00+08:00")
        .unwrap()
        .timestamp_nanos_opt()
        .unwrap();
    assert!(
        (0..available.len()).any(|i| available.value(i) == expected),
        "A-1 must be available at its announcement-day close"
    );
}

#[test]
fn vendor_revision_creates_a_new_dsv_and_conflicting_revisions_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let frozen = standard(dir.path());
    let first = publish(dir.path(), &frozen).unwrap();

    let revised = tdx(vec![
        tdx_bar("159901", "2026-01-05", "1.051"),
        tdx_bar("159901", "2026-01-06", "1.060"),
        tdx_bar("159901", "2026-01-08", "1.070"),
        tdx_bar("159902", "2026-01-05", "0.950"),
    ]);
    let revised_hash = freeze_raw(
        &frozen.store,
        &bytes(revised),
        meta("tdx", "2026-10-15T10:00:00+08:00"),
    )
    .unwrap();
    assert_ne!(revised_hash, frozen.hashes[1]);
    let mut hashes = frozen.hashes.clone();
    hashes[1] = revised_hash.clone();
    let second = publish(
        dir.path(),
        &Frozen {
            store: frozen.store.clone(),
            hashes,
        },
    )
    .unwrap();
    assert_ne!(first.dsv, second.dsv);
    // The earlier version still rebuilds from its own frozen Raw set.
    let registry = NormalizerRegistry::with_builtins();
    assert_eq!(
        rebuild_dataset(&frozen.store, &registry, &first).unwrap(),
        first.dsv
    );

    let mut both = frozen.hashes.clone();
    both.push(revised_hash);
    let error = publish(
        dir.path(),
        &Frozen {
            store: frozen.store.clone(),
            hashes: both,
        },
    )
    .unwrap_err();
    assert!(issue_codes(error).contains(&"source_revision_conflict".to_owned()));
}

#[test]
fn missing_and_corrupt_raw_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let frozen = standard(dir.path());
    let manifest = publish(dir.path(), &frozen).unwrap();
    let hex = frozen.hashes[1].strip_prefix("sha256:").unwrap();
    let object = dir.path().join("raw/sha256").join(&hex[..2]).join(hex);

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
fn missing_source_kind_and_bad_rows_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let no_calendar = freeze_all(dir.path(), &[("tdx", default_tdx())]);
    assert!(
        issue_codes(publish(dir.path(), &no_calendar).unwrap_err())
            .contains(&"missing_source".to_owned())
    );

    let off_calendar = freeze_all(
        dir.path(),
        &[
            ("szse-calendar", calendar()),
            ("tdx", tdx(vec![tdx_bar("159901", "2026-01-09", "1.050")])),
        ],
    );
    assert!(
        issue_codes(publish(dir.path(), &off_calendar).unwrap_err())
            .contains(&"bar_outside_calendar".to_owned())
    );

    let mut bad = tdx_bar("159901", "2026-01-05", "1.0505");
    bad["high"] = json!(1.100);
    let misaligned = freeze_all(
        dir.path(),
        &[("szse-calendar", calendar()), ("tdx", tdx(vec![bad]))],
    );
    assert!(
        issue_codes(publish(dir.path(), &misaligned).unwrap_err())
            .contains(&"invalid_price".to_owned())
    );

    let unknown_field = freeze_all(
        dir.path(),
        &[
            ("szse-calendar", calendar()),
            (
                "tdx",
                json!({"frozen_format_version": 1, "kind": "tdx_daily", "bars": [], "extra": 1}),
            ),
        ],
    );
    assert!(
        issue_codes(publish(dir.path(), &unknown_field).unwrap_err())
            .contains(&"invalid_envelope".to_owned())
    );
}

#[test]
fn fixture_raw_and_blank_provenance_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = RawStore::open(dir.path()).unwrap();
    let hash = store
        .put(
            &bytes(calendar()),
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
            ..meta("s", "2026-10-14T10:00:00+08:00")
        },
        FrozenSourceMeta {
            observed_at: "synthetic".into(),
            ..meta("s", "x")
        },
    ] {
        assert!(freeze_raw(&store, b"{}", broken).is_err());
    }
}

#[test]
fn source_record_keeps_endpoint_observation_terms_coverage_and_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let store = RawStore::open(dir.path()).unwrap();
    let hash = freeze_raw(
        &store,
        &bytes(calendar()),
        meta("szse-calendar", "2026-10-14T10:00:00+08:00"),
    )
    .unwrap();
    let records = store.sources(&hash).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].source_kind, SourceKind::FileImport);
    assert_eq!(records[0].observed_at, "2026-10-14T10:00:00+08:00");
    assert_eq!(records[0].request["terms_conclusion"], "unresolved");
    assert_eq!(records[0].request["coverage"], json!({"sessions": 4}));
    assert_eq!(records[0].request["gaps"], json!([]));
    assert!(
        records[0].request["endpoint"]
            .as_str()
            .unwrap()
            .starts_with("synthetic://")
    );
}
