//! Deterministic UNKNOWN coverage accounting for the MVP-6 fixed ETF window.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
};

use chrono::NaiveDate;
use prajna_domain::InstrumentId;
use serde::{Deserialize, Serialize};

use crate::{
    etf_market::{DerivedMarket, TradeState},
    pit_universe::HistoricalCatalog,
};

pub const UNKNOWN_COVERAGE_VERSION: &str = "mvp6-unknown-coverage@1";
pub const DEFAULT_TOTAL_UNKNOWN_PERCENT: u8 = 5;
pub const DEFAULT_PER_INSTRUMENT_UNKNOWN_PERCENT: u8 = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageLifecycle {
    pub instrument_id: InstrumentId,
    pub native_symbol: String,
    pub listed_on: NaiveDate,
    /// Exclusive. The first non-trading day after termination is excluded.
    pub terminated_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleUnknownReason {
    NoApplicableRule,
    CategoryUnknown,
    NotYetEffective,
    Repealed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UnknownReason {
    MissingBarWithoutHaltNotice,
    MissingReferencePrice,
    SourceConflict,
    RuleUnknown { reason: RuleUnknownReason },
    UnexplainedCorporateAction,
    OtherExecutionEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageObservation {
    pub instrument_id: InstrumentId,
    pub session_date: NaiveDate,
    pub execution_state: TradeState,
    pub has_bar: bool,
    pub has_opening_halt_notice: bool,
    pub has_reference_price: bool,
    pub source_conflict: bool,
    pub rule_unknown: Option<RuleUnknownReason>,
    pub unexplained_corporate_action: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnknownCell {
    pub instrument_id: InstrumentId,
    pub session_date: NaiveDate,
    pub reason: UnknownReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentCoverage {
    pub instrument_id: InstrumentId,
    pub native_symbol: String,
    pub cells: u64,
    pub unknown_cells: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnknownCoverageThresholds {
    pub total_unknown_percent: u8,
    pub per_instrument_unknown_percent: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineeringAcceptance {
    Passed,
    Failed,
}

pub struct UnknownCoverageInput<'a> {
    pub catalog_version: String,
    pub derived_market_version: String,
    pub execution_status_version: String,
    pub rule_table_version: String,
    pub corporate_action_version: String,
    pub sessions: &'a [NaiveDate],
    pub lifecycles: &'a [CoverageLifecycle],
    pub observations: &'a [CoverageObservation],
    pub thresholds: UnknownCoverageThresholds,
}

impl Default for UnknownCoverageThresholds {
    fn default() -> Self {
        Self {
            total_unknown_percent: DEFAULT_TOTAL_UNKNOWN_PERCENT,
            per_instrument_unknown_percent: DEFAULT_PER_INSTRUMENT_UNKNOWN_PERCENT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnknownCoverageReport {
    pub version: String,
    pub catalog_version: String,
    pub derived_market_version: String,
    pub execution_status_version: String,
    pub rule_table_version: String,
    pub corporate_action_version: String,
    pub decision_window_start: NaiveDate,
    pub decision_window_end: NaiveDate,
    pub denominator_cells: u64,
    pub unknown_cells: u64,
    pub thresholds: UnknownCoverageThresholds,
    pub engineering_acceptance: EngineeringAcceptance,
    pub instruments: Vec<InstrumentCoverage>,
    pub unknowns: Vec<UnknownCell>,
}

impl UnknownCoverageReport {
    pub fn summary_json(&self) -> serde_json::Value {
        serde_json::json!({
            "version": self.version,
            "rule_table_version": self.rule_table_version,
            "decision_window": {
                "start": self.decision_window_start,
                "end": self.decision_window_end,
            },
            "denominator_cells": self.denominator_cells,
            "unknown_cells": self.unknown_cells,
            "unknown_ratio": ratio(self.unknown_cells, self.denominator_cells),
            "unknown_percent": percent(self.unknown_cells, self.denominator_cells),
            "thresholds": self.thresholds,
            "engineering_acceptance": self.engineering_acceptance,
            "instruments": self.instruments.iter().map(|instrument| serde_json::json!({
                "symbol": instrument.native_symbol,
                "cells": instrument.cells,
                "unknown_cells": instrument.unknown_cells,
                "unknown_ratio": ratio(instrument.unknown_cells, instrument.cells),
                "unknown_percent": percent(instrument.unknown_cells, instrument.cells),
            })).collect::<Vec<_>>(),
        })
    }
}

fn ratio(numerator: u64, denominator: u64) -> String {
    if denominator == 0 {
        return "0/0".into();
    }
    format!("{numerator}/{denominator}")
}

fn percent(numerator: u64, denominator: u64) -> String {
    if denominator == 0 {
        return "0.00%".into();
    }
    let divisor = u128::from(denominator);
    let hundredths = (u128::from(numerator) * 10_000 + divisor / 2) / divisor;
    format!("{}.{:02}%", hundredths / 100, hundredths % 100)
}

/// Evaluate the complete lifecycle × decision-session grid. Extra observations
/// outside the grid are ignored; a missing in-grid observation is itself an
/// UNKNOWN and is attributed to missing bar evidence.
pub fn evaluate_unknown_coverage(input: UnknownCoverageInput<'_>) -> UnknownCoverageReport {
    let UnknownCoverageInput {
        catalog_version,
        derived_market_version,
        execution_status_version,
        rule_table_version,
        corporate_action_version,
        sessions,
        lifecycles,
        observations,
        thresholds,
    } = input;
    let mut dates = sessions.to_vec();
    dates.sort();
    dates.dedup();
    let mut lifecycle_rows = lifecycles.to_vec();
    lifecycle_rows.sort_by(|a, b| a.instrument_id.cmp(&b.instrument_id));

    let observed: BTreeMap<_, _> = observations
        .iter()
        .map(|row| ((row.instrument_id.clone(), row.session_date), row))
        .collect();
    let mut unknowns = Vec::new();
    let mut instruments = Vec::new();
    let mut denominator_cells = 0_u64;

    for lifecycle in lifecycle_rows {
        let mut cells = 0_u64;
        let mut unknown_cells = 0_u64;
        for date in dates.iter().copied().filter(|date| {
            *date >= lifecycle.listed_on && lifecycle.terminated_on.is_none_or(|end| *date < end)
        }) {
            cells += 1;
            denominator_cells += 1;
            let key = (lifecycle.instrument_id.clone(), date);
            let reason = match observed.get(&key) {
                Some(row) => primary_unknown_reason(row),
                None => Some(UnknownReason::MissingBarWithoutHaltNotice),
            };
            if let Some(reason) = reason {
                unknown_cells += 1;
                unknowns.push(UnknownCell {
                    instrument_id: lifecycle.instrument_id.clone(),
                    session_date: date,
                    reason,
                });
            }
        }
        instruments.push(InstrumentCoverage {
            instrument_id: lifecycle.instrument_id,
            native_symbol: lifecycle.native_symbol,
            cells,
            unknown_cells,
        });
    }

    let unknown_cells = unknowns.len() as u64;
    let per_instrument_passes = instruments.iter().all(|instrument| {
        instrument.cells == 0
            || ratio_within(
                instrument.unknown_cells,
                instrument.cells,
                thresholds.per_instrument_unknown_percent,
            )
    });
    let passes = ratio_within(
        unknown_cells,
        denominator_cells,
        thresholds.total_unknown_percent,
    ) && per_instrument_passes;
    UnknownCoverageReport {
        version: UNKNOWN_COVERAGE_VERSION.into(),
        catalog_version,
        derived_market_version,
        execution_status_version,
        rule_table_version,
        corporate_action_version,
        decision_window_start: dates.first().copied().unwrap_or(NaiveDate::MIN),
        decision_window_end: dates.last().copied().unwrap_or(NaiveDate::MIN),
        denominator_cells,
        unknown_cells,
        thresholds,
        engineering_acceptance: if passes {
            EngineeringAcceptance::Passed
        } else {
            EngineeringAcceptance::Failed
        },
        instruments,
        unknowns,
    }
}

fn ratio_within(numerator: u64, denominator: u64, max_percent: u8) -> bool {
    if denominator == 0 {
        return false;
    }
    u128::from(numerator) * 100 <= u128::from(denominator) * u128::from(max_percent)
}

fn primary_unknown_reason(row: &CoverageObservation) -> Option<UnknownReason> {
    if row.execution_state != TradeState::Unknown && !row.unexplained_corporate_action {
        return None;
    }
    Some(if row.source_conflict {
        UnknownReason::SourceConflict
    } else if !row.has_bar && !row.has_opening_halt_notice {
        UnknownReason::MissingBarWithoutHaltNotice
    } else if let Some(reason) = &row.rule_unknown {
        UnknownReason::RuleUnknown {
            reason: reason.clone(),
        }
    } else if !row.has_reference_price {
        UnknownReason::MissingReferencePrice
    } else if row.unexplained_corporate_action {
        UnknownReason::UnexplainedCorporateAction
    } else {
        UnknownReason::OtherExecutionEvidence
    })
}

/// Adapt the validated Catalog and DerivedMarket to the pure coverage evaluator.
/// `unexplained_actions` is populated by the corporate-action integrity stage;
/// it is empty when no unresolved price discontinuity was found.
pub fn evaluate_derived_market(
    catalog: &HistoricalCatalog,
    market: &DerivedMarket,
    corporate_action_version: impl Into<String>,
    unexplained_actions: &BTreeSet<(InstrumentId, NaiveDate)>,
    thresholds: UnknownCoverageThresholds,
) -> Result<UnknownCoverageReport, String> {
    let table = super::etf_market::limit_rules::FundLimitRuleTable::official_v1()
        .map_err(|error| error.to_string())?;
    let catalog_version = catalog.version().map_err(|error| error.to_string())?;
    let lifecycles = catalog
        .input()
        .lifecycles
        .iter()
        .map(|life| {
            let instrument_id = life.instrument_id().map_err(|error| error.to_string())?;
            let listed_on = life
                .listing
                .effective_date
                .max(life.identity_start.unwrap_or(life.listing.effective_date));
            let terminated_on = life
                .termination
                .as_ref()
                .map(|end| end.effective_date)
                .into_iter()
                .chain(life.identity_end_exclusive)
                .min();
            Ok(CoverageLifecycle {
                instrument_id,
                native_symbol: life.native_symbol.clone(),
                listed_on,
                terminated_on,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let openings: BTreeMap<_, _> = market
        .evidence()
        .openings
        .iter()
        .map(|opening| {
            (
                (opening.instrument_id.clone(), opening.session_date),
                opening,
            )
        })
        .collect();
    let observations = market
        .cells()
        .values()
        .map(|cell| {
            let key = (cell.instrument_id.clone(), cell.session_date);
            let opening = openings.get(&key).copied();
            let rule_is_supported = opening
                .and_then(|item| item.limit_rule.as_ref())
                .is_some_and(|rule| table.supports(rule, &cell.instrument_id, cell.session_date));
            let rule_unknown = (!rule_is_supported)
                .then(|| classify_unknown_rule(&table, &cell.instrument_id, cell.session_date));
            CoverageObservation {
                instrument_id: cell.instrument_id.clone(),
                session_date: cell.session_date,
                execution_state: cell.state,
                has_bar: cell.close.is_some(),
                has_opening_halt_notice: opening.is_some_and(|item| item.halt.is_some()),
                has_reference_price: opening.is_some_and(|item| item.reference.is_some()),
                source_conflict: cell.reasons.contains("sources_conflicting")
                    || cell.reasons.contains("sources_missing_or_conflicting")
                    || cell.reasons.contains("halt_trade_conflict"),
                rule_unknown,
                unexplained_corporate_action: unexplained_actions.contains(&key),
            }
        })
        .collect::<Vec<_>>();
    let dates: Vec<_> = market
        .sessions()
        .iter()
        .map(|session| session.session_date())
        .collect();
    Ok(evaluate_unknown_coverage(UnknownCoverageInput {
        catalog_version,
        derived_market_version: market.version().to_owned(),
        execution_status_version: market.evidence().rule_version.clone(),
        rule_table_version: table.version.clone(),
        corporate_action_version: corporate_action_version.into(),
        sessions: &dates,
        lifecycles: &lifecycles,
        observations: &observations,
        thresholds,
    }))
}

fn classify_unknown_rule(
    table: &super::etf_market::limit_rules::FundLimitRuleTable,
    instrument_id: &InstrumentId,
    date: NaiveDate,
) -> RuleUnknownReason {
    if date < table.coverage_start {
        return RuleUnknownReason::NotYetEffective;
    }
    if date >= table.coverage_end_exclusive {
        return RuleUnknownReason::Repealed;
    }
    let assignment = table.category_assignments.iter().find(|assignment| {
        assignment.instrument_id == *instrument_id
            && assignment.effective_from <= date
            && assignment.effective_until.is_none_or(|end| date < end)
    });
    let Some(assignment) = assignment else {
        return RuleUnknownReason::CategoryUnknown;
    };
    let records: Vec<_> = table
        .records
        .iter()
        .filter(|record| record.fund_category == assignment.fund_category)
        .collect();
    if records.is_empty() {
        return RuleUnknownReason::NoApplicableRule;
    }
    if records.iter().all(|record| date < record.effective_from) {
        RuleUnknownReason::NotYetEffective
    } else if records
        .iter()
        .all(|record| record.effective_until.is_some_and(|end| date >= end))
    {
        RuleUnknownReason::Repealed
    } else {
        RuleUnknownReason::NoApplicableRule
    }
}

/// Persist the detailed cell list in the local acceptance directory. This file
/// is deterministic for a fixed report and contains no market values or Raw hashes.
pub fn write_local_coverage_report(root: &Path, report: &UnknownCoverageReport) -> io::Result<()> {
    let _lock = prajna_data::lock_mvp6_coverage_report(root)?;
    let directory = root.join("acceptance");
    fs::create_dir_all(&directory)?;
    let mut bytes = serde_json::to_vec_pretty(report).map_err(io::Error::other)?;
    bytes.push(b'\n');
    let destination = directory.join("mvp6-unknown-coverage.json");
    let staging = directory.join("mvp6-unknown-coverage.json.tmp");
    fs::write(&staging, bytes)?;
    fs::rename(staging, destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, NaiveDate};

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, day).unwrap()
    }

    fn id(code: &str) -> InstrumentId {
        format!("{code}.SZSE").parse().unwrap()
    }

    fn life(code: &str, listed: u32, terminated: Option<u32>) -> CoverageLifecycle {
        CoverageLifecycle {
            instrument_id: id(code),
            native_symbol: code.into(),
            listed_on: date(listed),
            terminated_on: terminated.map(date),
        }
    }

    fn known(code: &str, day: u32) -> CoverageObservation {
        CoverageObservation {
            instrument_id: id(code),
            session_date: date(day),
            execution_state: TradeState::Tradable,
            has_bar: true,
            has_opening_halt_notice: false,
            has_reference_price: true,
            source_conflict: false,
            rule_unknown: None,
            unexplained_corporate_action: false,
        }
    }

    fn assess(
        sessions: &[NaiveDate],
        lifecycles: &[CoverageLifecycle],
        observations: &[CoverageObservation],
    ) -> UnknownCoverageReport {
        evaluate_unknown_coverage(UnknownCoverageInput {
            catalog_version: "catalog@synthetic-1".into(),
            derived_market_version: "market@synthetic-2".into(),
            execution_status_version: "status@synthetic-3".into(),
            rule_table_version: "rules@synthetic-4".into(),
            corporate_action_version: "actions@synthetic-5".into(),
            sessions,
            lifecycles,
            observations,
            thresholds: UnknownCoverageThresholds::default(),
        })
    }

    #[test]
    fn five_percent_passes_and_more_than_five_percent_fails() {
        let sessions: Vec<_> = (1..=20).map(date).collect();
        let mut rows: Vec<_> = sessions.iter().map(|d| known("159001", d.day())).collect();
        rows[0].execution_state = TradeState::Unknown;
        let exact = assess(&sessions, &[life("159001", 1, None)], &rows);
        assert_eq!(exact.unknown_cells, 1);
        assert_eq!(exact.engineering_acceptance, EngineeringAcceptance::Passed);

        rows[1].execution_state = TradeState::Unknown;
        let over = assess(&sessions, &[life("159001", 1, None)], &rows);
        assert_eq!(over.unknown_cells, 2);
        assert_eq!(over.engineering_acceptance, EngineeringAcceptance::Failed);
    }

    #[test]
    fn per_instrument_twenty_percent_boundary_passes_but_exceeding_it_fails() {
        let sessions: Vec<_> = (1..=20).map(date).collect();
        let mut lifecycles = vec![life("159001", 1, None)];
        let mut rows: Vec<_> = sessions.iter().map(|d| known("159001", d.day())).collect();
        for code in ["159002", "159003", "159004", "159005"] {
            lifecycles.push(life(code, 1, None));
            rows.extend(sessions.iter().map(|d| known(code, d.day())));
        }
        for row in rows.iter_mut().take(4) {
            row.execution_state = TradeState::Unknown;
        }
        assert_eq!(
            assess(&sessions, &lifecycles, &rows).engineering_acceptance,
            EngineeringAcceptance::Passed
        );
        rows[4].execution_state = TradeState::Unknown;
        assert_eq!(
            assess(&sessions, &lifecycles, &rows).engineering_acceptance,
            EngineeringAcceptance::Failed
        );
    }

    #[test]
    fn lifecycle_bounds_count_the_listing_session_and_exclude_external_dates() {
        let sessions: Vec<_> = (1..=5).map(date).collect();
        let mut rows: Vec<_> = sessions.iter().map(|d| known("159001", d.day())).collect();
        rows[0].execution_state = TradeState::Unknown;
        rows[0].rule_unknown = Some(RuleUnknownReason::NotYetEffective);
        rows[1].execution_state = TradeState::Unknown;
        rows[1].rule_unknown = Some(RuleUnknownReason::CategoryUnknown);
        let report = assess(&sessions, &[life("159001", 2, Some(5))], &rows);
        assert_eq!(report.denominator_cells, 3);
        assert_eq!(report.unknown_cells, 1);
        assert_eq!(report.unknowns[0].session_date, date(2));
        assert_eq!(
            report.unknowns[0].reason,
            UnknownReason::RuleUnknown {
                reason: RuleUnknownReason::CategoryUnknown
            }
        );
    }

    #[test]
    fn lifecycles_with_no_sessions_in_window_do_not_fail_per_instrument_gate() {
        let sessions = [date(1), date(2)];
        let lifecycles = [life("159001", 1, None), life("159002", 3, None)];
        let observations = sessions
            .iter()
            .map(|d| known("159001", d.day()))
            .collect::<Vec<_>>();

        let report = assess(&sessions, &lifecycles, &observations);

        assert_eq!(report.denominator_cells, 2);
        assert_eq!(report.instruments[1].cells, 0);
        assert_eq!(report.engineering_acceptance, EngineeringAcceptance::Passed);
    }

    #[test]
    fn bundled_rule_table_changes_category_rule_on_its_effective_session() {
        use crate::{
            etf_market::limit_rules::{EvidenceLevel, FundCategoryAssignment, FundLimitRuleTable},
            pit_universe::{Evidence, Publication},
        };

        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument = id("159001");
        let assignment = |category: &str, from: &str, until: Option<&str>| {
            let document = format!("synthetic-{category}");
            FundCategoryAssignment {
                instrument_id: instrument.clone(),
                fund_category: category.into(),
                effective_from: NaiveDate::parse_from_str(from, "%Y-%m-%d").unwrap(),
                effective_until: until
                    .map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()),
                document_number: document.clone(),
                evidence: Evidence {
                    raw_sha256: format!("sha256:{}", "0".repeat(64)),
                    announcement_id: document,
                    locator: "synthetic rule switch".into(),
                    publication: Publication::DateOnly {
                        date: NaiveDate::parse_from_str("2023-01-01", "%Y-%m-%d").unwrap(),
                    },
                },
                evidence_level: EvidenceLevel::Primary,
            }
        };
        let assignments = [
            assignment("szse-fund-default-10pct", "2022-10-01", Some("2023-04-10")),
            assignment("szse-20pct-index-etf-lof", "2023-04-10", None),
        ];
        let before = table
            .rule_for_with_assignments(
                &instrument,
                NaiveDate::parse_from_str("2023-04-09", "%Y-%m-%d").unwrap(),
                &assignments,
            )
            .unwrap()
            .unwrap();
        let switch_day = table
            .rule_for_with_assignments(
                &instrument,
                NaiveDate::parse_from_str("2023-04-10", "%Y-%m-%d").unwrap(),
                &assignments,
            )
            .unwrap()
            .unwrap();
        assert_eq!(before.rate_bps, 1000);
        assert_eq!(switch_day.rate_bps, 2000);
        assert_ne!(before.version, switch_day.version);
    }

    #[test]
    fn every_unknown_gets_one_versioned_reason_and_serialization_is_stable() {
        let sessions = [date(1)];
        let mut row = known("159001", 1);
        row.execution_state = TradeState::Unknown;
        row.source_conflict = true;
        row.rule_unknown = Some(RuleUnknownReason::Repealed);
        row.unexplained_corporate_action = true;
        let first = assess(&sessions, &[life("159001", 1, None)], &[row.clone()]);
        let second = assess(&sessions, &[life("159001", 1, None)], &[row]);
        assert_eq!(first.unknowns.len(), 1);
        assert_eq!(first.unknowns[0].reason, UnknownReason::SourceConflict);
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
        assert_eq!(first.catalog_version, "catalog@synthetic-1");
        assert_eq!(first.derived_market_version, "market@synthetic-2");
        assert_eq!(first.execution_status_version, "status@synthetic-3");
        assert_eq!(first.rule_table_version, "rules@synthetic-4");
        assert_eq!(first.corporate_action_version, "actions@synthetic-5");
        let public_summary = first.summary_json();
        assert_eq!(public_summary["instruments"][0]["symbol"], "159001");
        assert!(public_summary.get("catalog_version").is_none());
        assert!(public_summary.get("derived_market_version").is_none());
        assert!(public_summary.get("corporate_action_version").is_none());
    }

    #[test]
    fn missing_observation_is_unknown_but_lifecycle_external_dates_are_ignored() {
        let report = assess(
            &[date(1), date(2), date(3)],
            &[life("159001", 2, Some(3))],
            &[],
        );
        assert_eq!(report.denominator_cells, 1);
        assert_eq!(report.unknown_cells, 1);
        assert_eq!(
            report.unknowns[0].reason,
            UnknownReason::MissingBarWithoutHaltNotice
        );
    }
}
