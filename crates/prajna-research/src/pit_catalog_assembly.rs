//! Offline Catalog assembly from a published v4 Dataset and the MVP-6 public
//! identity table (#221).
//!
//! Inputs are local and frozen: a `szse-etf-frozen@4` Dataset Version (sessions,
//! daily TDX code tables, traded Bars) plus `configs/mvp-6/identity.csv` and the
//! verified Raw citations that bind each identity field to an announcement. No
//! network request is made and no real source bytes or hashes are written to
//! the repository.
//!
//! The assembly is deliberately a *fixed-list* Catalog: only codes with an
//! identity row enter the Catalog, and the daily TDX code tables are narrowed
//! to those codes before the v2 lifecycle validator runs. Codes outside the
//! identity table are out of scope for this `retrospective_static` Universe;
//! codes from [`MVP6_FIXED_ETF_CANDIDATES`] that lack an identity row are
//! reported as exclusions instead of creating un-evidenced lifecycles.
//!
//! Publication time is never inferred from local capture time or from an
//! effective date. Each identity field resolves to a Raw object whose frozen
//! provenance or announcement JSON carries the announcement date; a missing or
//! ambiguous date fails closed.
//!
//! A continuous code whose legal full name changes needs two rows: the
//! successor declares `change_kind=legal_name_change` and #229's shared
//! validator accepts that one non-terminated overlap. Every row also carries a
//! local `contract` citation whose Raw hash becomes `contract_raw_sha256`;
//! name, category and index citations must share one Raw page because
//! `AttributeFact` has one evidence. The coverage layer measures UNKNOWN over
//! the official listing/termination lifetime, not the TDX code-table presence
//! bounds.

use crate::pit_universe::{
    AttributeFact, CatalogError, CatalogInput, CodeRange, CodeTableSnapshot, Coverage, Evidence,
    FundCategory, HistoricalCatalog, Lifecycle, LifecycleFact, MVP6_FIXED_ETF_CANDIDATES,
    Publication, ScanObservation, ScanStatus, in_code_scope, raw_hash,
};
use chrono::NaiveDate;
use prajna_data::cninfo::{Citation, EvidenceLevel, LEGAL_NAME_CHANGE, Mvp6Facts};
use prajna_data::{
    RawStore, TdxCodeTableRow, bars_from_record_batch, read_dataset_table, read_manifest,
    sessions_from_record_batch, tdx_code_table_from_record_batch,
};
use prajna_domain::Session;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// Version of the extraction/selection rules. It enters the Catalog identity.
pub const ASSEMBLY_RULE_VERSION: &str = "mvp6-catalog-assembly@1";
/// Contract tag written into the local report.
pub const ASSEMBLY_REPORT_CONTRACT: &str = "mvp-6-catalog-assembly-v1";

pub const ASSEMBLY_SCAN_METHOD: &str = "mvp6-tdx-v4@1: filtered to identity-table codes; first/last traded Session per evidenced lifecycle";
pub const ASSEMBLY_ANNOUNCEMENT_METHOD: &str =
    "mvp6-cninfo-fixed-list@1: per-field frozen Raw citations; no pagination completeness claim";

type Result<T> = std::result::Result<T, CatalogError>;

fn error(message: impl Into<String>) -> CatalogError {
    CatalogError(message.into())
}

/// One identity/TDX discrepancy. `resolution` is either `accepted_identity`
/// (official evidence wins and the difference is disclosed), `excluded`
/// (the code is removed from this Catalog), or `unresolved` (no Catalog
/// lifecycle can be evidenced).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityDifferenceKind {
    CodeTableAbsent,
    ListingDateVsCodeTableFirst,
    TerminationVsCodeTablePresence,
    CodeTableExitBeforeTermination,
    PricedBarOutsideLifecycle,
    PricedBarOutsideTdxPresence,
    CodeTablePresenceOutsideLifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityDifferenceResolution {
    AcceptedIdentity,
    Excluded,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityDifference {
    pub code: String,
    pub kind: IdentityDifferenceKind,
    pub expected_date: Option<NaiveDate>,
    pub observed_date: Option<NaiveDate>,
    pub evidence_level: Option<EvidenceLevel>,
    pub resolution: IdentityDifferenceResolution,
}

/// Local-only assembly report. It contains dates and no market values, source
/// bytes, PDFs or Raw hashes; keep it under `~/PrajnaData/mvp-6/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogAssemblyReport {
    pub contract: String,
    pub dsv: String,
    pub catalog_version: String,
    pub rule_version: String,
    pub identity_rows: usize,
    pub lifecycles: usize,
    pub included_codes: BTreeSet<String>,
    pub excluded_codes: BTreeMap<String, String>,
    pub differences: Vec<IdentityDifference>,
    pub catalog_coverage_gaps: BTreeSet<String>,
    pub notes: Vec<String>,
}

/// A built Catalog and the report that explains every accepted or excluded
/// identity.
#[derive(Debug, Clone)]
pub struct CatalogAssembly {
    pub input: CatalogInput,
    pub catalog: HistoricalCatalog,
    pub report: CatalogAssemblyReport,
}

struct V4Data {
    dsv: String,
    manifest_inputs: BTreeSet<String>,
    sessions: Vec<Session>,
    code_tables: Vec<TdxCodeTableRow>,
    code_ranges: Vec<CodeRange>,
    bar_dates: BTreeMap<String, BTreeSet<NaiveDate>>,
    first_session: NaiveDate,
    last_session: NaiveDate,
}

#[derive(Debug, Clone)]
struct IdentitySeed {
    code: String,
    listing: NaiveDate,
    termination: Option<NaiveDate>,
    legal_name: String,
    tracked_index: String,
    category: FundCategory,
    evidence_level: EvidenceLevel,
    legal_name_change: bool,
    /// First Session on which a legal-name change is knowable; `None` for an
    /// initial listing or reused code. The official `listing` date remains the
    /// effective name-change Session.
    available_from: Option<NaiveDate>,
    contract_raw_sha256: String,
    listing_evidence: Evidence,
    termination_evidence: Option<Evidence>,
    attribute_evidence: Evidence,
}

