//! Synthetic end-to-end tests for the MVP-6 corporate-action Dataset (#223).
//! No real market data, announcements, hashes or network access are used.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use prajna_data::{
    CorporateActionPublish, FrozenSourceMeta, Manifest, Normalizer, NormalizerRegistry, RawStore,
    SourceKind, SourceRecordInput, SzseEtfFrozen, publish_corporate_actions, publish_dataset,
    read_parquet, rebuild_dataset,
};
use serde_json::{Value, json};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const OBSERVED: &str = "2026-10-19T12:00:00+08:00";
const OPEN_DAYS: [u32; 10] = [5, 6, 7, 8, 9, 12, 13, 14, 15, 16];

fn meta(source: &str, content_type: &str) -> FrozenSourceMeta {
    FrozenSourceMeta {
        content_type: content_type.into(),
        source_id: source.into(),
        endpoint: format!("synthetic://{source}"),
        observed_at: OBSERVED.into(),
        terms_conclusion: "unresolved".into(),
        coverage: json!({"sessions": OPEN_DAYS.len()}),
        gaps: json!([]),
        ingested_by: "mvp6-corporate-actions-test".into(),
    }
}

fn calendar() -> Vec<u8> {
    let first = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
    let days: Vec<Value> = first
        .iter_days()
        .take_while(|day| chrono::Datelike::month(day) == 1)
        .map(|day| {
            let open = OPEN_DAYS.contains(&chrono::Datelike::day(&day));
            json!({"jyrq": day.to_string(), "jybz": if open { "1" } else { "0" }, "zrxh": 1})
        })
        .collect();
    serde_json::to_vec(&json!({"data": days, "nowdate": "2026-10-19"})).unwrap()
}

