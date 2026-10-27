//! Synthetic `cn-stock-etf-daily` v1 Datasets for multi-Venue panel tests.
//!
//! The fixtures use the real #330 Normalizer and the normal Dataset publisher;
//! they are not a second publication path. Raw bytes, source details and
//! identities stay in the temporary test lake.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use prajna_data::{
    EvidenceSource, FieldEvidence, NormalizerRegistry, RawStore, SecurityEvidenceRecord,
    SecurityEvidenceStore, SourceKind, SourceRecordInput, publish_dataset,
    session_calendar::CalendarStore,
};
use prajna_domain::{InstrumentId, InstrumentKind, Price, Quantity, VenueId};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const CREATED_AT: &str = "2026-10-27T00:00:00Z";

pub fn date(value: &str) -> NaiveDate {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
}

fn sha256_id(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
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

fn record(symbol: &str, venue: &str, kind: InstrumentKind) -> SecurityEvidenceRecord {
    let price_increment = match kind {
        InstrumentKind::Equity => "0.01",
        InstrumentKind::Etf => "0.001",
        _ => panic!("the panel fixture only covers stocks and ETFs"),
    };
    SecurityEvidenceRecord {
        instrument_id: InstrumentId::new(symbol, VenueId::new(venue).unwrap()).unwrap(),
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
            value: Price::parse(price_increment).unwrap(),
            source: evidence_source(),
        },
        size_increment: FieldEvidence::Known {
            value: Quantity::parse("1").unwrap(),
            source: evidence_source(),
        },
    }
}

pub fn shanghai_shenzhen_records() -> Vec<SecurityEvidenceRecord> {
    vec![
        record("600519", "XSHG", InstrumentKind::Equity),
        record("510300", "XSHG", InstrumentKind::Etf),
        record("000001", "XSHE", InstrumentKind::Equity),
        record("159952", "XSHE", InstrumentKind::Etf),
    ]
}

#[derive(Clone)]
pub struct CalendarVenue {
    pub venue: &'static str,
    pub dates: Vec<&'static str>,
    pub open: &'static str,
    pub close: &'static str,
}

/// Both Venues publish the same dates and open/close instants.
pub fn matched_calendar(dates: &[&'static str]) -> Vec<CalendarVenue> {
    vec![
        CalendarVenue {
            venue: "XSHG",
            dates: dates.to_vec(),
            open: "09:30:00",
            close: "15:00:00",
        },
        CalendarVenue {
            venue: "XSHE",
            dates: dates.to_vec(),
            open: "09:30:00",
            close: "15:00:00",
        },
    ]
}

fn calendar_document(venues: &[CalendarVenue]) -> Value {
    json!({
        "schema_version": 1,
        "venues": venues
            .iter()
            .map(|venue| {
                let source = format!("synthetic-{}-calendar", venue.venue.to_lowercase());
                let sessions = venue
                    .dates
                    .iter()
                    .map(|day| {
                        json!({
                            "session_date": day,
                            "ts_open": format!("{day}T{}+08:00", venue.open),
                            "ts_close": format!("{day}T{}+08:00", venue.close),
                        })
                    })
                    .collect::<Vec<_>>();
                json!({
                    "venue_id": venue.venue,
                    "source": {
                        "source_id": source,
                        "source_version": "1",
                        "source_ref": "synthetic-fixture",
                    },
                    "coverage_start": venue.dates.first().expect("calendar fixture dates"),
                    "coverage_end": venue.dates.last().expect("calendar fixture dates"),
                    "sessions": sessions,
                })
            })
            .collect::<Vec<_>>(),
    })
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
                ingested_by: "prajna-research-panel-test".into(),
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
            "instrument_id": instrument_id.to_string(),
        }),
    )
}

pub struct SyntheticDailyDataset {
    _temp: tempfile::TempDir,
    root: PathBuf,
    pub dsv: String,
    pub ids: Vec<InstrumentId>,
}

impl SyntheticDailyDataset {
    /// Publishes one Dataset through `publish_dataset` using the pinned
    /// calendar/snapshot identities and the real `cn-stock-etf-daily` v1 rules.
    ///
    /// `missing_bars` lists `(symbol, date)` pairs whose daily row is omitted;
    /// those Sessions still need a Bar to be present in the panel.
    pub fn publish(
        venue_calendars: &[CalendarVenue],
        records: Vec<SecurityEvidenceRecord>,
        missing_bars: &[(&str, &str)],
    ) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let store = RawStore::open(&root).unwrap();

        let calendars = CalendarStore::open(&root).unwrap();
        let calendar_version = calendars
            .import_json(&serde_json::to_vec(&calendar_document(venue_calendars)).unwrap())
            .unwrap();
        let calendar_bytes =
            serde_json::to_vec(&calendars.get(&calendar_version).unwrap()).unwrap();
        assert_eq!(sha256_id(&calendar_bytes), calendar_version.as_str());

        let evidence = SecurityEvidenceStore::open(&root).unwrap();
        let selections = records
            .iter()
            .map(|record| {
                let version = evidence.import(record).unwrap();
                (record.instrument_id.clone(), version)
            })
            .collect::<Vec<_>>();
        let (snapshot_id, snapshot) = evidence.select_snapshot(selections).unwrap();
        let snapshot_bytes = serde_json::to_vec(&snapshot).unwrap();
        assert_eq!(sha256_id(&snapshot_bytes), snapshot_id.as_str());

        let mut hashes = vec![
            put(
                &store,
                &calendar_bytes,
                SourceKind::FileImport,
                "synthetic-calendar",
                json!({}),
            ),
            put(
                &store,
                &snapshot_bytes,
                SourceKind::FileImport,
                "synthetic-security-snapshot",
                json!({}),
            ),
        ];

        for record in &records {
            let venue = venue_calendars
                .iter()
                .find(|calendar| calendar.venue == record.instrument_id.venue().as_str())
                .expect("record Venue has a calendar fixture");
            let native = format!(
                "{}{}",
                if record.instrument_id.venue().as_str() == "XSHG" {
                    "sh"
                } else {
                    "sz"
                },
                record.instrument_id.symbol()
            );
            let rows = venue
                .dates
                .iter()
                .filter(|day| {
                    !missing_bars.iter().any(|(symbol, missing)| {
                        *symbol == record.instrument_id.symbol() && *missing == **day
                    })
                })
                .map(|day| json!([day, "10.00", "10.10", "10.20", "9.90", "100"]))
                .collect::<Vec<_>>();
            let response = serde_json::to_vec(&json!({
                "code": 0,
                "data": {native: {"day": rows}},
            }))
            .unwrap();
            hashes.push(put_daily(&store, &response, &record.instrument_id));
        }

        let registry =
            NormalizerRegistry::with_cn_stock_etf_daily_v1(calendar_version, snapshot_id).unwrap();
        let dsv = publish_dataset(
            &root,
            &store,
            &registry,
            "cn-stock-etf-daily",
            "1",
            &hashes,
            CREATED_AT,
        )
        .unwrap()
        .dsv;
        let ids = records
            .iter()
            .map(|record| record.instrument_id.clone())
            .collect();
        Self {
            _temp: temp,
            root,
            dsv,
            ids,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}
