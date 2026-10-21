//! Local deterministic builder for `szse-derived-open@3` inputs.

use prajna_data::{read_dataset_table, read_manifest, sessions_from_record_batch};
use prajna_research::{
    etf_market::{
        ExecutionStatusVersion, MarketEvidence, OpeningEvidence,
        limit_rules::{FundLimitRuleTable, RuleBinding},
    },
    pit_universe::HistoricalCatalog,
};
use std::{collections::BTreeSet, path::Path};

const EVIDENCE_RULE_VERSION: &str = "mvp6-opening-evidence@1";

/// Build one deterministic opening row for each Catalog lifecycle/session pair.
/// Only the frozen Dataset calendar and versioned public limit rules contribute
/// evidence; halt facts and citation indexes do not define tradability.
pub fn generate(
    root: &Path,
    dsv: &str,
    catalog: &HistoricalCatalog,
) -> Result<MarketEvidence, String> {
    let manifest = read_manifest(root, dsv).map_err(|error| error.to_string())?;
    if manifest.core.normalizer.id != "szse-etf-frozen"
        || !matches!(manifest.core.normalizer.version.as_str(), "4" | "7")
    {
        return Err("opening evidence requires szse-etf-frozen@4 or @7".into());
    }
    let session_batches =
        read_dataset_table(root, &manifest, "sessions").map_err(|error| error.to_string())?;
    if session_batches.len() != 1 {
        return Err("Dataset must contain one normalized sessions batch".into());
    }
    let dataset_sessions =
        sessions_from_record_batch(&session_batches[0]).map_err(|error| error.to_string())?;
    let sessions = catalog
        .input()
        .sessions
        .iter()
        .filter(|session| {
            session.session_date() >= catalog.input().coverage.start
                && session.session_date() <= catalog.input().coverage.end
        })
        .cloned()
        .collect::<Vec<_>>();
    let scoped_dataset_sessions = dataset_sessions
        .iter()
        .filter(|session| {
            session.session_date() >= catalog.input().coverage.start
                && session.session_date() <= catalog.input().coverage.end
        })
        .cloned()
        .collect::<Vec<_>>();
    if sessions != scoped_dataset_sessions || sessions.is_empty() {
        return Err("Dataset calendar differs from Catalog coverage".into());
    }

    let rule_table = FundLimitRuleTable::official_v2().map_err(|error| error.to_string())?;
    let mut openings = Vec::new();
    for life in &catalog.input().lifecycles {
        let id = life.instrument_id().map_err(|error| error.to_string())?;
        let binding = RuleBinding::from_lifecycles(&catalog.input().lifecycles, &id)
            .map_err(|error| error.to_string())?;
        for session in &sessions {
            let date = session.session_date();
            if !life.validity().contains(date) {
                continue;
            }
            let limit_rule = binding
                .as_ref()
                .map(|binding| rule_table.rule_for(binding, date))
                .transpose()
                .map_err(|error| error.to_string())?
                .flatten();
            openings.push(OpeningEvidence {
                instrument_id: id.clone(),
                session_date: date,
                announcement_coverage: None,
                source_independence: None,
                halt: None,
                limit_rule,
                reference: None,
                market_available_at: Some(session.ts_close()),
            });
        }
    }
    openings.sort_by_key(|opening| (opening.instrument_id.clone(), opening.session_date));
    Ok(MarketEvidence {
        semantic_version: Some(ExecutionStatusVersion::V3),
        rule_version: EVIDENCE_RULE_VERSION.into(),
        start: catalog.input().coverage.start,
        end: catalog.input().coverage.end,
        coverage_gaps: BTreeSet::new(),
        openings,
    })
}

/// Stable JSON bytes for local evidence replay and deterministic-output checks.
pub fn serialize(evidence: &MarketEvidence) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec_pretty(evidence).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn serializing_a_market_evidence_value_is_deterministic() {
        let value = MarketEvidence {
            semantic_version: Some(ExecutionStatusVersion::V3),
            rule_version: EVIDENCE_RULE_VERSION.into(),
            start: NaiveDate::from_ymd_opt(2023, 1, 1).unwrap(),
            end: NaiveDate::from_ymd_opt(2023, 1, 2).unwrap(),
            coverage_gaps: BTreeSet::new(),
            openings: Vec::new(),
        };
        assert_eq!(serialize(&value).unwrap(), serialize(&value).unwrap());
    }
}
