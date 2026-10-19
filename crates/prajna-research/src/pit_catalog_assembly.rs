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

use crate::pit_universe::{
    AttributeFact, CatalogError, CatalogInput, CodeRange, CodeTableSnapshot, Coverage, Evidence,
    FundCategory, HistoricalCatalog, Lifecycle, LifecycleFact, MVP6_FIXED_ETF_CANDIDATES,
    Publication, ScanObservation, ScanStatus,
};
use chrono::{DateTime, FixedOffset, NaiveDate};
use prajna_data::cninfo::{Citation, EvidenceLevel, Mvp6Facts};
use prajna_data::{
    RawStore, TdxCodeTableRow, bars_from_record_batch, read_dataset_table, read_manifest,
    sessions_from_record_batch, tdx_code_table_from_record_batch,
};
use prajna_domain::Session;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityDifference {
    pub code: String,
    pub kind: String,
    pub expected_date: Option<NaiveDate>,
    pub observed_date: Option<NaiveDate>,
    pub evidence_level: Option<EvidenceLevel>,
    pub resolution: String,
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

    fn lifecycle(&self, identity_start: NaiveDate, termination: Option<NaiveDate>) -> Lifecycle {
        Lifecycle {
            native_symbol: self.code.clone(),
            contract_raw_sha256: self.attribute_evidence.raw_sha256.clone(),
            listing: LifecycleFact {
                effective_date: self.listing,
                evidence: self.listing_evidence.clone(),
            },
            termination: termination.map(|date| LifecycleFact {
                effective_date: date,
                evidence: self
                    .termination_evidence
                    .clone()
                    .expect("termination date has termination evidence"),
            }),
            attributes: vec![self.attribute()],
            identity_start: Some(identity_start),
            identity_end_exclusive: None,
            code_table_first_seen: None,
            code_table_last_seen: None,
            code_table_exit_date: None,
        }
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
    let v4 = load_v4(root, dsv)?;
    let seeds = parse_identities(&v4, facts, citations, &raw)?;
    let (planned, excluded_codes, differences, notes) = plan(&v4, &seeds)?;
    if planned.is_empty() {
        return Err(error(
            "no identity row can be assembled into a Catalog lifecycle",
        ));
    }
    let input = build_input(&v4, &planned, citations)?;
    let catalog =
        HistoricalCatalog::build(input.clone(), &raw).map_err(|e| error(e.to_string()))?;
    let report = CatalogAssemblyReport {
        contract: ASSEMBLY_REPORT_CONTRACT.into(),
        dsv: v4.dsv.clone(),
        catalog_version: catalog.version().map_err(|e| error(e.to_string()))?,
        rule_version: ASSEMBLY_RULE_VERSION.into(),
        identity_rows: facts.identities.len(),
        lifecycles: input.lifecycles.len(),
        included_codes: planned
            .iter()
            .map(|lifecycle| lifecycle.seed.code.clone())
            .collect(),
        excluded_codes,
        differences,
        catalog_coverage_gaps: catalog.coverage_gaps().clone(),
        notes,
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

fn parse_identities(
    v4: &V4Data,
    facts: &Mvp6Facts,
    citations: &[Citation],
    raw: &RawStore,
) -> Result<Vec<IdentitySeed>> {
    let mut seeds = Vec::new();
    for fact in &facts.identities {
        if !code_in_ranges(&fact.code, &v4.code_ranges) {
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
        if termination.is_some_and(|end| end <= listing) {
            return Err(error(format!(
                "termination_date must follow listing_date for {}",
                fact.code
            )));
        }
        let evidence_level = parse_evidence_level(&fact.evidence_level, &fact.code)?;
        let code_citation = citation(citations, &fact.source_announcement_id, "code")?;
        if !normalized(code_citation.excerpt()).contains(&normalized(&fact.code)) {
            return Err(error(format!(
                "code {} is not supported by its cited excerpt",
                fact.code
            )));
        }
        let org_citation = citation(citations, &fact.source_announcement_id, "org_id")?;
        if !normalized(org_citation.excerpt()).contains(&normalized(&fact.org_id)) {
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
        let listing_evidence = evidence(raw, listing_citation, "identity.csv listing_date")?;
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
                Some(evidence(raw, citation, "identity.csv termination_date")?)
            }
            None => None,
        };
        let attribute = citation(citations, &fact.source_announcement_id, "legal_name")?;
        let attribute_evidence = evidence(raw, attribute, "identity.csv legal_name")?;
        if !normalized(attribute.excerpt()).contains(&normalized(&fact.legal_name)) {
            return Err(error(format!(
                "legal_name {} is not supported by its cited excerpt",
                fact.legal_name
            )));
        }
        if !fact.tracked_index.trim().is_empty() {
            let index = citation(citations, &fact.source_announcement_id, "tracked_index")?;
            if !normalized(index.excerpt()).contains(&normalized(&fact.tracked_index)) {
                return Err(error(format!(
                    "tracked_index {} is not supported by its cited excerpt",
                    fact.tracked_index
                )));
            }
            citation(citations, &fact.source_announcement_id, "fund_category")?;
        } else {
            citation(citations, &fact.source_announcement_id, "fund_category")?;
        }
        seeds.push(IdentitySeed {
            code: fact.code.clone(),
            listing,
            termination,
            legal_name: fact.legal_name.clone(),
            tracked_index: fact.tracked_index.clone(),
            category: fund_category(&fact.fund_category),
            evidence_level,
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
#[allow(clippy::type_complexity)]
fn plan(
    v4: &V4Data,
    seeds: &[IdentitySeed],
) -> Result<(
    Vec<PlannedLifecycle>,
    BTreeMap<String, String>,
    Vec<IdentityDifference>,
    Vec<String>,
)> {
    let mut by_code = BTreeMap::<String, Vec<&IdentitySeed>>::new();
    for seed in seeds {
        by_code.entry(seed.code.clone()).or_default().push(seed);
    }
    let mut planned = Vec::new();
    let mut excluded = BTreeMap::new();
    let mut differences = Vec::new();
    let mut notes = Vec::new();
    for code in MVP6_FIXED_ETF_CANDIDATES {
        if !by_code.contains_key(code) {
            excluded.insert(code.to_owned(), "identity_missing".to_owned());
            notes.push(format!(
                "{code}: no identity row; excluded from the fixed list"
            ));
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
            excluded.insert(code.clone(), "code_table_absent".to_owned());
            notes.push(format!(
                "{code}: identity row exists but the v4 code table never contains the code"
            ));
            continue;
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

    // Remaining fixed-list codes without an identity row but present in the
    // published code table are reported under the same exclusion.
    for code in MVP6_FIXED_ETF_CANDIDATES {
        if !excluded.contains_key(code)
            && !planned.iter().any(|lifecycle| lifecycle.seed.code == code)
            && !presence_runs(v4, code).is_empty()
        {
            excluded.insert(code.to_owned(), "identity_missing".to_owned());
        }
    }
    for (code, reason) in &excluded {
        if !planned.iter().any(|lifecycle| lifecycle.seed.code == *code) {
            notes.push(format!("{code}: excluded from the Catalog ({reason})"));
        }
    }
    Ok((planned, excluded, differences, notes))
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
    let notes = Vec::new();
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
                    if row.listing > runs[next].first
                        || row.termination.is_some_and(|end| end <= runs[next].first)
                    {
                        return Err(format!(
                            "{code}: reused-code lifecycle does not align with the code-table absence"
                        ));
                    }
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
            let run = runs[run_index];
            if run.first < row.listing && row.listing >= v4.first_session {
                return Err(format!(
                    "{code}: code table contains the code before the evidenced listing date"
                ));
            }
            assigned.push((run_index, run.first, false, index));
        }
    }

    // Every run with an identity must have a row; a run that starts before the
    // first evidenced identity is an un-evidenced prior lifecycle.
    for (index, run) in runs.iter().enumerate() {
        if !assigned
            .iter()
            .any(|(run_index, _, _, _)| *run_index == index)
        {
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
        });
    }

    for (index, lifecycle) in lifecycles.iter().enumerate() {
        let seed = &lifecycle.seed;
        // Listing vs the first code-table Session of the lifecycle's run. A
        // pre-window listing cannot be compared to a window that starts later.
        if !lifecycle.name_change
            && seed.listing >= v4.first_session
            && seed.listing != lifecycle.run.first
        {
            differences.push(IdentityDifference {
                code: code.to_owned(),
                kind: "listing_date_vs_code_table_first".into(),
                expected_date: Some(seed.listing),
                observed_date: Some(lifecycle.run.first),
                evidence_level: Some(seed.evidence_level),
                resolution: if seed.evidence_level == EvidenceLevel::Official {
                    "accepted_identity".into()
                } else {
                    "unresolved".into()
                },
            });
            if seed.evidence_level == EvidenceLevel::Secondary {
                return Ok((
                    Vec::new(),
                    Some("secondary_listing_disagrees_with_code_table".into()),
                    notes,
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
                    "termination_vs_code_table_presence",
                    Some(termination),
                    Some(observed),
                ))
            } else if lifecycle.run.exit.is_some_and(|exit| termination > exit) {
                Some((
                    "code_table_exit_before_termination",
                    Some(termination),
                    lifecycle.run.exit,
                ))
            } else {
                None
            };
            if let Some((kind, expected, observed)) = mismatch {
                differences.push(IdentityDifference {
                    code: code.to_owned(),
                    kind: kind.into(),
                    expected_date: expected,
                    observed_date: observed,
                    evidence_level: Some(seed.evidence_level),
                    resolution: if seed.evidence_level == EvidenceLevel::Official {
                        "accepted_identity".into()
                    } else {
                        "unresolved".into()
                    },
                });
                if seed.evidence_level == EvidenceLevel::Secondary {
                    return Ok((
                        Vec::new(),
                        Some("secondary_termination_disagrees_with_code_table".into()),
                        notes,
                    ));
                }
            }
        }
    }

    // P27-19: a priced Bar outside every evidenced lifecycle excludes the code.
    if let Some(date) = outside_lifecycle_bar(code, &lifecycles, v4) {
        differences.push(IdentityDifference {
            code: code.to_owned(),
            kind: "priced_bar_outside_lifecycle".into(),
            expected_date: None,
            observed_date: Some(date),
            evidence_level: None,
            resolution: "excluded".into(),
        });
        return Ok((
            Vec::new(),
            Some(format!("priced_bar_outside_lifecycle:{date}")),
            notes,
        ));
    }

    Ok((lifecycles, None, notes))
}

fn outside_lifecycle_bar(
    code: &str,
    lifecycles: &[PlannedLifecycle],
    v4: &V4Data,
) -> Option<NaiveDate> {
    let dates = v4.bar_dates.get(code)?;
    dates.iter().copied().find(|date| {
        !lifecycles.iter().any(|lifecycle| {
            *date >= lifecycle.seed.listing
                && lifecycle
                    .seed
                    .termination
                    .is_none_or(|termination| *date < termination)
        })
    })
}

fn build_input(
    v4: &V4Data,
    planned: &[PlannedLifecycle],
    citations: &[Citation],
) -> Result<CatalogInput> {
    let included = planned
        .iter()
        .map(|lifecycle| lifecycle.seed.code.clone())
        .collect::<BTreeSet<_>>();
    let lifecycles = planned
        .iter()
        .map(|lifecycle| {
            lifecycle
                .seed
                .lifecycle(lifecycle.identity_start, lifecycle.seed.termination)
        })
        .collect::<Vec<_>>();

    let mut scans = Vec::new();
    for lifecycle in planned {
        let start = lifecycle.identity_start;
        let end = lifecycle.end_exclusive;
        let dates = v4
            .bar_dates
            .get(&lifecycle.seed.code)
            .map(|dates| {
                dates
                    .range(start..)
                    .copied()
                    .take_while(|date| end.is_none_or(|end| *date < end))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let raw_sha256 = scan_raw(v4, &lifecycle.seed.code, start, end, dates.first().copied())?;
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
            gaps: BTreeSet::new(),
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

fn evidence(raw: &RawStore, citation: &Citation, locator: &str) -> Result<Evidence> {
    let date = publication_date(raw, citation)?;
    Ok(Evidence {
        raw_sha256: citation.raw_sha256().to_owned(),
        announcement_id: citation.announcement_id().to_owned(),
        locator: locator.to_owned(),
        publication: Publication::DateOnly { date },
    })
}

/// Derives the historical announcement date from the frozen Raw, never from the
/// local capture time (`observed_at`) or the fact's effective date.
fn publication_date(raw: &RawStore, citation: &Citation) -> Result<NaiveDate> {
    let bytes = raw
        .get(citation.raw_sha256())
        .map_err(|e| error(e.to_string()))?;
    let mut dates = BTreeSet::new();
    for record in raw
        .sources(citation.raw_sha256())
        .map_err(|e| error(e.to_string()))?
    {
        if let Some(url) = record.request.get("url").and_then(Value::as_str)
            && let Some(date) = adjunct_date(url)
        {
            dates.insert(date);
        }
    }
    if let Some(date) = json_announcement_date(&bytes, citation.announcement_id())? {
        dates.insert(date);
    }
    match dates.len() {
        1 => Ok(*dates.first().expect("one date")),
        0 => Err(error(format!(
            "cannot evidence the announcement date for {} from its frozen Raw",
            citation.announcement_id()
        ))),
        _ => Err(error(format!(
            "ambiguous announcement dates for {} in its frozen Raw",
            citation.announcement_id()
        ))),
    }
}

fn adjunct_date(url: &str) -> Option<NaiveDate> {
    let (_, rest) = url.split_once("finalpage/")?;
    NaiveDate::parse_from_str(rest.get(..10)?, "%Y-%m-%d").ok()
}

fn json_announcement_date(bytes: &[u8], announcement_id: &str) -> Result<Option<NaiveDate>> {
    if bytes.starts_with(b"%PDF-") {
        return Ok(None);
    }
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return Ok(None);
    };
    let mut dates = BTreeSet::new();
    collect_announcement_dates(&value, announcement_id, &mut dates);
    match dates.len() {
        0 => Ok(None),
        1 => Ok(dates.first().copied()),
        _ => Err(error(format!(
            "ambiguous announcementTime values for {announcement_id}"
        ))),
    }
}

fn collect_announcement_dates(
    value: &Value,
    announcement_id: &str,
    dates: &mut BTreeSet<NaiveDate>,
) {
    match value {
        Value::Object(object) => {
            let numeric_id = announcement_id.parse::<i64>().ok();
            if object.get("announcementId").is_some_and(|id| {
                id.as_str() == Some(announcement_id)
                    || numeric_id.is_some_and(|value| id.as_i64() == Some(value))
            }) && let Some(millis) = object.get("announcementTime").and_then(Value::as_i64)
                && let Some(date) = shanghai_date(millis)
            {
                dates.insert(date);
            }
            for value in object.values() {
                collect_announcement_dates(value, announcement_id, dates);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_announcement_dates(item, announcement_id, dates);
            }
        }
        _ => {}
    }
}

fn shanghai_date(millis: i64) -> Option<NaiveDate> {
    let offset = FixedOffset::east_opt(8 * 3600)?;
    Some(
        DateTime::from_timestamp_millis(millis)?
            .with_timezone(&offset)
            .date_naive(),
    )
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
fn fund_category(value: &str) -> FundCategory {
    let value = value.trim();
    if [
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
    ]
    .iter()
    .any(|token| value.eq_ignore_ascii_case(token))
    {
        FundCategory::Equity
    } else {
        FundCategory::Other
    }
}

fn date_supported(excerpt: &str, date: NaiveDate) -> bool {
    use chrono::Datelike;
    let (year, month, day) = (date.year(), date.month(), date.day());
    let excerpt = normalized(excerpt);
    [
        date.to_string(),
        format!("{year}/{month:02}/{day:02}"),
        format!("{year}.{month:02}.{day:02}"),
        format!("{year}年{month}月{day}日"),
        format!("{year}年{month:02}月{day:02}日"),
    ]
    .iter()
    .any(|form| excerpt.contains(&normalized(form)))
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn code_in_ranges(code: &str, ranges: &[CodeRange]) -> bool {
    code.len() == 6
        && code.bytes().all(|byte| byte.is_ascii_digit())
        && code.parse::<u32>().is_ok_and(|value| {
            ranges
                .iter()
                .any(|range| range.start <= value && value <= range.end)
        })
}

fn raw_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}
