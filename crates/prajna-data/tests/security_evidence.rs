use chrono::NaiveDate;
use prajna_data::{
    EvidenceClaim, EvidenceSource, FieldEvidence, RawStore, SecurityEvidenceRecord,
    SecurityEvidenceStore, SourceKind, SourceRecordInput,
};
use prajna_domain::{InstrumentId, InstrumentKind, Price, Quantity, VenueId};
use serde_json::json;

fn date(value: &str) -> NaiveDate {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
}

fn source(source_id: &str, evidence_ref: &str, version: &str) -> EvidenceSource {
    EvidenceSource {
        source_id: source_id.to_owned(),
        evidence_ref: evidence_ref.to_owned(),
        source_version: version.to_owned(),
        observed_at: "2026-10-26T18:30:00+08:00".to_owned(),
        effective_from: Some(date("2000-01-01")),
        effective_to_exclusive: None,
    }
}

fn stock_record(evidence_ref: &str) -> SecurityEvidenceRecord {
    SecurityEvidenceRecord {
        instrument_id: InstrumentId::new("SYNTH_STOCK_SH", VenueId::new("XSHG").unwrap()).unwrap(),
        kind: InstrumentKind::Equity,
        established_date: FieldEvidence::Known {
            value: date("1999-10-29"),
            source: source("synthetic-registration", evidence_ref, "fixture-v1"),
        },
        listing_date: FieldEvidence::Known {
            value: date("1999-11-10"),
            source: source("synthetic-listing", evidence_ref, "fixture-v1"),
        },
        lot_size: FieldEvidence::Known {
            value: Quantity::parse("100").unwrap(),
            source: source("synthetic-terms", evidence_ref, "fixture-v1"),
        },
        price_increment: FieldEvidence::Conflict {
            candidates: vec![
                EvidenceClaim {
                    value: Price::parse("0.01").unwrap(),
                    source: source("synthetic-terms-a", evidence_ref, "fixture-v1"),
                },
                EvidenceClaim {
                    value: Price::parse("0.001").unwrap(),
                    source: source("synthetic-terms-b", evidence_ref, "fixture-v2"),
                },
            ],
        },
        size_increment: FieldEvidence::Unknown {
            reason: "synthetic source does not provide this field".to_owned(),
            source: source("synthetic-terms", evidence_ref, "fixture-v1"),
        },
    }
}

fn etf_record(evidence_ref: &str) -> SecurityEvidenceRecord {
    SecurityEvidenceRecord {
        instrument_id: InstrumentId::new("SYNTH_ETF_SZ", VenueId::new("XSHE").unwrap()).unwrap(),
        kind: InstrumentKind::Etf,
        established_date: FieldEvidence::Known {
            value: date("2015-01-08"),
            source: source("synthetic-fund-registration", evidence_ref, "fixture-v1"),
        },
        listing_date: FieldEvidence::Known {
            value: date("2015-02-02"),
            source: source("synthetic-listing", evidence_ref, "fixture-v1"),
        },
        lot_size: FieldEvidence::Unknown {
            reason: "synthetic fixture omits the effective lot size".to_owned(),
            source: source("synthetic-terms", evidence_ref, "fixture-v1"),
        },
        price_increment: FieldEvidence::Known {
            value: Price::parse("0.001").unwrap(),
            source: source("synthetic-terms", evidence_ref, "fixture-v1"),
        },
        size_increment: FieldEvidence::NotApplicable {
            reason: "explicit synthetic non-applicability case".to_owned(),
            source: source("synthetic-terms", evidence_ref, "fixture-v1"),
        },
    }
}

