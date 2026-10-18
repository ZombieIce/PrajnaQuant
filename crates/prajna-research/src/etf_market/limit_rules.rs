use super::{FundCategoryEvidence, LimitRule, OfficialLimitRule};
use crate::pit_universe::{CatalogError, CatalogInput, Evidence, Publication};
use chrono::NaiveDate;
use prajna_domain::{InstrumentId, Price, TimestampNs};
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, CatalogError>;

const OFFICIAL_V1: &str = include_str!("limit_rules_v1.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FundCategoryAssignment {
    pub instrument_id: InstrumentId,
    pub fund_category: String,
    pub effective_from: NaiveDate,
    pub effective_until: Option<NaiveDate>,
    pub document_number: String,
    pub evidence: Evidence,
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
    pub first_session_reference: String,
    pub document_number: String,
    pub evidence: Evidence,
    #[serde(default)]
    pub supporting_evidence: Vec<SupportingRuleEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportingRuleEvidence {
    pub effective_from: NaiveDate,
    pub evidence: Evidence,
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

        let mut matching_assignments = assignments.iter().filter(|assignment| {
            assignment.instrument_id == *instrument_id
                && assignment.effective_from <= session_date
                && assignment
                    .effective_until
                    .is_none_or(|end| session_date < end)
        });
        let Some(assignment) = matching_assignments.next() else {
            return Ok(None);
        };
        if matching_assignments.next().is_some() {
            return Ok(None);
        }
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

        let mut matching_records = self.records.iter().filter(|record| {
            record.fund_category == assignment.fund_category
                && record.effective_from <= session_date
                && record.effective_until.is_none_or(|end| session_date < end)
        });
        let Some(record) = matching_records.next() else {
            return Ok(None);
        };
        if matching_records.next().is_some() {
            return Ok(None);
        }

        Ok(Some(LimitRule {
            version: format!("{}/{}", self.version, record.id),
            effective_from: record.effective_from,
            effective_until: record.effective_until,
            rate_bps: record.rate_bps,
            tick: record.tick,
            rounding: record.rounding.clone(),
            applies_on_first_session: record.applies_on_first_session,
            first_session_reference: Some(record.first_session_reference.clone()),
            evidence: record.evidence.clone(),
            supporting_evidence: record
                .supporting_evidence
                .iter()
                .map(|support| support.evidence.clone())
                .collect(),
            official_rule: Some(OfficialLimitRule {
                document_number: record.document_number.clone(),
                fund_category: record.fund_category.clone(),
                category_evidence: Some(FundCategoryEvidence {
                    document_number: assignment.document_number.clone(),
                    effective_from: assignment.effective_from,
                    effective_until: assignment.effective_until,
                    evidence: assignment.evidence.clone(),
                }),
            }),
        }))
    }

    pub fn supports(&self, rule: &LimitRule, session_date: NaiveDate) -> bool {
        if session_date < self.coverage_start || session_date >= self.coverage_end_exclusive {
            return false;
        }
        let Some(official_rule) = rule.official_rule.as_ref() else {
            return false;
        };
        let Some(category_evidence) = official_rule.category_evidence.as_ref() else {
            return false;
        };
        if category_evidence.document_number.trim().is_empty()
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

        let mut matching_records = self.records.iter().filter(|record| {
            record.fund_category == official_rule.fund_category
                && record.effective_from <= session_date
                && record.effective_until.is_none_or(|end| session_date < end)
        });
        let Some(record) = matching_records.next() else {
            return false;
        };
        matching_records.next().is_none()
            && rule.version == format!("{}/{}", self.version, record.id)
            && rule.effective_from == record.effective_from
            && rule.effective_until == record.effective_until
            && rule.rate_bps == record.rate_bps
            && rule.tick == record.tick
            && rule.rounding == record.rounding
            && rule.applies_on_first_session == record.applies_on_first_session
            && rule.first_session_reference.as_deref()
                == Some(record.first_session_reference.as_str())
            && official_rule.document_number == record.document_number
            && rule.evidence.announcement_id == record.evidence.announcement_id
            && rule.evidence.locator == record.evidence.locator
            && rule
                .supporting_evidence
                .iter()
                .map(|evidence| (evidence.announcement_id.as_str(), evidence.locator.as_str()))
                .eq(record.supporting_evidence.iter().map(|support| {
                    (
                        support.evidence.announcement_id.as_str(),
                        support.evidence.locator.as_str(),
                    )
                }))
    }

    pub fn evidence_available_at(
        &self,
        rule: &LimitRule,
        evidence: &Evidence,
        session_date: NaiveDate,
        session_open: TimestampNs,
        calendar: &CatalogInput,
    ) -> Result<TimestampNs> {
        if !self.supports(rule, session_date) {
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
        let effective_from = if category_evidence.evidence == *evidence {
            Some(category_evidence.effective_from)
        } else {
            self.records
                .iter()
                .find(|record| {
                    record.fund_category == official_rule.fund_category
                        && (record.evidence == *evidence
                            || record
                                .supporting_evidence
                                .iter()
                                .any(|support| support.evidence == *evidence))
                })
                .and_then(|record| {
                    record
                        .supporting_evidence
                        .iter()
                        .find(|support| support.evidence == *evidence)
                        .map(|support| support.effective_from)
                        .or(Some(record.effective_from))
                })
        };
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

    fn validate(&self) -> Result<()> {
        if self.version.trim().is_empty()
            || self.coverage_start >= self.coverage_end_exclusive
            || self.records.is_empty()
        {
            return Err(CatalogError("invalid fund limit-rule table header".into()));
        }
        for record in &self.records {
            if record.id.trim().is_empty()
                || record.fund_category.trim().is_empty()
                || record.category_description.trim().is_empty()
                || record.document_number.trim().is_empty()
                || record
                    .effective_until
                    .is_some_and(|end| end <= record.effective_from)
                || record.rate_bps == 0
                || record.rate_bps >= 10000
                || record.tick <= Price::ZERO
                || record.rounding != "nearest_tick_half_up"
                || !record.applies_on_first_session
                || record.first_session_reference != "previous_net_asset_value"
            {
                return Err(CatalogError(format!(
                    "invalid fund limit-rule record {}",
                    record.id
                )));
            }
            record.evidence.validate()?;
            for supporting_evidence in &record.supporting_evidence {
                supporting_evidence.evidence.validate()?;
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
        assert!(after.effective_until.is_some());
        assert!(after.applies_on_first_session);
        assert_eq!(
            after.first_session_reference.as_deref(),
            Some("previous_net_asset_value")
        );
        assert_ne!(before.version, after.version);
        assert_eq!(
            after.official_rule.as_ref().unwrap().fund_category,
            "szse-20pct-index-etf-lof"
        );
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
}
