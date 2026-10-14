use chrono::NaiveDate;
use prajna_data::{RawStore, SourceKind, SourceRecordInput};
use prajna_domain::{Session, TimestampNs};
use prajna_research::pit_universe::*;
use std::collections::BTreeSet;
fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2023, 1, day).unwrap()
}
fn time(day: u32, hour: u32) -> TimestampNs {
    format!("2023-01-{day:02}T{hour:02}:00:00+08:00")
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
        sessions: (1..=10).map(|d| Session::new("SZSE".parse().unwrap(), date(d), time(d, 9), time(d, 15)).unwrap()).collect(),
        coverage: Coverage { start: date(1), end: date(10), scan_method: "synthetic 159000..159999 full-window scan".into(), announcement_method: "synthetic full listings/contracts/terminations + attachments and pre-window identities".into(), announcement_raw_sha256: BTreeSet::from([h.clone()]), gaps: BTreeSet::new(), resolutions: vec![] },
        lifecycles: vec![life("159001", 2, Some(5)), life("159001", 7, None), life("159002", 1, None)],
        scans: (159000..=159999).map(|c| ScanObservation { native_symbol: c.to_string(), status: if c == 159001 || c == 159002 { ScanStatus::Observed } else { ScanStatus::NoRows }, raw_sha256: h.clone(), first_observed: (c == 159001 || c == 159002).then(|| date(if c == 159001 {2} else {1})), last_observed: (c == 159001 || c == 159002).then(|| date(10)) }).collect(),
        broad_indices: BroadIndexList { version: "synthetic-domestic@1".into(), index_ids: BTreeSet::from(["CSI:300".into()]), evidence: vec![evidence("index-scope")] },
    };
    (raw, input)
}
#[test]
fn three_lifecycles_ten_sessions_listing_termination_and_code_reuse() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(temp.path());
    let catalog = HistoricalCatalog::build(input, &raw).unwrap();
    let a = catalog.resolve_row("159001", date(2)).unwrap();
    let b = catalog.resolve_row("159001", date(7)).unwrap();
    assert_ne!(a, b);
    let expected = [1, 2, 2, 2, 1, 1, 2, 2, 2, 2];
    for (day, count) in (1..=10).zip(expected) {
        assert_eq!(catalog.universe(date(day)).unwrap().members.len(), count);
    }
    assert!(!catalog.universe(date(1)).unwrap().members.contains(&a));
    assert!(!catalog.universe(date(5)).unwrap().members.contains(&a));
    assert!(catalog.resolve_row("159001", date(5)).is_err());
    assert!(catalog.resolve_row("159003", date(8)).is_err());
}
#[test]
fn date_only_late_announcement_changes_attributes_at_next_session_close() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, mut input) = fixture(temp.path());
    let l = &mut input.lifecycles[2];
    let id = l.instrument_id().unwrap();
    let mut change = l.attributes[0].clone();
    change.effective_date = date(2);
    change.index_id = "SECTOR:TECH".into();
    change.evidence.publication = Publication::DateOnly { date: date(4) };
    l.attributes.push(change);
    let catalog = HistoricalCatalog::build(input, &raw).unwrap();
    assert!(catalog.universe(date(4)).unwrap().members.contains(&id));
    assert_eq!(
        catalog.universe(date(5)).unwrap().excluded[&id],
        "not_domestic_broad_index"
    );
}
#[test]
fn missing_or_conflicting_attributes_are_excluded_without_last_write_wins() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, mut input) = fixture(temp.path());
    let id = input.lifecycles[2].instrument_id().unwrap();
    let mut conflict = input.lifecycles[2].attributes[0].clone();
    conflict.index_id = "CSI:500".into();
    input.lifecycles[2].attributes.push(conflict);
    let catalog = HistoricalCatalog::build(input.clone(), &raw).unwrap();
    assert_eq!(
        catalog.diagnose_membership(date(3)).unwrap().excluded[&id],
        "attribute_conflict"
    );
    assert!(catalog.universe(date(3)).is_err());
    assert!(
        !catalog
            .diagnose_membership(date(3))
            .unwrap()
            .run_spec
            .coverage_gaps
            .is_empty()
    );
    input.lifecycles[2].attributes.clear();
    assert_eq!(
        HistoricalCatalog::build(input, &raw)
            .unwrap()
            .diagnose_membership(date(3))
            .unwrap()
            .excluded[&id],
        "missing_attributes"
    );
}
#[test]
fn late_listing_evidence_is_never_backfilled() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, mut input) = fixture(temp.path());
    let l = &mut input.lifecycles[0];
    let id = l.instrument_id().unwrap();
    l.listing.evidence.publication = Publication::DateOnly { date: date(3) };
    let catalog = HistoricalCatalog::build(input, &raw).unwrap();
    assert_eq!(
        catalog.universe(date(3)).unwrap().excluded[&id],
        "listing_not_known"
    );
    assert!(catalog.universe(date(4)).unwrap().members.contains(&id));
}
#[test]
fn current_directory_changes_do_not_rewrite_frozen_universe_or_version() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(temp.path());
    let catalog = HistoricalCatalog::build(input, &raw).unwrap();
    let before = catalog.universe(date(3)).unwrap();
    let version = catalog.version().unwrap();
    let differences = catalog.compare_current(&BTreeSet::from(["159999".into()]));
    assert_eq!(differences["159001"], "historical_only");
    assert_eq!(differences["159999"], "current_only");
    assert_eq!(before, catalog.universe(date(3)).unwrap());
    assert_eq!(version, catalog.version().unwrap());
}
#[test]
fn full_scan_and_announcement_coverage_fail_closed_and_bidirectional_differences_need_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, mut input) = fixture(temp.path());
    input.scans[2].status = ScanStatus::NoRows;
    input.scans[2].first_observed = None;
    input.scans[2].last_observed = None;
    input.scans[3].status = ScanStatus::Observed;
    input.scans[3].first_observed = Some(date(1));
    input.scans[3].last_observed = Some(date(10));
    let catalog = HistoricalCatalog::build(input.clone(), &raw).unwrap();
    assert_eq!(
        catalog
            .differences()
            .iter()
            .map(|d| d.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["announcement_only", "scan_only"]
    );
    assert!(catalog.universe(date(3)).is_err());
    for c in ["159002", "159003"] {
        input.coverage.resolutions.push(DifferenceResolution {
            native_symbol: c.into(),
            explanation: "synthetic attachment explicitly resolves difference".into(),
            evidence: input.lifecycles[0].listing.evidence.clone(),
        });
    }
    assert!(
        HistoricalCatalog::build(input.clone(), &raw)
            .unwrap()
            .universe(date(3))
            .is_ok()
    );
    input.scans.pop();
    assert!(
        HistoricalCatalog::build(input.clone(), &raw)
            .unwrap()
            .universe(date(3))
            .is_err()
    );
    input
        .coverage
        .gaps
        .insert("announcement attachment missing".into());
    assert!(
        HistoricalCatalog::build(input, &raw)
            .unwrap()
            .universe(date(3))
            .is_err()
    );
}
#[test]
fn identity_provenance_and_replay_are_canonical_and_raw_verified() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, input) = fixture(temp.path());
    let catalog = HistoricalCatalog::build(input.clone(), &raw).unwrap();
    let first = catalog.universe(date(3)).unwrap();
    assert_eq!(first.run_spec.catalog_version, catalog.version().unwrap());
    assert!(!first.run_spec.sources.is_empty());
    assert!(first.run_spec.coverage_gaps.is_empty());
    let path = temp.path().join("catalog.json");
    catalog.store(&path).unwrap();
    assert!(catalog.store(&path).is_err());
    assert_eq!(
        HistoricalCatalog::load(&path, &catalog.version().unwrap(), &raw)
            .unwrap()
            .universe(date(3))
            .unwrap(),
        first
    );
    let mut shuffled = input.clone();
    shuffled.scans.reverse();
    shuffled.sessions.reverse();
    shuffled.lifecycles.reverse();
    assert_eq!(
        HistoricalCatalog::build(shuffled, &raw)
            .unwrap()
            .version()
            .unwrap(),
        catalog.version().unwrap()
    );
    let mut revised = input.clone();
    revised.rule_version = "rules@2".into();
    assert_ne!(
        HistoricalCatalog::build(revised, &raw)
            .unwrap()
            .universe(date(3))
            .unwrap()
            .universe_version,
        first.universe_version
    );
    let mut missing = input;
    missing.lifecycles[0].contract_raw_sha256 = format!("sha256:{}", "0".repeat(64));
    assert!(HistoricalCatalog::build(missing, &raw).is_err());
}
#[test]
fn overlapping_reuse_and_unknown_publication_calendar_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, mut input) = fixture(temp.path());
    input.lifecycles[0].termination = None;
    assert!(HistoricalCatalog::build(input.clone(), &raw).is_err());
    input.lifecycles.remove(1);
    input.lifecycles[0].listing.evidence.publication = Publication::DateOnly { date: date(10) };
    assert!(HistoricalCatalog::build(input, &raw).is_err());
}

