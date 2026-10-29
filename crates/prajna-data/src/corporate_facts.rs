//! Offline, immutable CN stock/ETF facts; no account or price-derived events.
use crate::{
    CorporateActionKind,
    daily_raw::{SecurityKind, checked_output_path, validate_raw_root},
    dataset_preview::PreviewError,
    session_calendar::CalendarSource,
};
use chrono::{DateTime, NaiveDate};
use prajna_domain::{Amount, InstrumentId, Quantity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, PreviewError>;
fn invalid(message: &str) -> PreviewError {
    PreviewError::InvalidInput(message.into())
}
fn io(error: impl ToString) -> PreviewError {
    PreviewError::Io(error.to_string())
}
fn corrupt() -> PreviewError {
    PreviewError::Table {
        table: "corporate_facts".into(),
        message: "invalid or corrupt fact version".into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Covered,
    NoEvents,
    MissingFacts,
    Unknown,
}

/// Missing fields stay null. `available_at` is historical source availability;
/// `observed_at` on the enclosing document is only local collection time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    pub event_id: String,
    pub kind: CorporateActionKind,
    pub record_date: Option<NaiveDate>,
    pub ex_date: Option<NaiveDate>,
    pub pay_date: Option<NaiveDate>,
    pub effective_date: Option<NaiveDate>,
    pub cash_per_share: Option<String>,
    pub share_ratio: Option<String>,
    pub source: Option<CalendarSource>,
    pub available_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub instrument_id: String,
    pub kind: SecurityKind,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub state: State,
    pub reason: String,
    /// Provenance and historical availability of the scope assertion itself.
    pub source: Option<CalendarSource>,
    pub available_at: Option<String>,
    pub facts: Vec<Fact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub schema_version: u32,
    pub observed_at: String,
    pub coverage: Vec<Coverage>,
}

fn source(source: &Option<CalendarSource>) -> Result<()> {
    if let Some(source) = source
        && [
            &source.source_id,
            &source.source_version,
            &source.source_ref,
        ]
        .iter()
        .any(|v| v.trim().is_empty())
    {
        return Err(invalid(
            "source fields must be nonempty; use null for missing source",
        ));
    }
    Ok(())
}
fn timestamp(value: &Option<String>) -> Result<()> {
    if value
        .as_ref()
        .is_some_and(|v| DateTime::parse_from_rfc3339(v).is_err())
    {
        return Err(invalid("invalid historical available_at timestamp"));
    }
    Ok(())
}
fn instrument(value: &str) -> Result<()> {
    let id: InstrumentId = value.parse().map_err(|_| invalid("invalid instrument"))?;
    if !matches!(id.venue().as_str(), "XSHG" | "XSHE")
        || id.symbol().len() != 6
        || !id.symbol().bytes().all(|v| v.is_ascii_digit())
    {
        return Err(invalid("explicit six-digit XSHG/XSHE identity is required"));
    }
    Ok(())
}

impl Fact {
    /// Completeness concerns this event only, never an announcement-universe proof.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.source.is_none() {
            missing.push("source");
        }
        if self.available_at.is_none() {
            missing.push("available_at");
        }
        match self.kind {
            CorporateActionKind::Dividend => {
                if self.record_date.is_none() {
                    missing.push("record_date");
                }
                if self.ex_date.is_none() {
                    missing.push("ex_date");
                }
                if self.pay_date.is_none() {
                    missing.push("pay_date");
                }
                if self.cash_per_share.is_none() {
                    missing.push("cash_per_share");
                }
            }
            CorporateActionKind::ShareConversion => {
                if self.effective_date.is_none() {
                    missing.push("effective_date");
                }
                if self.share_ratio.is_none() {
                    missing.push("share_ratio");
                }
            }
            CorporateActionKind::Liquidation => {
                if self.effective_date.is_none() {
                    missing.push("effective_date");
                }
                if self.pay_date.is_none() {
                    missing.push("pay_date");
                }
                if self.cash_per_share.is_none() {
                    missing.push("cash_per_share");
                }
            }
        }
        missing
    }
}

