//! Synthetic-only tests for the offline MVP-6 Catalog assembly. No real Raw,
//! PDFs, prices or hashes are committed; every fixture is generated locally.

use arrow_array::RecordBatch;
use chrono::NaiveDate;
use prajna_data::cninfo::{
    Citation, CitationInput, EvidenceLevel, IdentityFact, LEGAL_NAME_CHANGE, Mvp6Facts, PdfText,
};
use prajna_data::{
    Manifest, ManifestCore, ManifestInput, ManifestProvenance, ManifestTable, NormalizerIdentity,
    RawStore, SourceKind, SourceRecordInput, TdxCodeTableRow, bars_to_record_batch, logical_hash,
    schema_fingerprint, sessions_to_record_batch, tdx_code_table_to_record_batch, write_manifest,
    write_parquet,
};
use prajna_domain::{Bar, BarData, InstrumentId, Price, Quantity, Session, TimestampNs, VenueId};
use prajna_research::pit_catalog_assembly::{
    IdentityDifferenceKind, IdentityDifferenceResolution, assemble_mvp6_catalog,
};
use prajna_research::pit_universe::{
    AttributeFact, CatalogInput, CodeRange, CodeTableSnapshot, Coverage, Evidence,
    FixedUniverseSpec, FundCategory, Lifecycle, LifecycleFact, Publication, ScanObservation,
    ScanStatus,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

fn d(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2023, 1, day).unwrap()
}

fn session(day: u32) -> Session {
    let date = d(day);
    Session::new(
        VenueId::new("SZSE").unwrap(),
        date,
        TimestampNs::parse(&format!("{date}T09:30:00+08:00")).unwrap(),
        TimestampNs::parse(&format!("{date}T15:00:00+08:00")).unwrap(),
    )
    .unwrap()
}

struct NoopPdf;
impl PdfText for NoopPdf {
    fn text(&mut self, _: &[u8]) -> prajna_data::cninfo::Result<String> {
        Ok("synthetic PDF text".into())
    }
}

#[derive(Clone)]
struct RowInput {
    code: String,
    legal_name: String,
    listing: NaiveDate,
    termination: Option<NaiveDate>,
    tracked_index: String,
    evidence_level: String,
    announcement: NaiveDate,
    change_kind: String,
}

impl RowInput {
    fn legal_name_change(mut self) -> Self {
        self.change_kind = LEGAL_NAME_CHANGE.into();
        self
    }
}

fn row(
    code: &str,
    name: &str,
    listing: u32,
    termination: Option<u32>,
    announcement: u32,
    index: &str,
    evidence_level: &str,
) -> RowInput {
    RowInput {
        code: code.into(),
        legal_name: name.into(),
        listing: d(listing),
        termination: termination.map(d),
        tracked_index: index.into(),
        evidence_level: evidence_level.into(),
        announcement: d(announcement),
        change_kind: String::new(),
    }
}

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    dsv: String,
    facts: Mvp6Facts,
    citations: Vec<Citation>,
    sessions: Vec<Session>,
    code_table_raws: BTreeMap<NaiveDate, String>,
    manifest_inputs: BTreeSet<String>,
}

fn world(rows: &[RowInput], presence: &[(u32, &[&str])], bars: &[(&str, &[u32])]) -> World {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let raw = RawStore::open(&root).unwrap();
    let sessions = presence
        .iter()
        .map(|(day, _)| session(*day))
        .collect::<Vec<_>>();

    let mut code_tables = Vec::new();
    let mut code_table_raws = BTreeMap::new();
    let mut manifest_inputs = BTreeSet::new();
    for (day, symbols) in presence {
        let bytes = format!("synthetic TDX code table {}", d(*day)).into_bytes();
        let hash = raw
            .put(
                &bytes,
                SourceRecordInput {
                    content_type: "application/zip".into(),
                    source_kind: SourceKind::Fixture,
                    source_id: "tdx".into(),
                    request: json!({"url": format!("https://example.test/tdx/{}.zip", d(*day))}),
                    observed_at: "synthetic".into(),
                    ingested_by: "synthetic".into(),
                },
            )
            .unwrap();
        manifest_inputs.insert(hash.clone());
        code_table_raws.insert(d(*day), hash.clone());
        let mut native_symbols = symbols
            .iter()
            .map(|symbol| (*symbol).to_owned())
            .collect::<Vec<_>>();
        native_symbols.sort();
        code_tables.push(TdxCodeTableRow {
            session_date: d(*day),
            native_symbols,
            code_ranges: vec![(159_000, 159_999)],
            source_raw_sha256: hash,
        });
    }

    let mut bars_rows = Vec::new();
    for (code, days) in bars {
        for day in *days {
            let session = sessions
                .iter()
                .find(|session| session.session_date() == d(*day))
                .unwrap();
            bars_rows.push(
                Bar::new(
                    BarData {
                        instrument_id: InstrumentId::new(*code, VenueId::new("SZSE").unwrap())
                            .unwrap(),
                        bar_spec: "1d@session".parse().unwrap(),
                        session_date: d(*day),
                        ts_open: session.ts_open(),
                        ts_close: session.ts_close(),
                        open: Price::parse("1.0").unwrap(),
                        high: Price::parse("1.0").unwrap(),
                        low: Price::parse("1.0").unwrap(),
                        close: Price::parse("1.0").unwrap(),
                        volume: Quantity::parse("100").unwrap(),
                        amount: None,
                        available_at: None,
                    },
                    Some(session),
                )
                .unwrap(),
            );
        }
    }

    let dsv = publish_v4(&root, &sessions, &bars_rows, &code_tables, &manifest_inputs);

    let (facts, citations) = facts_for(&raw, rows);
    World {
        _dir: dir,
        root,
        dsv,
        facts,
        citations,
        sessions,
        code_table_raws,
        manifest_inputs,
    }
}

