//! Frozen, evidence-backed historical SZSE ETF identities and close-session Universe.
//! This prepares P7/P8 inputs; liquidity, execution status and Engine consumption are
//! separate contracts. Documents are explicitly extracted/adjudicated, never classified
//! using fund names or announcement title keywords.
use chrono::{DateTime, FixedOffset, NaiveDate};
use prajna_data::{RawStore, restricted_jcs};
use prajna_domain::{InstrumentId, Session, TimestampNs, VenueId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogError(pub String);
impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CatalogError {}
type Result<T> = std::result::Result<T, CatalogError>;
fn error(message: impl Into<String>) -> CatalogError {
    CatalogError(message.into())
}
fn hash<T: Serialize>(prefix: &str, value: &T) -> Result<String> {
    let value = serde_json::to_value(value).map_err(|e| error(e.to_string()))?;
    let bytes = restricted_jcs(&value).map_err(|e| error(e.to_string()))?;
    Ok(format!("{prefix}:sha256:{:x}", Sha256::digest(bytes)))
}
fn raw_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn scoped(code: &str) -> bool {
    code.len() == 6 && code.starts_with("159") && code.bytes().all(|b| b.is_ascii_digit())
}

/// Historical knowledge, never the local collection timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "precision", rename_all = "snake_case", deny_unknown_fields)]
pub enum Publication {
    DateOnly { date: NaiveDate },
    Exact { timestamp: TimestampNs },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub raw_sha256: String,
    pub announcement_id: String,
    /// Page/section locating the extracted statement in the frozen official attachment.
    pub locator: String,
    pub publication: Publication,
}
impl Evidence {
    fn validate(&self) -> Result<()> {
        if !raw_hash(&self.raw_sha256)
            || self.announcement_id.trim().is_empty()
            || self.locator.trim().is_empty()
        {
            return Err(error(
                "evidence needs Raw SHA-256, announcement identity and locator",
            ));
        }
        Ok(())
    }
    fn available_at(&self, calendar: &CatalogInput) -> Result<TimestampNs> {
        match self.publication {
            Publication::Exact { timestamp } => Ok(timestamp),
            Publication::DateOnly { date } => {
                if date < calendar.calendar_start || date >= calendar.calendar_end {
                    return Err(error("date-only publication outside covered calendar"));
                }
                calendar
                    .sessions
                    .iter()
                    .find(|s| s.session_date() > date)
                    .map(Session::ts_close)
                    .ok_or_else(|| error("next Session after publication is not covered"))
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleFact {
    pub effective_date: NaiveDate,
    pub evidence: Evidence,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FundCategory {
    Equity,
    Other,
}
/// An extracted, adjudicated attribute pair. Different statements with the same
/// effective date remain conflicts. A later known effective date changes attributes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttributeFact {
    pub effective_date: NaiveDate,
    pub category: FundCategory,
    pub index_id: String,
    pub evidence: Evidence,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lifecycle {
    pub native_symbol: String,
    /// Hash of the fund contract attachment proving the legal fund identity.
    pub contract_raw_sha256: String,
    pub listing: LifecycleFact,
    /// First date on which new entry is prohibited; no last-price liquidation.
    pub termination: Option<LifecycleFact>,
    pub attributes: Vec<AttributeFact>,
}
impl Lifecycle {
    pub fn instrument_id(&self) -> Result<InstrumentId> {
        if !scoped(&self.native_symbol) || !raw_hash(&self.contract_raw_sha256) {
            return Err(error("invalid SZSE code or contract hash"));
        }
        // 6 + 1 + 8 + 1 + 48 = 64 ASCII characters. Full contract hash remains
        // in the Catalog; collision/duplicate IDs are rejected by construction.
        let symbol = format!(
            "{}-{}-{}",
            self.native_symbol,
            self.listing.effective_date.format("%Y%m%d"),
            self.contract_raw_sha256[7..55].to_ascii_uppercase()
        );
        InstrumentId::new(symbol, VenueId::new("SZSE").expect("static venue"))
            .map_err(|e| error(e.to_string()))
    }
    fn contains(&self, date: NaiveDate) -> bool {
        date >= self.listing.effective_date
            && self
                .termination
                .as_ref()
                .is_none_or(|end| date < end.effective_date)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    Observed,
    NoRows,
    Unresolved,
}
/// Each of the 1,000 codes must have one full-window scan outcome. NoRows is an
/// observation, never a proof that a legal fund did not exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanObservation {
    pub native_symbol: String,
    pub status: ScanStatus,
    pub raw_sha256: String,
    pub first_observed: Option<NaiveDate>,
    pub last_observed: Option<NaiveDate>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DifferenceResolution {
    pub native_symbol: String,
    pub explanation: String,
    pub evidence: Evidence,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub scan_method: String,
    pub announcement_method: String,
    /// Includes listing/contract/termination queries, pagination and attachments,
    /// plus window-before listing evidence. Each referenced original is verified.
    pub announcement_raw_sha256: BTreeSet<String>,
    pub gaps: BTreeSet<String>,
    pub resolutions: Vec<DifferenceResolution>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BroadIndexList {
    pub version: String,
    /// Stable provider-qualified domestic broad equity index IDs, not fund names.
    pub index_ids: BTreeSet<String>,
    pub evidence: Vec<Evidence>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogInput {
    pub schema_version: u32,
    pub rule_version: String,
    pub calendar_start: NaiveDate,
    pub calendar_end: NaiveDate,
    pub calendar_raw_sha256: BTreeSet<String>,
    #[serde(with = "catalog_sessions")]
    pub sessions: Vec<Session>,
    pub coverage: Coverage,
    pub lifecycles: Vec<Lifecycle>,
    pub scans: Vec<ScanObservation>,
    pub broad_indices: BroadIndexList,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Difference {
    pub native_symbol: String,
    pub kind: String,
    pub resolution: Option<DifferenceResolution>,
}
/// Constructed only after structural and Raw integrity validation. Serialization
/// of an input does not bypass validation; rebuild on every load.
#[derive(Debug, Clone)]
pub struct HistoricalCatalog {
    input: CatalogInput,
    differences: Vec<Difference>,
    gaps: BTreeSet<String>,
}

impl HistoricalCatalog {
    pub fn build(mut input: CatalogInput, raw: &RawStore) -> Result<Self> {
        if input.schema_version != 1
            || input.rule_version.trim().is_empty()
            || input.broad_indices.version.trim().is_empty()
        {
            return Err(error(
                "unsupported/empty Catalog or broad-index rule version",
            ));
        }
        if input.coverage.start > input.coverage.end
            || input.calendar_start > input.coverage.start
            || input.calendar_end < input.coverage.end
            || input.sessions.is_empty()
        {
            return Err(error("invalid coverage/calendar interval"));
        }
        if input.coverage.scan_method.trim().is_empty()
            || input.coverage.announcement_method.trim().is_empty()
            || input.coverage.announcement_raw_sha256.is_empty()
            || input.calendar_raw_sha256.is_empty()
            || input.broad_indices.index_ids.is_empty()
            || input.broad_indices.evidence.is_empty()
            || input
                .broad_indices
                .index_ids
                .iter()
                .any(|id| id.trim().is_empty())
        {
            return Err(error(
                "coverage and index list require explicit methods and evidence",
            ));
        }
        input.sessions.sort_by_key(Session::session_date);
        for (i, s) in input.sessions.iter().enumerate() {
            let local_date = |ts: TimestampNs| {
                DateTime::from_timestamp_nanos(ts.as_unix_nanos())
                    .with_timezone(&FixedOffset::east_opt(8 * 3600).expect("Shanghai offset"))
                    .date_naive()
            };
            if local_date(s.ts_open()) != s.session_date()
                || local_date(s.ts_close()) != s.session_date()
                || (i > 0 && input.sessions[i - 1].ts_close() >= s.ts_open())
            {
                return Err(error("Session timestamps do not match ordered local dates"));
            }
            if s.venue_id().as_str() != "SZSE"
                || s.session_date() < input.calendar_start
                || s.session_date() > input.calendar_end
                || i > 0 && input.sessions[i - 1].session_date() == s.session_date()
            {
                return Err(error("invalid/duplicate SZSE Session"));
            }
        }
        input
            .lifecycles
            .sort_by_key(|life| (life.native_symbol.clone(), life.listing.effective_date));
        let mut ids = BTreeSet::new();
        let mut hashes = input.calendar_raw_sha256.clone();
        hashes.extend(input.coverage.announcement_raw_sha256.iter().cloned());
        let mut evidence = input.broad_indices.evidence.clone();
        for (i, life) in input.lifecycles.iter_mut().enumerate() {
            if !ids.insert(life.instrument_id()?) {
                return Err(error("duplicate lifecycle identity"));
            }
            if life
                .termination
                .as_ref()
                .is_some_and(|end| end.effective_date <= life.listing.effective_date)
            {
                return Err(error("termination must follow listing"));
            }
            hashes.insert(life.contract_raw_sha256.clone());
            evidence.push(life.listing.evidence.clone());
            if let Some(end) = &life.termination {
                evidence.push(end.evidence.clone());
            }
            life.attributes
                .sort_by_cached_key(|a| serde_json::to_string(a).expect("serializable attribute"));
            life.attributes.dedup();
            for a in &life.attributes {
                if a.index_id.trim().is_empty() || a.effective_date < life.listing.effective_date {
                    return Err(error(format!("invalid attribute at lifecycle {i}")));
                }
                evidence.push(a.evidence.clone());
            }
        }
        for pair in input.lifecycles.windows(2) {
            if pair[0].native_symbol == pair[1].native_symbol
                && pair[0]
                    .termination
                    .as_ref()
                    .is_none_or(|end| end.effective_date > pair[1].listing.effective_date)
            {
                return Err(error("overlapping reused-code lifecycles"));
            }
        }
        let mut scans = BTreeMap::new();
        for scan in &input.scans {
            if !scoped(&scan.native_symbol)
                || scans.insert(scan.native_symbol.clone(), scan).is_some()
            {
                return Err(error("invalid or duplicate scanned code"));
            }
            if scan.status == ScanStatus::Observed {
                match (scan.first_observed, scan.last_observed) {
                    (Some(first), Some(last))
                        if first <= last
                            && first >= input.coverage.start
                            && last <= input.coverage.end => {}
                    _ => {
                        return Err(error(
                            "observed scan requires ordered dates inside coverage",
                        ));
                    }
                }
            } else if scan.first_observed.is_some() || scan.last_observed.is_some() {
                return Err(error("non-observed scan must not have price dates"));
            }
            hashes.insert(scan.raw_sha256.clone());
        }
        let mut gaps = input.coverage.gaps.clone();
        let mut differences = Vec::new();
        let mut resolutions = BTreeMap::new();
        for resolution in &input.coverage.resolutions {
            if !scoped(&resolution.native_symbol)
                || resolution.explanation.trim().is_empty()
                || resolutions
                    .insert(resolution.native_symbol.clone(), resolution.clone())
                    .is_some()
            {
                return Err(error("invalid/duplicate difference resolution"));
            }
            evidence.push(resolution.evidence.clone());
        }
        for code in 159000..=159999 {
            let code = code.to_string();
            let known = input.lifecycles.iter().any(|l| {
                l.native_symbol == code
                    && l.listing.effective_date <= input.coverage.end
                    && l.termination
                        .as_ref()
                        .is_none_or(|t| t.effective_date > input.coverage.start)
            });
            let kind = match scans.get(&code) {
                None => {
                    gaps.insert(format!("scan_missing:{code}"));
                    continue;
                }
                Some(s) if s.status == ScanStatus::Unresolved => {
                    gaps.insert(format!("scan_unresolved:{code}"));
                    continue;
                }
                Some(s) if s.status == ScanStatus::Observed && !known => Some("scan_only"),
                Some(s) if s.status == ScanStatus::NoRows && known => Some("announcement_only"),
                _ => None,
            };
            if let Some(kind) = kind {
                let resolution = resolutions.remove(&code);
                if resolution.is_none() {
                    gaps.insert(format!("unresolved_difference:{code}:{kind}"));
                }
                differences.push(Difference {
                    native_symbol: code,
                    kind: kind.into(),
                    resolution,
                });
            }
        }
        for scan in &input.scans {
            if scan.status == ScanStatus::Observed {
                for date in [scan.first_observed, scan.last_observed]
                    .into_iter()
                    .flatten()
                {
                    if !input
                        .lifecycles
                        .iter()
                        .any(|l| l.native_symbol == scan.native_symbol && l.contains(date))
                        && !differences.iter().any(|d| {
                            d.native_symbol == scan.native_symbol && d.resolution.is_some()
                        })
                    {
                        gaps.insert(format!(
                            "unattributed_scan_endpoint:{}:{date}",
                            scan.native_symbol
                        ));
                    }
                }
            }
        }
        if !resolutions.is_empty() {
            return Err(error(
                "resolution does not match a bidirectional difference",
            ));
        }
        for e in &evidence {
            e.validate()?;
            e.available_at(&input)?;
            hashes.insert(e.raw_sha256.clone());
        }
        for h in hashes {
            if !raw_hash(&h) {
                return Err(error("invalid Raw reference"));
            }
            raw.get(&h).map_err(|e| error(e.to_string()))?;
        }
        input.scans.sort_by_key(|s| s.native_symbol.clone());
        input
            .coverage
            .resolutions
            .sort_by_key(|r| r.native_symbol.clone());
        input
            .broad_indices
            .evidence
            .sort_by_cached_key(|e| serde_json::to_string(e).expect("serializable evidence"));
        input.broad_indices.evidence.dedup();
        Ok(Self {
            input,
            differences,
            gaps,
        })
    }
    pub fn version(&self) -> Result<String> {
        hash("catalog", &self.input)
    }
    pub fn input(&self) -> &CatalogInput {
        &self.input
    }
    pub fn differences(&self) -> &[Difference] {
        &self.differences
    }
    pub fn coverage_gaps(&self) -> &BTreeSet<String> {
        &self.gaps
    }
    /// Physical price-row mapping, separate from as-of strategy knowledge. Missing
    /// or ambiguous lifecycles are quarantined, never attached to nearest dates.
    pub fn resolve_row(&self, native_symbol: &str, date: NaiveDate) -> Result<InstrumentId> {
        let mut matches = self
            .input
            .lifecycles
            .iter()
            .filter(|l| l.native_symbol == native_symbol && l.contains(date));
        let life = matches
            .next()
            .ok_or_else(|| error("unattributed row: no evidenced lifecycle"))?;
        if matches.next().is_some() {
            return Err(error("ambiguous price-row lifecycle"));
        }
        life.instrument_id()
    }
    /// Current directory differences are diagnostic only: they do not change the
    /// frozen Catalog, version, attributes or historical snapshots.
    pub fn compare_current(&self, current: &BTreeSet<String>) -> BTreeMap<String, String> {
        let historical: BTreeSet<_> = self
            .input
            .lifecycles
            .iter()
            .map(|l| l.native_symbol.clone())
            .collect();
        historical
            .symmetric_difference(current)
            .map(|c| {
                (
                    c.clone(),
                    if historical.contains(c) {
                        "historical_only"
                    } else {
                        "current_only"
                    }
                    .into(),
                )
            })
            .collect()
    }
    pub fn universe(&self, session_date: NaiveDate) -> Result<UniverseBuildResult> {
        let result = self.evaluate_membership(session_date)?;
        if !result.run_spec.coverage_gaps.is_empty() {
            return Err(error(format!(
                "incomplete Session membership evidence: {:?}; exclusions: {:?}",
                result.run_spec.coverage_gaps, result.excluded
            )));
        }
        Ok(result)
    }
    /// Diagnostic report has no successful Universe identity or usable member pool.
    pub fn diagnose_membership(&self, session_date: NaiveDate) -> Result<MembershipDiagnostics> {
        let result = self.evaluate_membership(session_date)?;
        Ok(MembershipDiagnostics {
            run_spec: result.run_spec,
            excluded: result.excluded,
        })
    }
    fn evaluate_membership(&self, session_date: NaiveDate) -> Result<UniverseBuildResult> {
        if !self.gaps.is_empty() {
            return Err(error(format!(
                "incomplete historical coverage: {:?}",
                self.gaps
            )));
        }
        if session_date < self.input.coverage.start || session_date > self.input.coverage.end {
            return Err(error("decision outside Catalog coverage"));
        }
        let session = self
            .input
            .sessions
            .iter()
            .find(|s| s.session_date() == session_date)
            .ok_or_else(|| error("decision is not a covered Session"))?;
        let now = session.ts_close();
        // Every constituent of the list must itself be historically available.
        if self
            .input
            .broad_indices
            .evidence
            .iter()
            .any(|e| e.available_at(&self.input).is_ok_and(|t| t > now))
        {
            return Err(error("broad-index list not yet knowable"));
        }
        let mut members = Vec::new();
        let mut excluded = BTreeMap::new();
        let mut session_gaps = self.gaps.clone();
        for life in &self.input.lifecycles {
            let id = life.instrument_id()?;
            let reason = if session_date < life.listing.effective_date {
                Some("before_listing")
            } else if life.listing.evidence.available_at(&self.input)? > now {
                Some("listing_not_known")
            } else if life.termination.as_ref().is_some_and(|t| {
                session_date >= t.effective_date
                    && t.evidence
                        .available_at(&self.input)
                        .is_ok_and(|at| at <= now)
            }) {
                Some("terminated")
            } else {
                let facts: Vec<_> = life
                    .attributes
                    .iter()
                    .filter(|a| {
                        a.effective_date <= session_date
                            && a.evidence
                                .available_at(&self.input)
                                .is_ok_and(|at| at <= now)
                    })
                    .collect();
                let latest = facts.iter().map(|a| a.effective_date).max();
                let latest: Vec<_> = facts
                    .into_iter()
                    .filter(|a| Some(a.effective_date) == latest)
                    .collect();
                match latest.first() {
                    None => Some("missing_attributes"),
                    Some(first)
                        if latest.iter().any(|a| {
                            a.category != first.category || a.index_id != first.index_id
                        }) =>
                    {
                        Some("attribute_conflict")
                    }
                    Some(first) if first.category != FundCategory::Equity => {
                        Some("not_equity_fund")
                    }
                    Some(first)
                        if !self.input.broad_indices.index_ids.contains(&first.index_id) =>
                    {
                        Some("not_domestic_broad_index")
                    }
                    Some(_) => None,
                }
            };
            if let Some(reason) = reason {
                if matches!(reason, "missing_attributes" | "attribute_conflict") {
                    session_gaps.insert(format!(
                        "membership_unresolved:{id}:{session_date}:{reason}"
                    ));
                }
                excluded.insert(id, reason.into());
            } else {
                members.push(id);
            }
        }
        let mut active_codes = BTreeMap::new();
        for id in &members {
            let code = self
                .input
                .lifecycles
                .iter()
                .find(|l| l.instrument_id().as_ref() == Ok(id))
                .expect("member from Catalog")
                .native_symbol
                .clone();
            if active_codes.insert(code, id.clone()).is_some() {
                return Err(error("as-of code reuse identity conflict"));
            }
        }
        members.sort();
        let run_spec = UniverseRunSpec {
            semantic_version: "szse-pit-universe@1".into(),
            catalog_version: self.version()?,
            broad_index_version: hash("broad-index", &self.input.broad_indices)?,
            rule_version: self.input.rule_version.clone(),
            coverage: self.input.coverage.clone(),
            coverage_gaps: session_gaps,
            sources: self.sources(),
            decision_session: session_date,
            decision_at: now,
        };
        let universe_version = hash("pit-uni", &(&run_spec, &members, &excluded))?;
        Ok(UniverseBuildResult {
            universe_version,
            run_spec,
            members,
            excluded,
        })
    }
    fn sources(&self) -> BTreeSet<String> {
        let mut hashes = self.input.calendar_raw_sha256.clone();
        hashes.extend(self.input.coverage.announcement_raw_sha256.iter().cloned());
        hashes.extend(self.input.scans.iter().map(|s| s.raw_sha256.clone()));
        hashes.extend(
            self.input
                .broad_indices
                .evidence
                .iter()
                .map(|e| e.raw_sha256.clone()),
        );
        hashes.extend(
            self.input
                .coverage
                .resolutions
                .iter()
                .map(|r| r.evidence.raw_sha256.clone()),
        );
        for l in &self.input.lifecycles {
            hashes.insert(l.contract_raw_sha256.clone());
            hashes.insert(l.listing.evidence.raw_sha256.clone());
            if let Some(t) = &l.termination {
                hashes.insert(t.evidence.raw_sha256.clone());
            }
            hashes.extend(l.attributes.iter().map(|a| a.evidence.raw_sha256.clone()));
        }
        hashes
    }
    /// Write a reviewable frozen document. Revisions must use a new destination;
    /// loading always verifies the referenced Raw objects again.
    pub fn store(&self, path: impl AsRef<Path>) -> Result<()> {
        use std::io::Write;
        let value = serde_json::to_value(&self.input).map_err(|e| error(e.to_string()))?;
        let bytes = restricted_jcs(&value).map_err(|e| error(e.to_string()))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| error(e.to_string()))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| error(e.to_string()))
    }
    pub fn load(path: impl AsRef<Path>, expected_version: &str, raw: &RawStore) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| error(e.to_string()))?;
        let input = serde_json::from_slice(&bytes).map_err(|e| error(e.to_string()))?;
        let catalog = Self::build(input, raw)?;
        if catalog.version()? != expected_version {
            return Err(error("frozen Catalog version mismatch"));
        }
        Ok(catalog)
    }
}
/// Identity-bearing input fragment for downstream Run Specs. Kept with the
/// build result at all detail levels; Engine wiring is #177, Python views #178.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UniverseRunSpec {
    pub semantic_version: String,
    pub catalog_version: String,
    pub broad_index_version: String,
    pub rule_version: String,
    pub coverage: Coverage,
    pub coverage_gaps: BTreeSet<String>,
    pub sources: BTreeSet<String>,
    pub decision_session: NaiveDate,
    pub decision_at: TimestampNs,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UniverseBuildResult {
    pub universe_version: String,
    pub run_spec: UniverseRunSpec,
    pub members: Vec<InstrumentId>,
    pub excluded: BTreeMap<InstrumentId, String>,
}

mod catalog_sessions {
    use super::*;
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Row {
        venue_id: VenueId,
        session_date: NaiveDate,
        ts_open: TimestampNs,
        ts_close: TimestampNs,
    }
    pub fn serialize<S: serde::Serializer>(
        sessions: &[Session],
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        sessions
            .iter()
            .map(|s| Row {
                venue_id: s.venue_id().clone(),
                session_date: s.session_date(),
                ts_open: s.ts_open(),
                ts_close: s.ts_close(),
            })
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Vec<Session>, D::Error> {
        Vec::<Row>::deserialize(deserializer)?
            .into_iter()
            .map(|r| {
                Session::new(r.venue_id, r.session_date, r.ts_open, r.ts_close)
                    .map_err(serde::de::Error::custom)
            })
            .collect()
    }
}

/// Incomplete selection diagnostics, never accepted as a PIT Universe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipDiagnostics {
    pub run_spec: UniverseRunSpec,
    pub excluded: BTreeMap<InstrumentId, String>,
}
