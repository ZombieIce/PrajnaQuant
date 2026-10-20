use super::{
    FirstSessionReference, FundCategoryEvidence, LimitRule, LimitRuleEvidence, OfficialLimitRule,
};
use crate::pit_universe::{
    CatalogError, CatalogInput, Evidence, Lifecycle, LifecycleValidity, Publication,
};
use chrono::NaiveDate;
use prajna_domain::{InstrumentId, Price, Session, TimestampNs};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

type Result<T> = std::result::Result<T, CatalogError>;

const OFFICIAL_V1: &str = include_str!("../../../../configs/mvp-6/limit_rules_v1.json");
const OFFICIAL_V2: &str = include_str!("../../../../configs/mvp-6/limit_rules_v2.json");

/// The Catalog lifecycle identity that a public category assignment binds to.
/// Public assignments keep their six-digit `native_symbol` key; this binding is
/// recorded separately in the rule evidence and must equal the Catalog lifecycle
/// under which the session is evaluated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleBinding {
    pub instrument_id: InstrumentId,
    pub native_symbol: String,
    /// First Session attributed to the lifecycle (`Lifecycle::validity`).
    pub effective_from: NaiveDate,
    /// Exclusive end of the lifecycle; `None` means open-ended.
    pub effective_until: Option<NaiveDate>,
}
impl LifecycleBinding {
    pub fn from_lifecycle(life: &Lifecycle) -> Result<Self> {
        let validity = life.validity();
        Ok(Self {
            instrument_id: life.instrument_id()?,
            native_symbol: life.native_symbol.clone(),
            effective_from: validity.from,
            effective_until: validity.until,
        })
    }
    pub fn validity(&self) -> LifecycleValidity {
        LifecycleValidity {
            from: self.effective_from,
            until: self.effective_until,
        }
    }
    pub fn contains(&self, session_date: NaiveDate) -> bool {
        self.validity().contains(session_date)
    }
}

/// One lifecycle identity plus the first identity of the fund it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChainMember {
    binding: LifecycleBinding,
    fund_start: NaiveDate,
}

/// A rule-binding target plus every lifecycle sharing its native symbol.
/// Assignments are keyed by code, so a binding must also reject a category whose
/// interval spans a reused code's different fund. Successors that continue the
/// same fund (for example a legal-name change) stay in one fund chain and bind
/// inside each lifecycle's own validity window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleBinding {
    target: LifecycleBinding,
    /// Every lifecycle for the native symbol, tagged with its fund chain.
    chain: Vec<ChainMember>,
}
impl RuleBinding {
    pub fn from_lifecycles(
        lifecycles: &[Lifecycle],
        instrument_id: &InstrumentId,
    ) -> Result<Option<Self>> {
        let Some(target) = lifecycles
            .iter()
            .find(|life| life.instrument_id().as_ref() == Ok(instrument_id))
        else {
            return Ok(None);
        };
        let native_symbol = target.native_symbol.clone();
        let mut group = lifecycles
            .iter()
            .filter(|life| life.native_symbol == native_symbol)
            .collect::<Vec<_>>();
        group.sort_by_key(|life| life.identity_start.unwrap_or(life.listing.effective_date));
        let mut chain = Vec::with_capacity(group.len());
        let mut fund_start = None;
        let mut predecessor_terminated = true;
        for (index, life) in group.into_iter().enumerate() {
            let binding = LifecycleBinding::from_lifecycle(life)?;
            // `identity_available_from` is the Catalog's explicit legal-name-change
            // marker; it can only continue a fund whose predecessor was not
            // terminated. Everything else (a new listing after termination, or an
            // unmarked successor) starts a different fund that reused the code.
            let continues_fund =
                index > 0 && !predecessor_terminated && life.identity_available_from.is_some();
            if !continues_fund {
                fund_start = Some(binding.effective_from);
            }
            chain.push(ChainMember {
                binding,
                fund_start: fund_start.expect("the first lifecycle starts a fund chain"),
            });
            predecessor_terminated = life.termination.is_some();
        }
        let target = chain
            .iter()
            .find(|member| member.binding.instrument_id == *instrument_id)
            .map(|member| member.binding.clone())
            .expect("target is part of its native-symbol group");
        Ok(Some(Self { target, chain }))
    }

    pub fn target(&self) -> &LifecycleBinding {
        &self.target
    }

    fn target_chain_start(&self) -> NaiveDate {
        self.chain
            .iter()
            .find(|member| member.binding == self.target)
            .map(|member| member.fund_start)
            .expect("target is part of the binding chain")
    }

    /// An assignment may not bind when its interval also covers a lifecycle of
    /// a different fund that reused the same code. The affected cells stay
    /// `CategoryUnknown`; the resolver never picks one fund arbitrarily.
    fn spans_another_fund(&self, assignment: &FundCategoryAssignment) -> bool {
        let target_chain = self.target_chain_start();
        self.chain.iter().any(|member| {
            member.fund_start != target_chain
                && member.binding.native_symbol == assignment.instrument_id.symbol()
                && intervals_overlap(
                    assignment.effective_from,
                    assignment.effective_until,
                    member.binding.effective_from,
                    member.binding.effective_until,
                )
        })
    }
}

/// Resolve the unique assignment for the binding's native symbol and session.
/// Both the rule resolver and the coverage classifier use this function so a
/// market cell and the coverage report cannot disagree about category binding.
fn bound_assignment<'a>(
    binding: &RuleBinding,
    session_date: NaiveDate,
    assignments: &'a [FundCategoryAssignment],
) -> Option<&'a FundCategoryAssignment> {
    if !binding.target.contains(session_date) {
        return None;
    }
    let target = &binding.target;
    let assignment = unique_match(assignments.iter().filter(|assignment| {
        assignment.instrument_id.symbol() == target.native_symbol
            && assignment.instrument_id.venue() == target.instrument_id.venue()
            && assignment.effective_from <= session_date
            && assignment
                .effective_until
                .is_none_or(|end| session_date < end)
    }))?;
    (!binding.spans_another_fund(assignment)).then_some(assignment)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FundCategoryAssignment {
    pub instrument_id: InstrumentId,
    pub fund_category: String,
    pub effective_from: NaiveDate,
    pub effective_until: Option<NaiveDate>,
    pub document_number: String,
    pub evidence: CategorySourceEvidence,
    #[serde(default)]
    pub supporting_sources: Vec<CategorySourceEvidence>,
    pub evidence_level: EvidenceLevel,
}