fn publish_v4(
    root: &Path,
    sessions: &[Session],
    bars: &[Bar],
    code_tables: &[TdxCodeTableRow],
    inputs: &BTreeSet<String>,
) -> String {
    let sessions_batch = sessions_to_record_batch(sessions).unwrap();
    let bars_batch = bars_to_record_batch(bars).unwrap();
    let code_batch = tdx_code_table_to_record_batch(code_tables).unwrap();
    let manifest = Manifest::new(
        ManifestCore {
            manifest_version: 1,
            normalizer: NormalizerIdentity {
                id: "szse-etf-frozen".into(),
                version: "4".into(),
                config_sha256: format!("sha256:{}", "2".repeat(64)),
            },
            inputs: inputs
                .iter()
                .map(|hash| ManifestInput {
                    raw_sha256: hash.clone(),
                })
                .collect(),
            tables: vec![
                manifest_table(
                    "bars",
                    &bars_batch,
                    &["instrument_id", "bar_spec", "session_date"],
                ),
                manifest_table("sessions", &sessions_batch, &["venue_id", "session_date"]),
                manifest_table("tdx_code_table", &code_batch, &["session_date"]),
            ],
        },
        ManifestProvenance {
            files: Vec::new(),
            writer: json!({"name": "synthetic-test"}),
            created_at: "synthetic".into(),
            git_rev: None,
            git_dirty: None,
            host: None,
            synthetic_assumptions: vec!["synthetic v4 projection".into()],
            coverage: json!({}),
            ignored_input_fields: Vec::new(),
        },
    )
    .unwrap();
    write_manifest(root, &manifest).unwrap();
    let hex = manifest.dsv.strip_prefix("dsv:sha256:").unwrap();
    for (name, batch) in [
        ("bars", &bars_batch),
        ("sessions", &sessions_batch),
        ("tdx_code_table", &code_batch),
    ] {
        let directory = root.join("normalized").join(name).join(hex);
        fs::create_dir_all(&directory).unwrap();
        write_parquet(directory.join("part-00000.parquet"), batch, &manifest.dsv).unwrap();
    }
    manifest.dsv
}

fn manifest_table(name: &str, batch: &RecordBatch, primary_key: &[&str]) -> ManifestTable {
    ManifestTable {
        table: name.into(),
        schema_version: 1,
        schema_fingerprint: schema_fingerprint(batch.schema().as_ref()).unwrap(),
        logical_hash: logical_hash(
            name,
            batch.schema().as_ref(),
            std::slice::from_ref(batch),
            primary_key,
        )
        .unwrap(),
        row_count: batch.num_rows() as u64,
    }
}

fn facts_for(raw: &RawStore, rows: &[RowInput]) -> (Mvp6Facts, Vec<Citation>) {
    let mut identities = Vec::new();
    let mut citations = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let source = format!("src-{}-{index}", row.code);
        let mut document = format!(
            "公告编号：{source}\n基金代码：{}\n基金管理人：org-{}\n基金全称：{}\n基金类型：指数型基金\n上市日期：{}\n",
            row.code, row.code, row.legal_name, row.listing
        );
        if let Some(termination) = row.termination {
            document.push_str(&format!("终止日期：{termination}\n"));
        }
        if !row.tracked_index.is_empty() {
            document.push_str(&format!("标的指数：{}\n", row.tracked_index));
        }
        let hash = raw
            .put(
                document.as_bytes(),
                SourceRecordInput {
                    content_type: "application/octet-stream".into(),
                    source_kind: SourceKind::Fixture,
                    source_id: "cninfo".into(),
                    request: json!({
                        "url": format!(
                            "https://static.cninfo.com.cn/finalpage/{}/{source}.PDF",
                            row.announcement
                        ),
                        "announcement_id": source,
                    }),
                    observed_at: "synthetic".into(),
                    ingested_by: "synthetic".into(),
                },
            )
            .unwrap();
        citations.push(cite(
            raw,
            &hash,
            &source,
            "code",
            &format!("基金代码：{}", row.code),
        ));
        citations.push(cite(
            raw,
            &hash,
            &source,
            "org_id",
            &format!("基金管理人：org-{}", row.code),
        ));
        citations.push(cite(
            raw,
            &hash,
            &source,
            "legal_name",
            &format!("基金全称：{}", row.legal_name),
        ));
        citations.push(cite(
            raw,
            &hash,
            &source,
            "listing_date",
            &format!("上市日期：{}", row.listing),
        ));
        if let Some(termination) = row.termination {
            citations.push(cite(
                raw,
                &hash,
                &source,
                "termination_date",
                &format!("终止日期：{termination}"),
            ));
        }
        citations.push(cite(
            raw,
            &hash,
            &source,
            "fund_category",
            "基金类型：指数型基金",
        ));
        citations.push(cite(
            raw,
            &hash,
            &source,
            "contract",
            &format!("基金全称：{}", row.legal_name),
        ));
        if !row.tracked_index.is_empty() {
            citations.push(cite(
                raw,
                &hash,
                &source,
                "tracked_index",
                &format!("标的指数：{}", row.tracked_index),
            ));
        }
        identities.push(IdentityFact {
            code: row.code.clone(),
            org_id: format!("org-{}", row.code),
            legal_name: row.legal_name.clone(),
            listing_date: row.listing.to_string(),
            termination_date: row
                .termination
                .map(|date| date.to_string())
                .unwrap_or_default(),
            fund_category: "指数型基金".into(),
            tracked_index: row.tracked_index.clone(),
            evidence_level: row.evidence_level.clone(),
            source_announcement_id: source,
            source_page: "1".into(),
            change_kind: row.change_kind.clone(),
        });
    }
    (
        Mvp6Facts {
            identities,
            actions: Vec::new(),
            events: Vec::new(),
        },
        citations,
    )
}

fn cite(raw: &RawStore, hash: &str, source: &str, field: &str, excerpt: &str) -> Citation {
    Citation::from_store(
        raw,
        &mut NoopPdf,
        CitationInput {
            fact_source: source.into(),
            field: field.into(),
            announcement_id: source.into(),
            page: 1,
            raw_sha256: hash.into(),
            excerpt: excerpt.into(),
            verified_by: "synthetic-reviewer".into(),
            verified_at: "2026-01-01T00:00:00Z".into(),
        },
    )
    .unwrap()
}