fn assert_source(field: &FieldEvidence<impl Sized>, raw_id: &str) {
    let sources = match field {
        FieldEvidence::Known { source, .. }
        | FieldEvidence::Unknown { source, .. }
        | FieldEvidence::NotApplicable { source, .. } => vec![source],
        FieldEvidence::Conflict { candidates } => candidates
            .iter()
            .map(|candidate| &candidate.source)
            .collect(),
    };
    assert!(!sources.is_empty());
    for source in sources {
        assert!(!source.source_id.is_empty());
        assert_eq!(source.evidence_ref, raw_id);
        assert!(!source.source_version.is_empty());
        assert_eq!(source.observed_at, "2026-10-26T18:30:00+08:00");
        assert_eq!(source.effective_from, Some(date("2000-01-01")));
    }
}

#[test]
fn evidence_import_is_idempotent_and_revisions_remain_addressable() {
    let directory = tempfile::tempdir().unwrap();
    let raw = RawStore::open(directory.path().join("raw-lake")).unwrap();
    let raw_id = raw
        .put(
            b"synthetic security evidence fixture",
            SourceRecordInput {
                content_type: "text/plain".to_owned(),
                source_kind: SourceKind::Fixture,
                source_id: "synthetic-fixture".to_owned(),
                request: json!({"fixture": "security-evidence-v1"}),
                observed_at: "2026-10-26T18:30:00+08:00".to_owned(),
                ingested_by: "security-evidence-test".to_owned(),
            },
        )
        .unwrap();
    let root = directory.path().join("metadata-lake");
    let store = SecurityEvidenceStore::open(&root).unwrap();
    let original = stock_record(&raw_id);

    let original_id = store.import(&original).unwrap();
    assert_eq!(store.import(&original).unwrap(), original_id);
    let concurrent_imports = (0..8)
        .map(|_| {
            let worker_store = store.clone();
            let worker_record = original.clone();
            std::thread::spawn(move || worker_store.import(&worker_record).unwrap())
        })
        .collect::<Vec<_>>();
    for import in concurrent_imports {
        assert_eq!(import.join().unwrap(), original_id);
    }
    assert_eq!(
        store.versions_for(&original.instrument_id).unwrap(),
        vec![original_id.clone()]
    );

    let loaded = SecurityEvidenceStore::open(&root)
        .unwrap()
        .get(&original_id)
        .unwrap();
    assert_eq!(loaded, original);
    let (established, listing) = match (&loaded.established_date, &loaded.listing_date) {
        (
            FieldEvidence::Known {
                value: established, ..
            },
            FieldEvidence::Known { value: listing, .. },
        ) => (established, listing),
        _ => panic!("fixture dates must be known"),
    };
    assert_ne!(
        established, listing,
        "dates remain separate evidence fields"
    );
    assert_source(&loaded.established_date, &raw_id);
    assert_source(&loaded.listing_date, &raw_id);
    assert_source(&loaded.lot_size, &raw_id);
    assert_source(&loaded.price_increment, &raw_id);
    assert_source(&loaded.size_increment, &raw_id);
    assert_eq!(
        match &loaded.price_increment {
            FieldEvidence::Conflict { candidates } => candidates.len(),
            _ => panic!("conflicting evidence must remain explicit"),
        },
        2
    );
    assert!(matches!(
        loaded.size_increment,
        FieldEvidence::Unknown { .. }
    ));

    let mut revised = original.clone();
    revised.lot_size = FieldEvidence::Known {
        value: Quantity::parse("200").unwrap(),
        source: source("synthetic-terms", &raw_id, "fixture-v2"),
    };
    let revised_id = store.import(&revised).unwrap();
    assert_ne!(revised_id, original_id);
    assert_eq!(store.get(&original_id).unwrap(), original);
    assert_eq!(store.get(&revised_id).unwrap(), revised);
    assert_eq!(
        store.versions_for(&original.instrument_id).unwrap(),
        vec![original_id, revised_id]
    );

    let etf = etf_record(&raw_id);
    let etf_id = store.import(&etf).unwrap();
    assert_eq!(store.get(&etf_id).unwrap(), etf);
    assert!(matches!(
        &etf.size_increment,
        FieldEvidence::NotApplicable { .. }
    ));
    assert_eq!(
        store.versions_for(&etf.instrument_id).unwrap(),
        vec![etf_id]
    );
    assert_source(&etf.established_date, &raw_id);
    assert_source(&etf.listing_date, &raw_id);
    assert_source(&etf.lot_size, &raw_id);
    assert_source(&etf.price_increment, &raw_id);
    assert_source(&etf.size_increment, &raw_id);

    let mut shenzhen_stock = stock_record(&raw_id);
    shenzhen_stock.instrument_id = InstrumentId::new(
        shenzhen_stock.instrument_id.symbol(),
        VenueId::new("XSHE").unwrap(),
    )
    .unwrap();
    let mut shanghai_etf = etf_record(&raw_id);
    shanghai_etf.instrument_id = InstrumentId::new(
        shanghai_etf.instrument_id.symbol(),
        VenueId::new("XSHG").unwrap(),
    )
    .unwrap();
    for sample in [shenzhen_stock, shanghai_etf] {
        let sample_id = store.import(&sample).unwrap();
        assert_eq!(store.get(&sample_id).unwrap(), sample);
        assert_eq!(
            store.versions_for(&sample.instrument_id).unwrap(),
            vec![sample_id]
        );
    }
    assert_eq!(
        store.versions_for(&original.instrument_id).unwrap().len(),
        2
    );
    assert_eq!(store.versions_for(&etf.instrument_id).unwrap().len(), 1);
}

