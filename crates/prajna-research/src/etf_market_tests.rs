use super::*;
use crate::pit_universe::*;
use chrono::NaiveDate;
use prajna_data::{RawStore, SourceKind, SourceRecordInput};
use prajna_domain::{Session, TimestampNs};
use std::collections::BTreeSet;
fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2023, 1, 1).unwrap() + chrono::Duration::days(i64::from(day) - 1)
}
fn time(day: u32, hour: u32) -> TimestampNs {
    format!("{}T{hour:02}:00:00+08:00", date(day))
        .parse()
        .unwrap()
}
fn fixture(root: &std::path::Path) -> (RawStore, CatalogInput) {
    let raw = RawStore::open(root).unwrap();
    let h = raw
        .put(
            b"synthetic official attachment/calendar/scan; no market data",
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::FileImport,
                source_id: "synthetic".into(),
                request: serde_json::json!({}),
                observed_at: "2026-10-14T00:00:00Z".parse().unwrap(),
                ingested_by: "test".into(),
            },
        )
        .unwrap();
    let evidence = |id: &str| Evidence {
        raw_sha256: h.clone(),
        announcement_id: id.into(),
        locator: "synthetic p1".into(),
        publication: Publication::Exact {
            timestamp: time(1, 8),
        },
    };
    let life = |code: &str, start, end: Option<u32>| Lifecycle {
        native_symbol: code.into(),
        contract_raw_sha256: h.clone(),
        listing: LifecycleFact {
            effective_date: date(start),
            evidence: evidence("listing"),
        },
        termination: end.map(|end| LifecycleFact {
            effective_date: date(end),
            evidence: evidence("termination"),
        }),
        attributes: vec![AttributeFact {
            effective_date: date(start),
            category: FundCategory::Equity,
            index_id: "CSI:300".into(),
            evidence: evidence("contract"),
        }],
    };
    let input = CatalogInput {
        schema_version: 1, rule_version: "synthetic-rules@1".into(), calendar_start: date(1), calendar_end: date(10), calendar_raw_sha256: BTreeSet::from([h.clone()]),
        sessions: (1..=10).map(|d| Session::new("SZSE".parse().unwrap(), date(d), format!("2023-01-{d:02}T09:30:00+08:00").parse().unwrap(), time(d, 15)).unwrap()).collect(),
        coverage: Coverage { start: date(1), end: date(10), scan_method: "synthetic 159000..159999 full-window scan".into(), announcement_method: "synthetic full listings/contracts/terminations + attachments and pre-window identities".into(), announcement_raw_sha256: BTreeSet::from([h.clone()]), gaps: BTreeSet::new(), resolutions: vec![] },
        lifecycles: vec![life("159001", 1, None), life("159002", 1, None), life("159003", 1, None)],
        scans: (159000..=159999).map(|c| ScanObservation { native_symbol: c.to_string(), status: if (159001..=159003).contains(&c) { ScanStatus::Observed } else { ScanStatus::NoRows }, raw_sha256: h.clone(), first_observed: ((159001..=159003).contains(&c)).then(|| date(1)), last_observed: ((159001..=159003).contains(&c)).then(|| date(10)) }).collect(),
        broad_indices: BroadIndexList { version: "synthetic-domestic@1".into(), index_ids: BTreeSet::from(["CSI:300".into()]), evidence: vec![evidence("index-scope")] },
    };
    (raw, input)
}
use prajna_data::{
    NormalizedTables, SourceCrosscheckRow, bars_to_record_batch, source_crosscheck_to_record_batch,
};
use prajna_domain::{BarData, Notional, Price, Quantity};
fn data(catalog: &HistoricalCatalog) -> (NormalizedTables, MarketEvidence) {
    let mut bars = Vec::new();
    let mut checks = Vec::new();
    let mut openings = Vec::new();
    let input = catalog.input();
    let doc = input.lifecycles[0].listing.evidence.clone();
    for l in &input.lifecycles {
        for s in &input.sessions {
            let native: InstrumentId = format!("{}.SZSE", l.native_symbol).parse().unwrap();
            bars.push(
                Bar::new(
                    BarData {
                        instrument_id: native.clone(),
                        bar_spec: "1d@session".parse().unwrap(),
                        session_date: s.session_date(),
                        ts_open: s.ts_open(),
                        ts_close: s.ts_close(),
                        open: Price::parse("1").unwrap(),
                        high: Price::parse("1").unwrap(),
                        low: Price::parse("1").unwrap(),
                        close: Price::parse("1").unwrap(),
                        volume: Quantity::parse("20000").unwrap(),
                        amount: Some(Notional::parse("10000000").unwrap()),
                        available_at: Some(s.ts_close()),
                    },
                    Some(s),
                )
                .unwrap(),
            );
            checks.push(SourceCrosscheckRow {
                instrument_id: native,
                session_date: s.session_date(),
                status: "match".into(),
                mismatch_fields: String::new(),
                primary_raw_sha256: Some(format!("sha256:{}", "a".repeat(64))),
                secondary_raw_sha256: Some(format!("sha256:{}", "b".repeat(64))),
            });
            openings.push(OpeningEvidence {
                instrument_id: l.instrument_id().unwrap(),
                session_date: s.session_date(),
                announcement_coverage: Some(doc.clone()),
                source_independence: Some(doc.clone()),
                halt: None,
                limit_rule: Some(LimitRule {
                    version: "synthetic-rule@1".into(),
                    effective_from: date(1),
                    effective_until: None,
                    rate_bps: 1000,
                    tick: Price::parse("0.001").unwrap(),
                    rounding: "nearest_tick_half_up".into(),
                    applies_on_first_session: true,
                    evidence: doc.clone(),
                }),
                reference: Some(ReferencePrice {
                    adjusted_previous_close: Price::parse("1").unwrap(),
                    evidence: doc.clone(),
                }),
                market_available_at: Some(s.ts_close()),
            });
        }
    }
    let batch = bars_to_record_batch(&bars).unwrap();
    (
        NormalizedTables {
            instruments: batch.clone(),
            sessions: prajna_data::sessions_to_record_batch(&cat_sessions(catalog)).unwrap(),
            bars: batch,
            execution_status: None,
            source_crosscheck: Some(source_crosscheck_to_record_batch(&checks).unwrap()),
            announcements: None,
        },
        MarketEvidence {
            rule_version: "synthetic-opening@1".into(),
            start: date(1),
            end: date(10),
            coverage_gaps: BTreeSet::new(),
            openings,
        },
    )
}
fn build(
    catalog: &HistoricalCatalog,
    raw: &RawStore,
    tables: &NormalizedTables,
    e: MarketEvidence,
) -> DerivedMarket {
    DerivedMarket::build("dsv:synthetic", catalog, e, raw, tables).unwrap()
}
#[test]
fn opening_halt_with_afternoon_trades_is_halted_not_tradable() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let (tables, mut e) = data(&cat);
    let id = e.openings[0].instrument_id.clone();
    e.openings[0].halt = Some(HaltEvidence {
        scope: HaltScope::OpeningOnly,
        evidence: e.openings[0].announcement_coverage.clone().unwrap(),
    });
    let market = build(&cat, &raw, &tables, e.clone());
    let cell = &market.cells()[&(id.clone(), date(1))];
    assert_eq!(cell.state, TradeState::Halted);
    assert_eq!(cell.amount, Some(Notional::ZERO));
    assert!(!cell.permits(true));
    assert!(!cell.permits(false));
    e.openings[0].halt.as_mut().unwrap().scope = HaltScope::FullSession;
    assert_eq!(
        build(&cat, &raw, &tables, e).cells()[&(id, date(1))].state,
        TradeState::Unknown
    );
}
#[test]
fn directional_limits_are_exact_and_not_a_two_way_halt() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let (mut tables, e) = data(&cat);
    let id = e.openings[0].instrument_id.clone();
    let mut bars = bars_from_record_batch(&tables.bars, &cat.input().sessions).unwrap();
    for (index, p) in [(0, "1.1"), (1, "0.9")] {
        let old = &bars[index];
        let price = Price::parse(p).unwrap();
        bars[index] = Bar::new(
            BarData {
                instrument_id: old.instrument_id().clone(),
                bar_spec: old.bar_spec(),
                session_date: old.session_date(),
                ts_open: old.ts_open(),
                ts_close: old.ts_close(),
                open: price,
                high: price,
                low: price,
                close: price,
                volume: old.volume(),
                amount: old.amount(),
                available_at: old.available_at(),
            },
            Some(&cat.input().sessions[index]),
        )
        .unwrap();
    }
    tables.bars = bars_to_record_batch(&bars).unwrap();
    let market = build(&cat, &raw, &tables, e);
    let upper = &market.cells()[&(id.clone(), date(1))];
    let lower = &market.cells()[&(id, date(2))];
    assert_eq!(upper.state, TradeState::Tradable);
    assert!(!upper.permits(true));
    assert!(upper.permits(false));
    assert!(lower.permits(true));
    assert!(!lower.permits(false));
    assert_eq!(
        limit(
            Price::parse("1.005").unwrap(),
            Price::parse("0.001").unwrap(),
            11000
        )
        .unwrap()
        .to_string(),
        "1.106"
    );
}
#[test]
fn mismatches_missing_rows_and_unknown_rules_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let (mut tables, mut e) = data(&cat);
    let id = e.openings[0].instrument_id.clone();
    e.openings[0].limit_rule = None;
    let market = build(&cat, &raw, &tables, e.clone());
    assert_eq!(
        market.cells()[&(id.clone(), date(1))].state,
        TradeState::Unknown
    );
    let checks = tables.source_crosscheck.as_ref().unwrap();
    let mut statuses = vec!["match"; checks.num_rows()];
    statuses[1] = "mismatch";
    let mut columns = checks.columns().to_vec();
    columns[2] = std::sync::Arc::new(StringArray::from(statuses));
    tables.source_crosscheck = Some(RecordBatch::try_new(checks.schema(), columns).unwrap());
    assert_eq!(
        build(&cat, &raw, &tables, e.clone()).cells()[&(id.clone(), date(2))].state,
        TradeState::Unknown
    );
    let filtered = bars_from_record_batch(&tables.bars, &cat.input().sessions)
        .unwrap()
        .into_iter()
        .filter(|b| !(b.instrument_id().symbol() == "159001" && b.session_date() == date(3)))
        .collect::<Vec<_>>();
    tables.bars = bars_to_record_batch(&filtered).unwrap();
    assert_eq!(
        build(&cat, &raw, &tables, e).cells()[&(id, date(3))].state,
        TradeState::Unknown
    );
}
#[test]
fn entire_session_or_declared_coverage_gap_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let (mut tables, mut e) = data(&cat);
    e.coverage_gaps.insert("missing archive".into());
    assert!(DerivedMarket::build("dsv", &cat, e.clone(), &raw, &tables).is_err());
    e.coverage_gaps.clear();
    // Build a synthetic crosscheck table without the first Session.
    let mut cross = Vec::new();
    for l in &cat.input().lifecycles {
        for s in &cat.input().sessions[1..] {
            cross.push(SourceCrosscheckRow {
                instrument_id: format!("{}.SZSE", l.native_symbol).parse().unwrap(),
                session_date: s.session_date(),
                status: "match".into(),
                mismatch_fields: String::new(),
                primary_raw_sha256: Some("p".into()),
                secondary_raw_sha256: Some("s".into()),
            });
        }
    }
    tables.source_crosscheck = Some(source_crosscheck_to_record_batch(&cross).unwrap());
    assert!(
        DerivedMarket::build("dsv", &cat, e, &raw, &tables)
            .unwrap_err()
            .0
            .contains("whole-market missing")
    );
}
#[test]
fn availability_is_not_backdated_and_evidence_changes_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let (tables, mut e) = data(&cat);
    let id = e.openings[0].instrument_id.clone();
    let market = build(&cat, &raw, &tables, e.clone());
    let cell = &market.cells()[&(id.clone(), date(1))];
    assert!(!cell.known_by(time(1, 9)));
    assert!(cell.known_by(time(1, 15)));
    e.openings[0].market_available_at = Some(time(2, 16));
    let later = build(&cat, &raw, &tables, e.clone());
    assert!(!later.cells()[&(id.clone(), date(1))].known_by(time(1, 15)));
    assert_ne!(later.version(), market.version());
    e.openings[0].market_available_at = None;
    assert!(!build(&cat, &raw, &tables, e).cells()[&(id, date(1))].known_by(time(10, 15)));
}

