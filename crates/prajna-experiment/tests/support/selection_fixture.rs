//! Synthetic domestic data through the production Normalizer and publisher.
use chrono::NaiveDate;
use prajna_data::{
    EvidenceSource, FieldEvidence, NormalizerRegistry, RawStore, SecurityEvidenceRecord,
    SecurityEvidenceStore, SourceKind, SourceRecordInput, publish_dataset,
    session_calendar::CalendarStore,
};
use prajna_domain::{InstrumentKind, Price, Quantity};
use serde_json::{Value, json};
use std::path::Path;

pub fn prepare(root: &Path, known_listing: bool) -> String {
    prepare_days(
        root,
        known_listing,
        &["2026-01-05", "2026-01-06", "2026-01-07"],
    )
}

#[allow(dead_code)]
pub fn prepare_days(root: &Path, known_listing: bool, days: &[&str]) -> String {
    let calendar = json!({"schema_version":1,"venues": (["XSHG","XSHE"].map(|venue|json!({
        "venue_id":venue,"source":{"source_id":"synthetic","source_version":"1","source_ref":"fixture"},
        "coverage_start":days[0],"coverage_end":days[days.len()-1],"sessions":days.iter().map(|day|json!({
            "session_date":day,"ts_open":format!("{day}T09:30:00+08:00"),"ts_close":format!("{day}T15:00:00+08:00")
        })).collect::<Vec<_>>()
    })))});
    let calendars = CalendarStore::open(root).unwrap();
    let calendar_id = calendars
        .import_json(&serde_json::to_vec(&calendar).unwrap())
        .unwrap();
    let evidence = SecurityEvidenceStore::open(root).unwrap();
    let records = [
        ("510300", "XSHG", InstrumentKind::Etf),
        ("510880", "XSHG", InstrumentKind::Etf),
        ("159952", "XSHE", InstrumentKind::Etf),
        ("600519", "XSHG", InstrumentKind::Equity),
        ("000001", "XSHE", InstrumentKind::Equity),
    ];
    let source = EvidenceSource {
        source_id: "synthetic".into(),
        source_version: "1".into(),
        evidence_ref: "fixture".into(),
        observed_at: "2026-10-28T00:00:00Z".into(),
        effective_from: None,
        effective_to_exclusive: None,
    };
    let mut selections = Vec::new();
    for (symbol, venue, kind) in records {
        let record = SecurityEvidenceRecord {
            instrument_id: format!("{symbol}.{venue}").parse().unwrap(),
            kind,
            established_date: FieldEvidence::Known {
                value: NaiveDate::from_ymd_opt(2010, 1, 1).unwrap(),
                source: source.clone(),
            },
            listing_date: if known_listing {
                FieldEvidence::Known {
                    value: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(),
                    source: source.clone(),
                }
            } else {
                FieldEvidence::Unknown {
                    reason: "synthetic unknown".into(),
                    source: source.clone(),
                }
            },
            lot_size: FieldEvidence::Known {
                value: Quantity::parse(if symbol == "510880" { "10" } else { "100" }).unwrap(),
                source: source.clone(),
            },
            price_increment: FieldEvidence::Known {
                value: Price::parse(if symbol == "510880" { "0.01" } else { "0.001" }).unwrap(),
                source: source.clone(),
            },
            size_increment: FieldEvidence::Known {
                value: Quantity::parse("1").unwrap(),
                source: source.clone(),
            },
        };
        let version = evidence.import(&record).unwrap();
        selections.push((record.instrument_id, version));
    }
    let (snapshot_id, snapshot) = evidence.select_snapshot(selections).unwrap();
    let store = RawStore::open(root).unwrap();
    let put = |bytes: &[u8], source_id: &str, request: Value| {
        store
            .put(
                bytes,
                SourceRecordInput {
                    content_type: "application/json".into(),
                    source_kind: SourceKind::VendorApi,
                    source_id: source_id.into(),
                    request,
                    observed_at: "synthetic".into(),
                    ingested_by: "selection-fixture".into(),
                },
            )
            .unwrap()
    };
    let mut hashes = vec![
        put(
            &serde_json::to_vec(&calendars.get(&calendar_id).unwrap()).unwrap(),
            "synthetic-calendar",
            json!({}),
        ),
        put(
            &serde_json::to_vec(&snapshot).unwrap(),
            "synthetic-evidence",
            json!({}),
        ),
    ];
    for (symbol, venue, _) in records {
        let native = format!("{}{symbol}", if venue == "XSHG" { "sh" } else { "sz" });
        let body = json!({"code":0,"data":{native:{"day":days.iter().map(|day|json!([day,"10","10","11","9","100"])).collect::<Vec<_>>()}}});
        hashes.push(put(&serde_json::to_vec(&body).unwrap(),"tencent_fqkline_daily",
            json!({"method_version":"tencent-fqkline-day/1","instrument_id":format!("{symbol}.{venue}")})));
    }
    let registry =
        NormalizerRegistry::with_cn_stock_etf_daily_v1(calendar_id, snapshot_id).unwrap();
    publish_dataset(
        root,
        &store,
        &registry,
        "cn-stock-etf-daily",
        "1",
        &hashes,
        "2026-10-28T00:00:00Z",
    )
    .unwrap()
    .dsv
}