fn mvp6_fixed() -> FixedUniverseSpec {
    FixedUniverseSpec::new(
        "synthetic-fixed@1",
        ["159919", "159560", "159522", "159612", "159952"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
    .unwrap()
}

#[test]
fn assembles_listing_termination_code_reuse_and_legal_name_change() {
    let world = world(
        &[
            row("159919", "合成沪深300ETF", 2, Some(6), 1, "", "official"),
            row("159560", "合成中证500ETF-A", 1, Some(6), 1, "", "official"),
            row("159560", "合成中证500ETF-B", 7, None, 6, "", "official"),
            row("159522", "合成创业板ETF-A", 1, None, 1, "", "official"),
            row("159522", "合成创业板ETF-B", 5, None, 4, "", "official").legal_name_change(),
            row("159612", "合成红利ETF", 3, None, 2, "", "official"),
            row("159952", "合成科创ETF", 4, None, 3, "", "official"),
        ],
        &[
            (1, &["159560", "159522"]),
            (2, &["159919", "159560", "159522"]),
            (3, &["159919", "159560", "159522", "159612"]),
            (4, &["159919", "159560", "159522", "159612", "159952"]),
            (5, &["159919", "159560", "159522", "159612", "159952"]),
            (6, &["159522", "159612", "159952"]),
            (7, &["159560", "159522", "159612", "159952"]),
            (8, &["159560", "159522", "159612", "159952"]),
            (9, &["159560", "159522", "159612", "159952"]),
            (10, &["159560", "159522", "159612", "159952"]),
        ],
        &[
            ("159919", &[2, 3, 4, 5]),
            ("159560", &[1, 2, 3, 4, 5, 7, 8, 9, 10]),
            ("159522", &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]),
            ("159612", &[3, 4, 5, 6, 7, 8, 9, 10]),
            ("159952", &[4, 5, 6, 7, 8, 9, 10]),
        ],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    assert!(assembly.catalog.coverage_gaps().is_empty());
    assert!(assembly.report.excluded_codes.is_empty());
    assert_eq!(assembly.input.schema_version, 2);
    assert!(assembly.input.broad_indices.is_none());
    assert_eq!(assembly.report.lifecycles, 7);
    assert!(assembly.input.coverage.gaps.is_empty());

    // Listing and termination boundaries.
    assert!(assembly.catalog.resolve_row("159919", d(2)).is_ok());
    assert!(assembly.catalog.resolve_row("159919", d(1)).is_err());
    assert!(assembly.catalog.resolve_row("159919", d(6)).is_err());

    // Code reuse: one InstrumentId cannot span the code-table absence.
    let first = assembly.catalog.resolve_row("159560", d(5)).unwrap();
    let second = assembly.catalog.resolve_row("159560", d(7)).unwrap();
    assert_ne!(first, second);
    assert!(assembly.catalog.resolve_row("159560", d(6)).is_err());

    // Legal-name change: the same continuous code changes identity.
    let old = assembly.catalog.resolve_row("159522", d(4)).unwrap();
    let new = assembly.catalog.resolve_row("159522", d(5)).unwrap();
    assert_ne!(old, new);
    let names = assembly
        .input
        .lifecycles
        .iter()
        .filter(|lifecycle| lifecycle.native_symbol == "159522")
        .map(|lifecycle| {
            lifecycle.attributes[0]
                .legal_name
                .clone()
                .expect("evidenced legal name")
        })
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["合成创业板ETF-A", "合成创业板ETF-B"]);

    let fixed = mvp6_fixed();
    let day5 = assembly.catalog.fixed_universe(d(5), &fixed).unwrap();
    assert_eq!(day5.members.len(), 5);
    assert_eq!(
        day5.run_spec.history_claim.as_deref(),
        Some("retrospective_static")
    );
    let day6 = assembly.catalog.fixed_universe(d(6), &fixed).unwrap();
    assert_eq!(day6.members.len(), 3);
    assert_eq!(day6.excluded.len(), 4);
    let day7 = assembly.catalog.fixed_universe(d(7), &fixed).unwrap();
    assert_eq!(day7.members.len(), 4);
}

#[test]
fn missing_identity_row_is_excluded_and_reported() {
    let world = world(
        &[
            row("159919", "合成沪深300ETF", 1, None, 1, "", "official"),
            row("159560", "合成中证500ETF", 1, None, 1, "", "official"),
            row("159612", "合成红利ETF", 1, None, 1, "", "official"),
            row("159952", "合成科创ETF", 1, None, 1, "", "official"),
        ],
        &[
            (1, &["159919", "159560", "159522", "159612", "159952"]),
            (2, &["159919", "159560", "159522", "159612", "159952"]),
        ],
        &[
            ("159919", &[1, 2]),
            ("159560", &[1, 2]),
            ("159522", &[1, 2]),
            ("159612", &[1, 2]),
            ("159952", &[1, 2]),
        ],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    assert_eq!(
        assembly
            .report
            .excluded_codes
            .get("159522")
            .map(String::as_str),
        Some("identity_missing")
    );
    assert!(!assembly.report.included_codes.contains("159522"));
    assert_eq!(assembly.report.lifecycles, 4);
    assert!(
        assembly
            .catalog
            .coverage_gaps()
            .contains("mvp6_excluded:159522:identity_missing")
    );
    let fixed = mvp6_fixed();
    let result = assembly.catalog.fixed_universe(d(2), &fixed).unwrap();
    assert_eq!(result.members.len(), 4);
    assert!(
        !result
            .members
            .iter()
            .any(|member| member.symbol().starts_with("159522"))
    );
    assert_eq!(
        result.run_spec.fixed_list_exclusions.as_ref().unwrap()["159522"],
        "identity_missing"
    );
}

#[test]
fn listing_and_termination_disagreements_are_graded_by_evidence() {
    let world = world(
        &[
            // Official listing is earlier than the first code-table appearance:
            // the difference is accepted and disclosed.
            row("159919", "合成沪深300ETF", 2, None, 1, "", "official"),
            // Official termination precedes code-table exit; no Bar exists after
            // termination, so the difference is accepted without exclusion.
            row("159612", "合成红利ETF", 1, Some(6), 1, "", "official"),
            // Secondary evidence disagrees with the code table: exclude.
            row("159952", "合成科创ETF", 4, None, 3, "", "secondary"),
        ],
        &[
            (1, &["159612"]),
            (2, &["159612"]),
            (3, &["159919", "159612"]),
            (4, &["159919", "159612"]),
            (5, &["159919", "159612", "159952"]),
            (6, &["159919", "159612", "159952"]),
            (7, &["159919", "159612", "159952"]),
            (8, &["159919", "159612", "159952"]),
            (9, &["159919", "159612", "159952"]),
            (10, &["159919", "159612", "159952"]),
        ],
        &[
            ("159919", &[3, 4, 5, 6, 7, 8, 9, 10]),
            ("159612", &[1, 2, 3, 4, 5]),
            ("159952", &[5, 6, 7, 8, 9, 10]),
        ],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    let listing = assembly
        .report
        .differences
        .iter()
        .find(|difference| {
            difference.code == "159919"
                && difference.kind == IdentityDifferenceKind::ListingDateVsCodeTableFirst
        })
        .expect("listing difference is reported");
    assert_eq!(listing.expected_date, Some(d(2)));
    assert_eq!(listing.observed_date, Some(d(3)));
    assert_eq!(
        listing.resolution,
        IdentityDifferenceResolution::AcceptedIdentity
    );
    assert_eq!(listing.evidence_level, Some(EvidenceLevel::Official));

    let termination = assembly
        .report
        .differences
        .iter()
        .find(|difference| {
            difference.code == "159612"
                && difference.kind == IdentityDifferenceKind::TerminationVsCodeTablePresence
        })
        .expect("termination difference is reported");
    assert_eq!(termination.expected_date, Some(d(6)));
    assert_eq!(termination.observed_date, Some(d(10)));
    assert_eq!(
        termination.resolution,
        IdentityDifferenceResolution::AcceptedIdentity
    );

    assert_eq!(
        assembly
            .report
            .excluded_codes
            .get("159952")
            .map(String::as_str),
        Some("secondary_listing_disagrees_with_code_table")
    );
    assert!(!assembly.report.included_codes.contains("159952"));
    assert!(assembly.catalog.resolve_row("159952", d(5)).is_err());
    // The official listing date (S2) is retained even though the first TDX
    // appearance is S3; the fixed-list selector honors the announcement.
    let listing_lifecycle = assembly
        .input
        .lifecycles
        .iter()
        .find(|lifecycle| lifecycle.native_symbol == "159919")
        .unwrap();
    assert_eq!(listing_lifecycle.listing.effective_date, d(2));
    assert_eq!(listing_lifecycle.identity_start, Some(d(3)));
    assert!(assembly.catalog.resolve_row("159919", d(3)).is_ok());
    let fixed = mvp6_fixed();
    assert!(
        assembly
            .catalog
            .fixed_universe(d(2), &fixed)
            .unwrap()
            .members
            .iter()
            .any(|member| member.symbol().starts_with("159919"))
    );
    assert!(assembly.catalog.resolve_row("159612", d(6)).is_err());
    assert!(assembly.catalog.resolve_row("159612", d(5)).is_ok());
}

#[test]
fn missing_or_invalid_inputs_fail_closed() {
    let world = world(
        &[row("159919", "合成沪深300ETF", 1, None, 1, "", "official")],
        &[(1, &["159919"]), (2, &["159919"])],
        &[("159919", &[1, 2])],
    );

    let missing = assemble_mvp6_catalog(
        &world.root,
        "dsv:sha256:0000000000000000000000000000000000000000000000000000000000000000",
        &world.facts,
        &world.citations,
    )
    .unwrap_err();
    assert!(missing.to_string().contains("manifest") || missing.to_string().contains("I/O"));

    let mut bad_dates = world.facts.clone();
    bad_dates.identities[0].termination_date = bad_dates.identities[0].listing_date.clone();
    assert!(
        assemble_mvp6_catalog(&world.root, &world.dsv, &bad_dates, &world.citations)
            .unwrap_err()
            .to_string()
            .contains("termination_date must follow")
    );

    let mut bad_code = world.facts.clone();
    bad_code.identities[0].code = "160000".into();
    assert!(
        assemble_mvp6_catalog(&world.root, &world.dsv, &bad_code, &world.citations)
            .unwrap_err()
            .to_string()
            .contains("outside the v4 configured code ranges")
    );

    let missing_citation = world
        .citations
        .iter()
        .filter(|citation| citation.field() != "listing_date")
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &missing_citation)
            .unwrap_err()
            .to_string()
            .contains("missing Raw citation")
    );

    let hash = world.citations[0].raw_sha256().to_owned();
    let hex = hash.strip_prefix("sha256:").unwrap();
    fs::remove_file(world.root.join("raw/sha256").join(&hex[..2]).join(hex)).unwrap();
    let error = assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("unavailable") || error.contains("I/O"),
        "{error}"
    );

    let empty = Mvp6Facts {
        identities: Vec::new(),
        actions: Vec::new(),
        events: Vec::new(),
    };
    assert!(
        assemble_mvp6_catalog(&world.root, &world.dsv, &empty, &world.citations)
            .unwrap_err()
            .to_string()
            .contains("no identity row")
    );
}