impl IdentitySeed {
    fn attribute(&self) -> AttributeFact {
        AttributeFact {
            effective_date: self.listing,
            category: self.category.clone(),
            index_id: self.tracked_index.clone(),
            evidence: self.attribute_evidence.clone(),
            legal_name: Some(self.legal_name.clone()),
        }
    }

    fn lifecycle(
        &self,
        identity_start: NaiveDate,
        termination: Option<NaiveDate>,
    ) -> Result<Lifecycle> {
        let termination = match termination {
            Some(date) => Some(LifecycleFact {
                effective_date: date,
                evidence: self.termination_evidence.clone().ok_or_else(|| {
                    error(format!(
                        "termination_date for {} has no verified Raw citation",
                        self.code
                    ))
                })?,
            }),
            None => None,
        };
        Ok(Lifecycle {
            native_symbol: self.code.clone(),
            contract_raw_sha256: self.contract_raw_sha256.clone(),
            listing: LifecycleFact {
                effective_date: self.listing,
                evidence: self.listing_evidence.clone(),
            },
            termination,
            attributes: vec![self.attribute()],
            identity_start: Some(identity_start),
            identity_end_exclusive: None,
            identity_available_from: self.available_from,
            code_table_first_seen: None,
            code_table_last_seen: None,
            code_table_exit_date: None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PresenceRun {
    first: NaiveDate,
    last: NaiveDate,
    /// First covered Session after the run, if any.
    exit: Option<NaiveDate>,
}

#[derive(Debug, Clone)]
struct PlannedLifecycle {
    seed: IdentitySeed,
    identity_start: NaiveDate,
    end_exclusive: Option<NaiveDate>,
    run: PresenceRun,
    name_change: bool,
    /// False for an officially evidenced identity with no TDX code-table
    /// presence: its physical price lifecycle is empty, but its official
    /// lifetime remains a fixed-list member for UNKNOWN coverage.
    has_presence: bool,
}

struct Plan {
    lifecycles: Vec<PlannedLifecycle>,
    excluded: BTreeMap<String, String>,
    differences: Vec<IdentityDifference>,
    notes: Vec<String>,
}

/// Builds a fixed-list MVP-6 Catalog from local frozen inputs.
///
/// `root` is the local data root (`~/PrajnaData/mvp-6`), which contains both the
/// Raw Store and the published `normalized/` tables. `facts` is parsed from the
/// public identity table; `citations` are the verified per-field Raw citations.
pub fn assemble_mvp6_catalog(
    root: &Path,
    dsv: &str,
    facts: &Mvp6Facts,
    citations: &[Citation],
) -> Result<CatalogAssembly> {
    let raw = RawStore::open(root).map_err(|e| error(e.to_string()))?;
    let identity_rows = facts.identities.len();
    let unresolved_org = facts
        .identities
        .iter()
        .filter(|fact| fact.org_id.trim().is_empty())
        .map(|fact| fact.code.clone())
        .collect::<BTreeSet<_>>();
    let mut filtered = facts.clone();
    if !unresolved_org.is_empty() {
        filtered
            .identities
            .retain(|fact| !unresolved_org.contains(&fact.code));
    }
    // Validate only rows that can become lifecycles; an unresolved orgId is a
    // P27-19 exclusion, not a table-wide failure.
    validate_citations(&filtered, citations)?;
    let v4 = load_v4(root, dsv)?;
    let seeds = parse_identities(&v4, &filtered, citations, &raw)?;
    let mut plan = plan(&v4, &seeds)?;
    for code in unresolved_org {
        plan.notes
            .retain(|note| !note.starts_with(&format!("{code}: ")));
        plan.notes.push(format!(
            "{code}: excluded from the Catalog (org_id_unresolved)"
        ));
        plan.excluded.insert(code, "org_id_unresolved".to_owned());
    }
    if plan.lifecycles.is_empty() {
        return Err(error(
            "no identity row can be assembled into a Catalog lifecycle",
        ));
    }
    let input = build_input(&v4, &plan, citations)?;
    let catalog =
        HistoricalCatalog::build(input.clone(), &raw).map_err(|e| error(e.to_string()))?;
    let report = CatalogAssemblyReport {
        contract: ASSEMBLY_REPORT_CONTRACT.into(),
        dsv: v4.dsv.clone(),
        catalog_version: catalog.version().map_err(|e| error(e.to_string()))?,
        rule_version: ASSEMBLY_RULE_VERSION.into(),
        identity_rows,
        lifecycles: input.lifecycles.len(),
        included_codes: plan
            .lifecycles
            .iter()
            .map(|lifecycle| lifecycle.seed.code.clone())
            .collect(),
        excluded_codes: plan.excluded,
        differences: plan.differences,
        catalog_coverage_gaps: catalog.coverage_gaps().clone(),
        notes: plan.notes,
    };
    Ok(CatalogAssembly {
        input,
        catalog,
        report,
    })
}

fn load_v4(root: &Path, dsv: &str) -> Result<V4Data> {
    let manifest = read_manifest(root, dsv).map_err(|e| error(e.to_string()))?;
    if manifest.core.normalizer.id != "szse-etf-frozen" || manifest.core.normalizer.version != "4" {
        return Err(error(format!(
            "Catalog assembly requires szse-etf-frozen@4, found {}@{}",
            manifest.core.normalizer.id, manifest.core.normalizer.version
        )));
    }

    let mut sessions = Vec::new();
    for batch in
        read_dataset_table(root, &manifest, "sessions").map_err(|e| error(e.to_string()))?
    {
        sessions.extend(sessions_from_record_batch(&batch).map_err(|e| error(e.to_string()))?);
    }
    sessions.sort_by_key(Session::session_date);
    if sessions.is_empty() {
        return Err(error("v4 Dataset has no Sessions"));
    }
    if sessions.windows(2).any(|pair| {
        pair[0].session_date() == pair[1].session_date() || pair[0].venue_id() != pair[1].venue_id()
    }) {
        return Err(error("v4 Sessions are duplicated or span multiple venues"));
    }

    let mut code_tables = Vec::new();
    for batch in
        read_dataset_table(root, &manifest, "tdx_code_table").map_err(|e| error(e.to_string()))?
    {
        code_tables
            .extend(tdx_code_table_from_record_batch(&batch).map_err(|e| error(e.to_string()))?);
    }
    code_tables.sort_by_key(|table| table.session_date);
    let session_dates = sessions
        .iter()
        .map(Session::session_date)
        .collect::<Vec<_>>();
    let table_dates = code_tables
        .iter()
        .map(|table| table.session_date)
        .collect::<Vec<_>>();
    if session_dates != table_dates {
        return Err(error(
            "v4 TDX code tables must cover every Session exactly once",
        ));
    }

    let mut ranges: Option<Vec<CodeRange>> = None;
    for table in &code_tables {
        if !raw_hash(&table.source_raw_sha256) {
            return Err(error("v4 TDX code table has an invalid Raw reference"));
        }
        let mut table_ranges = table
            .code_ranges
            .iter()
            .map(|(start, end)| CodeRange {
                start: *start,
                end: *end,
            })
            .collect::<Vec<_>>();
        table_ranges.sort_by_key(|range| range.start);
        match &ranges {
            None => ranges = Some(table_ranges),
            Some(expected) if *expected != table_ranges => {
                return Err(error("v4 TDX code-table scopes differ across Sessions"));
            }
            Some(_) => {}
        }
    }
    let code_ranges = ranges.ok_or_else(|| error("v4 Dataset has no TDX code table"))?;

    let mut bar_dates = BTreeMap::<String, BTreeSet<NaiveDate>>::new();
    for batch in read_dataset_table(root, &manifest, "bars").map_err(|e| error(e.to_string()))? {
        for bar in bars_from_record_batch(&batch, &sessions).map_err(|e| error(e.to_string()))? {
            bar_dates
                .entry(bar.instrument_id().symbol().to_owned())
                .or_default()
                .insert(bar.session_date());
        }
    }

    let first_session = session_dates[0];
    let last_session = *session_dates.last().expect("non-empty sessions");
    let manifest_inputs = manifest
        .core
        .inputs
        .iter()
        .map(|input| input.raw_sha256.clone())
        .collect::<BTreeSet<_>>();
    if manifest_inputs.is_empty() {
        return Err(error(
            "v4 manifest has no frozen Raw inputs for the calendar",
        ));
    }
    Ok(V4Data {
        dsv: manifest.dsv,
        manifest_inputs,
        sessions,
        code_tables,
        code_ranges,
        bar_dates,
        first_session,
        last_session,
    })
}

/// Rejects duplicate `(fact_source, field)` citations and requires every
/// identity row's own `source_page` to be bound to one of its field citations.
/// `read_citations_csv` already enforces uniqueness; this also protects
/// programmatic callers.
fn validate_citations(facts: &Mvp6Facts, citations: &[Citation]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for citation in citations {
        if !seen.insert((citation.fact_source(), citation.field())) {
            return Err(error(format!(
                "duplicate Raw citation for {} field {}",
                citation.fact_source(),
                citation.field()
            )));
        }
    }
    for fact in &facts.identities {
        let page = fact
            .source_page
            .parse::<u32>()
            .map_err(|_| error(format!("invalid source_page for {}", fact.code)))?;
        if !citations.iter().any(|citation| {
            citation.fact_source() == fact.source_announcement_id
                && citation.announcement_id() == fact.source_announcement_id
                && citation.page() == page
        }) {
            return Err(error(format!(
                "source_page {page} of {} is not bound to a cited field",
                fact.source_announcement_id
            )));
        }
    }
    Ok(())
}

/// Identity dates are legal dates, not necessarily covered Sessions: a
/// pre-window listing predates the v4 calendar. #229's calendar bound is
/// therefore not applied here; the v2 code-table boundaries still come from the
/// covered Sessions, and `Evidence::available_at` resolves the publication date
/// against the declared calendar.
fn parse_identities(
    v4: &V4Data,
    facts: &Mvp6Facts,
    citations: &[Citation],
    raw: &RawStore,
) -> Result<Vec<IdentitySeed>> {
    let mut seeds = Vec::new();
    for fact in &facts.identities {
        if !in_code_scope(&fact.code, &v4.code_ranges) {
            return Err(error(format!(
                "identity code {} is outside the v4 configured code ranges",
                fact.code
            )));
        }
        required(&fact.org_id, "org_id", &fact.code)?;
        required(&fact.legal_name, "legal_name", &fact.code)?;
        required(&fact.fund_category, "fund_category", &fact.code)?;
        let listing = parse_date(&fact.listing_date, "listing_date", &fact.code)?;
        let termination = if fact.termination_date.trim().is_empty() {
            None
        } else {
            Some(parse_date(
                &fact.termination_date,
                "termination_date",
                &fact.code,
            )?)
        };
        // #203 rejects a termination on or before the listing date; #229's
        // facts validator only rejects strictly earlier dates. This assembly
        // applies the stricter #203 rule here so the failure is explicit.
        if termination.is_some_and(|end| end <= listing) {
            return Err(error(format!(
                "termination_date must follow listing_date for {}",
                fact.code
            )));
        }
        let evidence_level = parse_evidence_level(&fact.evidence_level, &fact.code)?;
        let legal_name_change = match fact.change_kind.trim() {
            "" => false,
            LEGAL_NAME_CHANGE => true,
            other => {
                return Err(error(format!(
                    "invalid change_kind {other} for {}",
                    fact.code
                )));
            }
        };
        let code_citation = citation(citations, &fact.source_announcement_id, "code")?;
        if !without_whitespace(code_citation.excerpt()).contains(&without_whitespace(&fact.code)) {
            return Err(error(format!(
                "code {} is not supported by its cited excerpt",
                fact.code
            )));
        }
        let org_citation = citation(citations, &fact.source_announcement_id, "org_id")?;
        if !without_whitespace(org_citation.excerpt()).contains(&without_whitespace(&fact.org_id)) {
            return Err(error(format!(
                "org_id for {} is not supported by its cited excerpt",
                fact.code
            )));
        }
        let listing_citation = citation(citations, &fact.source_announcement_id, "listing_date")?;
        if !date_supported(listing_citation.excerpt(), listing) {
            return Err(error(format!(
                "listing_date for {} is not supported by its cited excerpt",
                fact.code
            )));
        }
        let listing_evidence = evidence(listing_citation)?;
        let available_from = if legal_name_change {
            Some(knowable_session(v4, &listing_evidence)?.max(listing))
        } else {
            None
        };
        let termination_evidence = match termination {
            Some(date) => {
                let citation =
                    citation(citations, &fact.source_announcement_id, "termination_date")?;
                if !date_supported(citation.excerpt(), date) {
                    return Err(error(format!(
                        "termination_date for {} is not supported by its cited excerpt",
                        fact.code
                    )));
                }
                Some(evidence(citation)?)
            }
            None => None,
        };
        let attribute = citation(citations, &fact.source_announcement_id, "legal_name")?;
        if !without_whitespace(attribute.excerpt()).contains(&without_whitespace(&fact.legal_name))
        {
            return Err(error(format!(
                "legal_name {} is not supported by its cited excerpt",
                fact.legal_name
            )));
        }
        let attribute_evidence = evidence(attribute)?;
        // AttributeFact carries one Evidence for the whole
        // name/category/index tuple. Emitting the legal-name page for all three
        // is only faithful when the citations are demonstrably the same page.
        let category = citation(citations, &fact.source_announcement_id, "fund_category")?;
        if !category_supported(&fact.fund_category, category.excerpt()) {
            return Err(error(format!(
                "fund_category {} for {} is not supported by its cited excerpt",
                fact.fund_category, fact.code
            )));
        }
        if evidence(category)? != attribute_evidence {
            return Err(error(format!(
                "legal_name and fund_category for {} are not evidenced by the same Raw page",
                fact.code
            )));
        }
        if !fact.tracked_index.trim().is_empty() {
            let index = citation(citations, &fact.source_announcement_id, "tracked_index")?;
            if !without_whitespace(index.excerpt())
                .contains(&without_whitespace(&fact.tracked_index))
            {
                return Err(error(format!(
                    "tracked_index {} is not supported by its cited excerpt",
                    fact.tracked_index
                )));
            }
            if evidence(index)? != attribute_evidence {
                return Err(error(format!(
                    "legal_name and tracked_index for {} are not evidenced by the same Raw page",
                    fact.code
                )));
            }
        }
        // The fund contract is cited as its own field rather than derived from
        // the legal-name field. It may be the same Raw only when the row's
        // source document is itself the contract; its page must state the legal
        // fund name.
        let contract = citation(citations, &fact.source_announcement_id, "contract")?;
        if !without_whitespace(contract.excerpt()).contains(&without_whitespace(&fact.legal_name)) {
            return Err(error(format!(
                "contract evidence for {} does not state its legal name",
                fact.code
            )));
        }
        raw.get(contract.raw_sha256())
            .map_err(|e| error(e.to_string()))?;
        let contract_raw_sha256 = contract.raw_sha256().to_owned();
        seeds.push(IdentitySeed {
            code: fact.code.clone(),
            listing,
            termination,
            legal_name: fact.legal_name.clone(),
            tracked_index: fact.tracked_index.clone(),
            category: fund_category(&fact.fund_category),
            evidence_level,
            legal_name_change,
            available_from,
            contract_raw_sha256,
            listing_evidence,
            termination_evidence,
            attribute_evidence,
        });
    }
    seeds.sort_by(|left, right| (&left.code, left.listing).cmp(&(&right.code, right.listing)));
    for pair in seeds.windows(2) {
        if pair[0].code == pair[1].code && pair[0].listing == pair[1].listing {
            return Err(error(format!(
                "duplicate identity listing_date for {}",
                pair[0].code
            )));
        }
    }
    Ok(seeds)
}

/// Groups identity rows into code-table-aligned lifecycles. A reused code needs
/// a code-table absence gap; a continuous presence can only split on an
/// evidenced legal-name change. Anything else is excluded and reported rather
/// than joining two identities.
fn plan(v4: &V4Data, seeds: &[IdentitySeed]) -> Result<Plan> {
    let mut by_code = BTreeMap::<String, Vec<&IdentitySeed>>::new();
    for seed in seeds {
        by_code.entry(seed.code.clone()).or_default().push(seed);
    }
    let mut planned = Vec::new();
    let mut excluded = BTreeMap::new();
    let mut differences = Vec::new();
    let mut notes = Vec::new();
    let all_codes = v4
        .code_tables
        .iter()
        .flat_map(|table| table.native_symbols.iter().cloned())
        .collect::<BTreeSet<_>>();
    let out_of_scope = all_codes
        .iter()
        .filter(|code| !by_code.contains_key(*code))
        .count();
    if out_of_scope > 0 {
        notes.push(format!(
            "{out_of_scope} TDX code(s) outside the assembled identity set were not assembled"
        ));
    }
    for code in MVP6_FIXED_ETF_CANDIDATES {
        if !by_code.contains_key(code) {
            excluded.insert(code.to_owned(), "identity_missing".to_owned());
        }
    }
    for (code, rows) in &by_code {
        let mut rows = rows.clone();
        rows.retain(|row| {
            if row.termination.is_some_and(|end| end <= v4.first_session) {
                notes.push(format!(
                    "{code}: identity lifecycle terminated before the covered window"
                ));
                false
            } else if row.listing > v4.last_session {
                notes.push(format!(
                    "{code}: identity listing is after the covered window"
                ));
                false
            } else {
                true
            }
        });
        if rows.is_empty() {
            if MVP6_FIXED_ETF_CANDIDATES.contains(&code.as_str()) {
                excluded.insert(code.clone(), "identity_outside_window".to_owned());
            }
            continue;
        }
        let runs = presence_runs(v4, code);
        if runs.is_empty() {
            // P27-7 makes a consistent TDX code table part of secondary
            // evidence. Without it a secondary row is not corroborated and must
            // not become a fixed-universe member. Official rows stand on their
            // own, so a reused code keeps its official lifecycle while only the
            // unsupported secondary row is excluded.
            let official = rows
                .iter()
                .copied()
                .filter(|row| row.evidence_level == EvidenceLevel::Official)
                .collect::<Vec<_>>();
            let secondary = rows
                .iter()
                .copied()
                .filter(|row| row.evidence_level == EvidenceLevel::Secondary)
                .collect::<Vec<_>>();
            for row in &secondary {
                differences.push(IdentityDifference {
                    code: code.clone(),
                    kind: IdentityDifferenceKind::CodeTableAbsent,
                    expected_date: Some(row.listing),
                    observed_date: None,
                    evidence_level: Some(row.evidence_level),
                    resolution: IdentityDifferenceResolution::Excluded,
                });
            }
            if official.is_empty() {
                excluded.insert(code.clone(), "secondary_code_table_absent".to_owned());
                notes.push(format!(
                    "{code}: secondary identity has no TDX code-table presence and is not corroborated"
                ));
                continue;
            }
            for row in &official {
                differences.push(IdentityDifference {
                    code: code.clone(),
                    kind: IdentityDifferenceKind::CodeTableAbsent,
                    expected_date: Some(row.listing),
                    observed_date: None,
                    evidence_level: Some(row.evidence_level),
                    resolution: IdentityDifferenceResolution::Unresolved,
                });
                planned.push(PlannedLifecycle {
                    seed: (*row).clone(),
                    identity_start: row.listing,
                    end_exclusive: None,
                    run: PresenceRun {
                        first: row.listing,
                        last: row.listing,
                        exit: row.termination,
                    },
                    name_change: false,
                    has_presence: false,
                });
            }
            if secondary.is_empty() {
                notes.push(format!(
                    "{code}: identity row exists but the v4 code table never contains the code; the official lifetime remains as UNKNOWN"
                ));
            } else {
                notes.push(format!(
                    "{code}: unsupported secondary lifecycle row(s) excluded; the official lifetime remains as UNKNOWN"
                ));
            }
            continue;
        }
        for row in &rows {
            if row.termination.is_some_and(|end| end > v4.last_session) {
                notes.push(format!(
                    "{code}: termination_date is after the covered window; omitted from the Catalog lifecycle"
                ));
            }
        }
        match align_runs(code, &rows, &runs, v4, &mut differences) {
            Ok(aligned) => {
                let (lifecycles, local_exclusions, local_notes) = aligned;
                if let Some(reason) = local_exclusions {
                    excluded.insert(code.clone(), reason);
                }
                notes.extend(local_notes);
                planned.extend(lifecycles);
            }
            Err(reason) => {
                excluded.insert(code.clone(), reason);
            }
        }
    }

    for (code, reason) in &excluded {
        if !planned.iter().any(|lifecycle| lifecycle.seed.code == *code) {
            notes.push(format!("{code}: excluded from the Catalog ({reason})"));
        }
    }
    Ok(Plan {
        lifecycles: planned,
        excluded,
        differences,
        notes,
    })
}

#[allow(clippy::type_complexity)]
fn align_runs(
    code: &str,
    rows: &[&IdentitySeed],
    runs: &[PresenceRun],
    v4: &V4Data,
    differences: &mut Vec<IdentityDifference>,
) -> std::result::Result<(Vec<PlannedLifecycle>, Option<String>, Vec<String>), String> {
    let mut assigned = Vec::<(usize, NaiveDate, bool, usize)>::new();
    for (index, row) in rows.iter().enumerate() {
        let run_index = if index == 0 {
            let containing = runs
                .iter()
                .position(|run| run.first <= row.listing && row.listing <= run.last);
            containing
                .or_else(|| runs.iter().position(|run| run.first >= row.listing))
                .ok_or_else(|| format!("{code}: identity row has no aligned code-table run"))?
        } else {
            let previous = rows[index - 1];
            let previous_index = assigned[index - 1].0;
            let previous_terminated = previous.termination.is_some();
            let next_run = runs
                .iter()
                .enumerate()
                .find(|(candidate, run)| {
                    *candidate > previous_index && run.first > runs[previous_index].last
                })
                .map(|(candidate, _)| candidate);
            match (previous_terminated, next_run) {
                (true, Some(next)) => {
                    if row.legal_name_change {
                        return Err(format!(
                            "{code}: change_kind=legal_name_change cannot follow a terminated identity"
                        ));
                    }
                    // The successor may appear in the code table before its
                    // official listing, or only after its official termination.
                    // Both are disclosed differences, not exclusions: only a
                    // priced Bar outside the official lifetime excludes the
                    // code (P27-19).
                    next
                }
                (false, Some(_)) if row.listing > runs[previous_index].last => {
                    return Err(format!(
                        "{code}: identity rows span a code-table absence without a termination fact"
                    ));
                }
                (false, _) => {
                    if row.listing <= runs[previous_index].first
                        || row.listing > runs[previous_index].last
                    {
                        return Err(format!(
                            "{code}: legal-name change is outside its code-table run"
                        ));
                    }
                    previous_index
                }
                (true, None) => {
                    return Err(format!(
                        "{code}: terminated identity has a successor without a code-table absence"
                    ));
                }
            }
        };
        if assigned.iter().any(|(run, _, _, _)| *run == run_index) {
            if !row.legal_name_change {
                return Err(format!(
                    "{code}: successor identity must declare change_kind=legal_name_change"
                ));
            }
            let name_change = rows[index - 1].legal_name != row.legal_name;
            if !name_change {
                return Err(format!(
                    "{code}: successor identity does not change the legal name"
                ));
            }
            if assigned
                .iter()
                .any(|(run, start, _, _)| *run == run_index && *start == row.listing)
            {
                return Err(format!("{code}: duplicate identity start date"));
            }
            assigned.push((run_index, row.listing, true, index));
        } else {
            assigned.push((run_index, runs[run_index].first, false, index));
        }
    }

    // Every run with an identity must have a row. A run strictly before the
    // first evidenced identity or strictly after the last one is a bare TDX
    // presence: keep the code when it has no priced Bar (P27-19) and report the
    // presence; a priced Bar there is excluded by `outside_lifecycle_bar`. A run
    // between assigned runs remains an un-evidenced prior lifecycle.
    let assigned_first = assigned
        .iter()
        .map(|(run_index, _, _, _)| runs[*run_index].first)
        .min()
        .expect("at least one assigned identity row");
    let assigned_last = assigned
        .iter()
        .map(|(run_index, _, _, _)| runs[*run_index].last)
        .max()
        .expect("at least one assigned identity row");
    let mut ignored_runs = Vec::new();
    for (index, run) in runs.iter().enumerate() {
        if assigned
            .iter()
            .any(|(run_index, _, _, _)| *run_index == index)
        {
            continue;
        }
        if run.last < assigned_first || run.first > assigned_last {
            ignored_runs.push(*run);
        } else {
            return Err(format!(
                "{code}: code-table run starting {} has no identity evidence",
                run.first
            ));
        }
    }

    assigned.sort_by_key(|(_, start, _, _)| *start);
    if assigned.windows(2).any(|pair| pair[0].1 == pair[1].1) {
        return Err(format!("{code}: duplicate identity start date"));
    }
    let mut lifecycles = Vec::new();
    for (position, (run_index, identity_start, name_change, seed_index)) in
        assigned.iter().enumerate()
    {
        let run = runs[*run_index];
        let end_exclusive = assigned.get(position + 1).map(|next| next.1).or(run.exit);
        let seed = rows[*seed_index];
        lifecycles.push(PlannedLifecycle {
            seed: (*seed).clone(),
            identity_start: *identity_start,
            end_exclusive,
            run,
            name_change: *name_change,
            has_presence: true,
        });
    }

    for (index, lifecycle) in lifecycles.iter().enumerate() {
        if !lifecycle.has_presence {
            continue;
        }
        let seed = &lifecycle.seed;
        // Listing vs the first code-table Session of the lifecycle's run. A
        // pre-window listing cannot be compared to a window that starts later.
        if !lifecycle.name_change
            && seed.listing >= v4.first_session
            && seed.listing != lifecycle.run.first
        {
            differences.push(IdentityDifference {
                code: code.to_owned(),
                kind: IdentityDifferenceKind::ListingDateVsCodeTableFirst,
                expected_date: Some(seed.listing),
                observed_date: Some(lifecycle.run.first),
                evidence_level: Some(seed.evidence_level),
                resolution: if seed.evidence_level == EvidenceLevel::Official {
                    IdentityDifferenceResolution::AcceptedIdentity
                } else {
                    IdentityDifferenceResolution::Unresolved
                },
            });
            if seed.evidence_level == EvidenceLevel::Secondary {
                return Ok((
                    Vec::new(),
                    Some("secondary_listing_disagrees_with_code_table".into()),
                    Vec::new(),
                ));
            }
        }
        // The last lifecycle of a run owns the termination comparison.
        let last_in_run = lifecycles
            .get(index + 1)
            .is_none_or(|next| next.run != lifecycle.run);
        if last_in_run
            && let Some(termination) = seed.termination
            && termination <= v4.last_session
        {
            let observed = lifecycle.run.last;
            let mismatch = if termination < observed {
                Some((
                    IdentityDifferenceKind::TerminationVsCodeTablePresence,
                    Some(termination),
                    Some(observed),
                ))
            } else if lifecycle.run.exit.is_some_and(|exit| termination > exit) {
                Some((
                    IdentityDifferenceKind::CodeTableExitBeforeTermination,
                    Some(termination),
                    lifecycle.run.exit,
                ))
            } else {
                None
            };
            if let Some((kind, expected, observed)) = mismatch {
                differences.push(IdentityDifference {
                    code: code.to_owned(),
                    kind,
                    expected_date: expected,
                    observed_date: observed,
                    evidence_level: Some(seed.evidence_level),
                    resolution: if seed.evidence_level == EvidenceLevel::Official {
                        IdentityDifferenceResolution::AcceptedIdentity
                    } else {
                        IdentityDifferenceResolution::Unresolved
                    },
                });
                if seed.evidence_level == EvidenceLevel::Secondary {
                    return Ok((
                        Vec::new(),
                        Some("secondary_termination_disagrees_with_code_table".into()),
                        Vec::new(),
                    ));
                }
            }
        }
    }

    // P27-19 excludes a priced Bar outside the official lifetime. A Bar that
    // is inside the official lifetime but beyond the TDX presence run is a
    // source disagreement: report it and keep the ETF (its Session is UNKNOWN
    // in the coverage grid).
    if let Some(date) = outside_official_lifespan(code, &lifecycles, v4) {
        differences.push(IdentityDifference {
            code: code.to_owned(),
            kind: IdentityDifferenceKind::PricedBarOutsideLifecycle,
            expected_date: None,
            observed_date: Some(date),
            evidence_level: None,
            resolution: IdentityDifferenceResolution::Excluded,
        });
        return Ok((
            Vec::new(),
            Some(format!("priced_bar_outside_lifecycle:{date}")),
            Vec::new(),
        ));
    }
    for date in priced_bars_outside_tdx_presence(code, &lifecycles, v4) {
        differences.push(IdentityDifference {
            code: code.to_owned(),
            kind: IdentityDifferenceKind::PricedBarOutsideTdxPresence,
            expected_date: None,
            observed_date: Some(date),
            evidence_level: None,
            resolution: IdentityDifferenceResolution::Unresolved,
        });
    }

    for run in ignored_runs {
        differences.push(IdentityDifference {
            code: code.to_owned(),
            kind: IdentityDifferenceKind::CodeTablePresenceOutsideLifecycle,
            expected_date: None,
            observed_date: Some(run.first),
            evidence_level: None,
            resolution: IdentityDifferenceResolution::Unresolved,
        });
    }

    Ok((lifecycles, None, Vec::new()))
}

fn planned_official_end(
    lifecycle: &PlannedLifecycle,
    lifecycles: &[PlannedLifecycle],
) -> Option<NaiveDate> {
    let successor_listing = lifecycles
        .iter()
        .filter(|other| {
            other.seed.code == lifecycle.seed.code
                && other.identity_start > lifecycle.identity_start
        })
        .map(|other| other.seed.listing)
        .min();
    match (lifecycle.seed.termination, successor_listing) {
        (Some(termination), Some(successor)) => Some(termination.min(successor)),
        (Some(termination), None) => Some(termination),
        (None, Some(successor)) => Some(successor),
        (None, None) => None,
    }
}

fn date_in_official_lifetime(date: NaiveDate, lifecycles: &[PlannedLifecycle]) -> bool {
    lifecycles.iter().any(|lifecycle| {
        date >= lifecycle.seed.listing
            && planned_official_end(lifecycle, lifecycles).is_none_or(|end| date < end)
    })
}

fn outside_official_lifespan(
    code: &str,
    lifecycles: &[PlannedLifecycle],
    v4: &V4Data,
) -> Option<NaiveDate> {
    let dates = v4.bar_dates.get(code)?;
    dates
        .iter()
        .copied()
        .find(|date| !date_in_official_lifetime(*date, lifecycles))
}

fn date_in_tdx_presence(date: NaiveDate, lifecycles: &[PlannedLifecycle]) -> bool {
    lifecycles.iter().any(|lifecycle| {
        date >= lifecycle.seed.listing
            && date
                >= lifecycle
                    .seed
                    .available_from
                    .unwrap_or(lifecycle.identity_start)
            && lifecycle.end_exclusive.is_none_or(|end| date < end)
            && lifecycle
                .seed
                .termination
                .is_none_or(|termination| date < termination)
    })
}

fn priced_bars_outside_tdx_presence(
    code: &str,
    lifecycles: &[PlannedLifecycle],
    v4: &V4Data,
) -> Vec<NaiveDate> {
    let Some(dates) = v4.bar_dates.get(code) else {
        return Vec::new();
    };
    dates
        .iter()
        .copied()
        .filter(|date| {
            date_in_official_lifetime(*date, lifecycles) && !date_in_tdx_presence(*date, lifecycles)
        })
        .collect()
}

fn build_input(v4: &V4Data, plan: &Plan, citations: &[Citation]) -> Result<CatalogInput> {
    let planned = &plan.lifecycles;
    let included = planned
        .iter()
        .map(|lifecycle| lifecycle.seed.code.clone())
        .collect::<BTreeSet<_>>();
    let lifecycles = planned
        .iter()
        .map(|lifecycle| {
            // A termination after the covered window cannot be resolved against
            // the frozen calendar and does not affect any in-window decision.
            let termination = lifecycle
                .seed
                .termination
                .filter(|date| *date <= v4.last_session);
            lifecycle
                .seed
                .lifecycle(lifecycle.identity_start, termination)
        })
        .collect::<Result<Vec<_>>>()?;

    let mut scans = Vec::new();
    for lifecycle in planned {
        if !lifecycle.has_presence {
            continue;
        }
        let start = lifecycle.identity_start;
        let scan_start = start.max(lifecycle.seed.available_from.unwrap_or(start));
        let end = lifecycle.end_exclusive;
        let dates = v4
            .bar_dates
            .get(&lifecycle.seed.code)
            .map(|dates| {
                dates
                    .range(scan_start..)
                    .copied()
                    .take_while(|date| end.is_none_or(|end| *date < end))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let raw_sha256 = scan_raw(
            v4,
            &lifecycle.seed.code,
            scan_start,
            end,
            dates.first().copied(),
        )?;
        scans.push(ScanObservation {
            native_symbol: lifecycle.seed.code.clone(),
            status: if dates.is_empty() {
                ScanStatus::NoRows
            } else {
                ScanStatus::Observed
            },
            raw_sha256,
            first_observed: dates.first().copied(),
            last_observed: dates.last().copied(),
            identity_start: Some(start),
        });
    }
    scans.sort_by(|left, right| {
        (&left.native_symbol, left.identity_start)
            .cmp(&(&right.native_symbol, right.identity_start))
    });

    let code_tables = v4
        .code_tables
        .iter()
        .map(|table| CodeTableSnapshot {
            session_date: table.session_date,
            raw_sha256: table.source_raw_sha256.clone(),
            symbols: table
                .native_symbols
                .iter()
                .filter(|code| included.contains(*code))
                .cloned()
                .collect(),
            code_ranges: v4.code_ranges.clone(),
        })
        .collect::<Vec<_>>();

    let publications = lifecycles
        .iter()
        .flat_map(|lifecycle| {
            std::iter::once(&lifecycle.listing.evidence)
                .chain(lifecycle.termination.iter().map(|fact| &fact.evidence))
                .chain(
                    lifecycle
                        .attributes
                        .iter()
                        .map(|attribute| &attribute.evidence),
                )
        })
        .filter_map(|evidence| match evidence.publication {
            Publication::DateOnly { date } => Some(date),
            Publication::Exact { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    if let Some(date) = publications.iter().find(|date| **date >= v4.last_session) {
        return Err(error(format!(
            "announcement date {date} has no following covered Session for P6 availability"
        )));
    }
    let calendar_start = publications
        .iter()
        .copied()
        .chain(std::iter::once(v4.first_session))
        .min()
        .expect("non-empty calendar candidates");

    let announcement_raw_sha256 = citations
        .iter()
        .filter(|citation| {
            matches!(
                citation.field(),
                "listing_date"
                    | "termination_date"
                    | "legal_name"
                    | "fund_category"
                    | "tracked_index"
                    | "contract"
            )
        })
        .map(|citation| citation.raw_sha256().to_owned())
        .collect::<BTreeSet<_>>();
    if announcement_raw_sha256.is_empty() {
        return Err(error("identity facts have no verified announcement Raw"));
    }
    let input = CatalogInput {
        schema_version: 2,
        rule_version: ASSEMBLY_RULE_VERSION.into(),
        calendar_start,
        calendar_end: v4.last_session,
        calendar_raw_sha256: v4.manifest_inputs.clone(),
        sessions: v4.sessions.clone(),
        coverage: Coverage {
            start: v4.first_session,
            end: v4.last_session,
            scan_method: ASSEMBLY_SCAN_METHOD.into(),
            announcement_method: ASSEMBLY_ANNOUNCEMENT_METHOD.into(),
            announcement_raw_sha256,
            gaps: plan
                .excluded
                .iter()
                .map(|(code, reason)| format!("mvp6_excluded:{code}:{reason}"))
                .collect(),
            resolutions: vec![],
        },
        lifecycles,
        scans,
        broad_indices: None,
        code_ranges: Some(v4.code_ranges.clone()),
        code_tables,
    };
    Ok(input)
}

fn scan_raw(
    v4: &V4Data,
    code: &str,
    start: NaiveDate,
    end: Option<NaiveDate>,
    first_observed: Option<NaiveDate>,
) -> Result<String> {
    if let Some(date) = first_observed {
        return v4
            .code_tables
            .iter()
            .find(|table| {
                table.session_date == date
                    && table.native_symbols.iter().any(|symbol| symbol == code)
            })
            .map(|table| table.source_raw_sha256.clone())
            .ok_or_else(|| {
                error(format!(
                    "traded Bar for {code} at {date} has no TDX code-table observation"
                ))
            });
    }
    v4.code_tables
        .iter()
        .find(|table| {
            table.session_date >= start
                && end.is_none_or(|end| table.session_date < end)
                && table.native_symbols.iter().any(|symbol| symbol == code)
        })
        .map(|table| table.source_raw_sha256.clone())
        .ok_or_else(|| {
            error(format!(
                "no TDX code-table Raw supports the price scan for {code} at {start}"
            ))
        })
}

fn presence_runs(v4: &V4Data, code: &str) -> Vec<PresenceRun> {
    let dates = v4
        .code_tables
        .iter()
        .map(|table| {
            (
                table.session_date,
                table.native_symbols.iter().any(|symbol| symbol == code),
            )
        })
        .collect::<Vec<_>>();
    let mut runs = Vec::new();
    let mut start = None;
    for (index, (date, present)) in dates.iter().enumerate() {
        if *present {
            start.get_or_insert(*date);
        } else if let Some(first) = start.take() {
            let last = dates[index - 1].0;
            runs.push(PresenceRun {
                first,
                last,
                exit: Some(*date),
            });
        }
    }
    if let Some(first) = start {
        let last = dates.last().expect("non-empty code table").0;
        runs.push(PresenceRun {
            first,
            last,
            exit: None,
        });
    }
    runs
}

fn knowable_session(v4: &V4Data, evidence: &Evidence) -> Result<NaiveDate> {
    let session = match &evidence.publication {
        Publication::DateOnly { date } => v4
            .sessions
            .iter()
            .find(|session| session.session_date() > *date),
        Publication::Exact { timestamp } => v4
            .sessions
            .iter()
            .find(|session| session.ts_close() >= *timestamp),
    };
    session
        .map(|session| session.session_date())
        .ok_or_else(|| error("legal-name change announcement has no available covered Session"))
}

fn evidence(citation: &Citation) -> Result<Evidence> {
    let date = citation.publication_date().ok_or_else(|| {
        error(format!(
            "cannot evidence the announcement date for {} from its frozen Raw",
            citation.announcement_id()
        ))
    })?;
    Ok(Evidence {
        raw_sha256: citation.raw_sha256().to_owned(),
        announcement_id: citation.announcement_id().to_owned(),
        locator: format!("{}:p{}", citation.announcement_id(), citation.page()),
        publication: Publication::DateOnly { date },
    })
}

fn citation<'a>(citations: &'a [Citation], source: &str, field: &str) -> Result<&'a Citation> {
    citations
        .iter()
        .find(|citation| citation.fact_source() == source && citation.field() == field)
        .ok_or_else(|| error(format!("missing Raw citation for {source} field {field}")))
}

fn parse_evidence_level(value: &str, code: &str) -> Result<EvidenceLevel> {
    match value.trim() {
        "official" => Ok(EvidenceLevel::Official),
        "secondary" => Ok(EvidenceLevel::Secondary),
        other => Err(error(format!("invalid evidence_level {other} for {code}"))),
    }
}

fn required(value: &str, field: &str, code: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(error(format!("required field {field} is empty for {code}")))
    } else {
        Ok(())
    }
}

fn parse_date(value: &str, field: &str, code: &str) -> Result<NaiveDate> {
    required(value, field, code)?;
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .map_err(|_| error(format!("invalid {field} for {code}: {value}")))
}

/// The identity table records an explicit fund-category fact. Values outside
/// the small documented vocabulary are treated as `Other`; MVP-6 fixed-list
/// selection ignores the category, and a non-equity default is conservative.
const EQUITY_CATEGORY_TOKENS: &[&str] = &[
    "equity",
    "stock",
    "index",
    "etf",
    "指数",
    "指数型",
    "指数基金",
    "指数型基金",
    "股票",
    "股票型",
    "股票基金",
    "股票型基金",
    "equity-index-etf",
    "cross-border-index-etf",
];

fn fund_category(value: &str) -> FundCategory {
    let value = value.trim();
    if EQUITY_CATEGORY_TOKENS
        .iter()
        .any(|token| value.eq_ignore_ascii_case(token))
    {
        FundCategory::Equity
    } else {
        FundCategory::Other
    }
}

/// The category is a coded field, so the excerpt need not repeat a machine
/// token verbatim. It must still contain a category statement: any documented
/// equity token, or the declared value for an unrecognised category.
fn category_supported(value: &str, excerpt: &str) -> bool {
    let excerpt = without_whitespace(excerpt);
    if fund_category(value) == FundCategory::Equity {
        EQUITY_CATEGORY_TOKENS
            .iter()
            .any(|token| excerpt.contains(&without_whitespace(token)))
    } else {
        excerpt.contains(&without_whitespace(value))
    }
}

fn date_supported(excerpt: &str, date: NaiveDate) -> bool {
    use chrono::Datelike;
    let (year, month, day) = (date.year(), date.month(), date.day());
    let excerpt = without_whitespace(excerpt);
    [
        date.to_string(),
        format!("{year}/{month:02}/{day:02}"),
        format!("{year}.{month:02}.{day:02}"),
        format!("{year}年{month}月{day}日"),
        format!("{year}年{month:02}月{day:02}日"),
    ]
    .iter()
    .any(|form| excerpt.contains(&without_whitespace(form)))
}

fn without_whitespace(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}
