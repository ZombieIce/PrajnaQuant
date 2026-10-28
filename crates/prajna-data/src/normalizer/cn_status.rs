//! Fixed opening-state evidence; no inference from quotes or collection time.
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate};
use prajna_domain::{InstrumentId, TimestampNs, VenueId};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::{CnStockEtfDaily, Normalizer, RawInput};
use crate::{
    ExecutionStatusRow, NormalizationIssue, NormalizationOutput, SecuritySnapshotId, SourceKind,
    execution_status_to_record_batch, instruments_from_record_batch,
    session_calendar::CalendarVersionId, sessions_from_record_batch,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpeningEvidenceSnapshot {
    pub schema_version: u32,
    pub method_version: String,
    pub evidence_kind: EvidenceKind,
    pub source_id: String,
    pub source_version: String,
    pub extraction_version: String,
    pub calendar_version: String,
    pub security_snapshot: String,
    pub market_raw_sha256s: Vec<String>,
    pub instrument_id: InstrumentId,
    pub venue_id: VenueId,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub observed_at: String,
    pub evidence: Vec<OpeningEvidenceReference>,
    pub facts: Vec<OpeningFact>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Synthetic,
    RealSource,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpeningEvidenceReference {
    pub raw_sha256: String,
    pub locator: String,
    pub available_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OpeningState {
    Tradable,
    Halted,
    Unknown,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpeningFact {
    pub session_date: NaiveDate,
    pub open_from: String,
    pub open_to_exclusive: String,
    pub state: OpeningState,
    pub available_at: Option<String>,
    pub evidence_indices: Vec<usize>,
    pub reason: String,
}

fn require(condition: bool, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn timestamp(value: &str) -> Result<TimestampNs> {
    Ok(TimestampNs::parse(value)?)
}

impl OpeningEvidenceSnapshot {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value: Self = serde_json::from_slice(bytes)?;
        require(
            value.schema_version == 1 && value.method_version == "cn-opening-evidence/1",
            "unsupported opening evidence schema or method",
        )?;
        require(
            !value.source_id.trim().is_empty()
                && !value.source_version.trim().is_empty()
                && !value.extraction_version.trim().is_empty(),
            "missing fixed source/extraction version",
        )?;
        DateTime::parse_from_rfc3339(&value.observed_at)?;
        require(
            value.start <= value.end && value.instrument_id.venue() == &value.venue_id,
            "invalid evidence scope or Venue",
        )?;
        CalendarVersionId::parse(&value.calendar_version)?;
        SecuritySnapshotId::parse(&value.security_snapshot)?;
        let mut seen = BTreeSet::new();
        require(
            !value.market_raw_sha256s.is_empty(),
            "missing pinned market Raw",
        )?;
        for hash in &value.market_raw_sha256s {
            CalendarVersionId::parse(hash)?;
            require(seen.insert(hash), "duplicate pinned market Raw")?;
        }
        for evidence in &value.evidence {
            CalendarVersionId::parse(&evidence.raw_sha256)?;
            require(
                !evidence.locator.trim().is_empty(),
                "missing evidence locator",
            )?;
            if let Some(available) = &evidence.available_at {
                timestamp(available)?;
            }
        }
        Ok(value)
    }
}

pub struct CnDailyWithStatus {
    base: CnStockEtfDaily,
    calendar: String,
    security: String,
    snapshot: String,
}
impl CnDailyWithStatus {
    pub fn new(
        calendar: CalendarVersionId,
        security: SecuritySnapshotId,
        snapshot: String,
    ) -> Result<Self> {
        CalendarVersionId::parse(&snapshot)?;
        Ok(Self {
            calendar: calendar.as_str().into(),
            security: security.as_str().into(),
            base: CnStockEtfDaily::v1(calendar, security),
            snapshot,
        })
    }

    fn convert(&self, inputs: &[RawInput]) -> Result<NormalizationOutput> {
        let mut by_hash = BTreeMap::new();
        for raw in inputs {
            require(raw.has_valid_identity(), "opening input Raw hash mismatch")?;
            require(
                by_hash.insert(raw.raw_sha256.clone(), raw).is_none(),
                "duplicate opening input Raw",
            )?;
        }
        let snapshot = by_hash
            .get(&self.snapshot)
            .ok_or("missing fixed opening snapshot")?;
        let snapshot = OpeningEvidenceSnapshot::parse(&snapshot.bytes)?;
        require(
            snapshot.calendar_version == self.calendar
                && snapshot.security_snapshot == self.security,
            "opening snapshot calendar/security identity mismatch",
        )?;
        let market = snapshot
            .market_raw_sha256s
            .iter()
            .cloned()
            .chain([self.calendar.clone(), self.security.clone()])
            .collect::<BTreeSet<_>>();
        let expected = market
            .iter()
            .cloned()
            .chain([self.snapshot.clone()])
            .chain(snapshot.evidence.iter().map(|item| item.raw_sha256.clone()))
            .collect::<BTreeSet<_>>();
        require(
            expected == by_hash.keys().cloned().collect(),
            "missing or unexpected frozen opening inputs",
        )?;
        let base_inputs = inputs
            .iter()
            .filter(|raw| market.contains(&raw.raw_sha256))
            .cloned()
            .collect::<Vec<_>>();
        let mut output = self
            .base
            .normalize(&base_inputs)
            .map_err(|issues| format!("{issues:?}"))?;
        let instruments = instruments_from_record_batch(&output.tables.instruments)?;
        require(
            instruments.len() == 1 && instruments[0].id() == &snapshot.instrument_id,
            "opening snapshot Instrument mismatch",
        )?;
        let sessions = sessions_from_record_batch(&output.tables.sessions)?
            .into_iter()
            .filter(|session| {
                session.venue_id() == &snapshot.venue_id
                    && (snapshot.start..=snapshot.end).contains(&session.session_date())
            })
            .map(|session| (session.session_date(), session))
            .collect::<BTreeMap<_, _>>();
        require(
            !sessions.is_empty(),
            "opening publication scope has no Sessions",
        )?;
        let mut facts = BTreeMap::new();
        for fact in &snapshot.facts {
            let session = sessions
                .get(&fact.session_date)
                .ok_or("opening fact outside scope or non-Session")?;
            require(
                timestamp(&fact.open_from)? <= session.ts_open()
                    && timestamp(&fact.open_to_exclusive)? > session.ts_open(),
                "opening interval does not cover Session open",
            )?;
            require(
                !fact.reason.trim().is_empty() && !fact.evidence_indices.is_empty(),
                "opening fact needs evidence and reason",
            )?;
            let mut references = BTreeSet::new();
            let mut known = true;
            let available = fact.available_at.as_deref().map(timestamp).transpose()?;
            for index in &fact.evidence_indices {
                require(references.insert(index), "duplicate evidence reference")?;
                let evidence = snapshot
                    .evidence
                    .get(*index)
                    .ok_or("invalid opening evidence reference")?;
                match evidence
                    .available_at
                    .as_deref()
                    .map(timestamp)
                    .transpose()?
                {
                    Some(time) => {
                        if let Some(available) = available {
                            require(
                                available >= time,
                                "opening availability precedes required evidence",
                            )?;
                        }
                    }
                    None => known = false,
                }
            }
            let state = if !known || available.is_none() || fact.state == OpeningState::Conflict {
                "UNKNOWN"
            } else {
                match fact.state {
                    OpeningState::Tradable => "TRADABLE",
                    OpeningState::Halted => "HALTED",
                    _ => "UNKNOWN",
                }
            };
            if let Some((previous, _)) = facts.get(&fact.session_date) {
                require(*previous == fact, "conflicting duplicate opening facts")?;
            } else {
                facts.insert(
                    fact.session_date,
                    (
                        fact,
                        ExecutionStatusRow {
                            instrument_id: snapshot.instrument_id.clone(),
                            session_date: fact.session_date,
                            trade_status: state.into(),
                            is_tradable: state == "TRADABLE",
                            available_at: if known { available } else { None },
                            source: format!("cn-opening-evidence/1:{}", self.snapshot),
                        },
                    ),
                );
            }
        }
        let rows = sessions
            .keys()
            .map(|date| {
                facts
                    .get(date)
                    .map(|(_, row)| row.clone())
                    .unwrap_or_else(|| ExecutionStatusRow {
                        instrument_id: snapshot.instrument_id.clone(),
                        session_date: *date,
                        trade_status: "UNKNOWN".into(),
                        is_tradable: false,
                        available_at: None,
                        source: format!("cn-opening-evidence/1:missing:{}", self.snapshot),
                    })
            })
            .collect::<Vec<_>>();
        output.tables.execution_status = Some(execution_status_to_record_batch(&rows)?);
        output.coverage["normalizer_version"] = json!("2");
        output.coverage["execution_status"] = json!({"snapshot": self.snapshot,
            "rows": rows.len(), "unknown": rows.iter().filter(|row| row.trade_status == "UNKNOWN").count(),
            "evidence_kind": snapshot.evidence_kind, "missing_reason": "no explicit opening fact"});
        Ok(output)
    }
}
impl Normalizer for CnDailyWithStatus {
    fn id(&self) -> &str {
        "cn-stock-etf-daily"
    }
    fn version(&self) -> &str {
        "2"
    }
    fn config_sha256(&self) -> String {
        let bytes = crate::restricted_jcs(&json!({"base_v1": self.base.config_sha256(),
            "status_snapshot": self.snapshot, "opening_rule": "cn-opening-evidence/1"}))
        .expect("fixed strings");
        format!("sha256:{:x}", Sha256::digest(bytes))
    }
    fn accepted_source_kinds(&self) -> &[SourceKind] {
        &[SourceKind::FileImport, SourceKind::VendorApi]
    }
    fn normalize(
        &self,
        inputs: &[RawInput],
    ) -> std::result::Result<NormalizationOutput, Vec<NormalizationIssue>> {
        self.convert(inputs).map_err(|error| {
            vec![NormalizationIssue::new(
                "invalid_opening_evidence",
                "$",
                error.to_string(),
            )]
        })
    }
}