#[test]
fn legal_name_change_is_known_only_after_its_publication_date() {
    let world = world(
        &[
            row("159919", "合成ETF-A", 1, None, 1, "", "official"),
            row("159919", "合成ETF-B", 2, None, 2, "", "official").legal_name_change(),
        ],
        &[(1, &["159919"]), (2, &["159919"]), (3, &["159919"])],
        &[("159919", &[1, 2, 3])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    let fixed =
        FixedUniverseSpec::new("synthetic-fixed@1", BTreeSet::from(["159919".to_owned()])).unwrap();
    // The old identity ends on S2 and the new name is published on S2, so the
    // first close that can use the new identity is S3.
    assert!(
        assembly
            .catalog
            .fixed_universe(d(2), &fixed)
            .unwrap()
            .members
            .is_empty()
    );
    assert_eq!(
        assembly
            .catalog
            .fixed_universe(d(3), &fixed)
            .unwrap()
            .members
            .len(),
        1
    );
}

#[test]
fn priced_bar_after_termination_excludes_the_code() {
    let world = world(
        &[
            row("159919", "合成ETF", 1, Some(3), 1, "", "official"),
            row("159612", "合成红利ETF", 1, None, 1, "", "official"),
        ],
        &[
            (1, &["159919", "159612"]),
            (2, &["159919", "159612"]),
            (3, &["159612"]),
            (4, &["159612"]),
        ],
        &[("159919", &[1, 2, 4]), ("159612", &[1, 2, 3, 4])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    assert!(
        assembly
            .report
            .excluded_codes
            .get("159919")
            .is_some_and(|reason| reason.starts_with("priced_bar_outside_lifecycle"))
    );
    assert!(!assembly.report.included_codes.contains("159919"));
    assert_eq!(
        assembly.report.included_codes,
        BTreeSet::from(["159612".to_owned()])
    );
    assert!(assembly.catalog.resolve_row("159919", d(1)).is_err());
    assert!(assembly.catalog.resolve_row("159612", d(1)).is_ok());
}

#[test]
fn identity_without_code_table_presence_stays_in_the_official_lifetime() {
    let world = world(
        &[
            row("159919", "合成ETF", 1, None, 1, "", "official"),
            row("159952", "合成科创ETF", 1, Some(5), 1, "", "official"),
        ],
        &[(1, &["159919"]), (2, &["159919"])],
        &[("159919", &[1, 2])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    // The absence of TDX rows is missing market evidence, not evidence that
    // the officially evidenced ETF is outside the fixed list.
    assert!(!assembly.report.excluded_codes.contains_key("159952"));
    assert!(assembly.report.included_codes.contains("159952"));
    assert!(assembly.report.differences.iter().any(|difference| {
        difference.code == "159952"
            && difference.kind == IdentityDifferenceKind::CodeTableAbsent
            && difference.resolution == IdentityDifferenceResolution::Unresolved
    }));
    assert!(
        assembly
            .catalog
            .coverage_gaps()
            .iter()
            .any(|gap| gap.starts_with("lifecycle_without_code_table_identity:159952:"))
    );
    let lifecycle = assembly
        .catalog
        .input()
        .lifecycles
        .iter()
        .find(|lifecycle| lifecycle.native_symbol == "159952")
        .unwrap();
    assert_eq!(lifecycle.identity_start, Some(d(1)));
    assert_eq!(lifecycle.code_table_first_seen, None);
    let fixed = mvp6_fixed();
    let day2 = assembly.catalog.fixed_universe(d(2), &fixed).unwrap();
    assert!(
        day2.members
            .iter()
            .any(|member| member.symbol().starts_with("159952"))
    );
    assert_eq!(
        day2.run_spec
            .fixed_list_exclusions
            .as_ref()
            .unwrap()
            .get("159952"),
        None
    );
}

#[test]
fn termination_after_the_covered_window_is_omitted() {
    let world = world(
        &[row("159919", "合成ETF", 1, Some(5), 1, "", "official")],
        &[(1, &["159919"]), (2, &["159919"]), (3, &["159919"])],
        &[("159919", &[1, 2, 3])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    assert!(assembly.input.lifecycles[0].termination.is_none());
    assert!(
        assembly
            .report
            .notes
            .iter()
            .any(|note| note.contains("termination_date is after the covered window"))
    );
}

#[test]
fn fixed_list_keeps_a_code_through_a_tdx_exit_before_official_termination() {
    // TDX stops observing 159919 after S5, but the official termination is S7.
    // The S6 gap is missing-market evidence (UNKNOWN), not a reason to drop the
    // ETF from the fixed-list universe before its official termination.
    let world = world(
        &[row(
            "159919",
            "合成沪深300ETF",
            1,
            Some(7),
            1,
            "",
            "official",
        )],
        &[
            (1, &["159919"]),
            (2, &["159919"]),
            (3, &["159919"]),
            (4, &["159919"]),
            (5, &["159919"]),
            (6, &[]),
            (7, &[]),
        ],
        &[("159919", &[1, 2, 3, 4, 5])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    assert!(assembly.report.included_codes.contains("159919"));
    assert!(assembly.report.differences.iter().any(|difference| {
        difference.code == "159919"
            && difference.kind == IdentityDifferenceKind::CodeTableExitBeforeTermination
    }));
    // The Catalog keeps the physical TDX exit (S6) and the official
    // termination (S7); the coverage adapter measures [S1, S7) and reports the
    // missing S6 cell as UNKNOWN (`coverage_denominator_extends_to_official_termination_after_a_tdx_exit`).
    let lifecycle = assembly
        .catalog
        .input()
        .lifecycles
        .iter()
        .find(|lifecycle| lifecycle.native_symbol == "159919")
        .unwrap();
    assert_eq!(lifecycle.identity_end_exclusive, Some(d(6)));
    assert_eq!(
        lifecycle
            .termination
            .as_ref()
            .map(|fact| fact.effective_date),
        Some(d(7))
    );
    let fixed =
        FixedUniverseSpec::new("synthetic-fixed@1", BTreeSet::from(["159919".to_owned()])).unwrap();
    let gap = assembly.catalog.fixed_universe(d(6), &fixed).unwrap();
    assert!(
        gap.members
            .iter()
            .any(|member| member.symbol().starts_with("159919")),
        "official lifetime keeps the ETF after the TDX exit"
    );
    let terminated = assembly.catalog.fixed_universe(d(7), &fixed).unwrap();
    assert!(terminated.members.is_empty());
    assert_eq!(
        terminated.excluded.values().next().map(String::as_str),
        Some("terminated")
    );
}

#[test]
fn fixed_list_keeps_a_code_through_a_gap_before_a_later_successor() {
    // The old 159560 identity legally ends S7, TDX stops observing it after S5,
    // and the successor is not officially listed until S9. S6 is still inside
    // the old official lifetime, so the code remains a member there; S7/S8 are
    // between the old termination and the successor listing, and S9 is the
    // successor identity.
    let world = world(
        &[
            row("159560", "合成中证500ETF-A", 1, Some(7), 1, "", "official"),
            row("159560", "合成中证500ETF-B", 9, None, 8, "", "official"),
        ],
        &[
            (1, &["159560"]),
            (2, &["159560"]),
            (3, &["159560"]),
            (4, &["159560"]),
            (5, &["159560"]),
            (6, &[]),
            (7, &[]),
            (8, &["159560"]),
            (9, &["159560"]),
            (10, &["159560"]),
        ],
        &[("159560", &[1, 2, 3, 4, 5, 9, 10])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    assert!(assembly.report.included_codes.contains("159560"));
    let fixed = mvp6_fixed();
    let old_id = assembly.catalog.resolve_row("159560", d(5)).unwrap();
    let successor_id = assembly.catalog.resolve_row("159560", d(9)).unwrap();
    assert_ne!(old_id, successor_id);

    let gap = assembly.catalog.fixed_universe(d(6), &fixed).unwrap();
    assert_eq!(
        gap.members
            .iter()
            .filter(|member| member.symbol().starts_with("159560"))
            .collect::<Vec<_>>(),
        vec![&old_id]
    );
    for day in [7, 8] {
        let between = assembly.catalog.fixed_universe(d(day), &fixed).unwrap();
        assert!(
            !between
                .members
                .iter()
                .any(|member| member.symbol().starts_with("159560")),
            "no 159560 member outside the old/successor official lifetimes on S{day}"
        );
    }
    let after = assembly.catalog.fixed_universe(d(9), &fixed).unwrap();
    assert_eq!(
        after
            .members
            .iter()
            .filter(|member| member.symbol().starts_with("159560"))
            .collect::<Vec<_>>(),
        vec![&successor_id]
    );
}

#[test]
fn reused_code_bare_post_termination_run_without_a_priced_bar_is_reported_not_excluded() {
    // Old 159560 ends S4. The successor is officially listed S5 and terminates
    // S6, but TDX only shows it in a later S7-S8 run with no priced Bars. The
    // later run must not exclude the code; it is a disclosed termination
    // disagreement and the official lifetime S5-S6 remains in the universe.
    let world = world(
        &[
            row("159560", "合成中证500ETF-A", 1, Some(4), 1, "", "official"),
            row("159560", "合成中证500ETF-B", 5, Some(6), 4, "", "official"),
        ],
        &[
            (1, &["159560"]),
            (2, &["159560"]),
            (3, &["159560"]),
            (4, &[]),
            (5, &[]),
            (6, &[]),
            (7, &["159560"]),
            (8, &["159560"]),
        ],
        &[("159560", &[1, 2, 3])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    assert!(assembly.report.included_codes.contains("159560"));
    assert!(!assembly.report.excluded_codes.contains_key("159560"));
    let difference = assembly
        .report
        .differences
        .iter()
        .find(|difference| {
            difference.code == "159560"
                && difference.kind == IdentityDifferenceKind::TerminationVsCodeTablePresence
        })
        .expect("post-termination TDX presence is reported");
    assert_eq!(difference.expected_date, Some(d(6)));
    assert_eq!(difference.observed_date, Some(d(8)));
    assert_eq!(
        difference.resolution,
        IdentityDifferenceResolution::AcceptedIdentity
    );
    let fixed = mvp6_fixed();
    let listed = assembly.catalog.fixed_universe(d(5), &fixed).unwrap();
    assert!(
        listed
            .members
            .iter()
            .any(|member| member.symbol().starts_with("159560"))
    );
    let terminated = assembly.catalog.fixed_universe(d(7), &fixed).unwrap();
    assert!(
        !terminated
            .members
            .iter()
            .any(|member| member.symbol().starts_with("159560"))
    );
}

#[test]
fn reused_code_successor_may_appear_in_tdx_before_its_official_listing() {
    // Old 159560 identity ends S4. The reused code reappears in the TDX table at
    // S5, but the successor's official listing is S6 and its first priced Bar is
    // S6. The S5 presence has no priced Bar, so it is a disclosed difference,
    // not a reason to exclude the reused code.
    let world = world(
        &[
            row("159560", "合成中证500ETF-A", 1, Some(4), 1, "", "official"),
            row("159560", "合成中证500ETF-B", 6, None, 5, "", "official"),
        ],
        &[
            (1, &["159560"]),
            (2, &["159560"]),
            (3, &["159560"]),
            (4, &[]),
            (5, &["159560"]),
            (6, &["159560"]),
            (7, &["159560"]),
            (8, &["159560"]),
            (9, &["159560"]),
            (10, &["159560"]),
        ],
        &[("159560", &[1, 2, 3, 6, 7, 8, 9, 10])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    assert!(assembly.report.included_codes.contains("159560"));
    assert!(!assembly.report.excluded_codes.contains_key("159560"));
    let difference = assembly
        .report
        .differences
        .iter()
        .find(|difference| {
            difference.code == "159560"
                && difference.kind == IdentityDifferenceKind::ListingDateVsCodeTableFirst
        })
        .expect("pre-listing successor presence is disclosed");
    assert_eq!(difference.expected_date, Some(d(6)));
    assert_eq!(difference.observed_date, Some(d(5)));
    assert_eq!(
        difference.resolution,
        IdentityDifferenceResolution::AcceptedIdentity
    );
    // The successor's price identity begins on the official listing date.
    assert!(assembly.catalog.resolve_row("159560", d(6)).is_ok());
    assert!(assembly.catalog.resolve_row("159560", d(5)).is_err());
}

#[test]
fn priced_bar_after_code_table_exit_is_reported_not_excluded() {
    // 159919 is officially alive S1-S8, but TDX stops observing it after S3 and
    // still prices S5. The S5 Bar is inside the official lifespan, so it is a
    // source disagreement (UNKNOWN interval), not a legal termination: the code
    // stays in the fixed universe and the Bar is reported.
    let world = world(
        &[
            row("159919", "合成ETF", 1, Some(8), 1, "", "official"),
            row("159612", "合成红利ETF", 1, None, 1, "", "official"),
        ],
        &[
            (1, &["159919", "159612"]),
            (2, &["159919", "159612"]),
            (3, &["159919", "159612"]),
            (4, &["159612"]),
            (5, &["159612"]),
        ],
        &[("159919", &[1, 2, 3, 5]), ("159612", &[1, 2, 3, 4, 5])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    assert!(!assembly.report.excluded_codes.contains_key("159919"));
    assert!(assembly.report.included_codes.contains("159919"));
    let difference = assembly
        .report
        .differences
        .iter()
        .find(|difference| {
            difference.code == "159919"
                && difference.kind == IdentityDifferenceKind::PricedBarOutsideTdxPresence
        })
        .expect("in-official Bar beyond TDX presence is reported");
    assert_eq!(difference.observed_date, Some(d(5)));
    assert_eq!(
        difference.resolution,
        IdentityDifferenceResolution::Unresolved
    );
    // The Bar is inside the official lifetime but the physical TDX identity
    // ended at S4, so no price row resolves at S5; the fixed universe still
    // includes the code (and the coverage grid reports S5 as UNKNOWN).
    assert!(assembly.catalog.resolve_row("159919", d(5)).is_err());
    let fixed = mvp6_fixed();
    let day5 = assembly.catalog.fixed_universe(d(5), &fixed).unwrap();
    assert!(
        day5.members
            .iter()
            .any(|member| member.symbol().starts_with("159919"))
    );
}

#[test]
fn missing_or_ambiguous_announcement_date_fails_closed() {
    let world = world(
        &[row("159919", "合成ETF", 1, None, 1, "", "official")],
        &[(1, &["159919"]), (2, &["159919"])],
        &[("159919", &[1, 2])],
    );
    let raw = RawStore::open(&world.root).unwrap();
    let source = "src-159919-0";
    let document = format!("公告编号：{source}\n基金代码：159919\n上市日期：2023-01-01");
    let hash = raw
        .put(
            document.as_bytes(),
            SourceRecordInput {
                content_type: "application/octet-stream".into(),
                source_kind: SourceKind::Fixture,
                source_id: "cninfo".into(),
                request: json!({"url": "https://example.test/no-announcement-date"}),
                observed_at: "synthetic".into(),
                ingested_by: "synthetic".into(),
            },
        )
        .unwrap();
    let replacement = cite(&raw, &hash, source, "listing_date", "上市日期：2023-01-01");
    let citations = world
        .citations
        .iter()
        .filter(|citation| {
            !(citation.fact_source() == source && citation.field() == "listing_date")
        })
        .cloned()
        .chain(std::iter::once(replacement))
        .collect::<Vec<_>>();
    let error = assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &citations)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("cannot evidence the announcement date"),
        "{error}"
    );

    let hash = world.citations[0].raw_sha256().to_owned();
    let bytes = raw.get(&hash).unwrap();
    raw.put(
        &bytes,
        SourceRecordInput {
            content_type: "application/octet-stream".into(),
            source_kind: SourceKind::Fixture,
            source_id: "cninfo".into(),
            request: json!({
                "url": "https://static.cninfo.com.cn/finalpage/2023-01-02/src-159919-0.PDF"
            }),
            observed_at: "synthetic".into(),
            ingested_by: "synthetic".into(),
        },
    )
    .unwrap();
    let error = assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations)
        .unwrap_err()
        .to_string();
    assert!(error.contains("ambiguous announcement dates"), "{error}");
}

#[test]
fn pre_window_listing_is_available_at_the_first_covered_session() {
    let world = world(
        &[row("159919", "合成ETF", 1, None, 1, "", "official")],
        &[(2, &["159919"]), (3, &["159919"])],
        &[("159919", &[2, 3])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    assert_eq!(assembly.input.lifecycles[0].listing.effective_date, d(1));
    assert_eq!(assembly.input.lifecycles[0].identity_start, Some(d(2)));
    let fixed =
        FixedUniverseSpec::new("synthetic-fixed@1", BTreeSet::from(["159919".to_owned()])).unwrap();
    assert_eq!(
        assembly
            .catalog
            .fixed_universe(d(2), &fixed)
            .unwrap()
            .members
            .len(),
        1
    );
}

#[test]
fn unresolved_org_id_is_excluded_instead_of_failing_the_run() {
    let world = world(
        &[
            row("159919", "合成ETF", 1, None, 1, "", "official"),
            row("159952", "合成科创ETF", 1, None, 1, "", "official"),
        ],
        &[(1, &["159919", "159952"]), (2, &["159919", "159952"])],
        &[("159919", &[1, 2]), ("159952", &[1, 2])],
    );
    let mut facts = world.facts.clone();
    facts
        .identities
        .iter_mut()
        .find(|fact| fact.code == "159952")
        .unwrap()
        .org_id
        .clear();
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &facts, &world.citations).unwrap();
    assert_eq!(
        assembly
            .report
            .excluded_codes
            .get("159952")
            .map(String::as_str),
        Some("org_id_unresolved")
    );
    assert!(assembly.report.included_codes.contains("159919"));
    assert!(
        assembly
            .catalog
            .coverage_gaps()
            .contains("mvp6_excluded:159952:org_id_unresolved")
    );
}

#[test]
fn successor_identity_must_declare_a_legal_name_change() {
    let world = world(
        &[
            row("159919", "合成ETF-A", 1, None, 1, "", "official"),
            row("159919", "合成ETF-B", 2, None, 2, "", "official"),
            row("159612", "合成红利ETF", 1, None, 1, "", "official"),
        ],
        &[
            (1, &["159919", "159612"]),
            (2, &["159919", "159612"]),
            (3, &["159919", "159612"]),
        ],
        &[("159919", &[1, 2, 3]), ("159612", &[1, 2, 3])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    // The unmarked successor is no longer inferred as a name change; the
    // shared #229 row contract requires an explicit change_kind.
    assert!(
        assembly.report.excluded_codes["159919"]
            .contains("must declare change_kind=legal_name_change")
    );
    assert!(!assembly.report.included_codes.contains("159919"));
    assert!(assembly.report.included_codes.contains("159612"));
}

#[test]
fn code_table_presence_before_listing_without_a_priced_bar_is_disclosed_not_excluded() {
    // The TDX code table starts on S1 but the official listing is S3. No priced
    // Bar exists before S3, so official dates win: the code stays in the Catalog
    // and the difference is reported (P27-19). Only a priced Bar outside the
    // official interval excludes a code.
    let world = world(
        &[row("159919", "合成沪深300ETF", 3, None, 2, "", "official")],
        &[
            (1, &["159919"]),
            (2, &["159919"]),
            (3, &["159919"]),
            (4, &["159919"]),
        ],
        &[("159919", &[3, 4])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    assert!(assembly.report.included_codes.contains("159919"));
    let difference = assembly
        .report
        .differences
        .iter()
        .find(|difference| {
            difference.code == "159919"
                && difference.kind == IdentityDifferenceKind::ListingDateVsCodeTableFirst
        })
        .expect("pre-listing TDX presence is disclosed");
    assert_eq!(difference.expected_date, Some(d(3)));
    assert_eq!(difference.observed_date, Some(d(1)));
    assert_eq!(
        difference.resolution,
        IdentityDifferenceResolution::AcceptedIdentity
    );
    // The lifecycle identity still begins at the TDX appearance, but no price
    // row resolves before the official listing.
    assert!(assembly.catalog.resolve_row("159919", d(2)).is_err());
    assert!(assembly.catalog.resolve_row("159919", d(3)).is_ok());
}

#[test]
fn bare_pre_listing_code_table_run_without_a_priced_bar_is_reported_not_excluded() {
    // TDX contains the code on S1-S2, drops it on S3, then shows it on S4-S5.
    // The official listing is S5 and the only priced Bar is S5. The bare S1-S2
    // presence must not exclude the code; it is reported as outside the
    // evidenced lifecycle (P27-19).
    let world = world(
        &[row("159919", "合成沪深300ETF", 5, None, 4, "", "official")],
        &[
            (1, &["159919"]),
            (2, &["159919"]),
            (3, &[]),
            (4, &["159919"]),
            (5, &["159919"]),
        ],
        &[("159919", &[5])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    assert!(assembly.report.included_codes.contains("159919"));
    let presence = assembly
        .report
        .differences
        .iter()
        .find(|difference| {
            difference.kind == IdentityDifferenceKind::CodeTablePresenceOutsideLifecycle
        })
        .expect("bare pre-listing presence is reported");
    assert_eq!(presence.observed_date, Some(d(1)));
    assert_eq!(
        presence.resolution,
        IdentityDifferenceResolution::Unresolved
    );
}

#[test]
fn bare_post_termination_code_table_run_without_a_priced_bar_is_reported_not_excluded() {
    // The code is present on S1-S2, drops on S3, and reappears on S4-S5 without
    // a priced Bar. The official termination is S3, so the reappearance is a
    // bare TDX presence, not a reason to exclude the code.
    let world = world(
        &[row(
            "159919",
            "合成沪深300ETF",
            1,
            Some(3),
            1,
            "",
            "official",
        )],
        &[
            (1, &["159919"]),
            (2, &["159919"]),
            (3, &[]),
            (4, &["159919"]),
            (5, &["159919"]),
        ],
        &[("159919", &[1, 2])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    assert!(assembly.report.included_codes.contains("159919"));
    let presence = assembly
        .report
        .differences
        .iter()
        .find(|difference| {
            difference.kind == IdentityDifferenceKind::CodeTablePresenceOutsideLifecycle
        })
        .expect("bare post-termination presence is reported");
    assert_eq!(presence.observed_date, Some(d(4)));
}

#[test]
fn contract_citation_is_required_and_must_state_the_legal_name() {
    let world = world(
        &[row("159919", "合成沪深300ETF", 1, None, 1, "", "official")],
        &[(1, &["159919"]), (2, &["159919"])],
        &[("159919", &[1, 2])],
    );
    let without_contract = world
        .citations
        .iter()
        .filter(|citation| citation.field() != "contract")
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &without_contract)
            .unwrap_err()
            .to_string()
            .contains("field contract")
    );

    let raw = RawStore::open(&world.root).unwrap();
    let contract = world
        .citations
        .iter()
        .find(|citation| citation.field() == "contract")
        .unwrap();
    let mut citations = without_contract;
    citations.push(cite(
        &raw,
        contract.raw_sha256(),
        "src-159919-0",
        "contract",
        "基金类型：指数型基金",
    ));
    assert!(
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &citations)
            .unwrap_err()
            .to_string()
            .contains("does not state its legal name")
    );
}

#[test]
fn contract_raw_sha256_comes_from_its_own_attachment() {
    let world = world(
        &[row("159919", "合成沪深300ETF", 1, None, 1, "", "official")],
        &[(1, &["159919"]), (2, &["159919"])],
        &[("159919", &[1, 2])],
    );
    let raw = RawStore::open(&world.root).unwrap();
    let source = "src-159919-0";
    let contract_document = format!("公告编号：{source}\n基金合同\n基金全称：合成沪深300ETF");
    let contract_hash = raw
        .put(
            contract_document.as_bytes(),
            SourceRecordInput {
                content_type: "application/pdf".into(),
                source_kind: SourceKind::Fixture,
                source_id: "cninfo".into(),
                request: json!({
                    "url": format!("https://static.cninfo.com.cn/finalpage/2023-01-01/{source}-contract.PDF"),
                    "announcement_id": source,
                }),
                observed_at: "synthetic".into(),
                ingested_by: "synthetic".into(),
            },
        )
        .unwrap();
    let mut citations = world
        .citations
        .iter()
        .filter(|citation| citation.field() != "contract")
        .cloned()
        .collect::<Vec<_>>();
    citations.push(cite(
        &raw,
        &contract_hash,
        source,
        "contract",
        "基金合同\n基金全称：合成沪深300ETF",
    ));
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &citations).unwrap();
    let lifecycle = &assembly.input.lifecycles[0];
    assert_eq!(lifecycle.contract_raw_sha256, contract_hash);
    assert_ne!(
        lifecycle.contract_raw_sha256,
        lifecycle.attributes[0].evidence.raw_sha256
    );
}

#[test]
fn category_citation_must_support_the_declared_category() {
    let world = world(
        &[row("159919", "合成沪深300ETF", 1, None, 1, "", "official")],
        &[(1, &["159919"]), (2, &["159919"])],
        &[("159919", &[1, 2])],
    );
    let raw = RawStore::open(&world.root).unwrap();
    let original = world
        .citations
        .iter()
        .find(|citation| citation.field() == "fund_category")
        .unwrap();
    let mut citations = world
        .citations
        .iter()
        .filter(|citation| citation.field() != "fund_category")
        .cloned()
        .collect::<Vec<_>>();
    citations.push(cite(
        &raw,
        original.raw_sha256(),
        "src-159919-0",
        "fund_category",
        "基金代码：159919",
    ));
    assert!(
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &citations)
            .unwrap_err()
            .to_string()
            .contains("fund_category 指数型基金 for 159919")
    );
}

#[test]
fn attribute_citations_must_share_one_raw_page() {
    let world = world(
        &[row("159919", "合成沪深300ETF", 1, None, 1, "", "official")],
        &[(1, &["159919"]), (2, &["159919"])],
        &[("159919", &[1, 2])],
    );
    let raw = RawStore::open(&world.root).unwrap();
    let source = "src-159919-0";
    let document = format!("公告编号：{source}\n基金类型：指数型基金");
    let hash = raw
        .put(
            document.as_bytes(),
            SourceRecordInput {
                content_type: "application/octet-stream".into(),
                source_kind: SourceKind::Fixture,
                source_id: "cninfo".into(),
                request: json!({
                    "url": format!("https://static.cninfo.com.cn/finalpage/2023-01-01/{source}.PDF"),
                    "announcement_id": source,
                }),
                observed_at: "synthetic".into(),
                ingested_by: "synthetic".into(),
            },
        )
        .unwrap();
    let mut citations = world
        .citations
        .iter()
        .filter(|citation| citation.field() != "fund_category")
        .cloned()
        .collect::<Vec<_>>();
    citations.push(cite(
        &raw,
        &hash,
        source,
        "fund_category",
        "基金类型：指数型基金",
    ));
    // AttributeFact carries one Evidence, so citations on another Raw page are
    // rejected instead of being attributed to the legal-name page.
    assert!(
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &citations)
            .unwrap_err()
            .to_string()
            .contains("not evidenced by the same Raw page")
    );
}

#[test]
fn assembled_input_matches_an_equivalent_handwritten_input() {
    let world = world(
        &[row("159919", "合成沪深300ETF", 1, None, 1, "", "official")],
        &[(1, &["159919"]), (2, &["159919"]), (3, &["159919"])],
        &[("159919", &[1, 2, 3])],
    );
    let assembly =
        assemble_mvp6_catalog(&world.root, &world.dsv, &world.facts, &world.citations).unwrap();
    let hash = world.citations[0].raw_sha256().to_owned();
    let announcement = d(1);
    let evidence = |locator: &str| Evidence {
        raw_sha256: hash.clone(),
        announcement_id: "src-159919-0".into(),
        locator: locator.into(),
        publication: Publication::DateOnly { date: announcement },
    };
    let ranges = vec![CodeRange {
        start: 159_000,
        end: 159_999,
    }];
    let expected = CatalogInput {
        schema_version: 2,
        rule_version: "mvp6-catalog-assembly@1".into(),
        calendar_start: announcement.min(d(1)),
        calendar_end: d(3),
        calendar_raw_sha256: world.manifest_inputs.clone(),
        sessions: world.sessions.clone(),
        coverage: Coverage {
            start: d(1),
            end: d(3),
            scan_method: "mvp6-tdx-v4@1: filtered to identity-table codes; first/last traded Session per evidenced lifecycle".into(),
            announcement_method: "mvp6-cninfo-fixed-list@1: per-field frozen Raw citations; no pagination completeness claim".into(),
            announcement_raw_sha256: BTreeSet::from([hash.clone()]),
            gaps: ["159560", "159522", "159612", "159952"]
                .into_iter()
                .map(|code| format!("mvp6_excluded:{code}:identity_missing"))
                .collect(),
            resolutions: Vec::new(),
        },
        lifecycles: vec![Lifecycle {
            native_symbol: "159919".into(),
            contract_raw_sha256: hash.clone(),
            listing: LifecycleFact {
                effective_date: d(1),
                evidence: evidence("src-159919-0:p1"),
            },
            termination: None,
            attributes: vec![AttributeFact {
                effective_date: d(1),
                category: FundCategory::Equity,
                index_id: String::new(),
                evidence: evidence("src-159919-0:p1"),
                legal_name: Some("合成沪深300ETF".into()),
            }],
            identity_start: Some(d(1)),
            identity_end_exclusive: None,
            code_table_first_seen: None,
            code_table_last_seen: None,
            code_table_exit_date: None,
        }],
        scans: vec![ScanObservation {
            native_symbol: "159919".into(),
            status: ScanStatus::Observed,
            raw_sha256: world.code_table_raws[&d(1)].clone(),
            first_observed: Some(d(1)),
            last_observed: Some(d(3)),
            identity_start: Some(d(1)),
        }],
        broad_indices: None,
        code_ranges: Some(ranges.clone()),
        code_tables: [1, 2, 3]
            .into_iter()
            .map(|day| CodeTableSnapshot {
                session_date: d(day),
                raw_sha256: world.code_table_raws[&d(day)].clone(),
                symbols: BTreeSet::from(["159919".to_owned()]),
                code_ranges: ranges.clone(),
            })
            .collect(),
    };
    assert_eq!(
        serde_json::to_value(&assembly.input).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    let manual = prajna_research::pit_universe::HistoricalCatalog::build(
        expected,
        &RawStore::open(&world.root).unwrap(),
    )
    .unwrap();
    assert_eq!(
        manual.version().unwrap(),
        assembly.catalog.version().unwrap()
    );
}
