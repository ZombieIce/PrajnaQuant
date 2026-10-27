use std::process::Command;

use chrono::NaiveDate;
use prajna_data::{
    EvidenceClaim, EvidenceSource, FieldEvidence, PublicationStatus, SecurityEvidenceRecord,
    SecurityEvidenceSnapshot, SecurityEvidenceStore, SecurityInstrumentSnapshot,
    publication_status,
};
use prajna_domain::{InstrumentId, InstrumentKind, Price, Quantity, VenueId};

fn date(value: &str) -> NaiveDate {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
}

fn source(version: &str) -> EvidenceSource {
    EvidenceSource {
        source_id: "synthetic-security-evidence".to_owned(),
        evidence_ref: "synthetic-fixture".to_owned(),
        source_version: version.to_owned(),
        observed_at: "2026-10-27T09:15:00+08:00".to_owned(),
        effective_from: Some(date("2000-01-01")),
        effective_to_exclusive: None,
    }
}

fn record(symbol: &str, venue: &str, kind: InstrumentKind) -> SecurityEvidenceRecord {
    SecurityEvidenceRecord {
        instrument_id: InstrumentId::new(symbol.to_owned(), VenueId::new(venue).unwrap()).unwrap(),
        kind,
        established_date: FieldEvidence::Known {
            value: date("2010-01-04"),
            source: source("fixture-v1"),
        },
        listing_date: FieldEvidence::Known {
            value: date("2010-02-01"),
            source: source("fixture-v1"),
        },
        lot_size: FieldEvidence::Known {
            value: Quantity::parse("100").unwrap(),
            source: source("fixture-v1"),
        },
        price_increment: FieldEvidence::Known {
            value: Price::parse("0.01").unwrap(),
            source: source("fixture-v1"),
        },
        size_increment: FieldEvidence::Known {
            value: Quantity::parse("1").unwrap(),
            source: source("fixture-v1"),
        },
    }
}

fn selected<'a>(
    snapshot: &'a SecurityEvidenceSnapshot,
    instrument_id: &InstrumentId,
) -> &'a SecurityInstrumentSnapshot {
    snapshot
        .instruments
        .iter()
        .find(|entry| &entry.record.instrument_id == instrument_id)
        .unwrap()
}

fn blocked(publication: &PublicationStatus) -> Vec<(&str, &str)> {
    match publication {
        PublicationStatus::Publishable { terms } => panic!("expected block, got {terms:?}"),
        PublicationStatus::NotPublishable { issues } => issues
            .iter()
            .map(|issue| (issue.field.as_str(), issue.code.as_str()))
            .collect(),
    }
}

fn lot(publication: &PublicationStatus) -> Quantity {
    match publication {
        PublicationStatus::Publishable { terms } => terms.lot_size,
        PublicationStatus::NotPublishable { issues } => panic!("expected terms, got {issues:?}"),
    }
}

#[test]
fn snapshot_preserves_evidence_states_and_fixed_versions() {
    let directory = tempfile::tempdir().unwrap();
    let store = SecurityEvidenceStore::open(directory.path()).unwrap();
    let stock = record("600000", "XSHE", InstrumentKind::Equity);
    let mut etf = record("510300", "XSHG", InstrumentKind::Etf);
    etf.lot_size = FieldEvidence::Unknown {
        reason: "synthetic source omits the lot".to_owned(),
        source: source("fixture-v1"),
    };
    etf.price_increment = FieldEvidence::Conflict {
        candidates: vec![
            EvidenceClaim {
                value: Price::parse("0.001").unwrap(),
                source: source("fixture-v1"),
            },
            EvidenceClaim {
                value: Price::parse("0.01").unwrap(),
                source: source("fixture-v2"),
            },
        ],
    };
    etf.size_increment = FieldEvidence::NotApplicable {
        reason: "synthetic not-applicable case".to_owned(),
        source: source("fixture-v1"),
    };
    let stock_version = store.import(&stock).unwrap();
    let etf_version = store.import(&etf).unwrap();
    let selection = vec![
        (etf.instrument_id.clone(), etf_version.clone()),
        (stock.instrument_id.clone(), stock_version.clone()),
    ];

    let (snapshot_id, snapshot) = store.select_snapshot(selection.clone()).unwrap();
    assert_eq!(snapshot.id().unwrap(), snapshot_id);
    assert_eq!(
        snapshot
            .instruments
            .iter()
            .map(|entry| entry.record.instrument_id.clone())
            .collect::<Vec<_>>(),
        vec![etf.instrument_id.clone(), stock.instrument_id.clone()]
    );
    assert_eq!(
        lot(&selected(&snapshot, &stock.instrument_id).publication),
        Quantity::parse("100").unwrap()
    );
    assert_eq!(
        blocked(&selected(&snapshot, &etf.instrument_id).publication),
        vec![
            ("lot_size", "unknown"),
            ("price_increment", "conflict"),
            ("size_increment", "not_applicable"),
        ]
    );

    assert_eq!(
        store.select_snapshot(selection).unwrap(),
        (snapshot_id.clone(), snapshot.clone())
    );
    let mut revised = stock.clone();
    revised.lot_size = FieldEvidence::Known {
        value: Quantity::parse("200").unwrap(),
        source: source("fixture-v2"),
    };
    let revised_version = store.import(&revised).unwrap();
    let (revised_id, revised_snapshot) = store
        .select_snapshot(vec![(stock.instrument_id.clone(), revised_version)])
        .unwrap();
    assert_ne!(revised_id, snapshot_id);
    assert_eq!(
        lot(&selected(
            &store.get_snapshot(&snapshot_id).unwrap(),
            &stock.instrument_id
        )
        .publication),
        Quantity::parse("100").unwrap()
    );
    assert_eq!(
        lot(&selected(&revised_snapshot, &stock.instrument_id).publication),
        Quantity::parse("200").unwrap()
    );
    assert_eq!(store.get(&stock_version).unwrap(), stock);
    assert_eq!(store.get(&etf_version).unwrap(), etf);
    let mismatch = store
        .select_snapshot(vec![(etf.instrument_id.clone(), stock_version)])
        .unwrap_err();
    assert!(mismatch.to_string().contains("belongs to"));
}

