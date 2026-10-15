//! MVP-6 offline opening evidence. Daily crosschecks are never backdated to open.
//! The existing D7/execution path is unchanged; #177 consumes directional gates.
use crate::pit_universe::{CatalogError, Evidence, HistoricalCatalog};
use arrow_array::{Array, Date32Array, RecordBatch, StringArray};
use chrono::NaiveDate;
use prajna_data::{
    NormalizedTables, NormalizerRegistry, RawInput, RawStore, bars_from_record_batch, logical_hash,
    read_manifest, restricted_jcs,
};
use prajna_domain::{Bar, InstrumentId, Notional, Price, Quantity, Session, TimestampNs};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

type Result<T> = std::result::Result<T, CatalogError>;
fn err(e: impl std::fmt::Display) -> CatalogError {
    CatalogError(e.to_string())
}
pub(crate) fn identity(prefix: &str, value: &impl Serialize) -> Result<String> {
    let value = serde_json::to_value(value).map_err(err)?;
    let bytes = restricted_jcs(&value).map_err(err)?;
    Ok(format!("{prefix}:sha256:{:x}", Sha256::digest(bytes)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TradeState {
    Tradable,
    Halted,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HaltScope {
    OpeningOnly,
    FullSession,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaltEvidence {
    pub scope: HaltScope,
    pub evidence: Evidence,
}
/// Per-lifecycle PIT rule selection, including documented first-day applicability.
/// Only nearest-tick half-up rounding is currently supported; no implicit IPO exemption.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitRule {
    pub version: String,
    pub effective_from: NaiveDate,
    pub effective_until: Option<NaiveDate>,
    pub rate_bps: u32,
    pub tick: Price,
    pub rounding: String,
    pub applies_on_first_session: bool,
    pub evidence: Evidence,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferencePrice {
    pub adjusted_previous_close: Price,
    pub evidence: Evidence,
}
/// Explicit adjudication of frozen official attachments, not title keyword inference.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpeningEvidence {
    pub instrument_id: InstrumentId,
    pub session_date: NaiveDate,
    pub announcement_coverage: Option<Evidence>,
    pub source_independence: Option<Evidence>,
    pub halt: Option<HaltEvidence>,
    pub limit_rule: Option<LimitRule>,
    pub reference: Option<ReferencePrice>,
    /// Historical availability of both vendor daily inputs. None stays unknown.
    pub market_available_at: Option<TimestampNs>,
}
impl OpeningEvidence {
    fn documents(&self) -> Vec<&Evidence> {
        let mut out: Vec<_> = self
            .announcement_coverage
            .iter()
            .chain(self.source_independence.iter())
            .collect();
        out.extend(self.halt.iter().map(|x| &x.evidence));
        out.extend(self.limit_rule.iter().map(|x| &x.evidence));
        out.extend(self.reference.iter().map(|x| &x.evidence));
        out
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketEvidence {
    pub rule_version: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    /// These are coverage gaps, not individual UNKNOWN instrument cells.
    pub coverage_gaps: BTreeSet<String>,
    pub openings: Vec<OpeningEvidence>,
}
#[derive(Debug, Clone, Serialize)]
pub struct MarketCell {
    pub instrument_id: InstrumentId,
    pub session_date: NaiveDate,
    pub state: TradeState,
    pub can_buy: bool,
    pub can_sell: bool,
    pub reasons: BTreeSet<String>,
    pub available_at: Option<TimestampNs>,
    pub close: Option<Price>,
    pub amount: Option<Notional>,
    pub upper_limit: Option<Price>,
    pub lower_limit: Option<Price>,
    pub sources: BTreeSet<String>,
    pub rule_version: String,
    pub limit_rule_version: Option<String>,
}
impl MarketCell {
    /// Offline execution evidence; no assertion that this was knowable at open.
    pub fn permits(&self, buy: bool) -> bool {
        self.state == TradeState::Tradable
            && self.available_at.is_some()
            && if buy { self.can_buy } else { self.can_sell }
    }
    pub fn known_by(&self, timestamp: TimestampNs) -> bool {
        self.available_at.is_some_and(|at| at <= timestamp)
    }
}
#[derive(Debug, Clone)]
pub struct DerivedMarket {
    sessions: Vec<Session>,
    cells: BTreeMap<(InstrumentId, NaiveDate), MarketCell>,
    version: String,
    dsv: String,
    catalog_version: String,
    evidence: MarketEvidence,
}
impl DerivedMarket {
    /// Rebuild #174 originals and verify every normalized logical hash. Caller-supplied
    /// arbitrary Bars/crosscheck claims cannot become execution evidence.
    pub fn from_dataset(
        root: &Path,
        dsv: &str,
        catalog: &HistoricalCatalog,
        evidence: MarketEvidence,
    ) -> Result<Self> {
        let manifest = read_manifest(root, dsv).map_err(err)?;
        if manifest.core.normalizer.id != "szse-etf-frozen" {
            return Err(err("requires frozen SZSE ETF Dataset"));
        }
        let registry = NormalizerRegistry::with_builtins();
        let normalizer = registry
            .get(
                &manifest.core.normalizer.id,
                &manifest.core.normalizer.version,
            )
            .ok_or_else(|| err("unknown normalizer"))?;
        if normalizer.config_sha256() != manifest.core.normalizer.config_sha256 {
            return Err(err("normalizer config mismatch"));
        }
        let raw = RawStore::open(root).map_err(err)?;
        let inputs = manifest
            .core
            .inputs
            .iter()
            .map(|x| Ok(RawInput::from_bytes(raw.get(&x.raw_sha256).map_err(err)?)))
            .collect::<Result<Vec<_>>>()?;
        let output = normalizer
            .normalize(&inputs)
            .map_err(|e| err(format!("normalization failed: {e:?}")))?;
        let t = &output.tables;
        let tables = [
            ("instruments", &t.instruments, vec!["instrument_id"]),
            ("sessions", &t.sessions, vec!["venue_id", "session_date"]),
            ("bars", &t.bars, vec!["instrument_id", "session_date"]),
            (
                "source_crosscheck",
                t.source_crosscheck
                    .as_ref()
                    .ok_or_else(|| err("missing crosscheck"))?,
                vec!["instrument_id", "session_date"],
            ),
        ];
        for (name, batch, keys) in tables {
            let expected = manifest
                .core
                .tables
                .iter()
                .find(|t| t.table == name)
                .ok_or_else(|| err("missing normalized table"))?;
            if logical_hash(name, &batch.schema(), std::slice::from_ref(batch), &keys)
                .map_err(err)?
                != expected.logical_hash
            {
                return Err(err("rebuilt table differs from Dataset Version"));
            }
        }
        Self::build(dsv, catalog, evidence, &raw, &output.tables)
    }
    fn build(
        dsv: &str,
        catalog: &HistoricalCatalog,
        mut evidence: MarketEvidence,
        raw: &RawStore,
        tables: &NormalizedTables,
    ) -> Result<Self> {
        if evidence.rule_version.trim().is_empty()
            || evidence.start > evidence.end
            || !evidence.coverage_gaps.is_empty()
            || !catalog.coverage_gaps().is_empty()
        {
            return Err(err("incomplete/invalid market or Catalog coverage"));
        }
        if evidence.start < catalog.input().coverage.start
            || evidence.end > catalog.input().coverage.end
        {
            return Err(err("market window outside Catalog"));
        }
        let sessions: Vec<_> = catalog
            .input()
            .sessions
            .iter()
            .filter(|s| s.session_date() >= evidence.start && s.session_date() <= evidence.end)
            .cloned()
            .collect();
        if sessions.is_empty() {
            return Err(err("empty Session window"));
        }
        let dates: BTreeSet<_> = sessions.iter().map(Session::session_date).collect();
        let source_sessions =
            prajna_data::sessions_from_record_batch(&tables.sessions).map_err(err)?;
        for session in &sessions {
            if !source_sessions.contains(session) {
                return Err(err("Dataset calendar differs from Catalog"));
            }
        }

        let mut openings = BTreeMap::new();
        for opening in &evidence.openings {
            if !dates.contains(&opening.session_date)
                || !catalog
                    .input()
                    .lifecycles
                    .iter()
                    .any(|l| l.instrument_id().as_ref() == Ok(&opening.instrument_id))
            {
                return Err(err("opening outside lifecycle/calendar"));
            }
            for doc in opening.documents() {
                doc.validate()?;
                raw.get(&doc.raw_sha256).map_err(err)?;
                doc.available_at(catalog.input())?;
            }
            if openings
                .insert(
                    (opening.instrument_id.clone(), opening.session_date),
                    opening,
                )
                .is_some()
            {
                return Err(err("duplicate opening adjudication"));
            }
        }
        let mut bars = BTreeMap::new();
        for bar in bars_from_record_batch(&tables.bars, &catalog.input().sessions).map_err(err)? {
            if !dates.contains(&bar.session_date()) {
                continue;
            }
            // Unattributed code-reuse rows fail closed rather than joining nearest IDs.
            let id = catalog.resolve_row(bar.instrument_id().symbol(), bar.session_date())?;
            bars.insert((id, bar.session_date()), bar);
        }
        let checks = crosschecks(
            tables
                .source_crosscheck
                .as_ref()
                .ok_or_else(|| err("missing crosscheck table"))?,
            catalog,
            &dates,
        )?;
        for session in &sessions {
            if !checks
                .iter()
                .any(|((_, d), c)| *d == session.session_date() && c.has_primary)
            {
                return Err(err(format!(
                    "whole-market missing Session {}",
                    session.session_date()
                )));
            }
        }
        let mut cells = BTreeMap::new();
        for life in &catalog.input().lifecycles {
            let id = life.instrument_id()?;
            for session in &sessions {
                let date = session.session_date();
                if date < life.listing.effective_date
                    || life
                        .termination
                        .as_ref()
                        .is_some_and(|t| date >= t.effective_date)
                {
                    continue;
                }
                let key = (id.clone(), date);
                let cell = derive(
                    (&id, life.listing.effective_date),
                    session,
                    bars.get(&key),
                    checks.get(&key),
                    openings.get(&key).copied(),
                    catalog,
                    &evidence.rule_version,
                )?;
                cells.insert(key, cell);
            }
        }
        evidence
            .openings
            .sort_by_key(|o| (o.instrument_id.clone(), o.session_date));
        let catalog_version = catalog.version()?;
        let version = identity(
            "derived-market",
            &(
                "szse-derived-open@1",
                dsv,
                &catalog_version,
                &evidence,
                cells.values().collect::<Vec<_>>(),
            ),
        )?;
        Ok(Self {
            sessions,
            cells,
            version,
            dsv: dsv.into(),
            catalog_version,
            evidence,
        })
    }
    /// ADR 0005 preflight for an Engine's already computed required sell legs.
    /// Any blocked sale defers the entire target, approving no buys or sells.
    /// This is offline evidence; #177 must preserve pending targets and audit traces.
    pub fn rebalance_gate(
        &self,
        date: NaiveDate,
        required_sells: &[InstrumentId],
        requested_buys: &[InstrumentId],
    ) -> Result<RebalanceGate> {
        if !self.sessions.iter().any(|s| s.session_date() == date) {
            return Err(err("attempt outside covered Sessions"));
        }
        let blocked_sells: BTreeSet<_> = required_sells
            .iter()
            .filter(|id| {
                !self
                    .cells
                    .get(&((*id).clone(), date))
                    .is_some_and(|c| c.permits(false))
            })
            .cloned()
            .collect();
        if !blocked_sells.is_empty() {
            return Ok(RebalanceGate::Deferred { blocked_sells });
        }
        let allowed_buys = requested_buys
            .iter()
            .filter(|id| {
                self.cells
                    .get(&((*id).clone(), date))
                    .is_some_and(|c| c.permits(true))
            })
            .cloned()
            .collect();
        Ok(RebalanceGate::Permitted { allowed_buys })
    }
    pub fn evidence(&self) -> &MarketEvidence {
        &self.evidence
    }
    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }
    pub fn cells(&self) -> &BTreeMap<(InstrumentId, NaiveDate), MarketCell> {
        &self.cells
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn dsv(&self) -> &str {
        &self.dsv
    }
    pub fn catalog_version(&self) -> &str {
        &self.catalog_version
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RebalanceGate {
    Deferred {
        blocked_sells: BTreeSet<InstrumentId>,
    },
    Permitted {
        allowed_buys: BTreeSet<InstrumentId>,
    },
}
struct Crosscheck {
    has_primary: bool,
    matched: bool,
    sources: BTreeSet<String>,
}
fn crosschecks(
    batch: &RecordBatch,
    catalog: &HistoricalCatalog,
    dates: &BTreeSet<NaiveDate>,
) -> Result<BTreeMap<(InstrumentId, NaiveDate), Crosscheck>> {
    let strings = |name: &str| -> Result<&StringArray> {
        batch
            .column_by_name(name)
            .and_then(|a| a.as_any().downcast_ref())
            .ok_or_else(|| err("invalid crosscheck string column"))
    };
    let ids = strings("instrument_id")?;
    let statuses = strings("status")?;
    let fields = strings("mismatch_fields")?;
    let primary = strings("primary_raw_sha256")?;
    let secondary = strings("secondary_raw_sha256")?;
    let days = batch
        .column_by_name("session_date")
        .and_then(|a| a.as_any().downcast_ref::<Date32Array>())
        .ok_or_else(|| err("invalid crosscheck dates"))?;
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
    let mut out = BTreeMap::new();
    for i in 0..batch.num_rows() {
        let date = epoch
            .checked_add_signed(chrono::Duration::days(i64::from(days.value(i))))
            .ok_or_else(|| err("invalid date"))?;
        if !dates.contains(&date) {
            continue;
        }
        let native: InstrumentId = ids.value(i).parse().map_err(err)?;
        let id = catalog.resolve_row(native.symbol(), date)?;
        let sources = [primary, secondary]
            .into_iter()
            .filter(|a| !a.is_null(i))
            .map(|a| a.value(i).to_owned())
            .collect();
        let matched = statuses.value(i) == "match"
            && fields.value(i).is_empty()
            && !primary.is_null(i)
            && !secondary.is_null(i)
            && primary.value(i) != secondary.value(i);
        if out
            .insert(
                (id, date),
                Crosscheck {
                    has_primary: !primary.is_null(i),
                    matched,
                    sources,
                },
            )
            .is_some()
        {
            return Err(err("duplicate crosscheck"));
        }
    }
    Ok(out)
}
fn derive(
    identity: (&InstrumentId, NaiveDate),
    session: &Session,
    bar: Option<&Bar>,
    check: Option<&Crosscheck>,
    opening: Option<&OpeningEvidence>,
    catalog: &HistoricalCatalog,
    version: &str,
) -> Result<MarketCell> {
    let (id, listing) = identity;
    let mut cell = MarketCell {
        instrument_id: id.clone(),
        session_date: session.session_date(),
        state: TradeState::Unknown,
        can_buy: false,
        can_sell: false,
        reasons: BTreeSet::new(),
        available_at: None,
        close: bar.map(Bar::close),
        amount: bar.and_then(Bar::amount),
        upper_limit: None,
        lower_limit: None,
        sources: check.map(|c| c.sources.clone()).unwrap_or_default(),
        rule_version: version.into(),
        limit_rule_version: opening.and_then(|o| o.limit_rule.as_ref().map(|r| r.version.clone())),
    };
    let Some(o) = opening else {
        cell.reasons.insert("missing_opening_evidence".into());
        return Ok(cell);
    };
    let mut times = Vec::new();
    for doc in o.documents() {
        cell.sources.insert(doc.raw_sha256.clone());
        times.push(doc.available_at(catalog.input())?);
    }
    cell.available_at = o.market_available_at.map(|at| {
        times
            .iter()
            .copied()
            .fold(at.max(session.ts_close()), TimestampNs::max)
    });
    if let Some(halt) = &o.halt {
        if halt.scope == HaltScope::FullSession && bar.is_some_and(|b| b.volume() > Quantity::ZERO)
        {
            cell.reasons.insert("halt_trade_conflict".into());
        } else {
            cell.state = TradeState::Halted;
            cell.reasons.insert("official_opening_halt".into());
            cell.available_at = Some(halt.evidence.available_at(catalog.input())?);
            // HALTED amount counts as zero, including an afternoon resumption.
            cell.amount = Some(Notional::ZERO);
        }
        return Ok(cell);
    }
    if o.announcement_coverage.is_none() || o.source_independence.is_none() {
        cell.reasons
            .insert("missing_coverage_or_independence".into());
    }
    if !check.is_some_and(|c| c.matched) {
        cell.reasons.insert("sources_missing_or_conflicting".into());
    }
    if !bar.is_some_and(|b| b.volume() > Quantity::ZERO) {
        cell.reasons.insert("missing_positive_trade".into());
    }
    let limits = o
        .limit_rule
        .as_ref()
        .zip(o.reference.as_ref())
        .and_then(|(r, p)| {
            if r.version.trim().is_empty()
                || r.effective_from > session.session_date()
                || r.effective_until
                    .is_some_and(|d| d <= session.session_date())
                || r.rate_bps == 0
                || r.rate_bps >= 10000
                || r.tick <= Price::ZERO
                || r.rounding != "nearest_tick_half_up"
                || (listing == session.session_date() && !r.applies_on_first_session)
            {
                return None;
            }
            Some((
                limit(p.adjusted_previous_close, r.tick, 10000 + r.rate_bps)?,
                limit(p.adjusted_previous_close, r.tick, 10000 - r.rate_bps)?,
            ))
        });
    if let Some((upper, lower)) = limits {
        cell.upper_limit = Some(upper);
        cell.lower_limit = Some(lower);
        if let Some(b) = bar {
            if [b.open(), b.high(), b.low(), b.close()].iter().any(|p| {
                !p.is_aligned_to(o.limit_rule.as_ref().expect("limits require rule").tick)
                    .unwrap_or(false)
            }) || b.high() > upper
                || b.low() < lower
            {
                cell.reasons.insert("prices_outside_pit_rule".into());
            }
            cell.can_buy = b.open() < upper;
            cell.can_sell = b.open() > lower;
        }
    } else {
        cell.reasons
            .insert("unknown_price_limit_rule_or_reference".into());
    }
    if cell.reasons.is_empty() {
        cell.state = TradeState::Tradable;
        if !cell.can_buy {
            cell.reasons.insert("opening_upper_limit".into());
        }
        if !cell.can_sell {
            cell.reasons.insert("opening_lower_limit".into());
        }
    } else {
        cell.can_buy = false;
        cell.can_sell = false;
    }
    Ok(cell)
}
fn limit(reference: Price, tick: Price, bps: u32) -> Option<Price> {
    if reference <= Price::ZERO || tick <= Price::ZERO {
        return None;
    }
    let numerator = reference.mantissa().checked_mul(i128::from(bps))?;
    let denominator = tick.mantissa().checked_mul(10000)?;
    let units = numerator
        .checked_add(denominator / 2)?
        .checked_div(denominator)?;
    Price::from_mantissa(units.checked_mul(tick.mantissa())?)
        .ok()
        .filter(|p| *p > Price::ZERO)
}

#[cfg(test)]
#[path = "etf_market_tests.rs"]
mod tests;