#[test]
fn precise_time_non_equity_and_empty_universe_keep_provenance() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, mut input) = fixture(temp.path());
    for life in &mut input.lifecycles {
        life.attributes[0].category = FundCategory::Other;
    }
    let id = input.lifecycles[2].instrument_id().unwrap();
    let mut change = input.lifecycles[2].attributes[0].clone();
    change.effective_date = date(3);
    change.category = FundCategory::Equity;
    change.evidence.publication = Publication::Exact {
        timestamp: time(3, 16),
    };
    input.lifecycles[2].attributes.push(change);
    let catalog = HistoricalCatalog::build(input, &raw).unwrap();
    let empty = catalog.universe(date(3)).unwrap();
    assert!(empty.members.is_empty());
    assert!(!empty.run_spec.sources.is_empty());
    assert_eq!(empty.excluded[&id], "not_equity_fund");
    assert!(catalog.universe(date(4)).unwrap().members.contains(&id));
}
#[test]
fn late_termination_is_not_backfilled_and_reuse_conflicts_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, mut input) = fixture(temp.path());
    input.lifecycles[0]
        .termination
        .as_mut()
        .unwrap()
        .evidence
        .publication = Publication::DateOnly { date: date(8) };
    let old = input.lifecycles[0].instrument_id().unwrap();
    let catalog = HistoricalCatalog::build(input, &raw).unwrap();
    assert!(catalog.universe(date(6)).unwrap().members.contains(&old));
    assert!(catalog.universe(date(7)).is_err());
    assert!(!catalog.universe(date(9)).unwrap().members.contains(&old));
}
#[test]
fn frozen_version_mismatch_and_malformed_calendar_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, mut input) = fixture(temp.path());
    let catalog = HistoricalCatalog::build(input.clone(), &raw).unwrap();
    let path = temp.path().join("catalog.json");
    catalog.store(&path).unwrap();
    assert!(HistoricalCatalog::load(path, "catalog:sha256:wrong", &raw).is_err());
    input.sessions[0] =
        Session::new("SZSE".parse().unwrap(), date(1), time(2, 9), time(2, 15)).unwrap();
    assert!(HistoricalCatalog::build(input, &raw).is_err());
}
