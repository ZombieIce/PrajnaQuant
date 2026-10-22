//! Deterministic UNKNOWN coverage accounting for the MVP-6 fixed ETF window.

use std::{collections::BTreeMap, fs, io, path::Path};

use chrono::NaiveDate;
use prajna_domain::InstrumentId;
use serde::{Deserialize, Serialize};

use crate::{
    etf_market::{DerivedMarket, ExecutionStatusVersion, TradeState},
    pit_universe::{HistoricalCatalog, Lifecycle},
};

pub const UNKNOWN_COVERAGE_VERSION: &str = "mvp6-unknown-coverage@2";
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
            "unknown_percent": format_percent(self.unknown_cells, self.denominator_cells),
            "thresholds": self.thresholds,
            "engineering_acceptance": self.engineering_acceptance,
            "instruments": self.instruments.iter().map(|instrument| serde_json::json!({
                "symbol": instrument.native_symbol,
                "cells": instrument.cells,
                "unknown_cells": instrument.unknown_cells,
                "unknown_ratio": ratio(instrument.unknown_cells, instrument.cells),
                "unknown_percent": format_percent(instrument.unknown_cells, instrument.cells),
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

/// Format an unknown-cell percentage with the same rounding used by coverage reports.
pub fn format_percent(numerator: u64, denominator: u64) -> String {
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
    if row.execution_state != TradeState::Unknown {
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
    } else {
        UnknownReason::OtherExecutionEvidence
    })
}

/// Adapt the validated Catalog and DerivedMarket to the pure coverage evaluator.
pub fn evaluate_derived_market(
    catalog: &HistoricalCatalog,
    market: &DerivedMarket,
    corporate_action_version: impl Into<String>,
    thresholds: UnknownCoverageThresholds,
) -> Result<UnknownCoverageReport, String> {
    let table = super::etf_market::limit_rules::FundLimitRuleTable::official_v2()
        .map_err(|error| error.to_string())?;
    let catalog_version = catalog.version().map_err(|error| error.to_string())?;
    let lifecycles = catalog
        .input()
        .lifecycles
        .iter()
        .map(|life| coverage_lifecycle(life, &catalog.input().lifecycles))
        .collect::<Result<Vec<_>, String>>()?;
    // A lifecycle without a binding keeps its own cells CategoryUnknown instead
    // of aborting the rest of the coverage report (P27-20.4).
    let bindings: BTreeMap<_, _> = catalog
        .input()
        .lifecycles
        .iter()
        .filter_map(|life| {
            let id = life.instrument_id().ok()?;
            let binding = super::etf_market::limit_rules::RuleBinding::from_lifecycles(
                &catalog.input().lifecycles,
                &id,
            )
            .ok()
            .flatten()?;
            Some((id, binding))
        })
        .collect();
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
            let binding = bindings.get(&cell.instrument_id);
            let rule_is_supported = opening
                .and_then(|item| item.limit_rule.as_ref())
                .is_some_and(|rule| {
                    binding.is_some_and(|binding| table.supports(rule, binding, cell.session_date))
                });
            // Evidence whose recorded lifecycle does not match the Catalog
            // binding cannot establish the category, even if the public table
            // has a valid assignment for the authoritative lifecycle.
            let category_binding_is_stale = opening
                .and_then(|item| item.limit_rule.as_ref())
                .and_then(|rule| rule.official_rule.as_ref())
                .and_then(|rule| rule.category_evidence.as_ref())
                .is_some_and(|category| {
                    binding
                        .is_none_or(|binding| category.lifecycle.as_ref() != Some(binding.target()))
                });
            let is_v3 = market.evidence().semantic_version == Some(ExecutionStatusVersion::V3);
            let rule_unknown = if is_v3 {
                cell.reasons
                    .contains("unknown_preopen_price_limit_rule")
                    .then(|| {
                        if category_binding_is_stale {
                            RuleUnknownReason::CategoryUnknown
                        } else {
                            binding.map_or(RuleUnknownReason::CategoryUnknown, |binding| {
                                classify_unknown_rule(&table, binding, cell.session_date)
                            })
                        }
                    })
            } else {
                (!rule_is_supported).then(|| {
                    if category_binding_is_stale {
                        RuleUnknownReason::CategoryUnknown
                    } else {
                        binding.map_or(RuleUnknownReason::CategoryUnknown, |binding| {
                            classify_unknown_rule(&table, binding, cell.session_date)
                        })
                    }
                })
            };
            CoverageObservation {
                instrument_id: cell.instrument_id.clone(),
                session_date: cell.session_date,
                execution_state: cell.state,
                has_bar: cell.close.is_some(),
                has_opening_halt_notice: opening.is_some_and(|item| item.halt.is_some()),
                has_reference_price: if is_v3 {
                    !cell.reasons.contains("missing_tdx_reference_price")
                } else {
                    opening.is_some_and(|item| item.reference.is_some())
                },
                source_conflict: cell.reasons.contains("sources_conflicting")
                    || cell.reasons.contains("sources_missing_or_conflicting")
                    || cell.reasons.contains("halt_trade_conflict"),
                rule_unknown,
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

/// A name-change successor's coverage begins when the new name is knowable
/// (`identity_available_from`), not at its official effective date; a new
/// listing or reused code still starts at its official listing so a pre-TDX
/// official lifetime is not dropped.
fn coverage_start(life: &Lifecycle) -> NaiveDate {
    life.identity_available_from
        .unwrap_or(life.listing.effective_date)
}

/// The coverage denominator is the ETF's legal lifetime, not the TDX code-table
/// presence. An official listing before the first code-table appearance and a
/// TDX exit before the official termination stay inside the lifetime, so their
/// missing price cells are reported as UNKNOWN (P27-6/P27-19). The predecessor
/// is bounded by the next same-code successor's coverage start, so a delayed
/// name change leaves one UNKNOWN cell in the gap and never double counts.
fn coverage_lifecycle(
    life: &Lifecycle,
    lifecycles: &[Lifecycle],
) -> Result<CoverageLifecycle, String> {
    let instrument_id = life.instrument_id().map_err(|error| error.to_string())?;
    let own_start = life.identity_start.unwrap_or(life.listing.effective_date);
    let successor_start = lifecycles
        .iter()
        .filter(|other| {
            other.native_symbol == life.native_symbol
                && other.identity_start.unwrap_or(other.listing.effective_date) > own_start
        })
        .map(coverage_start)
        .min();
    let terminated_on = match (
        life.termination.as_ref().map(|end| end.effective_date),
        successor_start,
    ) {
        (Some(termination), Some(successor)) => Some(termination.min(successor)),
        (Some(termination), None) => Some(termination),
        (None, Some(successor)) => Some(successor),
        (None, None) => None,
    };
    Ok(CoverageLifecycle {
        instrument_id,
        native_symbol: life.native_symbol.clone(),
        listed_on: coverage_start(life),
        terminated_on,
    })
}

pub(crate) fn classify_unknown_rule(
    table: &super::etf_market::limit_rules::FundLimitRuleTable,
    binding: &super::etf_market::limit_rules::RuleBinding,
    date: NaiveDate,
) -> RuleUnknownReason {
    if date < table.coverage_start {
        return RuleUnknownReason::NotYetEffective;
    }
    if date >= table.coverage_end_exclusive {
        return RuleUnknownReason::Repealed;
    }
    let Some(assignment) = table.category_assignment_for(binding, date) else {
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

    fn coverage_evidence() -> crate::pit_universe::Evidence {
        crate::pit_universe::Evidence {
            raw_sha256: format!("sha256:{}", "a".repeat(64)),
            announcement_id: "synthetic-announcement".into(),
            locator: "synthetic p1".into(),
            publication: crate::pit_universe::Publication::DateOnly { date: date(1) },
        }
    }

    fn catalog_lifecycle(
        code: &str,
        listing: u32,
        identity_start: Option<u32>,
        termination: Option<u32>,
        identity_end_exclusive: Option<u32>,
    ) -> Lifecycle {
        catalog_lifecycle_at(
            code,
            listing,
            identity_start,
            termination,
            identity_end_exclusive,
            None,
        )
    }

    fn catalog_lifecycle_at(
        code: &str,
        listing: u32,
        identity_start: Option<u32>,
        termination: Option<u32>,
        identity_end_exclusive: Option<u32>,
        identity_available_from: Option<u32>,
    ) -> Lifecycle {
        Lifecycle {
            native_symbol: code.into(),
            contract_raw_sha256: format!("sha256:{}", "a".repeat(64)),
            listing: crate::pit_universe::LifecycleFact {
                effective_date: date(listing),
                evidence: coverage_evidence(),
            },
            termination: termination.map(|day| crate::pit_universe::LifecycleFact {
                effective_date: date(day),
                evidence: coverage_evidence(),
            }),
            attributes: Vec::new(),
            identity_start: identity_start.map(date),
            identity_end_exclusive: identity_end_exclusive.map(date),
            identity_available_from: identity_available_from.map(date),
            code_table_first_seen: None,
            code_table_last_seen: None,
            code_table_exit_date: None,
        }
    }

    /// A lifecycle with calendar dates covering the published v2 rule window,
    /// used to bind public six-digit category assignments to lifecycle IDs.
    fn rule_lifecycle(
        code: &str,
        listing: &str,
        identity_start: Option<&str>,
        termination: Option<&str>,
        identity_end_exclusive: Option<&str>,
        identity_available_from: Option<&str>,
    ) -> Lifecycle {
        let parse = |value: &str| NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap();
        Lifecycle {
            native_symbol: code.into(),
            contract_raw_sha256: format!("sha256:{}", "a".repeat(64)),
            listing: crate::pit_universe::LifecycleFact {
                effective_date: parse(listing),
                evidence: coverage_evidence(),
            },
            termination: termination.map(|value| crate::pit_universe::LifecycleFact {
                effective_date: parse(value),
                evidence: coverage_evidence(),
            }),
            attributes: Vec::new(),
            identity_start: identity_start.map(parse),
            identity_end_exclusive: identity_end_exclusive.map(parse),
            identity_available_from: identity_available_from.map(parse),
            code_table_first_seen: None,
            code_table_last_seen: None,
            code_table_exit_date: None,
        }
    }

    fn rule_binding(
        lifecycles: &[Lifecycle],
        code: &str,
    ) -> crate::etf_market::limit_rules::RuleBinding {
        let life = lifecycles
            .iter()
            .find(|life| life.native_symbol == code)
            .unwrap();
        let id = life.instrument_id().unwrap();
        crate::etf_market::limit_rules::RuleBinding::from_lifecycles(lifecycles, &id)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn coverage_denominator_uses_the_official_listing_and_keeps_missing_cells_unknown() {
        // Official listing S1, first TDX code-table presence S3.
        let lifecycle = catalog_lifecycle("159001", 1, Some(3), None, None);
        let mapped = coverage_lifecycle(&lifecycle, std::slice::from_ref(&lifecycle)).unwrap();
        assert_eq!(mapped.listed_on, date(1));
        // No successor and no termination: the legal lifetime runs to the
        // window end, including Sessions after the last TDX presence.
        assert_eq!(mapped.terminated_on, None);

        let sessions = [date(1), date(2), date(3), date(4)];
        let report = assess(&sessions, &[mapped], &[]);
        assert_eq!(report.denominator_cells, 4);
        assert_eq!(report.unknown_cells, 4);
        assert_eq!(
            report.unknowns[0].reason,
            UnknownReason::MissingBarWithoutHaltNotice
        );
    }

    #[test]
    fn coverage_denominator_extends_to_official_termination_after_a_tdx_exit() {
        // TDX presence ends S3, official termination is S5.
        let lifecycle = catalog_lifecycle("159001", 1, Some(1), Some(5), Some(3));
        let mapped = coverage_lifecycle(&lifecycle, std::slice::from_ref(&lifecycle)).unwrap();
        assert_eq!(mapped.terminated_on, Some(date(5)));

        let sessions = [date(1), date(2), date(3), date(4), date(5)];
        let report = assess(&sessions, &[mapped], &[]);
        // [S1, S5) is four Sessions even though TDX stopped observing at S3.
        assert_eq!(report.denominator_cells, 4);
        assert_eq!(report.unknown_cells, 4);
    }

    #[test]
    fn coverage_counts_a_delayed_name_change_gap_once_under_the_predecessor() {
        // Predecessor ends at the new name's effective S2; the rename is only
        // knowable at S4, so S2/S3 are the accepted P6 gap. The successor's
        // coverage starts S4, and the predecessor holds the gap cells once.
        let old = catalog_lifecycle_at("159001", 1, Some(1), None, Some(2), None);
        let new = catalog_lifecycle_at("159001", 2, Some(2), None, None, Some(4));
        let mapped = [
            coverage_lifecycle(&old, &[old.clone(), new.clone()]).unwrap(),
            coverage_lifecycle(&new, &[old.clone(), new.clone()]).unwrap(),
        ];
        assert_eq!(mapped[0].terminated_on, Some(date(4)));
        assert_eq!(mapped[1].listed_on, date(4));
        let sessions = [date(1), date(2), date(3), date(4)];
        let report = assess(&sessions, &mapped, &[]);
        assert_eq!(report.denominator_cells, 4);
        assert_eq!(
            report
                .instruments
                .iter()
                .map(|instrument| instrument.cells)
                .collect::<Vec<_>>(),
            vec![3, 1]
        );
    }

    #[test]
    fn coverage_denominator_is_disjoint_for_a_name_change() {
        let old = catalog_lifecycle("159001", 1, Some(1), None, Some(3));
        let new = catalog_lifecycle("159001", 3, Some(3), None, None);
        let mapped = [
            coverage_lifecycle(&old, &[old.clone(), new.clone()]).unwrap(),
            coverage_lifecycle(&new, &[old.clone(), new.clone()]).unwrap(),
        ];
        let sessions = [date(1), date(2), date(3), date(4), date(5)];
        let report = assess(&sessions, &mapped, &[]);
        // Old S1..S3 (2 cells) plus new S3..window (3 cells); no double count.
        assert_eq!(report.denominator_cells, 5);
        assert_eq!(
            report
                .instruments
                .iter()
                .map(|instrument| instrument.cells)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn coverage_denominator_does_not_overlap_a_pre_tdx_reused_code_successor() {
        // Old termination S5, old TDX presence ends S3. The reused code's new
        // fund is officially listed S3 but first appears in TDX at S4.
        let old = catalog_lifecycle("159001", 1, Some(1), Some(5), Some(3));
        let new = catalog_lifecycle("159001", 3, Some(4), None, None);
        let mapped = [
            coverage_lifecycle(&old, &[old.clone(), new.clone()]).unwrap(),
            coverage_lifecycle(&new, &[old.clone(), new.clone()]).unwrap(),
        ];
        assert_eq!(mapped[0].terminated_on, Some(date(3)));
        assert_eq!(mapped[1].listed_on, date(3));
        let sessions = [date(1), date(2), date(3), date(4), date(5)];
        let report = assess(&sessions, &mapped, &[]);
        // Old [S1, S3) + new [S3, window); S3 is counted exactly once.
        assert_eq!(report.denominator_cells, 5);
        assert_eq!(
            report
                .instruments
                .iter()
                .map(|instrument| instrument.cells)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn coverage_denominator_does_not_bridge_a_code_reuse_gap() {
        let old = catalog_lifecycle("159001", 1, Some(1), Some(3), Some(3));
        let new = catalog_lifecycle("159001", 5, Some(5), None, None);
        let mapped = [
            coverage_lifecycle(&old, &[old.clone(), new.clone()]).unwrap(),
            coverage_lifecycle(&new, &[old.clone(), new.clone()]).unwrap(),
        ];
        let sessions = [date(1), date(2), date(3), date(4), date(5)];
        let report = assess(&sessions, &mapped, &[]);
        // Old [S1, S3) is two cells; S3/S4 belong to no lifecycle; new S5.
        assert_eq!(report.denominator_cells, 3);
        assert_eq!(
            report
                .instruments
                .iter()
                .map(|instrument| instrument.cells)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
    }

    #[test]
    fn coverage_denominator_ends_at_the_next_identity_for_a_legal_name_change() {
        let old = catalog_lifecycle("159001", 1, Some(1), None, Some(3));
        let new = catalog_lifecycle("159001", 3, Some(3), None, None);
        let mapped = coverage_lifecycle(&old, &[old.clone(), new]).unwrap();
        assert_eq!(mapped.listed_on, date(1));
        assert_eq!(mapped.terminated_on, Some(date(3)));

        let sessions = [date(1), date(2), date(3), date(4)];
        let report = assess(&sessions, &[mapped], &[]);
        assert_eq!(report.denominator_cells, 2);
    }

    #[test]
    fn bundled_rule_table_changes_category_rule_on_its_effective_session() {
        use crate::{
            etf_market::limit_rules::{
                CategoryEvidenceType, CategorySourceEvidence, EvidenceLevel,
                FundCategoryAssignment, FundLimitRuleTable,
            },
            pit_universe::Publication,
        };

        let table = FundLimitRuleTable::official_v1().unwrap();
        let instrument = id("159001");
        let lifecycles = [rule_lifecycle(
            "159001",
            "2022-10-01",
            None,
            None,
            None,
            None,
        )];
        let binding = rule_binding(&lifecycles, "159001");
        let assignment = |category: &str, from: &str, until: Option<&str>| {
            let document = format!("synthetic-{category}");
            FundCategoryAssignment {
                instrument_id: instrument.clone(),
                fund_category: category.into(),
                effective_from: NaiveDate::parse_from_str(from, "%Y-%m-%d").unwrap(),
                effective_until: until
                    .map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()),
                document_number: document.clone(),
                evidence: CategorySourceEvidence {
                    source_type: CategoryEvidenceType::OfficialList,
                    announcement_id: document,
                    locator: "synthetic rule switch".into(),
                    publication: Publication::DateOnly {
                        date: NaiveDate::parse_from_str("2023-01-01", "%Y-%m-%d").unwrap(),
                    },
                },
                supporting_sources: Vec::new(),
                evidence_level: EvidenceLevel::Primary,
            }
        };
        let assignments = [
            assignment("szse-fund-default-10pct", "2022-10-01", Some("2023-04-10")),
            assignment("szse-20pct-index-etf-lof", "2023-04-10", None),
        ];
        let before = table
            .rule_for_with_assignments(
                &binding,
                NaiveDate::parse_from_str("2023-04-09", "%Y-%m-%d").unwrap(),
                &assignments,
            )
            .unwrap()
            .unwrap();
        let switch_day = table
            .rule_for_with_assignments(
                &binding,
                NaiveDate::parse_from_str("2023-04-10", "%Y-%m-%d").unwrap(),
                &assignments,
            )
            .unwrap()
            .unwrap();
        assert_eq!(before.rate_bps, 1000);
        assert_eq!(switch_day.rate_bps, 2000);
        assert_ne!(before.version, switch_day.version);
        assert_eq!(
            before
                .official_rule
                .as_ref()
                .unwrap()
                .category_evidence
                .as_ref()
                .unwrap()
                .lifecycle
                .as_ref(),
            Some(binding.target())
        );
        assert!(table.supports(&before, &binding, date_for("2023-04-09")));
    }

    fn date_for(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn coverage_classification_uses_the_same_lifecycle_binding_as_rule_resolution() {
        use crate::etf_market::limit_rules::FundLimitRuleTable;

        let table = FundLimitRuleTable::official_v2().unwrap();
        // 159919 keeps a valid public assignment across the covered window.
        let old = rule_lifecycle("159919", "2012-05-28", None, None, Some("2023-01-01"), None);
        let renamed = rule_lifecycle("159919", "2023-01-01", None, None, None, Some("2023-01-01"));
        let rename_group = [old.clone(), renamed.clone()];
        let old_binding = rule_binding(&rename_group, "159919");
        // A legal-name successor stays in one fund chain, so the same public
        // assignment binds in each lifecycle's own validity window.
        assert!(
            table
                .category_assignment_for(&old_binding, date_for("2022-12-30"))
                .is_some()
        );
        let new_binding = crate::etf_market::limit_rules::RuleBinding::from_lifecycles(
            &rename_group,
            &renamed.instrument_id().unwrap(),
        )
        .unwrap()
        .unwrap();
        assert!(
            table
                .category_assignment_for(&new_binding, date_for("2023-01-02"))
                .is_some()
        );
        assert!(
            table
                .rule_for(&new_binding, date_for("2023-01-02"))
                .unwrap()
                .is_some()
        );

        // A reused code is a different fund. The published assignment interval
        // spans both lifecycles, so both sides stay CategoryUnknown without error.
        let reused_old =
            rule_lifecycle("159919", "2012-05-28", None, Some("2023-05-01"), None, None);
        let reused_new = rule_lifecycle("159919", "2023-07-01", None, None, None, None);
        let reuse_group = [reused_old.clone(), reused_new.clone()];
        for (life, day) in [(&reused_old, "2023-04-20"), (&reused_new, "2023-07-04")] {
            let binding = crate::etf_market::limit_rules::RuleBinding::from_lifecycles(
                &reuse_group,
                &life.instrument_id().unwrap(),
            )
            .unwrap()
            .unwrap();
            assert!(
                table
                    .category_assignment_for(&binding, date_for(day))
                    .is_none()
            );
            assert!(table.rule_for(&binding, date_for(day)).unwrap().is_none());
            assert_eq!(
                classify_unknown_rule(&table, &binding, date_for(day)),
                RuleUnknownReason::CategoryUnknown
            );
        }

        // One assignment interval spanning the terminated fund and its reused
        // successor is ambiguous even when a rule record exists for both dates.
        let mut wide = table
            .category_assignments
            .iter()
            .find(|assignment| assignment.instrument_id.symbol() == "159919")
            .unwrap()
            .clone();
        wide.effective_from = date_for("2022-10-01");
        wide.effective_until = None;
        let mut ambiguous = table.clone();
        ambiguous.category_assignments = vec![wide];
        for (life, day) in [(&reused_old, "2023-04-20"), (&reused_new, "2023-07-04")] {
            let binding = crate::etf_market::limit_rules::RuleBinding::from_lifecycles(
                &reuse_group,
                &life.instrument_id().unwrap(),
            )
            .unwrap()
            .unwrap();
            assert!(
                ambiguous
                    .category_assignment_for(&binding, date_for(day))
                    .is_none()
            );
            assert_eq!(
                classify_unknown_rule(&ambiguous, &binding, date_for(day)),
                RuleUnknownReason::CategoryUnknown
            );
        }
    }

    #[test]
    fn coverage_classification_agrees_with_rule_resolution_across_lifecycle_boundaries() {
        use crate::etf_market::limit_rules::FundLimitRuleTable;

        let table = FundLimitRuleTable::official_v2().unwrap();
        let lifecycle =
            rule_lifecycle("159919", "2023-01-01", None, Some("2023-05-01"), None, None);
        let binding = rule_binding(std::slice::from_ref(&lifecycle), "159919");
        let resolve = |day: &str| table.rule_for(&binding, date_for(day)).unwrap();
        for (day, rule_exists) in [
            // Before the listing: the public assignment interval covers the
            // date, but the lifecycle does not, so the cell stays UNKNOWN.
            ("2022-12-30", false),
            // Listing Session, rule-switch Session and the day after: known.
            ("2023-01-03", true),
            ("2023-04-09", true),
            ("2023-04-10", true),
            // The termination date is exclusive.
            ("2023-05-01", false),
        ] {
            let rule = resolve(day);
            assert_eq!(rule.is_some(), rule_exists, "{day}");
            if let Some(rule) = rule {
                assert!(table.supports(&rule, &binding, date_for(day)));
                assert!(
                    table
                        .category_assignment_for(&binding, date_for(day))
                        .is_some()
                );
            } else {
                assert!(
                    table
                        .category_assignment_for(&binding, date_for(day))
                        .is_none()
                );
                assert_eq!(
                    classify_unknown_rule(&table, &binding, date_for(day)),
                    RuleUnknownReason::CategoryUnknown
                );
            }
        }
        // A whole-period category gap stays UNKNOWN without an error.
        let mut no_categories = table;
        no_categories.category_assignments.clear();
        assert!(
            no_categories
                .rule_for(&binding, date_for("2023-01-03"))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            classify_unknown_rule(&no_categories, &binding, date_for("2023-01-03")),
            RuleUnknownReason::CategoryUnknown
        );
    }

    #[test]
    fn every_unknown_gets_one_versioned_reason_and_serialization_is_stable() {
        let sessions = [date(1)];
        let mut row = known("159001", 1);
        row.execution_state = TradeState::Unknown;
        row.source_conflict = true;
        row.rule_unknown = Some(RuleUnknownReason::Repealed);
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
    fn diagnostics_on_a_tradable_cell_do_not_create_unknown_coverage() {
        let sessions = [date(1)];
        let mut row = known("159001", 1);
        row.rule_unknown = Some(RuleUnknownReason::CategoryUnknown);
        let report = assess(&sessions, &[life("159001", 1, None)], &[row]);
        assert!(report.unknowns.is_empty());
        assert_eq!(report.unknown_cells, 0);
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
