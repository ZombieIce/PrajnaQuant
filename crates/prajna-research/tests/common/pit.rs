//! Manually verifiable, entirely synthetic vendor originals for #177.
use chrono::{Datelike, NaiveDate};
use prajna_data::{NormalizerRegistry, RawStore, SourceKind, SourceRecordInput, publish_dataset};
use prajna_domain::{InstrumentId, Session, TimestampNs};
use prajna_research::{
    etf_eligible::EligibilityParams,
    etf_market::{
        DerivedMarket, HaltEvidence, HaltScope, LimitRule, MarketEvidence, OpeningEvidence,
        ReferencePrice,
    },
    etf_momentum::{EmptyActionCoverage, TotalReturnIndex},
    pit_strategy::{RiskKind, RiskNotice, RiskSignals},
    pit_universe::*,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::Path,
};
use zip::{ZipWriter, write::SimpleFileOptions};

pub fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2023, 1, day).unwrap()
}
pub fn time(day: u32, hour: u32) -> TimestampNs {
    format!("2023-01-{day:02}T{hour:02}:00:00+08:00")
        .parse()
        .unwrap()
}
pub fn lake() -> tempfile::TempDir {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/pit-test-tmp");
    std::fs::create_dir_all(&path).unwrap();
    tempfile::tempdir_in(path).unwrap()
}
#[allow(dead_code)]
pub struct Fixture {
    pub dsv: String,
    pub catalog: HistoricalCatalog,
    pub market: DerivedMarket,
    pub tri: TotalReturnIndex,
    pub risk: RiskSignals,
    pub input: Value,
    pub ids: Vec<InstrumentId>,
}

pub fn fixture(root: &Path) -> Fixture {
    fixture_at_price(root, 1.0)
}

pub fn fixture_at_price(root: &Path, price: f64) -> Fixture {
    fixture_options(root, price, false)
}

pub fn fixture_options(root: &Path, price: f64, terminated: bool) -> Fixture {
    fixture_with_prices(root, price, terminated, &BTreeMap::new())
}

#[allow(dead_code)]
pub fn fixture_with_action_prices(
    root: &Path,
    overrides: BTreeMap<(usize, u32), (f64, f64)>,
) -> Fixture {
    fixture_with_prices(root, 1.0, false, &overrides)
}

