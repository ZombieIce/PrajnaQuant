use super::{FirstSessionReference, FundCategoryEvidence, LimitRule, OfficialLimitRule};
use crate::pit_universe::{CatalogError, CatalogInput, Evidence, Publication};
use chrono::NaiveDate;
use prajna_domain::{InstrumentId, Price, Session, TimestampNs};
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, CatalogError>;

const OFFICIAL_V1: &str = include_str!("../../../../configs/mvp-6/limit_rules_v1.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FundCategoryAssignment {
    pub instrument_id: InstrumentId,
    pub fund_category: String,
    pub effective_from: NaiveDate,
    pub effective_until: Option<NaiveDate>,
    pub document_number: String,
    pub evidence: Evidence,
    pub evidence_level: EvidenceLevel,
}

impl FundCategoryAssignment {
    pub fn identity(&self) -> std::result::Result<String, CatalogError> {
        super::identity("fund-category-assignment@1", self)
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
        let table: Self = serde_json::from_str(OFFICIAL_V1)
            .map_err(|error| CatalogError(format!("invalid bundled limit-rule table: {error}")))?;
        table.validate()?;
        Ok(table)
    }

    pub fn rule_for(
        &self,
        instrument_id: &InstrumentId,
        session_date: NaiveDate,
    ) -> Result<Option<LimitRule>> {
        self.rule_for_with_assignments(instrument_id, session_date, &self.category_assignments)
    }

