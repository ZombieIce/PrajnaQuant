//! Fully synthetic frozen inputs for production daily publisher tests.
use prajna_data::{
    EvidenceSource, FieldEvidence, RawStore, SecurityEvidenceRecord, SecurityEvidenceStore,
    SourceKind, SourceRecordInput, session_calendar::CalendarStore,
};
use prajna_domain::{InstrumentKind, Price, Quantity};
use serde_json::{Value, json};
use std::{fs, path::Path};

pub fn put(root: &Path, value: &Value) -> String {
    RawStore::open(root)
        .unwrap()
        .put(
            &serde_json::to_vec(value).unwrap(),
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::FileImport,
                source_id: "synthetic-opening-fixture".into(),
                request: json!({}),
                observed_at: "2026-10-28T00:00:00Z".into(),
                ingested_by: "synthetic-test".into(),
            },
        )
        .unwrap()
}

/// Explicit Session dates and `(open, close, high, low)` bars for a custom series.
pub type Series<'a> = (&'a [&'a str], &'a [(&'a str, &'a str, &'a str, &'a str)]);

pub fn prepare(
    root: &Path,
    venue: &str,
    kind: InstrumentKind,
    missing_second_bar: bool,
    series: Option<Series<'_>>,
) -> (Value, Value) {
    let symbol = if venue == "XSHG" { "510300" } else { "159952" };
    let instrument = format!("{symbol}.{venue}");
    let native = format!("{}{symbol}", if venue == "XSHG" { "sh" } else { "sz" });
    let default_dates = ["2026-01-05", "2026-01-06", "2026-01-07"];
    let default_bars = [("10", "10.1", "11", "9"); 3];
    let (dates, bars) = series.unwrap_or((&default_dates, &default_bars));
    assert_eq!(
        dates.len(),
        bars.len(),
        "custom series dates/bars must align"
    );
    let first = dates[0];
    let last = dates[dates.len() - 1];
    let calendar = json!({"schema_version":1, "venues": (["XSHG","XSHE"].map(|v| json!({
        "venue_id":v, "source":{"source_id":"synthetic", "source_version":"1", "source_ref":"fixture"},
        "coverage_start":first, "coverage_end":last, "sessions":dates.iter().map(|date| json!({
            "session_date":date, "ts_open":format!("{date}T09:30:00+08:00"),
            "ts_close":format!("{date}T15:00:00+08:00")})).collect::<Vec<_>>()})))});
    let calendar = CalendarStore::open(root)
        .unwrap()
        .import_json(&serde_json::to_vec(&calendar).unwrap())
        .unwrap();
    let source = EvidenceSource {
        source_id: "synthetic".into(),
        source_version: "1".into(),
        evidence_ref: "fixture".into(),
        observed_at: "2026-10-28T00:00:00Z".into(),
        effective_from: None,
        effective_to_exclusive: None,
    };
    let record = SecurityEvidenceRecord {
        instrument_id: instrument.parse().unwrap(),
        kind,
        established_date: FieldEvidence::Known {
            value: "2010-01-01".parse().unwrap(),
            source: source.clone(),
        },
        listing_date: FieldEvidence::Known {
            value: "2020-01-01".parse().unwrap(),
            source: source.clone(),
        },
        lot_size: FieldEvidence::Known {
            value: Quantity::parse("100").unwrap(),
            source: source.clone(),
        },
        price_increment: FieldEvidence::Known {
            value: Price::parse("0.001").unwrap(),
            source: source.clone(),
        },
        size_increment: FieldEvidence::Known {
            value: Quantity::parse("1").unwrap(),
            source,
        },
    };
    let store = SecurityEvidenceStore::open(root).unwrap();
    let evidence = store.import(&record).unwrap();
    let (security, _) = store
        .select_snapshot([(record.instrument_id, evidence)])
        .unwrap();
    let mut selected_dates = Vec::new();
    let mut rows = Vec::new();
    for (index, date) in dates.iter().enumerate() {
        if missing_second_bar && index == 1 {
            continue;
        }
        let (open, close, high, low) = bars[index];
        selected_dates.push(*date);
        rows.push(json!([date, open, close, high, low, "100"]));
    }
    let range = json!({"start":first,"end":last});
    let security_kind = if kind == InstrumentKind::Etf {
        "etf"
    } else {
        "stock"
    };
    let raw = RawStore::open(root)
        .unwrap()
        .put(
            &serde_json::to_vec(&json!({"code":0,"data":{&native:{"day":rows}}})).unwrap(),
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::VendorApi,
                source_id: "tencent_fqkline_daily".into(),
                observed_at: "2026-10-28T00:00:00Z".into(),
                ingested_by: "synthetic-test".into(),
                request: json!({"method_version":"tencent-fqkline-day/1",
            "instrument_id":instrument,"native_symbol":native,"security_kind":security_kind,
            "requested_range":range,"chunk_index":0,"source":"synthetic.invalid",
            "response_outcome":"accepted_response"}),
            },
        )
        .unwrap();
    let report = json!({"schema_version":1,"report_id":"synthetic", "source":"Tencent FQKline",
        "method_version":"tencent-fqkline-day/1","instrument_id":instrument,"native_symbol":native,
        "security_kind":security_kind,"requested_range":range,"status":"complete",
        "observed_at":"2026-10-28T00:00:00Z","observed_at_semantics":"local collection",
        "availability":"unknown","field_units":{},"instrument_metadata":{},"scope_statement":"synthetic only",
        "summary":{"chunks_requested":1,"chunks_attempted":1,"data_chunks":1,"accepted_response_chunks":1,
            "empty_chunks":0,"failed_chunks":0,"not_attempted_chunks":0,"candidate_http_attempts":1,
            "candidate_failures":0,"raw_object_references":1},
        "chunks":[{"index":0,"requested_range":range,"status":"complete","candidate_attempts":1,
            "candidate_failures":[],"raw_objects":[{"raw_sha256":raw,"source":"synthetic.invalid","outcome":"accepted_response"}],
            "actual_response_dates":selected_dates,"actual_response_range":range,"not_returned_calendar_dates":[],
            "outside_requested_range_dates":[],"duplicate_response_dates":[],"observed_at":null,"error":null}]});
    fs::write(
        root.join("report.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    let original = put(root, &json!({"synthetic":"explicit opening evidence"}));
    let snapshot = json!({"schema_version":1,"method_version":"cn-opening-evidence/1", "evidence_kind":"synthetic",
        "source_id":"synthetic-source", "source_version":"1", "extraction_version":"1",
        "calendar_version":calendar.as_str(),"security_snapshot":security.as_str(),"market_raw_sha256s":[raw],
        "instrument_id":instrument,"venue_id":venue,"start":first,"end":last,
        "observed_at":"2026-10-28T00:00:00Z", "evidence":[{"raw_sha256":original,"locator":"/synthetic",
            "available_at":"2026-01-04T00:00:00Z"}],
        "facts":(dates.iter().map(|date| json!({"session_date":date,
            "open_from":format!("{date}T09:30:00+08:00"),"open_to_exclusive":format!("{date}T09:31:00+08:00"),
            "state":"TRADABLE","available_at":format!("{date}T09:30:00+08:00"),
            "evidence_indices":[0],"reason":"explicit synthetic opening fact"})).collect::<Vec<_>>()) });
    let selected = json!({"raw_root":root,"calendar_root":root,"evidence_root":root,"report":root.join("report.json"),
        "normalizer_id":"cn-stock-etf-daily","normalizer_version":"2","calendar_version":calendar.as_str(),
        "security_snapshot":security.as_str(),"raw_sha256s":[raw],"status_snapshot":put(root,&snapshot)});
    (selected, snapshot)
}