#[allow(dead_code)]
/// Imports one explicit synthetic adjusted research series for a published DSV.
/// `declared` is the supplier range; `dates` is the actual row coverage.
pub fn import_adjusted(
    root: &Path,
    dsv: &str,
    instrument: &str,
    declared_start: &str,
    declared_end: &str,
    dates: &[&str],
) -> String {
    let rows = dates
        .iter()
        .map(|date| {
            json!({
                "trade_date": date,
                "open": "10",
                "high": "11",
                "low": "9",
                "close": "10"
            })
        })
        .collect::<Vec<_>>();
    let input = json!({
        "schema_version": 1,
        "raw_dsv": dsv,
        "instrument_id": instrument,
        "method": "forward",
        "method_version": "synthetic-research@1",
        "source": {"source_id":"synthetic", "source_version":"1", "source_ref":"fixture"},
        "observed_at": "2026-10-28T00:00:00Z",
        "retrospective_assumption": "Synthetic supplier history may be revised; not strict PIT",
        "close_availability_assumption": "Synthetic daily close assumed known after the Session close",
        "start": declared_start,
        "end": declared_end,
        "rows": rows
    });
    prajna_data::adjusted_series::import(root, &serde_json::to_vec(&input).unwrap()).unwrap()
}

#[allow(dead_code)]
/// Publishes the synthetic Dataset and binds a full-coverage adjusted version to
/// every member, returning the DSV and the canonical per-member versions.
pub fn prepare_with_adjusted(
    root: &Path,
    known_listing: bool,
) -> (String, std::collections::BTreeMap<String, String>) {
    let dsv = prepare(root, known_listing);
    let mut versions = std::collections::BTreeMap::new();
    for instrument in [
        "510300.XSHG",
        "510880.XSHG",
        "159952.XSHE",
        "600519.XSHG",
        "000001.XSHE",
    ] {
        let version = import_adjusted(
            root,
            &dsv,
            instrument,
            "2026-01-05",
            "2026-01-07",
            &["2026-01-05", "2026-01-06", "2026-01-07"],
        );
        versions.insert(instrument.to_string(), version);
    }
    (dsv, versions)
}

#[allow(dead_code)]
/// Explicit synthetic adjusted closes, independent from the raw trading prices.
pub fn import_closes(root: &Path, dsv: &str, dates: &[&str], closes: &[&str]) -> String {
    assert_eq!(dates.len(), closes.len());
    let input = json!({"schema_version":1,"raw_dsv":dsv,"instrument_id":"510300.XSHG",
        "method":"forward","method_version":"synthetic-timing@1",
        "source":{"source_id":"synthetic","source_version":"1","source_ref":"timing-fixture"},
        "observed_at":"2026-10-28T00:00:00Z","retrospective_assumption":"Synthetic retrospective history, not strict PIT",
        "close_availability_assumption":"Session close assumed available at close",
        "start":dates[0],"end":dates[dates.len()-1],
        "rows":dates.iter().zip(closes).map(|(day, close)|json!({"trade_date":day,"open":close,"high":close,"low":close,"close":close})).collect::<Vec<_>>()});
    prajna_data::adjusted_series::import(root, &serde_json::to_vec(&input).unwrap()).unwrap()
}