#[test]
fn evidence_rejects_incomplete_provenance_invalid_identity_and_zero_increments() {
    let root = tempfile::tempdir().unwrap();
    let store = SecurityEvidenceStore::open(root.path()).unwrap();
    let raw_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    let mut missing_source = stock_record(raw_id);
    if let FieldEvidence::Known { source, .. } = &mut missing_source.lot_size {
        source.source_id.clear();
    }
    assert!(store.import(&missing_source).is_err());

    let mut missing_offset = stock_record(raw_id);
    if let FieldEvidence::Known { source, .. } = &mut missing_offset.listing_date {
        source.observed_at = "2026-10-26T18:30:00".to_owned();
    }
    assert!(store.import(&missing_offset).is_err());

    let mut invalid_effective_range = stock_record(raw_id);
    if let FieldEvidence::Known { source, .. } = &mut invalid_effective_range.lot_size {
        source.effective_from = Some(date("2001-01-01"));
        source.effective_to_exclusive = Some(date("2000-01-01"));
    }
    assert!(store.import(&invalid_effective_range).is_err());

    let mut unsupported_venue = stock_record(raw_id);
    unsupported_venue.instrument_id = InstrumentId::new(
        unsupported_venue.instrument_id.symbol(),
        VenueId::new("XNAS").unwrap(),
    )
    .unwrap();
    assert!(store.import(&unsupported_venue).is_err());

    let mut unsupported_kind = stock_record(raw_id);
    unsupported_kind.kind = InstrumentKind::Spot;
    assert!(store.import(&unsupported_kind).is_err());

    let mut zero_increment = stock_record(raw_id);
    zero_increment.price_increment = FieldEvidence::Known {
        value: Price::ZERO,
        source: source("synthetic-terms", raw_id, "fixture-v1"),
    };
    assert!(store.import(&zero_increment).is_err());

    let mut duplicate_conflict = stock_record(raw_id);
    duplicate_conflict.price_increment = FieldEvidence::Conflict {
        candidates: vec![
            EvidenceClaim {
                value: Price::parse("0.01").unwrap(),
                source: source("synthetic-terms-a", raw_id, "fixture-v1"),
            },
            EvidenceClaim {
                value: Price::parse("0.01").unwrap(),
                source: source("synthetic-terms-b", raw_id, "fixture-v2"),
            },
        ],
    };
    assert!(store.import(&duplicate_conflict).is_err());

    let mut serialized = serde_json::to_value(stock_record(raw_id)).unwrap();
    serialized.as_object_mut().unwrap().remove("listing_date");
    assert!(serde_json::from_value::<SecurityEvidenceRecord>(serialized).is_err());
}