fn fixture_with_prices(
    root: &Path,
    price: f64,
    terminated: bool,
    overrides: &BTreeMap<(usize, u32), (f64, f64)>,
) -> Fixture {
    let raw = RawStore::open(root).unwrap();
    let put = |bytes: &[u8]| {
        raw.put(
            bytes,
            SourceRecordInput {
                content_type: if bytes.starts_with(b"PK") {
                    "application/zip"
                } else {
                    "application/json"
                }
                .into(),
                source_kind: SourceKind::FileImport,
                source_id: "pit-3x10-synthetic".into(),
                request: json!({}),
                observed_at: "2026-10-15T00:00:00Z".into(),
                ingested_by: "pit-golden".into(),
            },
        )
        .unwrap()
    };
    let doc =
        put(b"synthetic complete official calendar/scans/contracts/risk notice; no real evidence");
    let evidence = |name: &str, at| Evidence {
        raw_sha256: doc.clone(),
        announcement_id: name.into(),
        locator: "synthetic p1".into(),
        publication: Publication::Exact { timestamp: at },
    };
    let sessions: Vec<_> = (1..=10)
        .map(|d| {
            Session::new(
                "SZSE".parse().unwrap(),
                date(d),
                format!("2023-01-{d:02}T09:30:00+08:00").parse().unwrap(),
                time(d, 15),
            )
            .unwrap()
        })
        .collect();
    let life = |code: &str, start, removal: Option<u32>| Lifecycle {
        native_symbol: code.into(),
        contract_raw_sha256: doc.clone(),
        listing: LifecycleFact {
            effective_date: date(start),
            evidence: evidence("listing", time(1, 8)),
        },
        termination: None,
        attributes: std::iter::once(AttributeFact {
            effective_date: date(start),
            category: FundCategory::Equity,
            index_id: "CSI:300".into(),
            evidence: evidence("contract", time(1, 8)),
            legal_name: None,
        })
        .chain(removal.map(|d| AttributeFact {
            effective_date: date(d),
            category: FundCategory::Other,
            index_id: "CSI:300".into(),
            evidence: evidence("category-change", time(d, 8)),
            legal_name: None,
        }))
        .collect(),
        identity_start: None,
        identity_end_exclusive: None,
        code_table_first_seen: None,
        code_table_last_seen: None,
        code_table_exit_date: None,
    };
    let mut catalog_input = CatalogInput {
        schema_version: 1,
        rule_version: "synthetic-catalog@1".into(),
        calendar_start: date(1),
        calendar_end: date(10),
        calendar_raw_sha256: BTreeSet::from([doc.clone()]),
        sessions: sessions.clone(),
        coverage: Coverage {
            start: date(1),
            end: date(10),
            scan_method: "synthetic exhaustive range".into(),
            announcement_method: "synthetic exhaustive listings/contracts/terminations attachments"
                .into(),
            announcement_raw_sha256: BTreeSet::from([doc.clone()]),
            gaps: BTreeSet::new(),
            resolutions: vec![],
        },
        lifecycles: vec![
            life("159001", 1, Some(5)),
            life("159002", 1, Some(7)),
            life("159003", 4, None),
        ],
        scans: (159000..=159999)
            .map(|code| {
                let seen = (159001..=159003).contains(&code);
                ScanObservation {
                    native_symbol: code.to_string(),
                    status: if seen {
                        ScanStatus::Observed
                    } else {
                        ScanStatus::NoRows
                    },
                    raw_sha256: doc.clone(),
                    first_observed: seen.then(|| date(if code == 159003 { 4 } else { 1 })),
                    last_observed: seen.then(|| date(10)),
                    identity_start: None,
                }
            })
            .collect(),
        broad_indices: Some(BroadIndexList {
            version: "synthetic-domestic@1".into(),
            index_ids: BTreeSet::from(["CSI:300".into()]),
            evidence: vec![evidence("scope", time(1, 8))],
            named_indices: vec![],
        }),
        code_ranges: None,
        code_tables: vec![],
    };
    if terminated {
        let life = &mut catalog_input.lifecycles[0];
        life.attributes.truncate(1);
        life.termination = Some(LifecycleFact {
            effective_date: date(8),
            evidence: evidence("formal-termination", time(8, 8)),
        });
        catalog_input
            .scans
            .iter_mut()
            .find(|s| s.native_symbol == "159001")
            .unwrap()
            .last_observed = Some(date(7));
    }
    let catalog = HistoricalCatalog::build(catalog_input.clone(), &raw).unwrap();
    let ids = catalog
        .input()
        .lifecycles
        .iter()
        .map(|l| l.instrument_id().unwrap())
        .collect::<Vec<_>>();
    let days = date(1)
        .iter_days()
        .take_while(|d| d.month() == 1)
        .map(|d| json!({"jyrq":d.to_string(),"jybz":if d.day() <= 10 {"1"} else {"0"}}))
        .collect::<Vec<_>>();
    let mut hashes = vec![put(&serde_json::to_vec(&json!({"data":days})).unwrap())];
    let open_price = |code: usize, day: u32| {
        if code == 0 && day == 7 {
            (price * 900.0).round() / 1000.0
        } else {
            price
        }
    };
    let bar_prices = |code: usize, day: u32| {
        overrides
            .get(&(code, day))
            .copied()
            .unwrap_or_else(|| (open_price(code, day), price))
    };
    for day in 1..=10 {
        let codes = if day < 4 { 2 } else { 3 };
        let mut cod = Vec::new();
        let mut md1 = Vec::new();
        for i in 0..codes {
            if terminated && i == 0 && day >= 8 {
                continue;
            }
            let mut record = [0u8; 150];
            record[..6].copy_from_slice(format!("15900{}", i + 1).as_bytes());
            record[32..34].copy_from_slice(&((cod.len() / 150) as u16).to_le_bytes());
            cod.extend_from_slice(&record);
            let mut block = [0u8; 512];
            let (open, close) = bar_prices(i, day);
            for (offset, v) in [
                (12, open),
                (20, open.max(close)),
                (28, open.min(close)),
                (36, close),
                (72, 20_000_000.0_f64),
            ] {
                block[offset..offset + 8].copy_from_slice(&v.to_le_bytes());
            }
            block[56..64].copy_from_slice(&20_000u64.to_le_bytes());
            md1.extend_from_slice(&block);
        }
        let mut zip = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().last_modified_time(zip::DateTime::default());
        for (name, data) in [
            (format!("sz2301{day:02}.cod"), cod),
            (format!("sz2301{day:02}.md1"), md1),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(&data).unwrap();
        }
        hashes.push(put(&zip.finish().unwrap().into_inner()));
    }
    for (i, _) in ids.iter().enumerate() {
        let rows = (if i == 2 { 4 } else { 1 }..=10)
            .filter(|d| !terminated || i != 0 || *d < 8)
            .map(|d| {
                let (open, close) = bar_prices(i, d);
                json!([
                    date(d).to_string(),
                    open.to_string(),
                    close.to_string(),
                    open.max(close).to_string(),
                    open.min(close).to_string(),
                    "200"
                ])
            })
            .collect::<Vec<_>>();
        let key = format!("sz15900{}", i + 1);
        hashes.push(put(&serde_json::to_vec(
            &json!({"code":0,"data":{key:{"day":rows}}}),
        )
        .unwrap()));
    }
    let dsv = publish_dataset(
        root,
        &raw,
        &NormalizerRegistry::with_builtins(),
        "szse-etf-frozen",
        "1",
        &hashes,
        "2026-10-15T00:00:00Z",
    )
    .unwrap()
    .dsv;
    let openings = ids
        .iter()
        .enumerate()
        .flat_map(|(i, id)| sessions.iter().map(move |s| (i, id, s)))
        .map(|(i, id, s)| {
            let day = s.session_date().day();
            let known = evidence("opening-coverage", time(1, 8));
            let reference_price = overrides.get(&(i, day)).map_or(price, |(open, _)| *open);
            OpeningEvidence {
                instrument_id: id.clone(),
                session_date: s.session_date(),
                announcement_coverage: Some(known.clone()),
                source_independence: if i == 0 && day == 6 {
                    None
                } else {
                    Some(known.clone())
                },
                halt: (i == 0 && day == 5).then(|| HaltEvidence {
                    scope: HaltScope::OpeningOnly,
                    evidence: evidence("halt", time(4, 16)),
                }),
                limit_rule: Some(LimitRule {
                    official_rule: None,
                    version: "synthetic-10pct@1".into(),
                    effective_from: date(1),
                    effective_until: None,
                    rate_bps: 1000,
                    tick: "0.001".parse().unwrap(),
                    rounding: "nearest_tick_half_up".into(),
                    applies_on_first_session: true,
                    first_session_reference: None,
                    evidence: known.clone(),
                    supporting_evidence: vec![],
                    rule_evidence: vec![],
                }),
                reference: Some(ReferencePrice {
                    adjusted_previous_close: reference_price.to_string().parse().unwrap(),
                    evidence: known,
                }),
                market_available_at: Some(s.ts_close()),
            }
        })
        .collect();
    let market_input = MarketEvidence {
        semantic_version: None,
        rule_version: "synthetic-execution@1".into(),
        start: date(1),
        end: date(10),
        coverage_gaps: BTreeSet::new(),
        openings,
    };
    let market = DerivedMarket::from_dataset(root, &dsv, &catalog, market_input.clone()).unwrap();
    let empty_actions = EmptyActionCoverage {
        version: "synthetic-empty-actions@1".into(),
        instruments: ids.iter().cloned().collect(),
        start: date(1),
        end: date(10),
        source_hashes: BTreeSet::from([doc.clone()]),
        available_at: Some(time(1, 8)),
    };
    let tri = TotalReturnIndex::build(&market, None, Some(&empty_actions), &raw).unwrap();
    let notices = vec![RiskNotice {
        instrument_id: ids[0].clone(),
        kind: RiskKind::TerminationProposal,
        evidence: evidence("termination-proposal", time(4, 15)),
    }];
    let risk = RiskSignals::build(&catalog, notices.clone(), &raw).unwrap();
    let eligibility = EligibilityParams {
        minimum_listing_sessions: 2,
        momentum_sessions: 1,
        amount_sessions: 1,
        minimum_average_amount: "10000000".parse().unwrap(),
    };
    let input = json!({"catalog":catalog_input,"market":market_input,"empty_actions":empty_actions,
        "risk_notices":notices,"eligibility":eligibility,"fixed_universe":null});
    Fixture {
        dsv,
        catalog,
        market,
        tri,
        risk,
        input,
        ids,
    }
}