impl FundCategoryAssignment {
    pub fn identity(&self) -> std::result::Result<String, CatalogError> {
        super::identity("fund-category-assignment@1", self)
    }

    fn evidence_tier_is_valid(&self) -> bool {
        match self.evidence_level {
            EvidenceLevel::Primary => {
                self.evidence.source_type == CategoryEvidenceType::OfficialList
                    && self.supporting_sources.is_empty()
            }
            EvidenceLevel::Secondary => {
                self.evidence.source_type == CategoryEvidenceType::FundContract
                    && self
                        .supporting_sources
                        .iter()
                        .any(|source| source.source_type == CategoryEvidenceType::SzseCategoryRule)
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FundLimitRuleRecord {
    pub id: String,
    pub fund_category: String,
    pub category_description: String,
    pub effective_from: NaiveDate,
    pub effective_until: Option<NaiveDate>,
    pub rate_bps: u32,
    pub tick: Price,
    pub rounding: String,
    pub applies_on_first_session: bool,
    pub first_session_reference: FirstSessionReference,
    pub document_number: String,
    pub evidence: RuleSourceEvidence,
    #[serde(default)]
    pub supporting_evidence: Vec<SupportingRuleEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportingRuleEvidence {
    pub effective_from: NaiveDate,
    pub document_number: String,
    pub evidence: RuleSourceEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    Primary,
    Secondary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryEvidenceType {
    OfficialList,
    FundContract,
    SzseCategoryRule,
}

/// Public source citation for a fund category. The source Raw and its hash
/// remain local; this citation identifies only the public document and its
/// locating information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategorySourceEvidence {
    pub source_type: CategoryEvidenceType,
    pub announcement_id: String,
    pub locator: String,
    pub publication: Publication,
}

impl CategorySourceEvidence {
    fn validate(&self) -> Result<()> {
        if self.announcement_id.trim().is_empty() || self.locator.trim().is_empty() {
            return Err(CatalogError("invalid fund category source citation".into()));
        }
        Ok(())
    }
}

/// Public citation for an official rule. Raw bytes and their hashes remain local.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSourceEvidence {
    pub evidence_level: EvidenceLevel,
    pub announcement_id: String,
    pub locator: String,
    pub publication: Publication,
}

impl RuleSourceEvidence {
    fn validate(&self, document_number: &str) -> Result<()> {
        if self.evidence_level != EvidenceLevel::Primary
            || self.announcement_id != document_number
            || self.announcement_id.trim().is_empty()
            || self.locator.trim().is_empty()
        {
            return Err(CatalogError("invalid official rule source citation".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FundLimitRuleTable {
    pub version: String,
    pub coverage_start: NaiveDate,
    pub coverage_end_exclusive: NaiveDate,
    pub records: Vec<FundLimitRuleRecord>,
    #[serde(default)]
    pub category_assignments: Vec<FundCategoryAssignment>,
    #[serde(default)]
    pub unresolved_memberships: Vec<UnresolvedMembership>,
    pub membership_scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedMembership {
    pub native_symbol: String,
    pub effective_from: NaiveDate,
    pub effective_until: NaiveDate,
    pub status: String,
    pub reason: String,
}

impl FundLimitRuleTable {
    pub fn official_v1() -> Result<Self> {
        let table = Self::parse_public_json(OFFICIAL_V1.as_bytes())?;
        Ok(table)
    }

    pub fn official_v2() -> Result<Self> {
        Self::parse_public_json(OFFICIAL_V2.as_bytes())
    }

    /// Parses and validates a public limit-rule JSON document. Public rule
    /// assignments carry document citations, never local Raw hashes.
    pub fn parse_public_json(bytes: &[u8]) -> Result<Self> {
        let table: Self = serde_json::from_slice(bytes)
            .map_err(|error| CatalogError(format!("invalid public limit-rule table: {error}")))?;
        table.validate_public()?;
        Ok(table)
    }

    pub fn rule_for(
        &self,
        binding: &RuleBinding,
        session_date: NaiveDate,
    ) -> Result<Option<LimitRule>> {
        self.rule_for_with_assignments(binding, session_date, &self.category_assignments)
    }

    /// Resolve the assignment for the binding's native symbol and then the
    /// matching rule record. Return `None` for missing, ambiguous, out-of-lifecycle
    /// or out-of-interval categories; such cells stay `UNKNOWN`.
    pub fn rule_for_with_assignments(
        &self,
        binding: &RuleBinding,
        session_date: NaiveDate,
        assignments: &[FundCategoryAssignment],
    ) -> Result<Option<LimitRule>> {
        if session_date < self.coverage_start || session_date >= self.coverage_end_exclusive {
            return Ok(None);
        }

        let Some(assignment) = bound_assignment(binding, session_date, assignments) else {
            return Ok(None);
        };
        assignment.evidence.validate()?;
        if !assignment.evidence_tier_is_valid() {
            return Err(CatalogError(
                "category assignment evidence tier is incomplete".into(),
            ));
        }
        for source in &assignment.supporting_sources {
            source.validate()?;
        }
        if assignment.document_number.trim().is_empty()
            || assignment.document_number != assignment.evidence.announcement_id
            || assignment.fund_category.trim().is_empty()
            || assignment
                .effective_until
                .is_some_and(|end| end <= assignment.effective_from)
        {
            return Err(CatalogError("invalid fund-category assignment".into()));
        }

        let Some(record) = unique_match(self.records.iter().filter(|record| {
            record.fund_category == assignment.fund_category
                && record.effective_from <= session_date
                && record.effective_until.is_none_or(|end| session_date < end)
        })) else {
            return Ok(None);
        };

        Ok(Some(LimitRule {
            version: format!("{}/{}", self.version, record.id),
            effective_from: record.effective_from,
            effective_until: record.effective_until,
            rate_bps: record.rate_bps,
            tick: record.tick,
            rounding: record.rounding.clone(),
            applies_on_first_session: record.applies_on_first_session,
            first_session_reference: Some(record.first_session_reference),
            evidence: LimitRuleEvidence::Citation(assignment.evidence.clone()),
            supporting_evidence: Vec::new(),
            rule_evidence: std::iter::once(record.evidence.clone())
                .chain(
                    record
                        .supporting_evidence
                        .iter()
                        .map(|support| support.evidence.clone()),
                )
                .collect(),
            official_rule: Some(OfficialLimitRule {
                document_number: record.document_number.clone(),
                fund_category: record.fund_category.clone(),
                category_evidence: Some(FundCategoryEvidence {
                    instrument_id: assignment.instrument_id.clone(),
                    fund_category: assignment.fund_category.clone(),
                    assignment_identity: assignment.identity()?,
                    document_number: assignment.document_number.clone(),
                    effective_from: assignment.effective_from,
                    effective_until: assignment.effective_until,
                    evidence: LimitRuleEvidence::Citation(assignment.evidence.clone()),
                    supporting_sources: assignment.supporting_sources.clone(),
                    evidence_level: assignment.evidence_level,
                    lifecycle: Some(binding.target.clone()),
                }),
            }),
        }))
    }

    /// The only category-assignment lookup used by coverage classification. It
    /// applies the same lifecycle and reused-code rules as rule resolution.
    pub fn category_assignment_for(
        &self,
        binding: &RuleBinding,
        session_date: NaiveDate,
    ) -> Option<&FundCategoryAssignment> {
        bound_assignment(binding, session_date, &self.category_assignments)
    }

    /// Validate the rule's evidence against an authoritative binding. Both the
    /// assignment identity (computed from the public six-digit assignment) and
    /// the lifecycle binding must hold; neither is derived by rewriting the
    /// public assignment's `instrument_id`.
    pub fn supports(
        &self,
        rule: &LimitRule,
        binding: &RuleBinding,
        session_date: NaiveDate,
    ) -> bool {
        if session_date < self.coverage_start || session_date >= self.coverage_end_exclusive {
            return false;
        }
        let Some(official_rule) = rule.official_rule.as_ref() else {
            return false;
        };
        let Some(category_evidence) = official_rule.category_evidence.as_ref() else {
            return false;
        };
        let Some(category_source) = category_evidence.evidence.as_citation() else {
            return false;
        };
        let assignment = FundCategoryAssignment {
            instrument_id: category_evidence.instrument_id.clone(),
            fund_category: category_evidence.fund_category.clone(),
            effective_from: category_evidence.effective_from,
            effective_until: category_evidence.effective_until,
            document_number: category_evidence.document_number.clone(),
            evidence: category_source.clone(),
            supporting_sources: category_evidence.supporting_sources.clone(),
            evidence_level: category_evidence.evidence_level,
        };
        if !assignment
            .identity()
            .is_ok_and(|identity| identity == category_evidence.assignment_identity)
        {
            return false;
        }
        if category_evidence.document_number.trim().is_empty()
            || category_evidence.lifecycle.as_ref() != Some(&binding.target)
            || category_evidence.instrument_id.symbol() != binding.target.native_symbol
            || category_evidence.instrument_id.venue() != binding.target.instrument_id.venue()
            || category_evidence.fund_category != official_rule.fund_category
            || rule.evidence != category_evidence.evidence
            || !rule.supporting_evidence.is_empty()
            || category_evidence.document_number != category_source.announcement_id
            || category_source.validate().is_err()
            || !assignment.evidence_tier_is_valid()
            || category_evidence
                .supporting_sources
                .iter()
                .any(|source| source.validate().is_err())
            || bound_assignment(binding, session_date, std::slice::from_ref(&assignment)).is_none()
        {
            return false;
        }

        let Some(record) = unique_match(self.records.iter().filter(|record| {
            record.fund_category == official_rule.fund_category
                && record.effective_from <= session_date
                && record.effective_until.is_none_or(|end| session_date < end)
        })) else {
            return false;
        };
        rule.version == format!("{}/{}", self.version, record.id)
            && rule.effective_from == record.effective_from
            && rule.effective_until == record.effective_until
            && rule.rate_bps == record.rate_bps
            && rule.tick == record.tick
            && rule.rounding == record.rounding
            && rule.applies_on_first_session == record.applies_on_first_session
            && rule.first_session_reference == Some(record.first_session_reference)
            && official_rule.document_number == record.document_number
            && rule.rule_evidence
                == std::iter::once(record.evidence.clone())
                    .chain(
                        record
                            .supporting_evidence
                            .iter()
                            .map(|support| support.evidence.clone()),
                    )
                    .collect::<Vec<_>>()
    }

    pub fn evidence_available_at(
        &self,
        rule: &LimitRule,
        binding: &RuleBinding,
        evidence: &Evidence,
        session_date: NaiveDate,
        calendar: &CatalogInput,
    ) -> Result<TimestampNs> {
        if !self.supports(rule, binding, session_date) {
            return Err(CatalogError(
                "cannot resolve availability for an unsupported limit rule".into(),
            ));
        }
        evidence.available_at(calendar)
    }

    /// Returns `None` when the public citation cannot be placed on the frozen
    /// calendar (for example a date-only contract publication before the
    /// covered window). The caller keeps the affected cell UNKNOWN; it must
    /// not fabricate an availability timestamp or abort other instruments.
    pub fn category_evidence_available_at(
        &self,
        rule: &LimitRule,
        binding: &RuleBinding,
        evidence: &CategorySourceEvidence,
        session_date: NaiveDate,
        _session_open: TimestampNs,
        calendar: &CatalogInput,
    ) -> Result<Option<TimestampNs>> {
        if !self.supports(rule, binding, session_date) {
            return Err(CatalogError(
                "cannot resolve availability for an unsupported limit rule".into(),
            ));
        }
        let assignment_has_source = rule
            .official_rule
            .as_ref()
            .and_then(|official| official.category_evidence.as_ref())
            .is_some_and(|category| {
                category.evidence.as_citation() == Some(evidence)
                    || category.supporting_sources.contains(evidence)
            });
        if !assignment_has_source {
            return Err(CatalogError("unsupported category source citation".into()));
        }
        Ok(match evidence.publication {
            Publication::Exact { timestamp } => Some(timestamp),
            Publication::DateOnly { date } => {
                if date < calendar.calendar_start {
                    return Ok(None);
                }
                calendar
                    .sessions
                    .iter()
                    .find(|session| session.session_date() > date)
                    .map(Session::ts_close)
            }
        })
    }

    pub fn rule_evidence_available_at(
        &self,
        rule: &LimitRule,
        binding: &RuleBinding,
        evidence: &RuleSourceEvidence,
        session_date: NaiveDate,
        session_open: TimestampNs,
        calendar: &CatalogInput,
    ) -> Result<TimestampNs> {
        if !self.supports(rule, binding, session_date) {
            return Err(CatalogError(
                "cannot resolve availability for an unsupported limit rule".into(),
            ));
        }
        let record = unique_match(self.records.iter().filter(|record| {
            record.fund_category == rule.official_rule.as_ref().unwrap().fund_category
                && record.effective_from <= session_date
                && record.effective_until.is_none_or(|end| session_date < end)
        }))
        .ok_or_else(|| CatalogError("limit-rule evidence has no unique rule record".into()))?;
        let effective_from = if record.evidence == *evidence {
            Some(record.effective_from)
        } else {
            record
                .supporting_evidence
                .iter()
                .find(|support| support.evidence == *evidence)
                .map(|support| support.effective_from)
        }
        .ok_or_else(|| CatalogError("unsupported limit-rule evidence citation".into()))?;

        match evidence.publication {
            Publication::Exact { timestamp } => Ok(timestamp),
            Publication::DateOnly { date }
                if date < calendar.calendar_start
                    && date < effective_from
                    && effective_from <= session_date =>
            {
                let nanos = session_open
                    .as_unix_nanos()
                    .checked_sub(1)
                    .ok_or_else(|| CatalogError("session open timestamp underflow".into()))?;
                Ok(TimestampNs::from_unix_nanos(nanos))
            }
            Publication::DateOnly { date } => {
                if date < calendar.calendar_start || date >= calendar.calendar_end {
                    return Err(CatalogError(
                        "date-only rule publication outside covered calendar".into(),
                    ));
                }
                calendar
                    .sessions
                    .iter()
                    .find(|session| session.session_date() > date)
                    .map(Session::ts_close)
                    .ok_or_else(|| {
                        CatalogError("next Session after rule publication is not covered".into())
                    })
            }
        }
    }

    pub fn validate_public(&self) -> Result<()> {
        if self.version.trim().is_empty()
            || self.coverage_start >= self.coverage_end_exclusive
            || self.records.is_empty()
            || self.membership_scope != "coverage_window_not_lifecycle"
        {
            return Err(CatalogError("invalid fund limit-rule table header".into()));
        }
        for record in &self.records {
            if record.id.trim().is_empty()
                || record.fund_category.trim().is_empty()
                || record.category_description.trim().is_empty()
                || record.document_number.trim().is_empty()
                || record.document_number != record.evidence.announcement_id
                || record
                    .effective_until
                    .is_some_and(|end| end <= record.effective_from)
                || record.rate_bps == 0
                || record.rate_bps >= 10000
                || record.tick <= Price::ZERO
                || record.rounding != "nearest_tick_half_up"
                || !record.applies_on_first_session
            {
                return Err(CatalogError(format!(
                    "invalid fund limit-rule record {}",
                    record.id
                )));
            }
            record.evidence.validate(&record.document_number)?;
            for supporting_evidence in &record.supporting_evidence {
                supporting_evidence
                    .evidence
                    .validate(&supporting_evidence.document_number)?;
            }
        }

        let expected_symbols = ["159919", "159560", "159522", "159612", "159952"];
        match self.version.as_str() {
            "szse-fund-limit-rules@1" => {
                if !self.category_assignments.is_empty()
                    || self.unresolved_memberships.len() != expected_symbols.len()
                    || expected_symbols.iter().any(|symbol| {
                        self.unresolved_memberships
                            .iter()
                            .filter(|row| {
                                row.native_symbol == *symbol
                                    && row.effective_from == self.coverage_start
                                    && row.effective_until == self.coverage_end_exclusive
                                    && row.status == "unknown"
                                    && !row.reason.trim().is_empty()
                            })
                            .count()
                            != 1
                    })
                {
                    return Err(CatalogError(
                        "invalid fixed-ETF unresolved category coverage".into(),
                    ));
                }
            }
            "szse-fund-limit-rules@2" => {
                let allowed: BTreeSet<_> = expected_symbols.into_iter().collect();
                for assignment in &self.category_assignments {
                    let symbol = assignment.instrument_id.symbol();
                    if !allowed.contains(symbol)
                        || assignment.effective_from < self.coverage_start
                        || assignment.effective_until.is_some_and(|end| {
                            end > self.coverage_end_exclusive || end <= assignment.effective_from
                        })
                        || !self
                            .records
                            .iter()
                            .any(|record| record.fund_category == assignment.fund_category)
                        || assignment.document_number.trim().is_empty()
                        || assignment.document_number != assignment.evidence.announcement_id
                    {
                        return Err(CatalogError("invalid PIT fund-category assignment".into()));
                    }
                    assignment.evidence.validate()?;
                    for source in &assignment.supporting_sources {
                        source.validate()?;
                    }
                    if !assignment.evidence_tier_is_valid() {
                        return Err(CatalogError("category evidence tier is incomplete".into()));
                    }
                }
                for unresolved in &self.unresolved_memberships {
                    if !allowed.contains(unresolved.native_symbol.as_str())
                        || unresolved.effective_from < self.coverage_start
                        || unresolved.effective_until > self.coverage_end_exclusive
                        || unresolved.effective_until <= unresolved.effective_from
                        || unresolved.status != "unknown"
                        || unresolved.reason.trim().is_empty()
                    {
                        return Err(CatalogError(
                            "invalid unresolved fund-category interval".into(),
                        ));
                    }
                }
                for symbol in expected_symbols {
                    let mut intervals = self
                        .category_assignments
                        .iter()
                        .filter(|assignment| assignment.instrument_id.symbol() == symbol)
                        .map(|assignment| {
                            (
                                assignment.effective_from,
                                assignment
                                    .effective_until
                                    .unwrap_or(self.coverage_end_exclusive),
                            )
                        })
                        .chain(
                            self.unresolved_memberships
                                .iter()
                                .filter(|row| row.native_symbol == symbol)
                                .map(|row| (row.effective_from, row.effective_until)),
                        )
                        .collect::<Vec<_>>();
                    intervals.sort_unstable();
                    if intervals.first().map(|range| range.0) != Some(self.coverage_start)
                        || intervals.last().map(|range| range.1)
                            != Some(self.coverage_end_exclusive)
                        || intervals.windows(2).any(|pair| pair[0].1 != pair[1].0)
                    {
                        return Err(CatalogError(format!(
                            "fund-category intervals must partition the coverage for {symbol}"
                        )));
                    }
                }
            }
            _ => {
                return Err(CatalogError(
                    "unsupported fund limit-rule table version".into(),
                ));
            }
        }

        for (index, record) in self.records.iter().enumerate() {
            if self.records[index + 1..].iter().any(|other| {
                record.fund_category == other.fund_category
                    && intervals_overlap(
                        record.effective_from,
                        record.effective_until,
                        other.effective_from,
                        other.effective_until,
                    )
            }) {
                return Err(CatalogError(format!(
                    "overlapping rule periods for {}",
                    record.fund_category
                )));
            }
        }
        Ok(())
    }
}

fn unique_match<T>(mut matches: impl Iterator<Item = T>) -> Option<T> {
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn intervals_overlap(
    first_start: NaiveDate,
    first_end: Option<NaiveDate>,
    second_start: NaiveDate,
    second_end: Option<NaiveDate>,
) -> bool {
    first_end.is_none_or(|end| second_start < end) && second_end.is_none_or(|end| first_start < end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pit_universe::Publication;
    use prajna_domain::TimestampNs;

    fn date(value: &str) -> NaiveDate {
        value.parse().unwrap()
    }

    fn category_assignment(
        instrument_id: &InstrumentId,
        fund_category: &str,
        effective_from: &str,
        effective_until: Option<&str>,
        document_number: &str,
    ) -> FundCategoryAssignment {
        FundCategoryAssignment {
            instrument_id: instrument_id.clone(),
            fund_category: fund_category.into(),
            effective_from: date(effective_from),
            effective_until: effective_until.map(date),
            document_number: document_number.into(),
            evidence: CategorySourceEvidence {
                source_type: CategoryEvidenceType::OfficialList,
                announcement_id: document_number.into(),
                locator: "synthetic category-list row".into(),
                publication: Publication::Exact {
                    timestamp: "2023-04-09T08:00:00+08:00".parse::<TimestampNs>().unwrap(),
                },
            },
            supporting_sources: Vec::new(),
            evidence_level: EvidenceLevel::Primary,
        }
    }

    fn lifecycle_evidence() -> Evidence {
        Evidence {
            raw_sha256: format!("sha256:{}", "a".repeat(64)),
            announcement_id: "SYNTHETIC-LIFECYCLE".into(),
            locator: "synthetic lifecycle fact".into(),
            publication: Publication::Exact {
                timestamp: "2022-01-01T08:00:00+08:00".parse::<TimestampNs>().unwrap(),
            },
        }
    }

    fn lifecycle(
        code: &str,
        listing: &str,
        identity_start: Option<&str>,
        identity_end: Option<&str>,
        termination: Option<&str>,
        identity_available_from: Option<&str>,
    ) -> Lifecycle {
        Lifecycle {
            native_symbol: code.into(),
            contract_raw_sha256: format!("sha256:{}", "b".repeat(64)),
            listing: crate::pit_universe::LifecycleFact {
                effective_date: date(listing),
                evidence: lifecycle_evidence(),
            },
            termination: termination.map(|value| crate::pit_universe::LifecycleFact {
                effective_date: date(value),
                evidence: lifecycle_evidence(),
            }),
            attributes: Vec::new(),
            identity_start: identity_start.map(date),
            identity_end_exclusive: identity_end.map(date),
            identity_available_from: identity_available_from.map(date),
            code_table_first_seen: None,
            code_table_last_seen: None,
            code_table_exit_date: None,
        }
    }

    fn binding_for(lifecycles: &[Lifecycle], instrument_id: &InstrumentId) -> RuleBinding {
        RuleBinding::from_lifecycles(lifecycles, instrument_id)
            .unwrap()
            .unwrap()
    }

    fn binding(lifecycles: &[Lifecycle], code: &str) -> RuleBinding {
        let id = lifecycles
            .iter()
            .find(|life| life.native_symbol == code)
            .unwrap()
            .instrument_id()
            .unwrap();
        binding_for(lifecycles, &id)
    }

    fn synthetic_calendar() -> CatalogInput {
        let hash = format!("sha256:{}", "a".repeat(64));
        CatalogInput {
            schema_version: 1,
            rule_version: "synthetic@1".into(),
            calendar_start: date("2022-10-01"),
            calendar_end: date("2026-10-01"),
            calendar_raw_sha256: BTreeSet::from([hash]),
            sessions: Vec::new(),
            coverage: crate::pit_universe::Coverage {
                start: date("2022-10-01"),
                end: date("2026-09-30"),
                scan_method: "synthetic".into(),
                announcement_method: "synthetic".into(),
                announcement_raw_sha256: BTreeSet::new(),
                gaps: BTreeSet::new(),
                resolutions: Vec::new(),
            },
            lifecycles: Vec::new(),
            scans: Vec::new(),
            broad_indices: None,
            code_ranges: None,
            code_tables: Vec::new(),
        }
    }

    fn public_code(code: &str) -> InstrumentId {
        format!("{code}.SZSE").parse().unwrap()
    }

    #[test]
    fn unique_match_requires_exactly_one_item() {
        assert_eq!(unique_match(std::iter::empty::<u8>()), None);
        assert_eq!(unique_match([1].into_iter()), Some(1));
        assert_eq!(unique_match([1, 2].into_iter()), None);
        assert_eq!(unique_match([1, 2, 3].into_iter()), None);
    }

    #[test]
    fn missing_or_overlapping_rule_records_are_unresolved_and_unsupported() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = "159001.SZSE".parse().unwrap();
        let lifecycles = [lifecycle("159001", "2022-10-01", None, None, None, None)];
        let binding = binding(&lifecycles, "159001");
        let session_date = date("2023-04-10");
        let assignments = [category_assignment(
            &instrument_id,
            "szse-fund-default-10pct",
            "2022-10-01",
            None,
            "SYNTHETIC-LIST-A",
        )];
        let rule = table
            .rule_for_with_assignments(&binding, session_date, &assignments)
            .unwrap()
            .unwrap();
        assert!(table.supports(&rule, &binding, session_date));

        let mut overlapping = table.clone();
        overlapping.records.extend(table.records.iter().cloned());
        let mut missing = table;
        missing.records.clear();
        for unresolved in [missing, overlapping] {
            assert!(
                unresolved
                    .rule_for_with_assignments(&binding, session_date, &assignments)
                    .unwrap()
                    .is_none()
            );
            assert!(!unresolved.supports(&rule, &binding, session_date));
        }
    }

    #[test]
    fn official_table_switches_rules_and_uses_pit_category_assignments() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = "159001.SZSE".parse().unwrap();
        let lifecycles = [lifecycle("159001", "2022-10-01", None, None, None, None)];
        let binding = binding(&lifecycles, "159001");
        let assignments = vec![
            category_assignment(
                &instrument_id,
                "szse-fund-default-10pct",
                "2022-10-01",
                Some("2023-04-10"),
                "SYNTHETIC-LIST-A",
            ),
            category_assignment(
                &instrument_id,
                "szse-20pct-index-etf-lof",
                "2023-04-10",
                None,
                "SYNTHETIC-LIST-B",
            ),
        ];
        let mut assignments = assignments;
        assignments[1].evidence_level = EvidenceLevel::Secondary;
        assignments[1].document_number = "SYNTHETIC-FUND-PROSPECTUS".into();
        assignments[1].evidence.announcement_id = "SYNTHETIC-FUND-PROSPECTUS".into();
        assignments[1].evidence.source_type = CategoryEvidenceType::FundContract;
        assignments[1].evidence.locator =
            "synthetic prospectus §1: the fund tracks a ChiNext-only index".into();
        assignments[1]
            .supporting_sources
            .push(CategorySourceEvidence {
                source_type: CategoryEvidenceType::SzseCategoryRule,
                announcement_id: "SYNTHETIC-SZSE-RULE".into(),
                locator: "SZSE fund limit category rule §2".into(),
                publication: Publication::DateOnly {
                    date: date("2023-02-17"),
                },
            });

        let before = table
            .rule_for_with_assignments(&binding, date("2023-04-09"), &assignments)
            .unwrap()
            .unwrap();
        let after = table
            .rule_for_with_assignments(&binding, date("2023-04-10"), &assignments)
            .unwrap()
            .unwrap();

        assert_eq!(before.rate_bps, 1000);
        assert_eq!(before.effective_until, Some(date("2023-04-10")));
        assert_eq!(after.rate_bps, 2000);
        let category_evidence = after
            .official_rule
            .as_ref()
            .unwrap()
            .category_evidence
            .as_ref()
            .unwrap();
        assert_eq!(category_evidence.evidence_level, EvidenceLevel::Secondary);
        assert_eq!(
            after.rule_evidence[0].evidence_level,
            EvidenceLevel::Primary
        );
        assert_eq!(
            category_evidence.evidence.as_citation().unwrap().locator,
            "synthetic prospectus §1: the fund tracks a ChiNext-only index"
        );
        assert!(after.effective_until.is_some());
        assert!(after.applies_on_first_session);
        assert_eq!(
            after.first_session_reference,
            Some(FirstSessionReference::MostRecentlyPublishedNetAssetValue)
        );
        assert_ne!(before.version, after.version);
        let mut altered_interval = after.clone();
        altered_interval
            .official_rule
            .as_mut()
            .unwrap()
            .category_evidence
            .as_mut()
            .unwrap()
            .effective_from = date("2022-10-01");
        assert!(!table.supports(&altered_interval, &binding, date("2023-04-10")));
        let mut reassigned = after.clone();
        reassigned
            .official_rule
            .as_mut()
            .unwrap()
            .category_evidence
            .as_mut()
            .unwrap()
            .instrument_id = "159002.SZSE".parse().unwrap();
        assert!(!table.supports(&reassigned, &binding, date("2023-04-10")));
        assert_eq!(
            after.official_rule.as_ref().unwrap().fund_category,
            "szse-20pct-index-etf-lof"
        );
    }

    #[test]
    fn default_rule_switches_first_session_reference_definition_on_2023_effective_date() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = "159001.SZSE".parse().unwrap();
        let lifecycles = [lifecycle("159001", "2022-10-01", None, None, None, None)];
        let binding = binding(&lifecycles, "159001");
        let assignments = [category_assignment(
            &instrument_id,
            "szse-fund-default-10pct",
            "2022-10-01",
            None,
            "SYNTHETIC-LIST-A",
        )];

        let before = table
            .rule_for_with_assignments(&binding, date("2023-04-09"), &assignments)
            .unwrap()
            .unwrap();
        let after = table
            .rule_for_with_assignments(&binding, date("2023-04-10"), &assignments)
            .unwrap()
            .unwrap();

        assert_eq!(
            before.first_session_reference,
            Some(FirstSessionReference::PreviousTradingDayNetAssetValue)
        );
        assert_eq!(
            after.first_session_reference,
            Some(FirstSessionReference::MostRecentlyPublishedNetAssetValue)
        );
    }

    #[test]
    fn public_table_lists_unknown_memberships_without_publishing_raw_hashes() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let codes: Vec<_> = table
            .unresolved_memberships
            .iter()
            .map(|row| row.native_symbol.as_str())
            .collect();
        assert_eq!(codes, ["159919", "159560", "159522", "159612", "159952"]);
        assert!(table.category_assignments.is_empty());
        assert!(!OFFICIAL_V1.contains("raw_sha256"));
        assert_eq!(table.membership_scope, "coverage_window_not_lifecycle");
    }

    #[test]
    fn public_v2_parser_resolves_evidenced_pit_categories_and_rejects_overlap() {
        let bytes = include_bytes!("../../../../configs/mvp-6/limit_rules_v2.json");
        let table = FundLimitRuleTable::parse_public_json(bytes).unwrap();
        assert_eq!(table.version, "szse-fund-limit-rules@2");
        assert!(!table.category_assignments.is_empty());
        assert!(!String::from_utf8_lossy(bytes).contains("raw_sha256"));

        let assignment = table.category_assignments[0].clone();
        let code = assignment.instrument_id.symbol().to_owned();
        let lifecycles = [lifecycle(&code, "2012-05-28", None, None, None, None)];
        let binding = binding(&lifecycles, &code);
        let rule = table
            .rule_for(&binding, assignment.effective_from)
            .unwrap()
            .unwrap();
        assert!(table.supports(&rule, &binding, assignment.effective_from));
        assert!(
            rule.official_rule
                .as_ref()
                .unwrap()
                .category_evidence
                .as_ref()
                .unwrap()
                .evidence
                .as_citation()
                .unwrap()
                .locator
                .contains("page")
        );

        let mut overlapping = table;
        overlapping.category_assignments.push(assignment);
        assert!(overlapping.validate_public().is_err());
    }

    #[test]
    fn missing_or_unknown_category_never_defaults_to_a_limit() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = "159001.SZSE".parse().unwrap();
        let lifecycles = [lifecycle("159001", "2022-10-01", None, None, None, None)];
        let binding = binding(&lifecycles, "159001");
        assert!(
            table
                .rule_for(&binding, date("2023-04-10"))
                .unwrap()
                .is_none()
        );

        let unknown = category_assignment(
            &instrument_id,
            "unlisted-category",
            "2022-10-01",
            None,
            "SYNTHETIC-UNKNOWN-LIST",
        );
        assert!(
            table
                .rule_for_with_assignments(&binding, date("2023-04-10"), &[unknown])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn overlapping_point_in_time_category_assignments_are_unresolved() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = "159001.SZSE".parse().unwrap();
        let lifecycles = [lifecycle("159001", "2022-10-01", None, None, None, None)];
        let binding = binding(&lifecycles, "159001");
        let first = category_assignment(
            &instrument_id,
            "szse-fund-default-10pct",
            "2022-10-01",
            None,
            "SYNTHETIC-LIST-A",
        );
        let second = category_assignment(
            &instrument_id,
            "szse-20pct-index-etf-lof",
            "2023-04-10",
            None,
            "SYNTHETIC-LIST-B",
        );
        assert!(
            table
                .rule_for_with_assignments(&binding, date("2023-04-10"), &[first, second])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn default_rule_periods_split_at_both_trading_rule_effective_dates() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = "159001.SZSE".parse().unwrap();
        let lifecycles = [lifecycle("159001", "2022-10-01", None, None, None, None)];
        let binding = binding(&lifecycles, "159001");
        let assignment = category_assignment(
            &instrument_id,
            "szse-fund-default-10pct",
            "2022-10-01",
            None,
            "SYNTHETIC-DEFAULT-CATEGORY",
        );
        let resolve = |session_date| {
            table
                .rule_for_with_assignments(
                    &binding,
                    date(session_date),
                    std::slice::from_ref(&assignment),
                )
                .unwrap()
                .unwrap()
        };

        assert_eq!(
            resolve("2023-04-09").version,
            "szse-fund-limit-rules@1/fund-default-10pct-2021"
        );
        assert_eq!(
            resolve("2023-04-10").version,
            "szse-fund-limit-rules@1/fund-default-10pct-2023"
        );
        assert_eq!(
            resolve("2026-07-05").version,
            "szse-fund-limit-rules@1/fund-default-10pct-2023"
        );
        assert_eq!(
            resolve("2026-07-06").version,
            "szse-fund-limit-rules@1/fund-default-10pct-2026"
        );
        assert!(
            table
                .rule_for_with_assignments(&binding, date("2026-10-01"), &[assignment])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn public_code_assignment_binds_through_the_lifecycle_and_stays_consistent() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = public_code("159001");
        let lifecycles = [lifecycle(
            "159001",
            "2022-12-01",
            None,
            None,
            Some("2023-06-01"),
            None,
        )];
        let binding = binding(&lifecycles, "159001");
        let assignments = [category_assignment(
            &instrument_id,
            "szse-fund-default-10pct",
            "2022-10-01",
            None,
            "SYNTHETIC-LIST-A",
        )];
        let resolve = |day: &str| {
            table
                .rule_for_with_assignments(&binding, date(day), &assignments)
                .unwrap()
        };

        // The listing Session is covered; the termination date is exclusive.
        let first = resolve("2022-12-01").unwrap();
        assert!(table.supports(&first, &binding, date("2022-12-01")));
        assert!(resolve("2023-06-01").is_none());
        assert!(!table.supports(&first, &binding, date("2023-06-01")));

        // The rule switch inside the lifecycle changes the record identity even
        // when the default category rate stays at 10%.
        let before = resolve("2023-04-09").unwrap();
        let after = resolve("2023-04-10").unwrap();
        assert_eq!(before.rate_bps, 1000);
        assert_eq!(after.rate_bps, 1000);
        assert_eq!(before.effective_until, Some(date("2023-04-10")));
        assert_ne!(before.version, after.version);

        // All availability gates require the same supported binding.
        let calendar = synthetic_calendar();
        let session_open: TimestampNs = "2023-04-10T09:30:00+08:00".parse().unwrap();
        let category_evidence = after
            .official_rule
            .as_ref()
            .unwrap()
            .category_evidence
            .as_ref()
            .unwrap();
        assert_eq!(category_evidence.lifecycle.as_ref(), Some(binding.target()));
        assert_eq!(category_evidence.instrument_id, instrument_id);
        assert!(
            table
                .evidence_available_at(
                    &after,
                    &binding,
                    &lifecycle_evidence(),
                    date("2023-04-10"),
                    &calendar,
                )
                .is_ok()
        );
        assert!(
            table
                .category_evidence_available_at(
                    &after,
                    &binding,
                    after.evidence.as_citation().unwrap(),
                    date("2023-04-10"),
                    session_open,
                    &calendar,
                )
                .unwrap()
                .is_some()
        );
        assert!(
            table
                .rule_evidence_available_at(
                    &before,
                    &binding,
                    &before.rule_evidence[0],
                    date("2023-04-09"),
                    session_open,
                    &calendar,
                )
                .is_ok()
        );
        for (rule, evidence) in [(&before, "2023-06-01"), (&after, "2023-06-01")] {
            assert!(
                table
                    .evidence_available_at(
                        rule,
                        &binding,
                        &lifecycle_evidence(),
                        date(evidence),
                        &calendar,
                    )
                    .is_err()
            );
        }

        // Tampering with either the assignment identity or the lifecycle binding
        // makes the evidence unsupported.
        let mut altered_assignment = after.clone();
        altered_assignment
            .official_rule
            .as_mut()
            .unwrap()
            .category_evidence
            .as_mut()
            .unwrap()
            .instrument_id = public_code("159002");
        assert!(!table.supports(&altered_assignment, &binding, date("2023-04-10")));
        let mut altered_binding = after.clone();
        altered_binding
            .official_rule
            .as_mut()
            .unwrap()
            .category_evidence
            .as_mut()
            .unwrap()
            .lifecycle
            .as_mut()
            .unwrap()
            .effective_from = date("2022-10-01");
        assert!(!table.supports(&altered_binding, &binding, date("2023-04-10")));
    }

    #[test]
    fn category_binding_does_not_cross_a_reused_code() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = public_code("159001");
        let old = lifecycle("159001", "2022-01-01", None, None, Some("2022-12-01"), None);
        let new = lifecycle("159001", "2023-01-01", None, None, None, None);
        let group = [old.clone(), new.clone()];
        let old_binding = binding_for(&group, &old.instrument_id().unwrap());
        let new_binding = binding_for(&group, &new.instrument_id().unwrap());
        // One assignment interval covers both funds, so it cannot be attributed
        // to either lifecycle. The result is UNKNOWN, not an error.
        let wide = [category_assignment(
            &instrument_id,
            "szse-fund-default-10pct",
            "2022-10-01",
            None,
            "SYNTHETIC-WIDE-LIST",
        )];
        for (binding, day) in [(&old_binding, "2022-11-01"), (&new_binding, "2023-01-02")] {
            assert!(
                table
                    .rule_for_with_assignments(binding, date(day), &wide)
                    .unwrap()
                    .is_none()
            );
            assert!(table.category_assignment_for(binding, date(day)).is_none());
        }

        // A fresh assignment for the reused code's new fund binds in its own
        // lifecycle and is not inherited from the predecessor.
        let new_only = [category_assignment(
            &instrument_id,
            "szse-fund-default-10pct",
            "2023-01-01",
            None,
            "SYNTHETIC-NEW-FUND-LIST",
        )];
        let rule = table
            .rule_for_with_assignments(&new_binding, date("2023-01-02"), &new_only)
            .unwrap()
            .unwrap();
        assert!(table.supports(&rule, &new_binding, date("2023-01-02")));
        assert!(!table.supports(&rule, &old_binding, date("2023-01-02")));
    }

    #[test]
    fn unmarked_successor_starts_a_new_fund_even_without_a_termination_fact() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = public_code("159001");
        // Reuse encoded only by the predecessor's identity end: there is no
        // termination fact and the successor carries no legal-name marker.
        let old = lifecycle("159001", "2022-01-01", None, Some("2022-12-01"), None, None);
        let new = lifecycle("159001", "2023-01-01", None, None, None, None);
        let group = [old.clone(), new.clone()];
        let assignments = [category_assignment(
            &instrument_id,
            "szse-fund-default-10pct",
            "2022-10-01",
            None,
            "SYNTHETIC-UNMARKED-REUSE",
        )];
        for (life, day) in [(&old, "2022-11-01"), (&new, "2023-01-02")] {
            let binding = binding_for(&group, &life.instrument_id().unwrap());
            assert!(
                table
                    .rule_for_with_assignments(&binding, date(day), &assignments)
                    .unwrap()
                    .is_none()
            );
            assert!(table.category_assignment_for(&binding, date(day)).is_none());
        }
    }

    #[test]
    fn category_binding_follows_a_legal_name_change_successor() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = public_code("159001");
        let old = lifecycle("159001", "2022-01-01", None, Some("2023-01-01"), None, None);
        let new = lifecycle("159001", "2023-01-01", None, None, None, Some("2023-01-01"));
        let group = [old.clone(), new.clone()];
        let old_binding = binding_for(&group, &old.instrument_id().unwrap());
        let new_binding = binding_for(&group, &new.instrument_id().unwrap());
        let assignments = [category_assignment(
            &instrument_id,
            "szse-fund-default-10pct",
            "2022-10-01",
            None,
            "SYNTHETIC-RENAME-LIST",
        )];
        for (binding, day) in [(&old_binding, "2022-12-30"), (&new_binding, "2023-01-02")] {
            let rule = table
                .rule_for_with_assignments(binding, date(day), &assignments)
                .unwrap()
                .unwrap();
            assert!(table.supports(&rule, binding, date(day)));
            assert_eq!(
                rule.official_rule
                    .as_ref()
                    .unwrap()
                    .category_evidence
                    .as_ref()
                    .unwrap()
                    .lifecycle
                    .as_ref(),
                Some(binding.target())
            );
        }
    }

    #[test]
    fn rule_document_number_must_match_its_evidence_identity() {
        let mut table = FundLimitRuleTable::official_v1().unwrap();
        table.records[0].document_number = "wrong-document".into();
        assert!(table.validate_public().is_err());
    }
}