/// TDX `g4day` package with per-code close and exchange reference values.
fn tdx_package(day_key: &str, rows: &[(&'static str, f64, f64)]) -> Vec<u8> {
    let mut cod = Vec::new();
    let mut md1 = vec![0_u8; rows.len() * 512];
    for (index, (code, close, reference)) in rows.iter().enumerate() {
        let (close, reference) = (*close, *reference);
        let sequence = (rows.len() - 1 - index) as u16;
        let mut record = [0_u8; 150];
        record[..6].copy_from_slice(code.as_bytes());
        record[32..34].copy_from_slice(&sequence.to_le_bytes());
        cod.extend_from_slice(&record);
        let block = &mut md1[usize::from(sequence) * 512..][..512];
        for (offset, value) in [
            (4, reference),
            (12, close),
            (20, close + 0.01),
            (28, close - 0.01),
            (36, close),
        ] {
            block[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        block[56..64].copy_from_slice(&20_000_u64.to_le_bytes());
        block[72..80].copy_from_slice(&21_000.5_f64.to_le_bytes());
    }
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());
    for (name, bytes) in [
        (format!("sz{day_key}.cod"), cod),
        (format!("sz{day_key}.md1"), md1),
    ] {
        writer.start_file(name, options).unwrap();
        writer.write_all(&bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn records_for_day(day: usize) -> Vec<(&'static str, f64, f64)> {
    let (close_919, ref_919) = if day < 4 {
        (1.000, 1.000)
    } else {
        (0.930, 0.930)
    };
    let (close_560, ref_560) = if day < 6 {
        (1.000, 1.000)
    } else {
        (0.500, 0.500)
    };
    let (close_952, ref_952) = if day < 5 {
        (1.000, 1.000)
    } else {
        (0.950, 0.950)
    };
    vec![
        ("159919", close_919, ref_919),
        ("159560", close_560, ref_560),
        ("159612", 1.000, 1.000),
        ("159952", close_952, ref_952),
        ("159522", 1.000, 1.000),
        ("158001", 1.000, 1.000),
        // No Catalog identity: v6 must drop these priced Bars and count them.
        ("159999", 1.000, 1.000),
    ]
}

fn lifecycle(code: &str, listing: &str, termination: Option<&str>) -> Value {
    json!({
        "code": code,
        "org_id": format!("synthetic-org-{code}"),
        "legal_name": format!("合成ETF{code}"),
        "listing_date": listing,
        "termination_date": termination,
        "fund_category": "index",
        "tracked_index": "synthetic-index",
        "evidence_level": "official",
        "source_announcement_id": format!("identity-{code}"),
        "source_page": 1,
    })
}

#[allow(clippy::too_many_arguments)]
fn action(
    code: &str,
    kind: &str,
    announcement: &str,
    record: Option<&str>,
    ex: Option<&str>,
    pay: Option<&str>,
    cash: Option<&str>,
    split: Option<&str>,
    liquidation: Option<&str>,
) -> Value {
    json!({
        "code": code,
        "kind": kind,
        "announcement_date": announcement,
        "record_date": record,
        "ex_date": ex,
        "pay_date": pay,
        "cash_per_share": cash,
        "split_ratio": split,
        "liquidation_per_share": liquidation,
        "evidence_level": "official",
        "source_announcement_id": format!("action-{code}-{kind}"),
        "source_page": 1,
    })
}

fn facts_session(dsv: &str, listing_612: &str) -> Value {
    json!({
        "contract": "mvp-6-cninfo-facts-v1",
        "dsv": dsv,
        "session": {
            "catalog_lifecycles": [
                lifecycle("159919", "2026-01-05", None),
                lifecycle("159560", "2026-01-05", None),
                lifecycle("159612", listing_612, None),
                lifecycle("159952", "2026-01-05", None),
                lifecycle("159522", "2026-01-05", None),
                lifecycle("158001", "2026-01-05", Some("2026-01-09")),
                lifecycle("158001", "2026-01-12", None),
            ],
            "corporate_actions": {
                "schema": "cninfo_corporateActions_structured_v2",
                "version": 2,
                "actions": [
                    action(
                        "159919", "dividend", "2026-01-05", Some("2026-01-08"),
                        Some("2026-01-09"), Some("2026-01-12"), Some("0.07"),
                        None, None
                    ),
                    action(
                        "159560", "conversion", "2026-01-05", None,
                        Some("2026-01-13"), None, None, Some("2"), None
                    ),
                    action(
                        "159522", "dividend", "2026-01-05", None,
                        Some("2026-01-14"), None, None, None, None
                    ),
                ],
            },
            "risk_events": [],
        },
    })
}

struct Fixture {
    store: RawStore,
    v4: Manifest,
    facts_path: PathBuf,
}

fn fixture(root: &Path, listing_612: &str) -> Fixture {
    let store = RawStore::open(root).unwrap();
    let mut hashes = vec![
        prajna_data::freeze_raw(
            &store,
            &calendar(),
            meta("szse-calendar", "application/json"),
        )
        .unwrap(),
    ];
    for (index, day) in OPEN_DAYS.iter().enumerate() {
        let day_key = format!("2601{day:02}");
        hashes.push(
            prajna_data::freeze_raw(
                &store,
                &tdx_package(&day_key, &records_for_day(index)),
                meta("tdx", "application/zip"),
            )
            .unwrap(),
        );
    }
    let registry = NormalizerRegistry::with_builtins();
    let v4 = publish_dataset(
        root,
        &store,
        &registry,
        "szse-etf-frozen",
        "4",
        &hashes,
        OBSERVED,
    )
    .unwrap();
    fs::create_dir_all(root.join("acceptance")).unwrap();
    fs::write(
        root.join("acceptance/mvp6-coverage.json"),
        serde_json::to_vec(&json!({
            "dataset_published": true,
            "dsv": v4.dsv,
            "mvp6_complete": false,
        }))
        .unwrap(),
    )
    .unwrap();
    let facts_path = root.join("facts-session.json");
    fs::write(
        &facts_path,
        serde_json::to_vec_pretty(&facts_session(&v4.dsv, listing_612)).unwrap(),
    )
    .unwrap();
    Fixture {
        store,
        v4,
        facts_path,
    }
}

fn publish(root: &Path, fixture: &Fixture) -> CorporateActionPublish {
    publish_corporate_actions(
        root,
        &fixture.v4.dsv,
        &fixture.facts_path,
        OBSERVED,
        OBSERVED,
    )
    .unwrap()
}

fn etf<'a>(p21: &'a Value, symbol: &str) -> &'a Value {
    p21["etfs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|etf| etf["native_symbol"] == symbol)
        .unwrap_or_else(|| panic!("missing P21 report for {symbol}"))
}

fn mismatch_kinds(p21: &Value) -> Vec<String> {
    p21["mismatches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["kind"].as_str().unwrap().to_owned())
        .collect()
}

fn table_strings(root: &Path, dsv: &str, table: &str, column: usize) -> Vec<String> {
    use arrow_array::StringArray;
    let hex = dsv.strip_prefix("dsv:sha256:").unwrap();
    let path = root
        .join("normalized")
        .join(table)
        .join(hex)
        .join("part-00000.parquet");
    read_parquet(&path, table, "1")
        .unwrap()
        .iter()
        .flat_map(|batch| {
            batch
                .column(column)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .iter()
                .flatten()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn store_sina(store: &RawStore, symbol: &str, kind: &str, points: &[(&str, &str)]) -> String {
    let rows: Vec<Value> = points
        .iter()
        .map(|(date, value)| json!({"d": date, "u": value, "s": "1"}))
        .collect();
    let bytes = format!("var {kind} = {};", json!({"data": rows})).into_bytes();
    store
        .put(
            &bytes,
            SourceRecordInput {
                content_type: "application/javascript".into(),
                source_kind: SourceKind::VendorApi,
                source_id: "sina_adjust_factor".into(),
                request: json!({
                    "url": format!(
                        "https://finance.sina.com.cn/realstock/company/{symbol}/{kind}.js"
                    ),
                    "symbol": symbol,
                    "adjustment_kind": kind,
                    "http_status": 200,
                    "byte_len": bytes.len(),
                    "network_path": "direct_no_proxy",
                }),
                observed_at: OBSERVED.into(),
                ingested_by: "synthetic-test".into(),
            },
        )
        .unwrap()
}

#[test]
fn v6_p21_reports_every_outcome_and_rebuilds_identically() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path(), "2026-01-05");
    let published = publish(dir.path(), &fixture);
    let coverage = &published.manifest.provenance.coverage;
    let p21 = &coverage["p21"];

    assert_eq!(published.manifest.core.normalizer.version, "6");
    assert_ne!(published.manifest.dsv, fixture.v4.dsv);
    assert_eq!(coverage["corporate_actions"], 3);
    assert_eq!(coverage["sina_adjust_factor"]["status"], "not_present");
    assert_eq!(coverage["excluded_etfs"], json!([]));
    assert_eq!(coverage["tdx_out_of_scope_priced_bars"], 10);

    // The P21 result is merged into the local report without changing the v4
    // market DSV and without claiming MVP-6 completion.
    let local: Value = serde_json::from_slice(
        &fs::read(dir.path().join("acceptance/mvp6-coverage.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(local["dsv"], fixture.v4.dsv);
    assert_eq!(
        local["corporate_action_dataset"]["dsv"],
        published.manifest.dsv
    );
    assert_eq!(local["source_status"]["p21_crosscheck"], "source_gap");
    assert_eq!(local["source_status"]["p21_evaluated"], true);
    assert_eq!(
        local["source_status"]["corporate_actions"],
        "published_with_unknown"
    );
    assert_eq!(local["p21"]["status"], "source_gap");
    assert_eq!(local["p21_mismatches"].as_array().unwrap().len(), 3);
    assert_eq!(local["mvp6_complete"], false);

    // Dividend and conversion jumps are explained by the announced facts.
    let dividend = etf(p21, "159919");
    assert_eq!(dividend["status"], "evaluated");
    assert_eq!(dividend["explained_reference_jumps"], 1);
    assert_eq!(dividend["checked_no_event"], 8);
    assert_eq!(dividend["sessions_without_prior_close"], 1);
    let conversion = etf(p21, "159560");
    assert_eq!(conversion["status"], "evaluated");
    assert_eq!(conversion["explained_reference_jumps"], 1);
    assert_eq!(conversion["checked_no_event"], 8);

    // A clean ETF proves "checked, no event" is distinguishable from gaps.
    let clean = etf(p21, "159612");
    assert_eq!(clean["status"], "evaluated");
    assert_eq!(clean["checked_no_event"], 9);
    assert_eq!(clean["unknown_sessions"], json!([]));
    assert_eq!(clean["source_gap_reasons"], json!([]));

    // A jump without an announcement is UNKNOWN, not an error.
    let unexplained = etf(p21, "159952");
    assert_eq!(unexplained["status"], "evaluated_with_unknown");
    assert!(
        unexplained["unknown_sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|cell| cell["session_date"] == "2026-01-12"
                && cell["reason"] == "unexplained_reference_jump")
    );

    // An announced action without a jump and with a missing amount is UNKNOWN.
    let incomplete = etf(p21, "159522");
    assert_eq!(incomplete["status"], "source_gap");
    assert!(
        incomplete["unknown_sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|cell| cell["session_date"] == "2026-01-14"
                && cell["reason"] == "announced_without_reference_jump")
    );
    assert!(
        incomplete["actions"][0]["missing_fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "cash_per_share")
    );

    // Reused code: two lifecycles, no price/reference baseline crossing.
    let reused: Vec<_> = p21["etfs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|etf| etf["native_symbol"] == "158001")
        .collect();
    assert_eq!(reused.len(), 2);
    assert_eq!(reused[0]["status"], "evaluated");
    assert_eq!(reused[1]["status"], "evaluated");
    assert_eq!(reused[0]["lifecycle_start"], "2026-01-05");
    assert_eq!(reused[1]["lifecycle_start"], "2026-01-12");

    let kinds = mismatch_kinds(p21);
    for expected in [
        "unexplained_reference_jump",
        "announced_without_reference_jump",
        "incomplete_corporate_action",
    ] {
        assert!(kinds.iter().any(|kind| kind == expected), "{kinds:?}");
    }
    assert_eq!(p21["status"], "source_gap");
    assert!(!p21["unknown_sessions"].as_array().unwrap().is_empty());

    let missing = coverage["corporate_actions_with_missing_fields"]
        .as_array()
        .unwrap();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0]["announcement_id"], "action-159522-dividend");

    let instruments = table_strings(dir.path(), &published.manifest.dsv, "instruments", 0);
    assert_eq!(
        instruments,
        [
            "158001.SZSE",
            "159522.SZSE",
            "159560.SZSE",
            "159612.SZSE",
            "159919.SZSE",
            "159952.SZSE",
        ]
    );
    let action_kinds = table_strings(dir.path(), &published.manifest.dsv, "corporate_actions", 3);
    assert_eq!(action_kinds, ["dividend", "share_conversion", "dividend"]);

    // Two independent builds of the same inputs produce the same identity.
    let registry = NormalizerRegistry::with_corporate_actions_v6(fixture.v4.dsv.clone()).unwrap();
    assert_eq!(
        rebuild_dataset(&fixture.store, &registry, &published.manifest).unwrap(),
        published.manifest.dsv
    );
    let rebuilt = publish(dir.path(), &fixture);
    assert_eq!(rebuilt.manifest.dsv, published.manifest.dsv);
    assert_eq!(rebuilt.manifest.core, published.manifest.core);
}

#[test]
fn optional_sina_missing_pair_and_inconsistency_are_listed_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path(), "2026-01-05");
    let store = &fixture.store;
    let points = [
        ("2026-01-05", "0"),
        ("2026-01-09", "0.08"),
        ("2026-01-15", "0.09"),
    ];
    let qfq_919 = store_sina(store, "sz159919", "qfq", &points);
    let hfq_919 = store_sina(store, "sz159919", "hfq", &points);
    let qfq_560 = store_sina(store, "sz159560", "qfq", &[("2026-01-13", "0")]);
    fs::create_dir_all(dir.path().join("acceptance/sina")).unwrap();
    fs::write(
        dir.path().join("acceptance/sina/collection.json"),
        serde_json::to_vec(&json!({
            "candidate_results": [
                {
                    "state": "completed",
                    "symbol": "sz159919",
                    "qfq_raw_sha256": qfq_919,
                    "hfq_raw_sha256": hfq_919,
                },
                {
                    "state": "completed",
                    "symbol": "sz159560",
                    "qfq_raw_sha256": qfq_560,
                },
            ],
        }))
        .unwrap(),
    )
    .unwrap();
    let published = publish(dir.path(), &fixture);
    let coverage = &published.manifest.provenance.coverage;
    assert_eq!(coverage["sina_adjust_factor"]["status"], "inconsistent");
    let mismatches = coverage["sina_adjust_factor"]["mismatches"]
        .as_array()
        .unwrap();
    let kinds: Vec<_> = mismatches
        .iter()
        .map(|row| row["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"sina_cash_per_share_mismatch"));
    assert!(kinds.contains(&"sina_event_without_announced_action"));
    assert!(kinds.contains(&"sina_event_without_reference_jump"));
    assert!(
        coverage["sina_adjust_factor"]["source_gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap["native_symbol"] == "159560" && gap["reason"] == "qfq_hfq_pair_missing")
    );
    // Sina mismatch never blocks or rewrites the authoritative P21 result.
    assert_eq!(etf(&coverage["p21"], "159919")["status"], "evaluated");
    assert_eq!(
        etf(&coverage["p21"], "159919")["explained_reference_jumps"],
        1
    );
}

#[test]
fn failed_sina_collection_is_a_local_source_gap_not_absent() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path(), "2026-01-05");
    fs::create_dir_all(dir.path().join("acceptance/sina")).unwrap();
    fs::write(
        dir.path().join("acceptance/sina/collection.json"),
        serde_json::to_vec(&json!({
            "candidate_results": [{
                "state": "gap",
                "symbol": "sz159919",
                "reason": "deterministic not found (HTTP 404)",
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    let published = publish(dir.path(), &fixture);
    let local: Value = serde_json::from_slice(
        &fs::read(dir.path().join("acceptance/mvp6-coverage.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(local["sina_input_selection"]["status"], "source_gap");
    assert_eq!(local["sina_optional_crosscheck"]["status"], "source_gap");
    // The Dataset itself saw no Sina Raw input; the local report keeps the
    // attempted collection as a gap rather than calling it absent.
    assert_eq!(
        published.manifest.provenance.coverage["sina_adjust_factor"]["status"],
        "not_present"
    );
}

#[test]
fn malformed_sina_raw_is_a_non_fatal_source_gap() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path(), "2026-01-05");
    let bytes = b"var qfq = {};".to_vec();
    let hash = fixture
        .store
        .put(
            &bytes,
            SourceRecordInput {
                content_type: "application/javascript".into(),
                source_kind: SourceKind::VendorApi,
                source_id: "sina_adjust_factor".into(),
                request: json!({
                    "url": "https://example.com/not-sina/sz159919/qfq.js",
                    "symbol": "sz159919",
                    "adjustment_kind": "qfq",
                    "http_status": 200,
                    "byte_len": bytes.len(),
                    "network_path": "direct_no_proxy",
                }),
                observed_at: OBSERVED.into(),
                ingested_by: "synthetic-test".into(),
            },
        )
        .unwrap();
    fs::create_dir_all(dir.path().join("acceptance/sina")).unwrap();
    fs::write(
        dir.path().join("acceptance/sina/collection.json"),
        serde_json::to_vec(&json!({
            "candidate_results": [{
                "state": "completed",
                "symbol": "sz159919",
                "qfq_raw_sha256": hash,
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    let published = publish(dir.path(), &fixture);
    let sina = &published.manifest.provenance.coverage["sina_adjust_factor"];
    assert_eq!(sina["status"], "source_gap");
    assert!(
        sina["source_gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap["reason"] == "invalid_sina_adjustment_source")
    );
    // The authoritative announced-facts P21 result still publishes.
    assert_eq!(
        etf(&published.manifest.provenance.coverage["p21"], "159919")["status"],
        "evaluated"
    );
}

#[test]
fn reused_code_sina_reset_is_parsed_per_lifecycle_not_as_a_decrease() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path(), "2026-01-05");
    let store = &fixture.store;
    let points = [
        ("2026-01-05", "0"),
        ("2026-01-08", "0.02"),
        // A reused code's cumulative dividend resets at the second lifecycle.
        ("2026-01-12", "0"),
    ];
    let qfq = store_sina(store, "sz158001", "qfq", &points);
    let hfq = store_sina(store, "sz158001", "hfq", &points);
    fs::create_dir_all(dir.path().join("acceptance/sina")).unwrap();
    fs::write(
        dir.path().join("acceptance/sina/collection.json"),
        serde_json::to_vec(&json!({
            "candidate_results": [{
                "state": "completed",
                "symbol": "sz158001",
                "qfq_raw_sha256": qfq,
                "hfq_raw_sha256": hfq,
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    let published = publish(dir.path(), &fixture);
    let sina = &published.manifest.provenance.coverage["sina_adjust_factor"];
    assert!(
        !sina["source_gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap["reason"] == "sina_lifecycle_parse_failed")
    );
    let reused: Vec<_> = sina["etfs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|etf| etf["native_symbol"] == "158001")
        .collect();
    assert_eq!(reused.len(), 2);
    assert_eq!(reused[0]["events"], 1);
    assert_eq!(reused[1]["events"], 0);
}

#[test]
fn priced_bar_outside_catalog_lifecycle_excludes_only_that_etf() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path(), "2026-01-07");
    let published = publish(dir.path(), &fixture);
    let coverage = &published.manifest.provenance.coverage;
    let excluded = coverage["excluded_etfs"].as_array().unwrap();
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0]["native_symbol"], "159612");
    assert_eq!(
        excluded[0]["reason"],
        "priced_tdx_bar_outside_catalog_lifecycle"
    );
    let instruments = table_strings(dir.path(), &published.manifest.dsv, "instruments", 0);
    assert!(
        !instruments
            .iter()
            .any(|instrument| instrument == "159612.SZSE")
    );
    assert!(
        instruments
            .iter()
            .any(|instrument| instrument == "159919.SZSE")
    );
    assert!(
        !coverage["p21"]["etfs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|etf| etf["native_symbol"] == "159612")
    );
}

#[test]
fn v6_missing_tdx_reference_is_a_p21_source_gap_not_a_publish_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = RawStore::open(dir.path()).unwrap();
    let calendar_hash = prajna_data::freeze_raw(
        &store,
        &calendar(),
        meta("szse-calendar", "application/json"),
    )
    .unwrap();
    let tdx_hash = prajna_data::freeze_raw(
        &store,
        &tdx_package("260105", &[("159919", 1.000, 1.000)]),
        meta("tdx", "application/zip"),
    )
    .unwrap();
    let tdx_missing = prajna_data::freeze_raw(
        &store,
        &tdx_package("260106", &[("159919", 1.000, 0.0)]),
        meta("tdx", "application/zip"),
    )
    .unwrap();
    let dsv = "dsv:sha256:2222222222222222222222222222222222222222222222222222222222222222";
    let facts = serde_json::to_vec(&json!({
        "contract": "mvp-6-cninfo-facts-v1",
        "dsv": dsv,
        "session": {
            "catalog_lifecycles": [lifecycle("159919", "2026-01-05", None)],
            "corporate_actions": {
                "schema": "cninfo_corporateActions_structured_v2",
                "version": 2,
                "actions": [],
            },
            "risk_events": [],
        },
    }))
    .unwrap();
    let facts_hash = store
        .put(
            &facts,
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::FileImport,
                source_id: "cninfo_corporate_action_facts".into(),
                request: json!({}),
                observed_at: OBSERVED.into(),
                ingested_by: "synthetic-test".into(),
            },
        )
        .unwrap();
    let registry = NormalizerRegistry::with_corporate_actions_v6(dsv).unwrap();
    let manifest = publish_dataset(
        dir.path(),
        &store,
        &registry,
        "szse-etf-frozen",
        "6",
        &[calendar_hash, tdx_hash, tdx_missing, facts_hash],
        OBSERVED,
    )
    .unwrap();
    let p21 = &manifest.provenance.coverage["p21"];
    assert_eq!(p21["status"], "source_gap");
    assert!(
        p21["unknown_sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|cell| cell["session_date"] == "2026-01-06"
                && cell["reason"] == "missing_tdx_reference")
    );
}

#[test]
fn p21_announced_dividend_amount_must_match_the_reference_jump() {
    let dir = tempfile::tempdir().unwrap();
    let store = RawStore::open(dir.path()).unwrap();
    let calendar_hash = prajna_data::freeze_raw(
        &store,
        &calendar(),
        meta("szse-calendar", "application/json"),
    )
    .unwrap();
    let first = prajna_data::freeze_raw(
        &store,
        &tdx_package("260105", &[("159919", 1.000, 1.000)]),
        meta("tdx", "application/zip"),
    )
    .unwrap();
    // The reference jumps by 0.070 while the announcement says 0.080.
    let ex_date = prajna_data::freeze_raw(
        &store,
        &tdx_package("260106", &[("159919", 0.930, 0.930)]),
        meta("tdx", "application/zip"),
    )
    .unwrap();
    let dsv = "dsv:sha256:3333333333333333333333333333333333333333333333333333333333333333";
    let facts = serde_json::to_vec(&json!({
        "contract": "mvp-6-cninfo-facts-v1",
        "dsv": dsv,
        "session": {
            "catalog_lifecycles": [lifecycle("159919", "2026-01-05", None)],
            "corporate_actions": {
                "schema": "cninfo_corporateActions_structured_v2",
                "version": 2,
                "actions": [action(
                    "159919", "dividend", "2026-01-05", Some("2026-01-05"),
                    Some("2026-01-06"), Some("2026-01-08"), Some("0.08"),
                    None, None
                )],
            },
            "risk_events": [],
        },
    }))
    .unwrap();
    let facts_hash = store
        .put(
            &facts,
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::FileImport,
                source_id: "cninfo_corporate_action_facts".into(),
                request: json!({}),
                observed_at: OBSERVED.into(),
                ingested_by: "synthetic-test".into(),
            },
        )
        .unwrap();
    let registry = NormalizerRegistry::with_corporate_actions_v6(dsv).unwrap();
    let manifest = publish_dataset(
        dir.path(),
        &store,
        &registry,
        "szse-etf-frozen",
        "6",
        &[calendar_hash, first, ex_date, facts_hash],
        OBSERVED,
    )
    .unwrap();
    let p21 = &manifest.provenance.coverage["p21"];
    assert_eq!(p21["status"], "evaluated_with_unknown");
    let dividend = etf(p21, "159919");
    assert_eq!(dividend["status"], "evaluated_with_unknown");
    assert_eq!(
        dividend["actions"][0]["observed"],
        "announced_action_amount_mismatch"
    );
    assert!(mismatch_kinds(p21).contains(&"announced_action_amount_vs_reference_jump".to_owned()));
    assert!(
        p21["unknown_sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|cell| cell["reason"] == "announced_action_amount_mismatch")
    );
}

#[test]
fn facts_bound_to_a_different_market_dsv_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path(), "2026-01-05");
    let wrong = dir.path().join("wrong-facts.json");
    fs::write(
        &wrong,
        serde_json::to_vec(&facts_session(
            "dsv:sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "2026-01-05",
        ))
        .unwrap(),
    )
    .unwrap();
    assert!(
        publish_corporate_actions(dir.path(), &fixture.v4.dsv, &wrong, OBSERVED, OBSERVED).is_err()
    );
}

#[test]
fn v4_v5_and_v6_config_identity_is_pinned() {
    let v4 = SzseEtfFrozen::v4(vec![(158000, 159999)]).unwrap();
    let v5 = SzseEtfFrozen::v5(vec![prajna_data::LifecycleRange {
        native_symbol: "159919".into(),
        start: chrono::NaiveDate::from_ymd_opt(2023, 1, 3).unwrap(),
        end_exclusive: None,
    }])
    .unwrap();
    assert_eq!(v4.config_sha256(), V4_CONFIG_SHA256);
    assert_eq!(v5.config_sha256(), V5_CONFIG_SHA256);
    let v6 = SzseEtfFrozen::v6(
        "dsv:sha256:1111111111111111111111111111111111111111111111111111111111111111",
    )
    .unwrap();
    assert_eq!(v6.config_sha256(), V6_CONFIG_SHA256);
    assert_ne!(v6.config_sha256(), v4.config_sha256());
    assert_ne!(v6.config_sha256(), v5.config_sha256());
}

const V4_CONFIG_SHA256: &str =
    "sha256:25c164e97cc22476f70a5baa6e49bebec8c0da5e9015ee06710823ce657e59f1";
const V5_CONFIG_SHA256: &str =
    "sha256:a7890a031af8475da07c3a1f42e1ee693f8dc1b4ff55f8acee74b268e7be42f9";
const V6_CONFIG_SHA256: &str =
    "sha256:22c8ffebfcf05cbd09b9eaa06948e6bb3d2002bd1f9c6af45f4abf2c9dd95fab";