#[test]
fn unknown_conflict_invalid_and_lifecycle_evidence_fail_closed() {
    let mut zero = record("600000", "XSHG", InstrumentKind::Equity);
    zero.lot_size = FieldEvidence::Known {
        value: Quantity::ZERO,
        source: source("fixture-v1"),
    };
    assert_eq!(
        blocked(&publication_status(&zero)),
        vec![("lot_size", "non_positive")]
    );

    let directory = tempfile::tempdir().unwrap();
    let store = SecurityEvidenceStore::open(directory.path()).unwrap();
    let mut unknown = record("600000", "XSHG", InstrumentKind::Equity);
    unknown.listing_date = FieldEvidence::Unknown {
        reason: "synthetic listing evidence is unavailable".to_owned(),
        source: source("fixture-v1"),
    };
    let version = store.import(&unknown).unwrap();
    let (snapshot_id, snapshot) = store
        .select_snapshot(vec![(unknown.instrument_id.clone(), version)])
        .unwrap();
    let stored = selected(&snapshot, &unknown.instrument_id);
    assert!(matches!(
        stored.record.listing_date,
        FieldEvidence::Unknown { .. }
    ));
    let serialized = serde_json::to_string(&snapshot).unwrap();
    for forbidden in ["coverage", "available_at", "tradable_from"] {
        assert!(!serialized.contains(forbidden), "unexpected {forbidden}");
    }
    assert!(serialized.contains("2026-10-27T09:15:00+08:00"));
    assert!(store.get_snapshot(&snapshot_id).is_ok());
}

fn run_cli(args: &[String]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_prajna-select-security-evidence"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn production_cli_selects_and_reads_the_fixed_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let store = SecurityEvidenceStore::open(directory.path()).unwrap();
    let stock = record("600000", "XSHG", InstrumentKind::Equity);
    let mut etf = record("159952", "XSHE", InstrumentKind::Etf);
    etf.lot_size = FieldEvidence::Unknown {
        reason: "synthetic source omits the lot".to_owned(),
        source: source("fixture-v1"),
    };
    let stock_version = store.import(&stock).unwrap();
    let etf_version = store.import(&etf).unwrap();
    let root = directory.path().to_str().unwrap().to_owned();
    let output = run_cli(&[
        "--root".to_owned(),
        root.clone(),
        "--select".to_owned(),
        format!("{}={stock_version}", stock.instrument_id),
        "--select".to_owned(),
        format!("{}={etf_version}", etf.instrument_id),
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("instruments=2 publishable=1 blocked=1"));
    assert!(stdout.contains("blocked=159952.XSHE reasons=lot_size:unknown"));
    let snapshot_id = stdout
        .lines()
        .next()
        .unwrap()
        .strip_prefix("snapshot=")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let read = run_cli(&[
        "--root".to_owned(),
        root.clone(),
        "--read".to_owned(),
        snapshot_id.clone(),
    ]);
    assert!(read.status.success());
    assert!(
        String::from_utf8(read.stdout)
            .unwrap()
            .starts_with(&format!("snapshot={snapshot_id}"))
    );
}
