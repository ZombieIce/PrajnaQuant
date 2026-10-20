use super::*;
use crate::pit_universe::*;
use chrono::NaiveDate;
use prajna_data::{
    CorporateActionKind, CorporateActionRow, RawStore, SourceKind, SourceRecordInput,
    bars_from_record_batch, bars_to_record_batch,
};
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
            legal_name: None,
        }],
        identity_start: None,
        identity_end_exclusive: None,
        identity_available_from: None,
        code_table_first_seen: None,
        code_table_last_seen: None,
        code_table_exit_date: None,
    };
    let input = CatalogInput {
        schema_version: 1, rule_version: "synthetic-rules@1".into(), calendar_start: date(1), calendar_end: date(10), calendar_raw_sha256: BTreeSet::from([h.clone()]),
        sessions: (1..=10).map(|d| Session::new("SZSE".parse().unwrap(), date(d), format!("2023-01-{d:02}T09:30:00+08:00").parse().unwrap(), time(d, 15)).unwrap()).collect(),
        coverage: Coverage { start: date(1), end: date(10), scan_method: "synthetic 159000..159999 full-window scan".into(), announcement_method: "synthetic full listings/contracts/terminations + attachments and pre-window identities".into(), announcement_raw_sha256: BTreeSet::from([h.clone()]), gaps: BTreeSet::new(), resolutions: vec![] },
        lifecycles: vec![life("159001", 1, None), life("159002", 1, None), life("159003", 1, None)],
        scans: (159000..=159999).map(|c| ScanObservation { native_symbol: c.to_string(), status: if (159001..=159003).contains(&c) { ScanStatus::Observed } else { ScanStatus::NoRows }, raw_sha256: h.clone(), first_observed: ((159001..=159003).contains(&c)).then(|| date(1)), last_observed: ((159001..=159003).contains(&c)).then(|| date(10)), identity_start: None }).collect(),
        broad_indices: Some(BroadIndexList { version: "synthetic-domestic@1".into(), index_ids: BTreeSet::from(["CSI:300".into()]), evidence: vec![evidence("index-scope")], named_indices: vec![] }),
        code_ranges: None,
        code_tables: vec![],
    };
    (raw, input)
}
use prajna_data::{NormalizedTables, SourceCrosscheckRow, source_crosscheck_to_record_batch};
use prajna_domain::{BarData, Notional, Price, Quantity};
fn data(catalog: &HistoricalCatalog) -> (NormalizedTables, MarketEvidence) {
    let mut bars = Vec::new();
    let mut checks = Vec::new();
    let mut openings = Vec::new();
    let input = catalog.input();
    let doc = input.lifecycles[0].listing.evidence.clone();
    for l in &input.lifecycles {
        for s in &input.sessions {
            if input.schema_version == 2
                && catalog
                    .resolve_row(&l.native_symbol, s.session_date())
                    .as_ref()
                    != l.instrument_id().as_ref()
            {
                continue;
            }
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
                    first_session_reference: None,
                    evidence: doc.clone(),
                    supporting_evidence: vec![],
                    rule_evidence: vec![],
                    official_rule: None,
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
            bar_sources: None,
            instruments: batch.clone(),
            sessions: prajna_data::sessions_to_record_batch(&cat_sessions(catalog)).unwrap(),
            bars: batch,
            execution_status: None,
            source_crosscheck: Some(source_crosscheck_to_record_batch(&checks).unwrap()),
            announcements: None,
            corporate_actions: None,
            tdx_code_table: None,
        },
        MarketEvidence {
            semantic_version: None,
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
    let unknown_time = build(&cat, &raw, &tables, e.clone());
    assert!(!unknown_time.cells()[&(id.clone(), date(1))].permits(true));
    assert!(!unknown_time.cells()[&(id.clone(), date(1))].can_buy);

    assert!(!build(&cat, &raw, &tables, e).cells()[&(id, date(1))].known_by(time(10, 15)));
}

#[test]
fn date_only_membership_evidence_before_truncated_calendar_is_not_backdated_to_open() {
    let tmp = tempfile::tempdir().unwrap();
    let (_raw, mut input) = fixture(tmp.path());
    let table = limit_rules::FundLimitRuleTable::official_v1().unwrap();
    let instrument_id: InstrumentId = "159001.SZSE".parse().unwrap();
    let mut assignment = limit_rules::FundCategoryAssignment {
        instrument_id: instrument_id.clone(),
        fund_category: "szse-fund-default-10pct".into(),
        effective_from: date(1),
        effective_until: None,
        document_number: "SYNTHETIC-CATEGORY-LIST".into(),
        evidence: input.broad_indices.as_ref().unwrap().evidence[0].clone(),
        evidence_level: limit_rules::EvidenceLevel::Primary,
    };
    assignment.evidence.announcement_id = assignment.document_number.clone();
    assignment.evidence.locator = "synthetic category-list row".into();
    assignment.evidence.publication = Publication::DateOnly {
        date: NaiveDate::from_ymd_opt(2022, 12, 31).unwrap(),
    };
    let rule = table
        .rule_for_with_assignments(&instrument_id, date(1), &[assignment])
        .unwrap()
        .unwrap();

    // The publication date predates this truncated calendar, so availability is unknown.
    input.calendar_start = date(1);
    assert!(
        table
            .evidence_available_at(&rule, &instrument_id, &rule.evidence, date(1), &input)
            .is_err()
    );

    // With the publication date covered, the date-only fact becomes available at the
    // first following session's close, never before that session's open.
    input.calendar_start = NaiveDate::from_ymd_opt(2022, 12, 31).unwrap();
    let available_at = table
        .evidence_available_at(&rule, &instrument_id, &rule.evidence, date(1), &input)
        .unwrap();
    assert_eq!(available_at, time(1, 15));
    assert!(available_at > time(1, 9));
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

fn action(
    id: &InstrumentId,
    raw_hash: &str,
    kind: CorporateActionKind,
    announced_day: u32,
    action_day: u32,
    cash: Option<&str>,
    ratio: Option<&str>,
) -> CorporateActionRow {
    CorporateActionRow {
        announcement_id: format!("synthetic-action-{announced_day}-{action_day}"),
        action_index: 0,
        instrument_id: id.clone(),
        kind,
        announced_date: date(announced_day),
        announced_at: time(announced_day, 0),
        available_at: time(announced_day + 1, 15),
        record_date: (kind == CorporateActionKind::Dividend).then(|| date(action_day - 1)),
        ex_date: (kind == CorporateActionKind::Dividend).then(|| date(action_day)),
        pay_date: (kind == CorporateActionKind::Dividend).then(|| date(action_day + 2)),
        effective_date: (kind != CorporateActionKind::Dividend).then(|| date(action_day)),
        cash_per_share: cash.map(str::to_owned),
        share_ratio: ratio.map(str::to_owned),
        liquidation_cash_per_share: None,
        source_raw_sha256: raw_hash.to_owned(),
        normalization_version: "szse-corporate-action@1".into(),
    }
}

fn set_close(
    catalog: &HistoricalCatalog,
    tables: &mut NormalizedTables,
    instrument_id: &InstrumentId,
    session: u32,
    close: &str,
) {
    let native_symbol = catalog
        .input()
        .lifecycles
        .iter()
        .find(|lifecycle| lifecycle.instrument_id().ok().as_ref() == Some(instrument_id))
        .unwrap()
        .native_symbol
        .clone();
    let id: InstrumentId = format!("{native_symbol}.SZSE").parse().unwrap();
    let mut bars = bars_from_record_batch(&tables.bars, &catalog.input().sessions).unwrap();
    let bar = bars
        .iter_mut()
        .find(|bar| bar.instrument_id() == &id && bar.session_date() == date(session))
        .unwrap();
    let px = Price::parse(close).unwrap();
    *bar = Bar::new(
        BarData {
            instrument_id: bar.instrument_id().clone(),
            bar_spec: bar.bar_spec(),
            session_date: bar.session_date(),
            ts_open: bar.ts_open(),
            ts_close: bar.ts_close(),
            open: px,
            high: px,
            low: px,
            close: px,
            volume: bar.volume(),
            amount: bar.amount(),
            available_at: bar.available_at(),
        },
        Some(&catalog.input().sessions[(session - 1) as usize]),
    )
    .unwrap();
    tables.bars = bars_to_record_batch(&bars).unwrap();
}

#[test]
fn corporate_action_tri_handles_dividend_split_late_notice_and_identity() {
    use crate::etf_momentum::{CorporateActionRules, EtfMomentum};
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let (mut tables, evidence) = data(&cat);
    let dividend_id = cat.universe(date(1)).unwrap().members[0].clone();
    set_close(&cat, &mut tables, &dividend_id, 4, "0.9");
    let market = build(&cat, &raw, &tables, evidence);
    let no_actions = TotalReturnIndex::build_with_actions(
        &market,
        &[],
        &CorporateActionRules {
            version: "tri-rules@1".into(),
            unexplained_jump_threshold_bps: 2000,
        },
        market.dsv(),
        &raw,
    )
    .unwrap();
    let six_a = tri(&market, &cat, &raw);
    for session in market.sessions() {
        assert_eq!(
            no_actions
                .point(&dividend_id, session.session_date())
                .unwrap()
                .level,
            six_a
                .point(&dividend_id, session.session_date())
                .unwrap()
                .level
        );
    }
    let ids = cat.universe(date(1)).unwrap().members;
    let momentum = EtfMomentum::new(1).unwrap();
    let price_momentum = momentum.compute(&market, &six_a, &ids).unwrap();
    let no_action_momentum = momentum.compute(&market, &no_actions, &ids).unwrap();
    let price_values = price_momentum.column("value").unwrap().f64().unwrap();
    let no_action_values = no_action_momentum.column("value").unwrap().f64().unwrap();
    let price_status = price_momentum.column("status").unwrap().str().unwrap();
    let no_action_status = no_action_momentum.column("status").unwrap().str().unwrap();
    for row in 0..price_values.len() {
        assert_eq!(price_values.get(row), no_action_values.get(row));
        assert_eq!(price_status.get(row), no_action_status.get(row));
    }
    let dividend = action(
        &dividend_id,
        cat.input().calendar_raw_sha256.iter().next().unwrap(),
        CorporateActionKind::Dividend,
        4,
        4,
        Some("0.1"),
        None,
    );
    let rules = CorporateActionRules {
        version: "tri-rules@1".into(),
        unexplained_jump_threshold_bps: 2000,
    };
    let dividend_tri =
        TotalReturnIndex::build_with_actions(&market, &[dividend], &rules, market.dsv(), &raw)
            .unwrap();
    assert_eq!(
        dividend_tri.point(&dividend_id, date(3)).unwrap().level,
        six_a.point(&dividend_id, date(3)).unwrap().level
    );
    assert!((dividend_tri.point(&dividend_id, date(4)).unwrap().level - 1.0).abs() < 1e-12);
    assert_eq!(
        dividend_tri
            .point(&dividend_id, date(4))
            .unwrap()
            .available_at,
        Some(time(5, 15))
    );
    let ids = cat.universe(date(1)).unwrap().members;
    let momentum = EtfMomentum::new(1).unwrap();
    assert_ne!(
        momentum.cache_key(&market, &six_a, "u", &ids).unwrap(),
        momentum
            .cache_key(&market, &dividend_tri, "u", &ids)
            .unwrap()
    );
    let other_rules = CorporateActionRules {
        version: "tri-rules@2".into(),
        unexplained_jump_threshold_bps: 2000,
    };
    let revised =
        TotalReturnIndex::build_with_actions(&market, &[], &other_rules, market.dsv(), &raw)
            .unwrap();
    assert_ne!(no_actions.version(), revised.version());
}

#[test]
fn conversion_adjusts_tri_and_unexplained_jump_is_reported_unknown() {
    use crate::etf_momentum::CorporateActionRules;
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let (mut tables, mut evidence) = data(&cat);
    let id = cat.universe(date(1)).unwrap().members[0].clone();
    set_close(&cat, &mut tables, &id, 4, "0.5");
    evidence
        .openings
        .iter_mut()
        .find(|opening| opening.instrument_id == id && opening.session_date == date(4))
        .unwrap()
        .reference
        .as_mut()
        .unwrap()
        .adjusted_previous_close = Price::parse("0.5").unwrap();
    let market = build(&cat, &raw, &tables, evidence);
    let raw_hash = cat.input().calendar_raw_sha256.iter().next().unwrap();
    let rules = CorporateActionRules {
        version: "tri-rules@1".into(),
        unexplained_jump_threshold_bps: 2000,
    };
    let split = action(
        &id,
        raw_hash,
        CorporateActionKind::ShareConversion,
        4,
        4,
        None,
        Some("2"),
    );
    let split_tri = TotalReturnIndex::build_with_actions(
        &market,
        std::slice::from_ref(&split),
        &rules,
        market.dsv(),
        &raw,
    )
    .unwrap();
    assert!((split_tri.point(&id, date(4)).unwrap().level - 1.0).abs() < 1e-12);
    let unexplained_by_action = action(
        &id,
        raw_hash,
        CorporateActionKind::ShareConversion,
        4,
        4,
        None,
        Some("3"),
    );
    let still_unexplained = TotalReturnIndex::build_with_actions(
        &market,
        &[unexplained_by_action],
        &rules,
        market.dsv(),
        &raw,
    )
    .unwrap();
    assert_eq!(
        still_unexplained.integrity()[0].unexplained_price_jumps,
        vec![date(4)]
    );
    assert!(still_unexplained.point(&id, date(4)).is_none());
    let first_day_action = action(
        &id,
        raw_hash,
        CorporateActionKind::ShareConversion,
        1,
        1,
        None,
        Some("2"),
    );
    let first_day_tri = TotalReturnIndex::build_with_actions(
        &market,
        &[first_day_action],
        &rules,
        market.dsv(),
        &raw,
    )
    .unwrap();
    assert_eq!(
        first_day_tri.integrity()[0].unapplied_action_dates,
        vec![date(1)]
    );
    let unexplained =
        TotalReturnIndex::build_with_actions(&market, &[], &rules, market.dsv(), &raw).unwrap();
    assert_eq!(
        unexplained.integrity()[0].unexplained_price_jumps,
        vec![date(4)]
    );
    assert!(unexplained.point(&id, date(4)).is_none());
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
    assert_eq!(cache.compute_count(), 1);
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
    let fixed = crate::pit_universe::FixedUniverseSpec::new(
        "synthetic-fixed@1",
        ["159001", "159002", "159003"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
    .unwrap();
    assert!(
        EligibleSet::build_with_fixed_universe(
            &cat,
            &market,
            &index,
            date(60),
            EligibilityParams::default(),
            Some(&fixed),
        )
        .unwrap()
        .members
        .is_empty()
    );
    assert_eq!(
        EligibleSet::build_with_fixed_universe(
            &cat,
            &market,
            &index,
            date(61),
            EligibilityParams::default(),
            Some(&fixed),
        )
        .unwrap()
        .members
        .len(),
        3
    );
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
    // A fallback-like Bar with unknown amount cannot count as zero, shorten
    // the 20-Session window to 19, or pass even a zero liquidity threshold.
    let mut missing_tables = tables.clone();
    let mut bars = bars_from_record_batch(&tables.bars, &cat.input().sessions).unwrap();
    let native = &cat
        .input()
        .lifecycles
        .iter()
        .find(|l| l.instrument_id().unwrap() == eligible.members[0])
        .unwrap()
        .native_symbol;
    let bar = bars
        .iter_mut()
        .find(|b| b.instrument_id().symbol() == native && b.session_date() == date(50))
        .unwrap();
    *bar = Bar::new(
        BarData {
            instrument_id: bar.instrument_id().clone(),
            bar_spec: bar.bar_spec(),
            session_date: bar.session_date(),
            ts_open: bar.ts_open(),
            ts_close: bar.ts_close(),
            open: bar.open(),
            high: bar.high(),
            low: bar.low(),
            close: bar.close(),
            volume: bar.volume(),
            amount: None,
            available_at: bar.available_at(),
        },
        Some(&cat.input().sessions[49]),
    )
    .unwrap();
    missing_tables.bars = bars_to_record_batch(&bars).unwrap();
    let missing_market = build(&cat, &raw, &missing_tables, e.clone());
    let missing_tri = tri(&missing_market, &cat, &raw);
    let missing = EligibleSet::build(
        &cat,
        &missing_market,
        &missing_tri,
        date(61),
        EligibilityParams {
            minimum_average_amount: Notional::ZERO,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(missing.members.len(), 2);
    assert!(missing.excluded[&eligible.members[0]].contains("amount_missing_input"));

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
    vendor_originals_edited(raw, |_, _, _| true, |_, _| true)
}
fn vendor_originals_edited(
    raw: &RawStore,
    edit_tdx: impl Fn(u32, usize, &mut [u8]) -> bool,
    include_tencent: impl Fn(u32, usize) -> bool,
) -> Vec<String> {
    use std::io::{Cursor, Write};
    use zip::{ZipWriter, write::SimpleFileOptions};
    let mut originals = Vec::new();
    let days: Vec<_> = (1..=31)
        .map(|d| serde_json::json!({"jyrq":date(d).to_string(),"jybz":if d<=10 {"1"} else {"0"}}))
        .collect();
    originals.push(serde_json::to_vec(&serde_json::json!({"data":days})).unwrap());
    for day in 1..=10 {
        let mut cod = Vec::new();
        let mut md1 = Vec::new();
        for (i, code) in ["159001", "159002", "159003"].iter().enumerate() {
            let mut record = [0_u8; 150];
            record[..6].copy_from_slice(code.as_bytes());
            record[32..34].copy_from_slice(&((cod.len() / 150) as u16).to_le_bytes());
            let mut block = [0_u8; 512];
            for off in [4, 12, 20, 28, 36] {
                block[off..off + 8].copy_from_slice(&1.0_f64.to_le_bytes());
            }
            block[56..64].copy_from_slice(&20000_u64.to_le_bytes());
            block[72..80].copy_from_slice(&10000000_f64.to_le_bytes());
            if edit_tdx(day, i, &mut block) {
                cod.extend_from_slice(&record);
                md1.extend_from_slice(&block);
            }
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
    let mut quotes = serde_json::Map::new();
    for (i, code) in ["sz159001", "sz159002", "sz159003"].iter().enumerate() {
        let rows: Vec<_> = (1..=10)
            .filter(|d| include_tencent(*d, i))
            .map(|d| serde_json::json!([date(d).to_string(), "1", "1", "1", "1", "200"]))
            .collect();
        quotes.insert((*code).into(), serde_json::json!({"day": rows}));
    }
    originals.push(serde_json::to_vec(&serde_json::json!({"code":0,"data":quotes})).unwrap());
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
    let legacy_json = serde_json::to_value(&e).unwrap();
    assert!(legacy_json.get("semantic_version").is_none());
    assert!(
        legacy_json["openings"][0]["limit_rule"]
            .get("official_rule")
            .is_none()
    );
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
    let missing_secondary = prajna_data::publish_dataset(
        tmp.path(),
        &raw,
        &NormalizerRegistry::with_builtins(),
        "szse-etf-frozen",
        "1",
        &hashes[..hashes.len() - 1],
        "2026-10-15T00:00:00Z",
    )
    .unwrap();
    assert!(
        DerivedMarket::from_dataset(tmp.path(), &missing_secondary.dsv, &cat, e.clone())
            .unwrap_err()
            .0
            .contains("whole-market missing")
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
fn v2_evidence(catalog: &HistoricalCatalog) -> MarketEvidence {
    let (_, evidence) = data(catalog);
    let mut value = serde_json::to_value(evidence).unwrap();
    value["semantic_version"] = serde_json::json!("szse-derived-open@2");
    for opening in value["openings"].as_array_mut().unwrap() {
        opening["source_independence"] = serde_json::Value::Null;
        opening["reference"] = serde_json::Value::Null;
        let mut category_evidence = opening["announcement_coverage"].clone();
        category_evidence["announcement_id"] = serde_json::json!("SYNTHETIC-CATEGORY-LIST");
        category_evidence["locator"] = serde_json::json!("synthetic category-list row");
        let category_assignment = limit_rules::FundCategoryAssignment {
            instrument_id: serde_json::from_value(opening["instrument_id"].clone()).unwrap(),
            fund_category: "szse-fund-default-10pct".into(),
            effective_from: NaiveDate::from_ymd_opt(2022, 10, 1).unwrap(),
            effective_until: None,
            document_number: "SYNTHETIC-CATEGORY-LIST".into(),
            evidence: serde_json::from_value(category_evidence.clone()).unwrap(),
            evidence_level: limit_rules::EvidenceLevel::Primary,
        };
        opening["limit_rule"]["version"] =
            serde_json::json!("szse-fund-limit-rules@1/fund-default-10pct-2021");
        opening["limit_rule"]["effective_from"] = serde_json::json!("2021-04-06");
        opening["limit_rule"]["effective_until"] = serde_json::json!("2023-04-10");
        opening["limit_rule"]["first_session_reference"] =
            serde_json::json!("previous_trading_day_net_asset_value");
        let table = limit_rules::FundLimitRuleTable::official_v1().unwrap();
        opening["limit_rule"]["rule_evidence"] =
            serde_json::to_value([table.records[0].evidence.clone()]).unwrap();
        opening["limit_rule"]["evidence"] = category_evidence.clone();
        opening["limit_rule"]["official_rule"] = serde_json::json!({
            "document_number": "深证上〔2021〕337号",
            "fund_category": "szse-fund-default-10pct",
            "category_evidence": {
                "document_number": "SYNTHETIC-CATEGORY-LIST",
                "instrument_id": opening["instrument_id"].clone(),
                "fund_category": "szse-fund-default-10pct",
                "assignment_identity": category_assignment.identity().unwrap(),
                "effective_from": "2022-10-01",
                "effective_until": null,
                "evidence": category_evidence,
                "evidence_level": "primary"
            }
        });
    }
    serde_json::from_value(value).unwrap()
}

fn refresh_assignment_identity(category_evidence: &mut FundCategoryEvidence) {
    let assignment = limit_rules::FundCategoryAssignment {
        instrument_id: category_evidence.instrument_id.clone(),
        fund_category: category_evidence.fund_category.clone(),
        effective_from: category_evidence.effective_from,
        effective_until: category_evidence.effective_until,
        document_number: category_evidence.document_number.clone(),
        evidence: category_evidence.evidence.clone(),
        evidence_level: category_evidence.evidence_level,
    };
    category_evidence.assignment_identity = assignment.identity().unwrap();
}

#[test]
fn official_pre_window_rule_sources_are_available_for_the_scoped_period() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let table = limit_rules::FundLimitRuleTable::official_v1().unwrap();
    let instrument_id: InstrumentId = "159001.SZSE".parse().unwrap();
    let mut category_evidence = cat.input().lifecycles[0].listing.evidence.clone();
    category_evidence.announcement_id = "SYNTHETIC-CATEGORY-LIST".into();
    category_evidence.locator = "synthetic category-list row".into();
    let assignment = limit_rules::FundCategoryAssignment {
        instrument_id: instrument_id.clone(),
        fund_category: "szse-chinext-index-etf-lof-class-b-20pct".into(),
        effective_from: NaiveDate::from_ymd_opt(2022, 10, 1).unwrap(),
        effective_until: None,
        document_number: "SYNTHETIC-CATEGORY-LIST".into(),
        evidence: category_evidence,
        evidence_level: limit_rules::EvidenceLevel::Secondary,
    };
    let rule = table
        .rule_for_with_assignments(&instrument_id, date(1), &[assignment])
        .unwrap()
        .unwrap();
    let available_at = table
        .rule_evidence_available_at(
            &rule,
            &instrument_id,
            &rule.rule_evidence[0],
            date(1),
            time(1, 9),
            cat.input(),
        )
        .unwrap();
    assert!(available_at < time(1, 9));
}

#[test]
fn v2_tdx_only_dataset_is_tradable_without_dual_source_claims() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let hashes = vendor_originals(&raw);
    let manifest = prajna_data::publish_dataset(
        tmp.path(),
        &raw,
        &NormalizerRegistry::with_builtins(),
        "szse-etf-frozen",
        "3",
        &hashes[..hashes.len() - 1],
        "2026-10-15T00:00:00Z",
    )
    .unwrap();
    let evidence = v2_evidence(&cat);
    let market =
        DerivedMarket::from_dataset(tmp.path(), &manifest.dsv, &cat, evidence.clone()).unwrap();
    assert_eq!(market.cells().len(), 30);
    assert!(
        market
            .cells()
            .values()
            .all(|c| c.permits(true) && c.permits(false))
    );
    assert_eq!(
        market.cells().values().next().unwrap().upper_limit,
        Some(Price::parse("1.1").unwrap())
    );
    let replay = DerivedMarket::from_dataset(tmp.path(), &manifest.dsv, &cat, evidence).unwrap();
    assert_eq!(market.version(), replay.version());
}

#[test]
fn v2_frozen_openings_use_tdx_ex_rights_reference_and_fail_closed() {
    use crate::etf_eligible::{EligibilityParams, EligibleSet};
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let hashes = vendor_originals_edited(
        &raw,
        |day, i, block| {
            if i != 0 {
                return true;
            }
            if matches!(day, 5 | 6) {
                return false;
            }
            if day == 2 {
                for off in [4, 12, 20, 28, 36] {
                    block[off..off + 8].copy_from_slice(&0.9_f64.to_le_bytes());
                }
            }
            if matches!(day, 3 | 4) {
                let price = if day == 3 { 1.1_f64 } else { 0.9_f64 };
                for off in [12, 20, 28, 36] {
                    block[off..off + 8].copy_from_slice(&price.to_le_bytes());
                }
            }
            true
        },
        |day, i| !(i == 0 && day == 5),
    );
    let manifest = prajna_data::publish_dataset(
        tmp.path(),
        &raw,
        &NormalizerRegistry::with_builtins(),
        "szse-etf-frozen",
        "3",
        &hashes,
        "2026-10-15T00:00:00Z",
    )
    .unwrap();
    let mut e = v2_evidence(&cat);
    let id = e.openings[0].instrument_id.clone();
    let doc = e.openings[0].announcement_coverage.clone().unwrap();
    e.openings[6].halt = Some(HaltEvidence {
        scope: HaltScope::OpeningOnly,
        evidence: doc.clone(),
    });
    e.openings[7].halt = Some(HaltEvidence {
        scope: HaltScope::FullSession,
        evidence: doc,
    });
    e.openings[8].limit_rule = None;
    e.openings[9].market_available_at = None;
    let market = DerivedMarket::from_dataset(tmp.path(), &manifest.dsv, &cat, e.clone()).unwrap();
    let cell = |d| &market.cells()[&(id.clone(), date(d))];
    // The listing-session rule applies and uses that session's TDX previous-NAV reference.
    assert_eq!(cell(1).upper_limit, Some(Price::parse("1.1").unwrap()));
    assert_eq!(cell(1).lower_limit, Some(Price::parse("0.9").unwrap()));
    // Prior close = 1, but TDX's ex-rights reference is 0.9: upper = 0.99.
    assert_eq!(cell(2).upper_limit, Some(Price::parse("0.99").unwrap()));
    assert!(cell(2).permits(true));
    assert!(!cell(3).permits(true));
    assert!(cell(3).permits(false));
    assert!(cell(4).permits(true));
    assert!(!cell(4).permits(false));
    assert_eq!(cell(5).state, TradeState::Unknown);
    assert!(cell(5).close.is_none());
    assert_eq!(cell(6).state, TradeState::Unknown);
    assert_eq!(cell(6).amount, None);
    assert_eq!(cell(7).state, TradeState::Halted);
    assert_eq!(cell(7).amount, Some(Notional::ZERO));
    for d in [5, 6, 7, 8, 9, 10] {
        assert!(!cell(d).permits(true) && !cell(d).permits(false));
        assert!(matches!(
            market
                .rebalance_gate(date(d), std::slice::from_ref(&id), &[])
                .unwrap(),
            RebalanceGate::Deferred { .. }
        ));
    }
    let index = tri(&market, &cat, &raw);
    let eligible = EligibleSet::build(
        &cat,
        &market,
        &index,
        date(10),
        EligibilityParams {
            minimum_listing_sessions: 1,
            momentum_sessions: 1,
            amount_sessions: 5,
            minimum_average_amount: Notional::ZERO,
        },
    )
    .unwrap();
    assert!(eligible.excluded[&id].contains("amount_missing_input"));
    assert_eq!(eligible.universe.members.len(), 3);

    // Unknown/expired/future rules, absent category evidence and coverage cannot trade.
    for variant in 0..7 {
        let mut missing = e.clone();
        let opening = &mut missing.openings[0];
        match variant {
            0 => opening.limit_rule.as_mut().unwrap().official_rule = None,
            1 => opening.limit_rule.as_mut().unwrap().effective_until = Some(date(1)),
            2 => {
                opening.limit_rule.as_mut().unwrap().evidence.publication = Publication::Exact {
                    timestamp: time(2, 8),
                }
            }
            3 => opening.announcement_coverage = None,
            4 => {
                missing.openings.remove(0);
            }
            5 => {
                let category_evidence = opening
                    .limit_rule
                    .as_mut()
                    .unwrap()
                    .official_rule
                    .as_mut()
                    .unwrap()
                    .category_evidence
                    .as_mut()
                    .unwrap();
                category_evidence.evidence.publication = Publication::Exact {
                    timestamp: time(2, 8),
                };
                refresh_assignment_identity(category_evidence);
            }
            _ => {
                let rule = opening
                    .limit_rule
                    .as_mut()
                    .unwrap()
                    .official_rule
                    .as_mut()
                    .unwrap();
                rule.fund_category = "unknown_category".into();
                let category_evidence = rule.category_evidence.as_mut().unwrap();
                category_evidence.fund_category = "unknown_category".into();
                refresh_assignment_identity(category_evidence);
            }
        }
        let m = DerivedMarket::from_dataset(tmp.path(), &manifest.dsv, &cat, missing).unwrap();
        assert_eq!(m.cells()[&(id.clone(), date(1))].state, TradeState::Unknown);
    }
}

#[test]
fn v2_observed_prices_outside_official_limit_error_instead_of_succeeding() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    for off in [12, 36] {
        let hashes = vendor_originals_edited(
            &raw,
            |day, i, block| {
                if day == 2 && i == 0 {
                    block[off..off + 8].copy_from_slice(&1.101_f64.to_le_bytes());
                    block[20..28].copy_from_slice(&1.101_f64.to_le_bytes());
                }

                true
            },
            |_, _| true,
        );
        let manifest = prajna_data::publish_dataset(
            tmp.path(),
            &raw,
            &NormalizerRegistry::with_builtins(),
            "szse-etf-frozen",
            "3",
            &hashes,
            "2026-10-15T00:00:00Z",
        )
        .unwrap();
        assert!(
            DerivedMarket::from_dataset(tmp.path(), &manifest.dsv, &cat, v2_evidence(&cat))
                .unwrap_err()
                .0
                .contains("prices_outside_pit_rule")
        );
        let mut halted = v2_evidence(&cat);
        halted.openings[1].halt = Some(HaltEvidence {
            scope: HaltScope::OpeningOnly,
            evidence: halted.openings[1].announcement_coverage.clone().unwrap(),
        });
        assert!(
            DerivedMarket::from_dataset(tmp.path(), &manifest.dsv, &cat, halted)
                .unwrap_err()
                .0
                .contains("prices_outside_pit_rule")
        );
    }
}
#[test]
fn v2_consumes_v4_catalog_lifecycle_boundaries_without_cross_identity_cells() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, mut input) = fixture(tmp.path());
    let hashes = vendor_originals(&raw);
    let registry = NormalizerRegistry::with_builtins();
    let manifest = prajna_data::publish_dataset(
        tmp.path(),
        &raw,
        &registry,
        "szse-etf-frozen",
        "4",
        &hashes,
        "2026-10-15T00:00:00Z",
    )
    .unwrap();
    let originals: Vec<_> = hashes
        .iter()
        .map(|hash| prajna_data::RawInput::from_bytes(raw.get(hash).unwrap()))
        .collect();
    let tables = registry
        .get("szse-etf-frozen", "4")
        .unwrap()
        .normalize(&originals)
        .unwrap()
        .tables;
    input.schema_version = 2;
    input.code_ranges = Some(vec![CodeRange {
        start: 158000,
        end: 159999,
    }]);
    input.code_tables =
        prajna_data::tdx_code_table_from_record_batch(tables.tdx_code_table.as_ref().unwrap())
            .unwrap()
            .into_iter()
            .map(Into::into)
            .collect();
    input.broad_indices.as_mut().unwrap().named_indices = vec![BroadIndexName {
        index_id: "CSI:300".into(),
        official_name: "SYNTHETIC-INDEX".into(),
        aliases: BTreeSet::new(),
    }];
    for life in &mut input.lifecycles {
        life.identity_start = Some(date(1));
        life.attributes[0].legal_name = Some("SYNTHETIC-INDEX A".into());
    }
    let mut next_life = input.lifecycles[0].clone();
    next_life.identity_start = Some(date(6));
    next_life.listing.effective_date = date(6);
    next_life.attributes[0].effective_date = date(6);
    next_life.attributes[0].legal_name = Some("SYNTHETIC-INDEX B".into());
    next_life.attributes[0].evidence.publication = Publication::Exact {
        timestamp: time(6, 8),
    };
    input.lifecycles.push(next_life);
    input.scans.retain(|s| s.status == ScanStatus::Observed);
    for scan in &mut input.scans {
        scan.identity_start = Some(date(1));
        if scan.native_symbol == "159001" {
            scan.last_observed = Some(date(5));
        }
    }
    let mut scan = input.scans[0].clone();
    scan.identity_start = Some(date(6));
    scan.first_observed = Some(date(6));
    scan.last_observed = Some(date(10));
    input.scans.push(scan);
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    assert!(cat.coverage_gaps().is_empty(), "{:?}", cat.coverage_gaps());
    let old = cat.resolve_row("159001", date(5)).unwrap();
    let new = cat.resolve_row("159001", date(6)).unwrap();
    assert_ne!(old, new);
    let e = v2_evidence(&cat);
    let m = DerivedMarket::from_dataset(tmp.path(), &manifest.dsv, &cat, e).unwrap();
    assert_eq!(m.cells().len(), 30);
    assert!(!m.cells().contains_key(&(old.clone(), date(6))));
    assert!(!m.cells().contains_key(&(new.clone(), date(5))));
    assert!(m.cells()[&(old, date(5))].permits(true));
    assert!(m.cells()[&(new, date(6))].permits(true));
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
#[test]
fn halted_state_preserves_latest_dependency_and_source_conflicts_remain_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(tmp.path());
    let cat = HistoricalCatalog::build(input, &raw).unwrap();
    let (mut tables, mut e) = data(&cat);
    let id = e.openings[1].instrument_id.clone();
    e.openings[1].halt = Some(HaltEvidence {
        scope: HaltScope::OpeningOnly,
        evidence: e.openings[1].announcement_coverage.clone().unwrap(),
    });
    e.openings[1].market_available_at = Some(time(9, 16));
    let market = build(&cat, &raw, &tables, e.clone());
    let cell = &market.cells()[&(id.clone(), date(2))];
    assert_eq!(cell.state, TradeState::Halted);
    assert_eq!(cell.available_at, Some(time(9, 16)));
    assert!(!cell.known_by(time(2, 15)));
    e.openings[1].market_available_at = None;
    assert_eq!(
        build(&cat, &raw, &tables, e.clone()).cells()[&(id.clone(), date(2))].available_at,
        None
    );
    let checks = tables.source_crosscheck.as_ref().unwrap();
    let mut statuses = vec!["match"; checks.num_rows()];
    statuses[1] = "mismatch";
    let mut columns = checks.columns().to_vec();
    columns[2] = std::sync::Arc::new(StringArray::from(statuses));
    tables.source_crosscheck = Some(RecordBatch::try_new(checks.schema(), columns).unwrap());
    let unknown = build(&cat, &raw, &tables, e);
    assert_eq!(unknown.cells()[&(id, date(2))].state, TradeState::Unknown);
    let index = tri(&unknown, &cat, &raw);
    let ids = cat.universe(date(3)).unwrap().members;
    let values = EtfMomentum::new(2)
        .unwrap()
        .compute(&unknown, &index, &ids)
        .unwrap();
    assert_eq!(
        values.column("status").unwrap().str().unwrap().get(2),
        Some("missing_input")
    );
}