fn cat_sessions(catalog: &HistoricalCatalog) -> Vec<Session> {
    catalog.input().sessions.clone()
}
use crate::etf_eligible::{EligibilityParams, EligibleSet};
use crate::etf_momentum::{EmptyActionCoverage, EtfMomentum, TotalReturnIndex};
fn tri(market: &DerivedMarket, cat: &HistoricalCatalog, raw: &RawStore) -> TotalReturnIndex {
    let input = cat.input();
    TotalReturnIndex::build(
        market,
        None,
        Some(&EmptyActionCoverage {
            version: "synthetic-empty-actions@1".into(),
            instruments: input
                .lifecycles
                .iter()
                .map(|l| l.instrument_id().unwrap())
                .collect(),
            start: input.coverage.start,
            end: input.coverage.end,
            source_hashes: input.calendar_raw_sha256.clone(),
            available_at: Some(time(1, 8)),
        }),
        raw,
    )
    .unwrap()
}
#[test]
fn full_momentum_window_halt_carry_unknown_gap_and_cache_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let (tables, mut e) = data(&cat);
    let ids = cat.universe(date(1)).unwrap().members;
    // First member's second day has afternoon trade, but open is officially halted.
    e.openings[1].halt = Some(HaltEvidence {
        scope: HaltScope::OpeningOnly,
        evidence: e.openings[1].announcement_coverage.clone().unwrap(),
    });
    e.openings[4].limit_rule = None;
    let market = build(&cat, &raw, &tables, e.clone());
    let index = tri(&market, &cat, &raw);
    let f = EtfMomentum::new(2).unwrap();
    let values = f.compute(&market, &index, &ids).unwrap();
    assert_eq!(index.point(&ids[0], date(2)).unwrap().level, 1.0);
    let statuses = values.column("status").unwrap().str().unwrap();
    assert_eq!(statuses.get(0), Some("insufficient_window"));
    assert_eq!(statuses.get(2), Some("ok"));
    assert_eq!(statuses.get(4), Some("missing_input"));
    assert_eq!(statuses.get(5), Some("missing_input"));
    assert_eq!(statuses.get(6), Some("missing_input"));
    assert_eq!(statuses.get(7), Some("ok"));
    assert_eq!(
        values.column("value").unwrap().f64().unwrap().get(2),
        Some(0.0)
    );
    let cache = crate::cache::store::FactorCache::open(tmp.path()).unwrap();
    assert_eq!(
        f.cached(&cache, &market, &index, &ids, "pit-v1").unwrap(),
        values
    );
    assert_eq!(
        f.cached(&cache, &market, &index, &ids, "pit-v1").unwrap(),
        values
    );
    assert!(cache.hit_count() > 0);
    let key = f.cache_key(&market, &index, "pit-v1", &ids).unwrap();
    assert_ne!(key, f.cache_key(&market, &index, "pit-v2", &ids).unwrap());
    assert_ne!(
        key,
        f.cache_key(&market, &index, "pit-v1", &ids[..1]).unwrap()
    );
    assert_ne!(
        key,
        EtfMomentum::new(3)
            .unwrap()
            .cache_key(&market, &index, "pit-v1", &ids)
            .unwrap()
    );
    e.openings[4].limit_rule = e.openings[3].limit_rule.clone();
    let revised = build(&cat, &raw, &tables, e);
    let revised_tri = tri(&revised, &cat, &raw);
    assert_ne!(
        key,
        f.cache_key(&revised, &revised_tri, "pit-v1", &ids).unwrap()
    );
    assert!(f.cached(&cache, &revised, &index, &ids, "pit-v1").is_err());
}
#[test]
fn eligibility_61_60_20_boundary_threshold_and_late_data() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, mut input) = fixture(tmp.path());
    input.calendar_end = date(62);
    input.coverage.end = date(62);
    input.sessions = (1..=62)
        .map(|d| {
            Session::new(
                "SZSE".parse().unwrap(),
                date(d),
                format!("{}T09:30:00+08:00", date(d)).parse().unwrap(),
                time(d, 15),
            )
            .unwrap()
        })
        .collect();
    for scan in &mut input.scans {
        if scan.status == ScanStatus::Observed {
            scan.last_observed = Some(date(62));
        }
    }
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let (tables, mut e) = data(&cat);
    e.end = date(62);
    let market = build(&cat, &raw, &tables, e.clone());
    let index = tri(&market, &cat, &raw);
    assert!(
        EligibleSet::build(
            &cat,
            &market,
            &index,
            date(60),
            EligibilityParams::default()
        )
        .unwrap()
        .members
        .is_empty()
    );
    let eligible = EligibleSet::build(
        &cat,
        &market,
        &index,
        date(61),
        EligibilityParams::default(),
    )
    .unwrap();
    assert_eq!(eligible.members.len(), 3);
    let changed = EligibleSet::build(
        &cat,
        &market,
        &index,
        date(61),
        EligibilityParams {
            minimum_average_amount: Notional::parse("10000000.001").unwrap(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(changed.members.is_empty());
    assert_ne!(eligible.version, changed.version);
    // HALTED amount is zero, so the equal-threshold historical window no longer qualifies.
    let at = e
        .openings
        .iter()
        .position(|o| o.instrument_id == eligible.members[0] && o.session_date == date(50))
        .unwrap();
    e.openings[at].halt = Some(HaltEvidence {
        scope: HaltScope::OpeningOnly,
        evidence: e.openings[at].announcement_coverage.clone().unwrap(),
    });
    let changed_market = build(&cat, &raw, &tables, e.clone());
    let changed_index = tri(&changed_market, &cat, &raw);
    let filtered = EligibleSet::build(
        &cat,
        &changed_market,
        &changed_index,
        date(61),
        EligibilityParams::default(),
    )
    .unwrap();
    assert_eq!(filtered.members.len(), 2);
    // The other member's current evidence arrives a day after its decision.
    let at = e
        .openings
        .iter()
        .position(|o| o.instrument_id == eligible.members[1] && o.session_date == date(61))
        .unwrap();
    e.openings[at].market_available_at = Some(time(62, 16));
    let late = build(&cat, &raw, &tables, e);
    let late_index = tri(&late, &cat, &raw);
    assert_eq!(
        EligibleSet::build(
            &cat,
            &late,
            &late_index,
            date(61),
            EligibilityParams::default()
        )
        .unwrap()
        .members
        .len(),
        1
    );
}
#[test]
fn unknown_sale_defers_entire_target_and_unknown_buy_is_excluded() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let (tables, mut e) = data(&cat);
    let ids = cat.universe(date(3)).unwrap().members;
    e.openings[2].limit_rule = None;
    let market = build(&cat, &raw, &tables, e);
    // A is held and must be sold to buy B: unknown A approves no legs.
    assert_eq!(
        market
            .rebalance_gate(date(3), &ids[..1], &ids[1..2])
            .unwrap(),
        RebalanceGate::Deferred {
            blocked_sells: BTreeSet::from([ids[0].clone()])
        }
    );
    // On a flat account UNKNOWN A cannot be a new buy; known B remains allowed.
    assert_eq!(
        market.rebalance_gate(date(3), &[], &ids[..2]).unwrap(),
        RebalanceGate::Permitted {
            allowed_buys: BTreeSet::from([ids[1].clone()])
        }
    );
    // Next covered open retries unchanged target after A's evidence is clear.
    assert!(matches!(
        market
            .rebalance_gate(date(4), &ids[..1], &ids[1..2])
            .unwrap(),
        RebalanceGate::Permitted { .. }
    ));
}
fn vendor_originals(raw: &RawStore) -> Vec<String> {
    use std::io::{Cursor, Write};
    use zip::{ZipWriter, write::SimpleFileOptions};
    let mut originals = Vec::new();
    let days: Vec<_> = (1..=31)
        .map(|d| serde_json::json!({"jyrq":date(d).to_string(),"jybz":if d<=10 {"1"} else {"0"}}))
        .collect();
    originals.push(serde_json::to_vec(&serde_json::json!({"data":days})).unwrap());
    for day in 1..=10 {
        let mut cod = Vec::new();
        let mut md1 = vec![0_u8; 3 * 512];
        for (i, code) in ["159001", "159002", "159003"].iter().enumerate() {
            let mut record = [0_u8; 150];
            record[..6].copy_from_slice(code.as_bytes());
            record[32..34].copy_from_slice(&(i as u16).to_le_bytes());
            cod.extend_from_slice(&record);
            let block = &mut md1[i * 512..][..512];
            for off in [12, 20, 28, 36] {
                block[off..off + 8].copy_from_slice(&1.0_f64.to_le_bytes());
            }
            block[56..64].copy_from_slice(&20000_u64.to_le_bytes());
            block[72..80].copy_from_slice(&10000000_f64.to_le_bytes());
        }
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().last_modified_time(zip::DateTime::default());
        for (name, bytes) in [
            (format!("sz2301{day:02}.cod"), cod),
            (format!("sz2301{day:02}.md1"), md1),
        ] {
            writer.start_file(name, options).unwrap();
            writer.write_all(&bytes).unwrap();
        }
        originals.push(writer.finish().unwrap().into_inner());
    }
    let rows: Vec<_> = (1..=10)
        .map(|d| serde_json::json!([date(d).to_string(), "1", "1", "1", "1", "200"]))
        .collect();
    originals.push(serde_json::to_vec(&serde_json::json!({"code":0,"data":{"sz159001":{"day":rows},"sz159002":{"day":rows},"sz159003":{"day":rows}}})).unwrap());
    originals
        .into_iter()
        .map(|bytes| {
            raw.put(
                &bytes,
                SourceRecordInput {
                    content_type: "application/octet-stream".into(),
                    source_kind: SourceKind::FileImport,
                    source_id: "synthetic vendor".into(),
                    request: serde_json::json!({}),
                    observed_at: time(10, 16).to_string(),
                    ingested_by: "test".into(),
                },
            )
            .unwrap()
        })
        .collect()
}
#[test]
fn rebuild_frozen_dataset_then_derive_replays_and_detects_raw_tampering() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let (_, e) = data(&cat);
    let hashes = vendor_originals(&raw);
    let manifest = prajna_data::publish_dataset(
        tmp.path(),
        &raw,
        &NormalizerRegistry::with_builtins(),
        "szse-etf-frozen",
        "1",
        &hashes,
        "2026-10-15T00:00:00Z",
    )
    .unwrap();
    let first = DerivedMarket::from_dataset(tmp.path(), &manifest.dsv, &cat, e.clone()).unwrap();
    let second = DerivedMarket::from_dataset(tmp.path(), &manifest.dsv, &cat, e.clone()).unwrap();
    assert_eq!(first.version(), second.version());
    assert_eq!(first.cells().len(), 30);
    assert!(
        first
            .cells()
            .values()
            .all(|c| c.state == TradeState::Tradable)
    );
    let hash = hashes.last().unwrap();
    let hex = hash.strip_prefix("sha256:").unwrap();
    std::fs::write(
        tmp.path().join("raw/sha256").join(&hex[..2]).join(hex),
        b"corrupt original",
    )
    .unwrap();
    assert!(DerivedMarket::from_dataset(tmp.path(), &manifest.dsv, &cat, e).is_err());
}
#[test]
fn provider_levels_are_used_and_actual_points_participate_in_cache_identity() {
    use crate::etf_momentum::{IndexPoint, TotalReturnProvider};
    struct Provider(f64);
    impl TotalReturnProvider for Provider {
        fn identity(&self) -> serde_json::Value {
            serde_json::json!({"provider":"synthetic@1"})
        }
        fn level(&self, _: &InstrumentId, d: NaiveDate) -> Option<IndexPoint> {
            Some(IndexPoint {
                level: 100.0 + self.0 * (d - date(1)).num_days() as f64,
                available_at: Some(time(10, 15)),
            })
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let (tables, e) = data(&cat);
    let market = build(&cat, &raw, &tables, e);
    let ids = cat.universe(date(3)).unwrap().members;
    let a = TotalReturnIndex::build(&market, Some(&Provider(1.0)), None, &raw).unwrap();
    let b = TotalReturnIndex::build(&market, Some(&Provider(2.0)), None, &raw).unwrap();
    let f = EtfMomentum::new(2).unwrap();
    let values = f.compute(&market, &a, &ids).unwrap();
    assert!(
        (values
            .column("value")
            .unwrap()
            .f64()
            .unwrap()
            .get(2)
            .unwrap()
            - 0.02)
            .abs()
            < 1e-12
    );
    assert_ne!(a.version(), b.version());
    assert_ne!(
        f.cache_key(&market, &a, "uni", &ids).unwrap(),
        f.cache_key(&market, &b, "uni", &ids).unwrap()
    );
    assert!(TotalReturnIndex::build(&market, None, None, &raw).is_err());
}