fn validate(input: &Input) -> Result<()> {
    if input.schema_version != 1
        || input.coverage.is_empty()
        || DateTime::parse_from_rfc3339(&input.observed_at).is_err()
    {
        return Err(invalid(
            "schema 1, observed_at and nonempty explicit coverage are required",
        ));
    }
    let mut events = BTreeSet::new();
    for (index, scope) in input.coverage.iter().enumerate() {
        instrument(&scope.instrument_id)?;
        source(&scope.source)?;
        timestamp(&scope.available_at)?;
        if scope.start > scope.end || scope.reason.trim().is_empty() {
            return Err(invalid("valid scope interval and reason are required"));
        }
        if input.coverage[..index].iter().any(|other| {
            other.instrument_id == scope.instrument_id
                && other.start <= scope.end
                && scope.start <= other.end
        }) {
            return Err(invalid("duplicate or conflicting coverage intervals"));
        }
        match scope.state {
            State::Covered | State::NoEvents
                if scope.source.is_none() || scope.available_at.is_none() =>
            {
                return Err(invalid(
                    "covered/no_events requires scope source and historical availability",
                ));
            }
            State::Covered if scope.facts.is_empty() => {
                return Err(invalid(
                    "covered requires events; use no_events for explicit absence",
                ));
            }
            State::NoEvents | State::Unknown if !scope.facts.is_empty() => {
                return Err(invalid("no_events/unknown cannot contain events"));
            }
            _ => {}
        }
        for fact in &scope.facts {
            if fact.event_id.trim().is_empty()
                || !events.insert((&scope.instrument_id, &fact.event_id))
            {
                return Err(invalid("empty, duplicate or conflicting event identity"));
            }
            source(&fact.source)?;
            timestamp(&fact.available_at)?;
            if let Some(value) = &fact.cash_per_share {
                let value =
                    Amount::parse(value).map_err(|_| invalid("invalid exact cash amount"))?;
                if value <= Amount::ZERO {
                    return Err(invalid("cash amount must be positive"));
                }
            }
            if let Some(value) = &fact.share_ratio {
                let value =
                    Quantity::parse(value).map_err(|_| invalid("invalid exact share ratio"))?;
                if value <= Quantity::ZERO {
                    return Err(invalid("share ratio must be positive"));
                }
            }
            if (matches!(fact.kind, CorporateActionKind::ShareConversion)
                && fact.cash_per_share.is_some())
                || (!matches!(fact.kind, CorporateActionKind::ShareConversion)
                    && fact.share_ratio.is_some())
            {
                return Err(invalid("amount/ratio does not match event kind"));
            }
            // Scope dates refer to ex/effective events; payment can occur outside it.
            let anchor = match fact.kind {
                CorporateActionKind::Dividend => fact.ex_date,
                _ => fact.effective_date,
            };
            if anchor.is_some_and(|date| date < scope.start || date > scope.end)
                || fact
                    .record_date
                    .zip(fact.ex_date)
                    .is_some_and(|(r, e)| r > e)
                || [fact.ex_date, fact.effective_date].into_iter().any(|date| {
                    date.zip(fact.pay_date)
                        .is_some_and(|(event, pay)| event > pay)
                })
            {
                return Err(invalid("event date order or coverage anchor is invalid"));
            }
            if scope.state == State::Covered && !fact.missing().is_empty() {
                return Err(invalid("incomplete event requires missing_facts state"));
            }
        }
    }
    Ok(())
}

fn version(bytes: &[u8]) -> String {
    format!("caf:sha256:{}", crate::sha256(bytes))
}
fn path(root: &Path, id: &str) -> Result<PathBuf> {
    let hash = id
        .strip_prefix("caf:sha256:")
        .ok_or_else(|| invalid("invalid fact version"))?;
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("invalid fact version"));
    }
    checked_output_path(root, Path::new(&format!("corporate-facts/{hash}.json"))).map_err(io)
}
fn root(root: &Path) -> Result<PathBuf> {
    validate_raw_root(root, &crate::default_mvp6_root()).map_err(io)
}

/// Exact source bytes determine identity, including unknown fields' rejection,
/// provenance, incomplete facts and local observed_at. No implicit latest version.
pub fn import(directory: &Path, bytes: &[u8]) -> Result<String> {
    let input: Input = serde_json::from_slice(bytes).map_err(|_| invalid("invalid fact JSON"))?;
    validate(&input)?;
    let root = root(directory)?;
    let id = version(bytes);
    let target = path(&root, &id)?;
    let parent = target.parent().expect("fact parent");
    fs::create_dir_all(parent).map_err(io)?;
    let target = path(&root, &id)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(io)?;
    temp.write_all(bytes).map_err(io)?;
    temp.as_file().sync_all().map_err(io)?;
    match temp.persist_noclobber(&target) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::read(target).map_err(io)? != bytes {
                return Err(corrupt());
            }
        }
        Err(error) => return Err(io(error.error)),
    }
    Ok(id)
}

/// Verifies persisted bytes and contract on every read, including incomplete versions.
pub fn read(directory: &Path, id: &str) -> Result<Input> {
    let root = root(directory)?;
    let bytes = fs::read(path(&root, id)?).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            PreviewError::NotFound("fact version does not exist".into())
        } else {
            io(error)
        }
    })?;
    if version(&bytes) != id {
        return Err(corrupt());
    }
    let input: Input = serde_json::from_slice(&bytes).map_err(|_| corrupt())?;
    validate(&input).map_err(|_| corrupt())?;
    Ok(input)
}

/// A range must fit wholly inside one explicit assertion. Outside/ambiguous
/// coverage is Unknown; no merging partial ranges into a completeness claim.
pub fn query(
    directory: &Path,
    id: &str,
    security: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Value> {
    instrument(security)?;
    if start > end {
        return Err(invalid("invalid query interval"));
    }
    let input = read(directory, id)?;
    let scope = input
        .coverage
        .iter()
        .find(|scope| scope.instrument_id == security && scope.start <= start && scope.end >= end);
    Ok(
        json!({"schema_version":1,"version":id,"instrument_id":security,"start":start,"end":end,
        "state":scope.map_or(State::Unknown, |scope| scope.state),
        "coverage":scope,"observed_at":input.observed_at,
        "missing":scope.map(|scope| scope.facts.iter().map(|fact| json!({"event_id":fact.event_id,"fields":fact.missing()})).collect::<Vec<_>>()),
        "notice":"Scoped supplied facts, not an announcement-universe completeness proof. No price or account calculation. observed_at is not historical availability."}),
    )
}
