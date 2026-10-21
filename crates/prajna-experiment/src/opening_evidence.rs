//! Local deterministic builder for `szse-derived-open@3` inputs.

use chrono::NaiveDate;
use prajna_data::{
    BarSourceRow, RawStore, bar_sources_from_record_batch, bars_from_record_batch,
    cninfo::{Citation, EventFact, Poppler, read_citations_csv_for_sources},
    read_dataset_table, read_manifest, sessions_from_record_batch,
};
use prajna_domain::{InstrumentId, Session};
use prajna_research::{
    etf_market::{
        ExecutionStatusVersion, HaltEvidence, HaltScope, MarketEvidence, OpeningEvidence,
        limit_rules::{FundLimitRuleTable, RuleBinding},
    },
    pit_universe::{Evidence, HistoricalCatalog, Publication},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const EVIDENCE_RULE_VERSION: &str = "mvp6-opening-evidence@1";

/// Build one deterministic opening row for each lifecycle/session pair.
/// Citation objects must have been loaded with `read_citations_csv_for_sources`, which
/// verifies every referenced local Raw and PDF page.
pub fn generate(
    root: &Path,
    dsv: &str,
    catalog: &HistoricalCatalog,
    events: &[EventFact],
    citations: &[Citation],
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
    let session_dates = sessions
        .iter()
        .map(Session::session_date)
        .collect::<BTreeSet<_>>();
    let bars_batches =
        read_dataset_table(root, &manifest, "bars").map_err(|error| error.to_string())?;
    let mut bars = BTreeMap::new();
    for batch in &bars_batches {
        for bar in bars_from_record_batch(batch, &catalog.input().sessions)
            .map_err(|error| error.to_string())?
        {
            if !session_dates.contains(&bar.session_date()) {
                continue;
            }
            let id = catalog
                .resolve_row(bar.instrument_id().symbol(), bar.session_date())
                .map_err(|error| error.to_string())?;
            if bars.insert((id, bar.session_date()), bar).is_some() {
                return Err("duplicate normalized Bar".into());
            }
        }
    }
    let source_batches =
        read_dataset_table(root, &manifest, "bar_sources").map_err(|error| error.to_string())?;
    let raw = RawStore::open(root).map_err(|error| error.to_string())?;
    let mut sources = BTreeMap::<(InstrumentId, NaiveDate), BarSourceRow>::new();
    for batch in &source_batches {
        for source in bar_sources_from_record_batch(batch).map_err(|error| error.to_string())? {
            if !session_dates.contains(&source.session_date) {
                continue;
            }
            raw.get(&source.source_raw_sha256)
                .map_err(|error| error.to_string())?;
            let id = catalog
                .resolve_row(source.instrument_id.symbol(), source.session_date)
                .map_err(|error| error.to_string())?;
            if sources.insert((id, source.session_date), source).is_some() {
                return Err("duplicate normalized Bar source".into());
            }
        }
    }
    if bars.keys().ne(sources.keys()) {
        return Err("normalized Bar and Bar source keys differ".into());
    }

    let halts = parse_halts(
        events,
        citations,
        &catalog.input().lifecycles,
        &session_dates,
    )?;
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
                halt: halts.get(&(life.native_symbol.clone(), date)).cloned(),
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

/// Read the fixed local facts and verified citations, then build @3 evidence.
pub fn generate_from_local_files(
    root: &Path,
    dsv: &str,
    catalog: &HistoricalCatalog,
    facts_dir: &Path,
) -> Result<MarketEvidence, String> {
    let events = fs::read_to_string(facts_dir.join("events.csv"))
        .map_err(|error| format!("read events.csv: {error}"))?;
    let events = prajna_data::cninfo::Mvp6Facts::events_from_csv(&events)
        .map_err(|error| format!("parse events.csv: {error}"))?;
    let citation_rows = fs::read_to_string(facts_dir.join("citations.csv"))
        .map_err(|error| format!("read citations.csv: {error}"))?;
    let event_sources = events
        .iter()
        .filter(|event| event.kind == "suspension")
        .map(|event| event.source_announcement_id.clone())
        .collect::<BTreeSet<_>>();
    let raw = RawStore::open(root).map_err(|error| error.to_string())?;
    let mut converter = Poppler;
    let citations =
        read_citations_csv_for_sources(&citation_rows, &raw, &mut converter, Some(&event_sources))
            .map_err(|error| format!("verify local citations: {error}"))?;
    generate(root, dsv, catalog, &events, &citations)
}

fn parse_halts(
    events: &[EventFact],
    citations: &[Citation],
    lifecycles: &[prajna_research::pit_universe::Lifecycle],
    session_dates: &BTreeSet<NaiveDate>,
) -> Result<BTreeMap<(String, NaiveDate), HaltEvidence>, String> {
    let mut halts = BTreeMap::new();
    for event in events {
        if event.kind == "termination" {
            // Formal termination is represented by Catalog lifecycle validity.
            continue;
        }
        let scope = match (event.kind.as_str(), event.session_part.as_str()) {
            ("suspension", "morning" | "open") => HaltScope::OpeningOnly,
            ("suspension", "full_day") => HaltScope::FullSession,
            ("suspension", _) => return Err("invalid session_part for suspension event".into()),
            _ => return Err(format!("unsupported event kind: {}", event.kind)),
        };
        if !matches!(event.evidence_level.as_str(), "official" | "secondary") {
            return Err("event evidence_level must be official or secondary".into());
        }
        let effective_date = NaiveDate::parse_from_str(&event.effective_date, "%Y-%m-%d")
            .map_err(|_| "event has invalid effective_date".to_owned())?;
        let announcement_date = NaiveDate::parse_from_str(&event.announcement_date, "%Y-%m-%d")
            .map_err(|_| "event has invalid announcement_date".to_owned())?;
        let page = event
            .source_page
            .parse::<u32>()
            .map_err(|_| "event has invalid source_page".to_owned())?;
        let fields = [
            "announcement_date",
            "effective_date",
            "kind",
            "session_part",
        ];
        let cited = fields
            .iter()
            .map(|field| {
                citations
                    .iter()
                    .find(|citation| {
                        citation.fact_source() == event.source_announcement_id
                            && citation.field() == *field
                            && citation.announcement_id() == event.source_announcement_id
                            && citation.page() == page
                    })
                    .ok_or_else(|| format!("event citation missing for field {field}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let evidence_citation = cited[1];
        if cited.iter().any(|citation| {
            citation.raw_sha256() != evidence_citation.raw_sha256()
                || citation.publication_date() != evidence_citation.publication_date()
        }) {
            return Err("event citations do not share one Raw and publication date".into());
        }
        if evidence_citation.publication_date() != Some(announcement_date) {
            return Err("event announcement_date differs from cited Raw publication date".into());
        }
        if !session_dates.contains(&effective_date)
            || !lifecycles.iter().any(|life| {
                life.native_symbol == event.code && life.validity().contains(effective_date)
            })
        {
            return Err("suspension event is outside the covered lifecycle/session grid".into());
        }
        let publication = Publication::DateOnly {
            date: evidence_citation
                .publication_date()
                .ok_or_else(|| "event Raw has no unambiguous publication date".to_owned())?,
        };
        let halt = HaltEvidence {
            scope,
            evidence: Evidence {
                raw_sha256: evidence_citation.raw_sha256().to_owned(),
                announcement_id: evidence_citation.announcement_id().to_owned(),
                locator: format!("page {} event effective_date", evidence_citation.page()),
                publication,
            },
        };
        if halts
            .insert((event.code.clone(), effective_date), halt)
            .is_some()
        {
            return Err("duplicate suspension event for a lifecycle/session".into());
        }
    }
    Ok(halts)
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
