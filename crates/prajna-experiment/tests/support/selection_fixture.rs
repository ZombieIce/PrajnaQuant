//! Synthetic domestic data through the production Normalizer and publisher.
use chrono::NaiveDate;
use prajna_data::{
    CnStockEtfDaily, EvidenceSource, ExecutionStatusRow, FieldEvidence, NormalizationIssue,
    NormalizationOutput, Normalizer, NormalizerRegistry, RawInput, RawStore,
    SecurityEvidenceRecord, SecurityEvidenceStore, SourceKind, SourceRecordInput,
    bars_from_record_batch, execution_status_to_record_batch, publish_dataset,
    session_calendar::CalendarStore, sessions_from_record_batch,
};
use prajna_domain::{InstrumentKind, Price, Quantity};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

const DAYS: [&str; 3] = ["2026-01-05", "2026-01-06", "2026-01-07"];

pub fn prepare(root: &Path, known_listing: bool) -> String {
    prepare_days(root, known_listing, &DAYS)
}

#[allow(dead_code)]
pub fn prepare_days(root: &Path, known_listing: bool, days: &[&str]) -> String {
    publish(root, known_listing, days, None, false)
}

/// Same synthetic publication as `prepare_days`, with the test-only TRADABLE
/// execution-status table used by executable pool/rotation fixtures.
#[allow(dead_code)]
pub fn prepare_executable_days(root: &Path, known_listing: bool, days: &[&str]) -> String {
    publish(root, known_listing, days, None, true)
}

/// Test-only wrapper around the production v1 Normalizer adding a synthetic TRADABLE status
/// table for executable multi-Venue fixtures; the id stays `cn-stock-etf-daily` (T07) and the version is synthetic.
struct ExecutableStatusNormalizer {
    base: CnStockEtfDaily,
    config: String,
}

impl Normalizer for ExecutableStatusNormalizer {
    fn id(&self) -> &str {
        "cn-stock-etf-daily"
    }
    fn version(&self) -> &str {
        "synthetic-pool-executable@1"
    }
    fn config_sha256(&self) -> String {
        self.config.clone()
    }
    fn accepted_source_kinds(&self) -> &[SourceKind] {
        &[SourceKind::FileImport, SourceKind::VendorApi]
    }
    fn normalize(
        &self,
        raw_inputs: &[RawInput],
    ) -> Result<NormalizationOutput, Vec<NormalizationIssue>> {
        let reject = |error: String| {
            vec![NormalizationIssue {
                code: "invalid_synthetic_executable_fixture".into(),
                path: "$".into(),
                message: error,
            }]
        };
        let mut output = self.base.normalize(raw_inputs)?;
        let sessions = sessions_from_record_batch(&output.tables.sessions)
            .map_err(|error| reject(error.to_string()))?;
        let opens = sessions
            .iter()
            .map(|session| {
                (
                    (
                        session.venue_id().as_str().to_owned(),
                        session.session_date(),
                    ),
                    session.ts_open(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let bars = bars_from_record_batch(&output.tables.bars, &sessions)
            .map_err(|error| reject(error.to_string()))?;
        let rows = bars
            .iter()
            .map(|bar| ExecutionStatusRow {
                instrument_id: bar.instrument_id().clone(),
                session_date: bar.session_date(),
                trade_status: "TRADABLE".into(),
                is_tradable: true,
                available_at: opens
                    .get(&(
                        bar.instrument_id().venue().as_str().to_owned(),
                        bar.session_date(),
                    ))
                    .copied(),
                source: "synthetic-pool-executable@1".into(),
            })
            .collect::<Vec<_>>();
        output.tables.execution_status =
            Some(execution_status_to_record_batch(&rows).map_err(|e| reject(e.to_string()))?);
        output.coverage["execution_status"] = json!({
            "basis": "synthetic; every Bar TRADABLE at its Session open",
            "rows": rows.len(),
        });
        output.synthetic_assumptions.push(
            "synthetic execution status marks every Bar TRADABLE; not a halt, limit or real-source analysis".into(),
        );
        Ok(output)
    }
}

/// One synthetic Dataset through the production v1 Normalizer, optionally with
/// the test-only TRADABLE status table and an XSHE Session omitted from both the
/// calendar and the XSHE source rows (a structural shared-axis disagreement).
fn publish(
    root: &Path,
    known_listing: bool,
    days: &[&str],
    missing_xsse_session: Option<&str>,
    executable: bool,
) -> String {
    let venue_days = |venue: &str| {
        days.iter()
            .copied()
            .filter(|day| !(venue == "XSHE" && Some(*day) == missing_xsse_session))
            .collect::<Vec<_>>()
    };
    let calendar = json!({"schema_version":1,"venues": (["XSHG","XSHE"].map(|venue|json!({
        "venue_id":venue,"source":{"source_id":"synthetic","source_version":"1","source_ref":"fixture"},
        "coverage_start":days[0],"coverage_end":days[days.len()-1],"sessions":venue_days(venue).iter().map(|day|json!({
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
        let body = json!({"code":0,"data":{native:{"day":venue_days(venue).iter().map(|day|json!([day,"10","10","11","9","100"])).collect::<Vec<_>>()}}});
        hashes.push(put(&serde_json::to_vec(&body).unwrap(),"tencent_fqkline_daily",
            json!({"method_version":"tencent-fqkline-day/1","instrument_id":format!("{symbol}.{venue}")})));
    }
    let (registry, version) = if executable {
        let base = CnStockEtfDaily::v1(calendar_id, snapshot_id);
        let config = format!(
            "sha256:{:x}",
            Sha256::digest(
                serde_json::to_vec(&json!({
                    "base": base.config_sha256(),
                    "execution_status": "synthetic-pool-executable@1",
                }))
                .unwrap()
            )
        );
        let mut registry = NormalizerRegistry::new();
        assert!(registry.register(ExecutableStatusNormalizer { base, config }));
        (registry, "synthetic-pool-executable@1")
    } else {
        (
            NormalizerRegistry::with_cn_stock_etf_daily_v1(calendar_id, snapshot_id).unwrap(),
            "1",
        )
    };
    publish_dataset(
        root,
        &store,
        &registry,
        "cn-stock-etf-daily",
        version,
        &hashes,
        "2026-10-28T00:00:00Z",
    )
    .unwrap()
    .dsv
}

/// Executable multi-Venue Dataset plus full-coverage adjusted versions for every
/// security. `missing_xshe_session` creates the structural shared-axis mismatch.
#[allow(dead_code)]
pub fn prepare_executable_multi_venue(
    root: &Path,
    known_listing: bool,
    missing_xshe_session: Option<&str>,
) -> (String, BTreeMap<String, String>) {
    let dsv = publish(root, known_listing, &DAYS, missing_xshe_session, true);
    let xsse_days = DAYS
        .iter()
        .copied()
        .filter(|day| Some(*day) != missing_xshe_session)
        .collect::<Vec<_>>();
    let mut versions = BTreeMap::new();
    let all_days = DAYS.to_vec();
    for instrument in [
        "510300.XSHG",
        "510880.XSHG",
        "159952.XSHE",
        "600519.XSHG",
        "000001.XSHE",
    ] {
        let days = if instrument.ends_with(".XSHE") {
            &xsse_days
        } else {
            &all_days
        };
        let version = import_adjusted(root, &dsv, instrument, DAYS[0], DAYS[DAYS.len() - 1], days);
        versions.insert(instrument.to_string(), version);
    }
    (dsv, versions)
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
