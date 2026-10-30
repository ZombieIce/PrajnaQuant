//! Fixed generic facts feed the existing account ledger, never research prices.
use crate::{ExperimentError, invalid};
use chrono::DateTime;
use prajna_data::{
    CorporateActionKind, CorporateActionRow,
    corporate_facts::{self, Input, State},
};
use prajna_domain::{Notional, TimestampNs};
use prajna_research::{
    Panel,
    corporate_actions::{ActionCoverageStatus, CorporateActionCoverage, CorporateActionSchedule},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

pub const SEMANTIC_VERSION: &str = "cn-static-action-ledger@1";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub version: String,
    /// Exact, explicit flat dividend withholding rate; not inferred tax policy.
    pub dividend_tax_rate: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Context {
    pub binding: Binding,
    input: Input,
    tax: Notional,
}

impl Context {
    pub fn load(root: &Path, binding: &Binding) -> Result<Self, ExperimentError> {
        let tax: Notional = binding
            .dividend_tax_rate
            .parse()
            .map_err(|e| invalid(format!("invalid dividend tax: {e}")))?;
        if tax < Notional::ZERO || tax >= "1".parse().unwrap() {
            return Err(invalid("dividend tax must be in [0,1)"));
        }
        let mut binding = binding.clone();
        binding.dividend_tax_rate = tax.to_string();
        let input = corporate_facts::read(root, &binding.version)
            .map_err(|e| invalid(format!("corporate facts: {e}")))?;
        Ok(Self {
            binding,
            input,
            tax,
        })
    }

    pub fn identity(&self) -> serde_json::Value {
        serde_json::json!({"semantic_version":SEMANTIC_VERSION,"version":self.binding.version,"dividend_tax_rate":self.tax.to_string()})
    }

    pub fn schedule(&self, panel: &Panel) -> Result<CorporateActionSchedule, ExperimentError> {
        let first = panel
            .sessions
            .first()
            .ok_or_else(|| invalid("actions require Sessions"))?
            .session_date;
        let last = panel.sessions.last().unwrap().session_date;
        let sessions = panel
            .sessions
            .iter()
            .map(|s| s.session_date)
            .collect::<BTreeSet<_>>();
        let mut rows = Vec::new();
        let mut coverage = Vec::new();
        let mut reasons = Vec::new();
        for instrument in &panel.instruments {
            let scopes = self
                .input
                .coverage
                .iter()
                .filter(|c| c.instrument_id == instrument.to_string())
                .collect::<Vec<_>>();
            let complete = scopes.iter().any(|c| {
                c.start <= first
                    && c.end >= last
                    && matches!(c.state, State::Covered | State::NoEvents)
            });
            for scope in scopes {
                if scope.start <= last
                    && scope.end >= first
                    && matches!(scope.state, State::MissingFacts | State::Unknown)
                {
                    reasons.push(format!("{instrument} {:?}: {}", scope.state, scope.reason));
                }
                // Keep pre-window registration facts when their ex/pay date is in-window.
                // No scope/availability statement is promoted into an event fact.
                for fact in &scope.facts {
                    let dates = [
                        fact.record_date,
                        fact.ex_date,
                        fact.pay_date,
                        fact.effective_date,
                    ];
                    if dates.iter().flatten().all(|d| *d < first || *d > last)
                        && dates.iter().any(Option::is_some)
                    {
                        continue;
                    }
                    let missing = fact.missing();
                    let unsupported_date = dates
                        .iter()
                        .flatten()
                        .any(|d| *d >= first && *d <= last && !sessions.contains(d))
                        || (fact.kind == CorporateActionKind::Dividend
                            && fact.record_date == fact.ex_date);
                    if !missing.is_empty() || unsupported_date {
                        reasons.push(format!("{instrument}/{} not applied: missing {missing:?}; unsupported Session/date order={unsupported_date}",fact.event_id));
                        continue;
                    }
                    let available =
                        DateTime::parse_from_rfc3339(fact.available_at.as_ref().unwrap())
                            .map_err(|e| invalid(e.to_string()))?;
                    let timestamp = TimestampNs::from_unix_nanos(
                        available
                            .timestamp_nanos_opt()
                            .ok_or_else(|| invalid("action timestamp overflow"))?,
                    );
                    rows.push(CorporateActionRow {
                        // Source uniqueness is per Instrument; the legacy schedule
                        // requires global announcement identities. JSON tuple encoding
                        // preserves both fields without delimiter/escaping collisions.
                        announcement_id: serde_json::json!([instrument.to_string(), fact.event_id])
                            .to_string(),
                        action_index: 0,
                        instrument_id: instrument.clone(),
                        kind: fact.kind,
                        // The legacy row requires a date; this is the availability date,
                        // not a claimed publication date. No announced_at is fabricated.
                        announced_date: available.date_naive(),
                        announced_at: None,
                        available_at: timestamp,
                        record_date: fact.record_date,
                        ex_date: fact.ex_date,
                        pay_date: fact.pay_date,
                        effective_date: fact.effective_date,
                        cash_per_share: if fact.kind == CorporateActionKind::Dividend {
                            fact.cash_per_share.clone()
                        } else {
                            None
                        },
                        share_ratio: fact.share_ratio.clone(),
                        liquidation_cash_per_share: if fact.kind == CorporateActionKind::Liquidation
                        {
                            fact.cash_per_share.clone()
                        } else {
                            None
                        },
                        source_raw_sha256: self
                            .binding
                            .version
                            .trim_start_matches("caf:sha256:")
                            .into(),
                        normalization_version: SEMANTIC_VERSION.into(),
                    });
                }
            }
            coverage.push(CorporateActionCoverage {
                instrument_id: instrument.clone(),
                start: first,
                end: last,
                observed_actions: rows
                    .iter()
                    .filter(|r| &r.instrument_id == instrument)
                    .count(),
                unapplied_action_dates: vec![],
                unexplained_price_jumps: vec![],
                status: if complete {
                    ActionCoverageStatus::Complete
                } else {
                    ActionCoverageStatus::Unknown
                },
            });
        }
        CorporateActionSchedule::new(self.binding.version.clone(), rows, coverage)
            .and_then(|s| s.with_dividend_tax_rate(self.tax))
            .map(|s| s.with_incomplete_reasons(reasons))
            .map_err(|e| invalid(e.to_string()))
    }
}