    pub fn rule_for_with_assignments(
        &self,
        instrument_id: &InstrumentId,
        session_date: NaiveDate,
        assignments: &[FundCategoryAssignment],
    ) -> Result<Option<LimitRule>> {
        if session_date < self.coverage_start || session_date >= self.coverage_end_exclusive {
            return Ok(None);
        }

        let Some(assignment) = unique_match(assignments.iter().filter(|assignment| {
            assignment.instrument_id == *instrument_id
                && assignment.effective_from <= session_date
                && assignment
                    .effective_until
                    .is_none_or(|end| session_date < end)
        })) else {
            return Ok(None);
        };
        assignment.evidence.validate()?;
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
            evidence: assignment.evidence.clone(),
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
                    evidence: assignment.evidence.clone(),
                    evidence_level: assignment.evidence_level,
                }),
            }),
        }))
    }

    pub fn supports(
        &self,
        rule: &LimitRule,
        instrument_id: &InstrumentId,
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
        let assignment = FundCategoryAssignment {
            instrument_id: category_evidence.instrument_id.clone(),
            fund_category: category_evidence.fund_category.clone(),
            effective_from: category_evidence.effective_from,
            effective_until: category_evidence.effective_until,
            document_number: category_evidence.document_number.clone(),
            evidence: category_evidence.evidence.clone(),
            evidence_level: category_evidence.evidence_level,
        };
        if !assignment
            .identity()
            .is_ok_and(|identity| identity == category_evidence.assignment_identity)
        {
            return false;
        }
        if category_evidence.document_number.trim().is_empty()
            || category_evidence.instrument_id != *instrument_id
            || category_evidence.fund_category != official_rule.fund_category
            || rule.evidence != category_evidence.evidence
            || !rule.supporting_evidence.is_empty()
            || category_evidence.document_number != category_evidence.evidence.announcement_id
            || category_evidence.evidence.validate().is_err()
            || category_evidence.effective_from > session_date
            || category_evidence
                .effective_until
                .is_some_and(|end| session_date >= end)
            || category_evidence
                .effective_until
                .is_some_and(|end| end <= category_evidence.effective_from)
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
        instrument_id: &InstrumentId,
        evidence: &Evidence,
        session_date: NaiveDate,
        session_open: TimestampNs,
        calendar: &CatalogInput,
    ) -> Result<TimestampNs> {
        if !self.supports(rule, instrument_id, session_date) {
            return Err(CatalogError(
                "cannot resolve availability for an unsupported limit rule".into(),
            ));
        }
        if let Ok(available_at) = evidence.available_at(calendar) {
            return Ok(available_at);
        }
        let Some(official_rule) = rule.official_rule.as_ref() else {
            return evidence.available_at(calendar);
        };
        let Some(category_evidence) = official_rule.category_evidence.as_ref() else {
            return evidence.available_at(calendar);
        };
        let effective_from =
            (category_evidence.evidence == *evidence).then_some(category_evidence.effective_from);
        let Some(effective_from) = effective_from else {
            return evidence.available_at(calendar);
        };
        let Publication::DateOnly { date } = evidence.publication else {
            return evidence.available_at(calendar);
        };
        if date >= calendar.calendar_start
            || date >= effective_from
            || effective_from > session_date
        {
            return evidence.available_at(calendar);
        }
        let nanos = session_open
            .as_unix_nanos()
            .checked_sub(1)
            .ok_or_else(|| CatalogError("session open timestamp underflow".into()))?;
        Ok(TimestampNs::from_unix_nanos(nanos))
    }

    pub fn rule_evidence_available_at(
        &self,
        rule: &LimitRule,
        instrument_id: &InstrumentId,
        evidence: &RuleSourceEvidence,
        session_date: NaiveDate,
        session_open: TimestampNs,
        calendar: &CatalogInput,
    ) -> Result<TimestampNs> {
        if !self.supports(rule, instrument_id, session_date) {
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

    fn validate(&self) -> Result<()> {
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
        if self.unresolved_memberships.len() != expected_symbols.len()
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
            evidence: Evidence {
                raw_sha256: format!("sha256:{}", "c".repeat(64)),
                announcement_id: document_number.into(),
                locator: "synthetic category-list row".into(),
                publication: Publication::Exact {
                    timestamp: "2023-04-09T08:00:00+08:00".parse::<TimestampNs>().unwrap(),
                },
            },
            evidence_level: EvidenceLevel::Primary,
        }
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
        let session_date = date("2023-04-10");
        let assignments = [category_assignment(
            &instrument_id,
            "szse-fund-default-10pct",
            "2022-10-01",
            None,
            "SYNTHETIC-LIST-A",
        )];
        let rule = table
            .rule_for_with_assignments(&instrument_id, session_date, &assignments)
            .unwrap()
            .unwrap();
        assert!(table.supports(&rule, &instrument_id, session_date));

        let mut overlapping = table.clone();
        overlapping.records.extend(table.records.iter().cloned());
        let mut missing = table;
        missing.records.clear();
        for unresolved in [missing, overlapping] {
            assert!(
                unresolved
                    .rule_for_with_assignments(&instrument_id, session_date, &assignments)
                    .unwrap()
                    .is_none()
            );
            assert!(!unresolved.supports(&rule, &instrument_id, session_date));
        }
    }

    #[test]
    fn official_table_switches_rules_and_uses_pit_category_assignments() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = "159001.SZSE".parse().unwrap();
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
        assignments[1].evidence.locator =
            "synthetic prospectus §1: the fund tracks a ChiNext-only index".into();

        let before = table
            .rule_for_with_assignments(&instrument_id, date("2023-04-09"), &assignments)
            .unwrap()
            .unwrap();
        let after = table
            .rule_for_with_assignments(&instrument_id, date("2023-04-10"), &assignments)
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
            category_evidence.evidence.locator,
            "synthetic prospectus §1: the fund tracks a ChiNext-only index"
        );
        assert!(after.effective_until.is_some());
        assert!(after.applies_on_first_session);
        assert_eq!(
            after.first_session_reference,
            Some(FirstSessionReference::PreviousNetAssetValue)
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
        assert!(!table.supports(&altered_interval, &instrument_id, date("2023-04-10")));
        let mut reassigned = after.clone();
        reassigned
            .official_rule
            .as_mut()
            .unwrap()
            .category_evidence
            .as_mut()
            .unwrap()
            .instrument_id = "159002.SZSE".parse().unwrap();
        assert!(!table.supports(&reassigned, &instrument_id, date("2023-04-10")));
        assert_eq!(
            after.official_rule.as_ref().unwrap().fund_category,
            "szse-20pct-index-etf-lof"
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
    fn missing_or_unknown_category_never_defaults_to_a_limit() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = "159001.SZSE".parse().unwrap();
        assert!(
            table
                .rule_for(&instrument_id, date("2023-04-10"))
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
                .rule_for_with_assignments(&instrument_id, date("2023-04-10"), &[unknown])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn overlapping_point_in_time_category_assignments_are_unresolved() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = "159001.SZSE".parse().unwrap();
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
                .rule_for_with_assignments(&instrument_id, date("2023-04-10"), &[first, second])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn default_rule_periods_split_at_both_trading_rule_effective_dates() {
        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument_id = "159001.SZSE".parse().unwrap();
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
                    &instrument_id,
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
                .rule_for_with_assignments(&instrument_id, date("2026-10-01"), &[assignment])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rule_document_number_must_match_its_evidence_identity() {
        let mut table = FundLimitRuleTable::official_v1().unwrap();
        table.records[0].document_number = "wrong-document".into();
        assert!(table.validate().is_err());
    }
}
