//! MVP-6 offline opening evidence. Daily crosschecks are never backdated to open.
//! The existing D7/execution path is unchanged; #177 consumes directional gates.
pub mod limit_rules;

use crate::pit_universe::{CatalogError, Evidence, HistoricalCatalog};
use arrow_array::{Array, Date32Array, RecordBatch, StringArray};
use chrono::NaiveDate;
use prajna_data::{
    BarSourceRow, NormalizedTables, NormalizerRegistry, RawInput, RawStore,
    bar_sources_from_record_batch, bars_from_record_batch, logical_hash, read_manifest,
    restricted_jcs,
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ExecutionStatusVersion {
    #[serde(rename = "szse-derived-open@2")]
    V2,
}
/// Evidence carried by a limit rule. `szse-derived-open@1` keeps the Catalog
/// `Evidence` wire contract (Raw hash plus announcement/locator/publication)
/// and validates the Raw bytes on read. `szse-derived-open@2` uses a public
/// document citation because the bundled rule table cannot contain Raw hashes.
/// `DerivedMarket::build` enforces the variant selected by
/// `MarketEvidence.semantic_version`, so v1 replay never silently drops its
/// primary evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LimitRuleEvidence {
    /// `szse-derived-open@1`: Raw-backed Catalog evidence.
    Raw(Evidence),
    /// `szse-derived-open@2`: public document citation; Raw stays local.
    Citation(limit_rules::CategorySourceEvidence),
}
impl LimitRuleEvidence {
    fn as_raw(&self) -> Option<&Evidence> {
        match self {
            Self::Raw(evidence) => Some(evidence),
            Self::Citation(_) => None,
        }
    }
    fn as_citation(&self) -> Option<&limit_rules::CategorySourceEvidence> {
        match self {
            Self::Citation(evidence) => Some(evidence),
            Self::Raw(_) => None,
        }
    }
    #[cfg(test)]
    fn as_citation_mut(&mut self) -> Option<&mut limit_rules::CategorySourceEvidence> {
        match self {
            Self::Citation(evidence) => Some(evidence),
            Self::Raw(_) => None,
        }
    }
    fn validate_for_version(&self, v2: bool, field: &str) -> Result<()> {
        match (v2, self) {
            (false, Self::Raw(evidence)) => evidence.validate(),
            (true, Self::Citation(_)) => Ok(()),
            (false, Self::Citation(_)) => Err(err(format!(
                "{field} citation requires szse-derived-open@2"
            ))),
            (true, Self::Raw(_)) => Err(err(format!(
                "{field} must use a public document citation under szse-derived-open@2"
            ))),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfficialLimitRule {
    pub document_number: String,
    /// Explicit PIT fund/rule category, not inferred from the trading symbol.
    pub fund_category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_evidence: Option<FundCategoryEvidence>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FundCategoryEvidence {
    /// Public assignment key (`<six-digit code>.SZSE`), not the lifecycle ID.
    pub instrument_id: InstrumentId,
    pub fund_category: String,
    pub assignment_identity: String,
    /// Catalog lifecycle this assignment bound to for the affected session.
    /// v2 rule resolution always records it; old v1 evidence may omit it, and
    /// v2 `supports` rejects a rule without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<limit_rules::LifecycleBinding>,
    pub document_number: String,
    pub effective_from: NaiveDate,
    pub effective_until: Option<NaiveDate>,
    pub evidence: LimitRuleEvidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supporting_sources: Vec<limit_rules::CategorySourceEvidence>,
    pub evidence_level: limit_rules::EvidenceLevel,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstSessionReference {
    PreviousTradingDayNetAssetValue,
    MostRecentlyPublishedNetAssetValue,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_session_reference: Option<FirstSessionReference>,
    pub evidence: LimitRuleEvidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supporting_evidence: Vec<Evidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rule_evidence: Vec<limit_rules::RuleSourceEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official_rule: Option<OfficialLimitRule>,
}
impl LimitRule {
    fn documents(&self) -> Vec<&Evidence> {
        let mut documents: Vec<&Evidence> = self.evidence.as_raw().into_iter().collect();
        documents.extend(self.supporting_evidence.iter());
        if let Some(category_evidence) = self
            .official_rule
            .as_ref()
            .and_then(|rule| rule.category_evidence.as_ref())
        {
            documents.extend(category_evidence.evidence.as_raw());
        }
        documents
    }

    fn category_sources(&self) -> Vec<&limit_rules::CategorySourceEvidence> {
        self.official_rule
            .as_ref()
            .and_then(|rule| rule.category_evidence.as_ref())
            .map(|category| {
                category
                    .evidence
                    .as_citation()
                    .into_iter()
                    .chain(category.supporting_sources.iter())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// v1 replay requires Raw-backed documents with integrity checks; v2
    /// requires public document citations. Mismatched variants are rejected
    /// instead of being treated as absent evidence.
    fn validate_wire_evidence(&self, v2: bool) -> Result<()> {
        self.evidence
            .validate_for_version(v2, "limit-rule primary evidence")?;
        if let Some(category_evidence) = self
            .official_rule
            .as_ref()
            .and_then(|rule| rule.category_evidence.as_ref())
        {
            category_evidence
                .evidence
                .validate_for_version(v2, "fund-category evidence")?;
        }
        Ok(())
    }
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
        out.extend(self.limit_rule.iter().flat_map(LimitRule::documents));
        out.extend(self.reference.iter().map(|x| &x.evidence));
        out
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_version: Option<ExecutionStatusVersion>,
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
    catalog: HistoricalCatalog,
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
        let raw = RawStore::open(root).map_err(err)?;
        let inputs = manifest
            .core
            .inputs
            .iter()
            .map(|input| {
                Ok(RawInput {
                    raw_sha256: input.raw_sha256.clone(),
                    bytes: raw.get(&input.raw_sha256).map_err(err)?,
                    source_records: raw.sources(&input.raw_sha256).map_err(err)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let registry = if manifest.core.normalizer.version == "5" {
            NormalizerRegistry::with_sina_adjustment_v5(catalog.sina_factor_lifecycles())
                .map_err(err)?
        } else if manifest.core.normalizer.version == "7" {
            let market_dsvs = inputs
                .iter()
                .flat_map(|input| &input.source_records)
                .filter(|source| source.source_id == "cninfo_corporate_action_facts")
                .filter_map(|source| source.request["market_dsv"].as_str())
                .collect::<std::collections::BTreeSet<_>>();
            if market_dsvs.len() != 1 {
                return Err(err(
                    "corporate-action Dataset must identify exactly one market DSV",
                ));
            }
            NormalizerRegistry::with_corporate_actions_v7(
                (*market_dsvs.first().expect("one market DSV")).to_owned(),
            )
            .map_err(err)?
        } else {
            NormalizerRegistry::with_builtins()
        };
        let normalizer = registry
            .get(
                &manifest.core.normalizer.id,
                &manifest.core.normalizer.version,
            )
            .ok_or_else(|| err("unknown normalizer"))?;
        if normalizer.config_sha256() != manifest.core.normalizer.config_sha256 {
            return Err(err("normalizer config mismatch"));
        }
        let output = normalizer
            .normalize(&inputs)
            .map_err(|e| err(format!("normalization failed: {e:?}")))?;
        let t = &output.tables;
        let mut tables = vec![
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
        if evidence.semantic_version.is_some() {
            if !matches!(manifest.core.normalizer.version.as_str(), "3" | "4") {
                return Err(err("v2 execution requires szse-etf-frozen@3 or @4"));
            }
            tables.push((
                "bar_sources",
                t.bar_sources
                    .as_ref()
                    .ok_or_else(|| err("missing bar_sources"))?,
                vec!["instrument_id", "session_date"],
            ));
        }
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
        let v2 = evidence.semantic_version.is_some();
        let limit_rule_table = if v2 {
            Some(limit_rules::FundLimitRuleTable::official_v2()?)
        } else {
            None
        };
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
            let rule_table = limit_rule_table.as_ref();
            // A missing binding leaves the rule unsupported (UNKNOWN); the
            // validated Catalog always has one for an opening lifecycle.
            let rule_binding = rule_table.and_then(|_| {
                limit_rules::RuleBinding::from_lifecycles(
                    &catalog.input().lifecycles,
                    &opening.instrument_id,
                )
                .ok()
                .flatten()
            });
            let supported_rule = opening.limit_rule.as_ref().filter(|rule| {
                rule_table
                    .zip(rule_binding.as_ref())
                    .is_some_and(|(table, binding)| {
                        table.supports(rule, binding, opening.session_date)
                    })
            });
            if let Some(rule) = &opening.limit_rule {
                rule.validate_wire_evidence(v2)?;
            }
            let session_open = sessions
                .iter()
                .find(|session| session.session_date() == opening.session_date)
                .expect("opening date was checked against selected sessions")
                .ts_open();
            for doc in opening.documents() {
                doc.validate()?;
                raw.get(&doc.raw_sha256).map_err(err)?;
                let is_rule_document = opening
                    .limit_rule
                    .as_ref()
                    .is_some_and(|rule| rule.documents().contains(&doc));
                if is_rule_document {
                    if let (Some(rule), Some(table), Some(binding)) =
                        (supported_rule, rule_table, rule_binding.as_ref())
                    {
                        table.evidence_available_at(
                            rule,
                            binding,
                            doc,
                            opening.session_date,
                            catalog.input(),
                        )?;
                    }
                } else {
                    doc.available_at(catalog.input())?;
                }
            }
            if let (Some(rule), Some(table), Some(binding)) =
                (supported_rule, rule_table, rule_binding.as_ref())
            {
                for source in rule.category_sources() {
                    // Unknown availability stays a per-cell UNKNOWN; it must
                    // not abort the other instruments or the coverage report.
                    let _ = table.category_evidence_available_at(
                        rule,
                        binding,
                        source,
                        opening.session_date,
                        session_open,
                        catalog.input(),
                    )?;
                }
                for source in &rule.rule_evidence {
                    table.rule_evidence_available_at(
                        rule,
                        binding,
                        source,
                        opening.session_date,
                        session_open,
                        catalog.input(),
                    )?;
                }
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
            if catalog
                .is_identity_availability_gap(bar.instrument_id().symbol(), bar.session_date())
            {
                continue;
            }
            // Unattributed code-reuse rows fail closed rather than joining nearest IDs.
            let id = catalog.resolve_row(bar.instrument_id().symbol(), bar.session_date())?;
            bars.insert((id, bar.session_date()), bar);
        }
        let mut sources = BTreeMap::new();
        if v2 {
            for source in bar_sources_from_record_batch(
                tables
                    .bar_sources
                    .as_ref()
                    .ok_or_else(|| err("missing bar_sources"))?,
            )
            .map_err(err)?
            {
                if !dates.contains(&source.session_date) {
                    continue;
                }
                raw.get(&source.source_raw_sha256).map_err(err)?;
                if catalog.is_identity_availability_gap(
                    source.instrument_id.symbol(),
                    source.session_date,
                ) {
                    continue;
                }
                let id = catalog.resolve_row(source.instrument_id.symbol(), source.session_date)?;
                if sources.insert((id, source.session_date), source).is_some() {
                    return Err(err("duplicate Bar source"));
                }
            }
            if sources.keys().ne(bars.keys()) {
                return Err(err("Bar source keys differ from Bars"));
            }
        }
        let checks = crosschecks(
            tables
                .source_crosscheck
                .as_ref()
                .ok_or_else(|| err("missing crosscheck table"))?,
            catalog,
            &dates,
        )?;
        for session in sessions.iter().filter(|_| !v2) {
            if !checks
                .iter()
                .any(|((_, d), c)| *d == session.session_date() && c.has_primary)
                || !checks
                    .iter()
                    .any(|((_, d), c)| *d == session.session_date() && c.has_secondary)
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
                if v2
                    && (life.identity_start.is_some_and(|start| date < start)
                        || life
                            .identity_available_from
                            .is_some_and(|start| date < start)
                        || life.identity_end_exclusive.is_some_and(|end| date >= end))
                {
                    continue;
                }
                let key = (id.clone(), date);
                let cell = if v2 {
                    derive_v2(
                        (&id, life.listing.effective_date),
                        session,
                        bars.get(&key),
                        sources.get(&key),
                        openings.get(&key).copied(),
                        V2DerivationContext {
                            catalog,
                            limit_rule_table: limit_rule_table
                                .as_ref()
                                .ok_or_else(|| err("missing v2 limit-rule table"))?,
                            version: &evidence.rule_version,
                        },
                    )?
                } else {
                    derive(
                        (&id, life.listing.effective_date),
                        session,
                        bars.get(&key),
                        checks.get(&key),
                        openings.get(&key).copied(),
                        catalog,
                        &evidence.rule_version,
                    )?
                };
                cells.insert(key, cell);
            }
        }
        evidence
            .openings
            .sort_by_key(|o| (o.instrument_id.clone(), o.session_date));
        let catalog_version = catalog.version()?;
        let version = if let Some(table) = &limit_rule_table {
            identity(
                "derived-market",
                &(
                    "szse-derived-open@2",
                    dsv,
                    &catalog_version,
                    &table.version,
                    &evidence,
                    cells.values().collect::<Vec<_>>(),
                ),
            )?
        } else {
            identity(
                "derived-market",
                &(
                    "szse-derived-open@1",
                    dsv,
                    &catalog_version,
                    &evidence,
                    cells.values().collect::<Vec<_>>(),
                ),
            )?
        };
        Ok(Self {
            sessions,
            cells,
            version,
            dsv: dsv.into(),
            catalog_version,
            evidence,
            catalog: catalog.clone(),
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
    pub(crate) fn resolve_native_row(
        &self,
        native_symbol: &str,
        date: NaiveDate,
    ) -> Result<InstrumentId> {
        self.catalog.resolve_row(native_symbol, date)
    }
    pub(crate) fn resolve_settlement_action(
        &self,
        native_symbol: &str,
        date: NaiveDate,
    ) -> Result<InstrumentId> {
        self.catalog.resolve_settlement_action(native_symbol, date)
    }
    pub(crate) fn termination_date(&self, id: &InstrumentId) -> Option<NaiveDate> {
        self.catalog.termination_date(id)
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
    has_secondary: bool,
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
        if catalog.is_identity_availability_gap(native.symbol(), date) {
            continue;
        }
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
                    has_secondary: !secondary.is_null(i),
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
    if check.is_some_and(|c| c.has_primary && c.has_secondary && !c.matched) {
        cell.reasons.insert("sources_conflicting".into());
        return Ok(cell);
    }
    if let Some(halt) = &o.halt {
        if halt.scope == HaltScope::FullSession && bar.is_some_and(|b| b.volume() > Quantity::ZERO)
        {
            cell.reasons.insert("halt_trade_conflict".into());
        } else {
            cell.state = TradeState::Halted;
            cell.reasons.insert("official_opening_halt".into());
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
    if cell.available_at.is_none() {
        cell.can_buy = false;
        cell.can_sell = false;
        cell.reasons.insert("unknown_availability".into());
    }
    Ok(cell)
}
fn limit(reference: Price, tick: Price, bps: u32) -> Option<Price> {
    if reference <= Price::ZERO || tick <= Price::ZERO || bps == 10000 {
        return None;
    }
    let numerator = reference.mantissa().checked_mul(i128::from(bps))?;
    let denominator = tick.mantissa().checked_mul(10000)?;
    let units = numerator
        .checked_add(denominator / 2)?
        .checked_div(denominator)?;
    let rounded = units.checked_mul(tick.mantissa())?;
    let direction = if bps > 10000 { 1 } else { -1 };
    let mut price = rounded.max(tick.mantissa());
    if price.checked_sub(reference.mantissa())?.checked_abs()? < tick.mantissa() {
        price = reference
            .mantissa()
            .checked_add(tick.mantissa().checked_mul(direction)?)?
            .max(tick.mantissa());
    }
    Price::from_mantissa(price)
        .ok()
        .filter(|p| *p > Price::ZERO)
}
struct V2DerivationContext<'a> {
    catalog: &'a HistoricalCatalog,
    limit_rule_table: &'a limit_rules::FundLimitRuleTable,
    version: &'a str,
}

fn derive_v2(
    identity: (&InstrumentId, NaiveDate),
    session: &Session,
    bar: Option<&Bar>,
    source: Option<&BarSourceRow>,
    opening: Option<&OpeningEvidence>,
    context: V2DerivationContext<'_>,
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
        sources: source.iter().map(|s| s.source_raw_sha256.clone()).collect(),
        rule_version: context.version.into(),
        limit_rule_version: opening.and_then(|o| o.limit_rule.as_ref().map(|r| r.version.clone())),
    };
    let Some(o) = opening else {
        cell.reasons.insert("missing_opening_evidence".into());
        return Ok(cell);
    };
    // A missing binding leaves the rule unsupported (UNKNOWN) instead of
    // aborting the other cells; a validated Catalog always has one.
    let binding =
        limit_rules::RuleBinding::from_lifecycles(&context.catalog.input().lifecycles, id)
            .ok()
            .flatten();
    let mut times = Vec::new();
    for doc in o
        .announcement_coverage
        .iter()
        .chain(o.halt.iter().map(|h| &h.evidence))
    {
        cell.sources.insert(doc.raw_sha256.clone());
        times.push(doc.available_at(context.catalog.input())?);
    }
    let supported_rule = o.limit_rule.as_ref().filter(|rule| {
        binding.as_ref().is_some_and(|binding| {
            context
                .limit_rule_table
                .supports(rule, binding, session.session_date())
        })
    });
    let mut rule_known_before_open = supported_rule.is_some();
    let mut category_evidence_availability_unknown = false;
    if let Some(rule) = &o.limit_rule {
        for doc in rule.documents() {
            cell.sources.insert(doc.raw_sha256.clone());
            if let (true, Some(binding)) = (supported_rule.is_some(), binding.as_ref()) {
                let at = context.limit_rule_table.evidence_available_at(
                    rule,
                    binding,
                    doc,
                    session.session_date(),
                    context.catalog.input(),
                )?;
                rule_known_before_open &= at < session.ts_open();
                times.push(at);
            }
        }
        if let (true, Some(binding)) = (supported_rule.is_some(), binding.as_ref()) {
            for source in rule.category_sources() {
                cell.sources
                    .insert(format!("category:{}", source.announcement_id));
                match context.limit_rule_table.category_evidence_available_at(
                    rule,
                    binding,
                    source,
                    session.session_date(),
                    session.ts_open(),
                    context.catalog.input(),
                )? {
                    Some(at) => {
                        rule_known_before_open &= at < session.ts_open();
                        times.push(at);
                    }
                    // Availability cannot be proven inside the frozen calendar.
                    // Keep the cell UNKNOWN; never fabricate a timestamp.
                    None => {
                        category_evidence_availability_unknown = true;
                        rule_known_before_open = false;
                    }
                }
            }
            for source in &rule.rule_evidence {
                let at = context.limit_rule_table.rule_evidence_available_at(
                    rule,
                    binding,
                    source,
                    session.session_date(),
                    session.ts_open(),
                    context.catalog.input(),
                )?;
                rule_known_before_open &= at < session.ts_open();
                times.push(at);
            }
        }
    }
    cell.available_at = o.market_available_at.map(|at| {
        times
            .iter()
            .copied()
            .fold(at.max(session.ts_close()), TimestampNs::max)
    });
    // The normalized TDX field is the only reference authority. A caller's
    // legacy ReferencePrice cannot repair or override a missing source field.
    let reference = source
        .filter(|s| s.source == "tdx")
        .and_then(|s| s.reference_price);
    let rule = o.limit_rule.as_ref().filter(|r| {
        rule_known_before_open
            && binding.as_ref().is_some_and(|binding| {
                context
                    .limit_rule_table
                    .supports(r, binding, session.session_date())
            })
            && (listing != session.session_date() || r.applies_on_first_session)
    });
    let limits = rule.zip(reference).and_then(|(r, p)| {
        Some((
            limit(p, r.tick, 10000 + r.rate_bps)?,
            limit(p, r.tick, 10000 - r.rate_bps)?,
        ))
    });
    if let Some((upper, lower)) = limits {
        cell.upper_limit = Some(upper);
        cell.lower_limit = Some(lower);
        if let Some(b) = bar {
            let tick = rule.expect("limits require rule").tick;
            if [b.open(), b.high(), b.low(), b.close()]
                .iter()
                .any(|p| !p.is_aligned_to(tick).unwrap_or(false) || *p > upper || *p < lower)
            {
                return Err(err(format!(
                    "prices_outside_pit_rule: {id} {}",
                    session.session_date()
                )));
            }
            cell.can_buy = b.open() < upper;
            cell.can_sell = b.open() > lower;
        }
    }
    if let Some(halt) = &o.halt {
        cell.can_buy = false;
        cell.can_sell = false;
        let at = halt.evidence.available_at(context.catalog.input())?;
        cell.available_at = Some(at.max(session.ts_close()));
        if at >= session.ts_open() {
            cell.reasons.insert("halt_not_known_before_open".into());
        } else if halt.scope == HaltScope::FullSession
            && bar.is_some_and(|b| b.volume() > Quantity::ZERO)
        {
            cell.reasons.insert("halt_trade_conflict".into());
        } else {
            cell.state = TradeState::Halted;
            cell.amount = Some(Notional::ZERO);
            cell.reasons.insert("official_opening_halt".into());
        }
        return Ok(cell);
    }
    if o.announcement_coverage.is_none() {
        cell.reasons.insert("missing_announcement_coverage".into());
    }
    if !bar.is_some_and(|b| b.volume() > Quantity::ZERO) {
        cell.reasons.insert("missing_positive_trade".into());
    }
    if cell.available_at.is_none() {
        cell.reasons.insert("unknown_availability".into());
    }
    if limits.is_none() {
        cell.reasons
            .insert("unknown_price_limit_rule_or_reference".into());
    }
    if category_evidence_availability_unknown
        && o.limit_rule
            .as_ref()
            .is_some_and(|rule| listing != session.session_date() || rule.applies_on_first_session)
    {
        cell.reasons
            .insert("unknown_category_evidence_availability".into());
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

#[cfg(test)]
mod limit_price_tests {
    use super::*;

    #[test]
    fn official_minimum_tick_gap_is_applied_after_rounding() {
        let tick = Price::parse("0.001").unwrap();
        assert_eq!(
            limit(Price::parse("0.002").unwrap(), tick, 11000),
            Some(Price::parse("0.003").unwrap())
        );
        assert_eq!(
            limit(Price::parse("0.002").unwrap(), tick, 9000),
            Some(Price::parse("0.001").unwrap())
        );
    }

    #[test]
    fn limit_prices_round_to_ticks_and_do_not_fall_below_one_tick() {
        let tick = Price::parse("0.001").unwrap();
        assert_eq!(
            limit(Price::parse("1").unwrap(), tick, 11000),
            Some(Price::parse("1.1").unwrap())
        );
        assert_eq!(
            limit(Price::parse("1").unwrap(), tick, 9000),
            Some(Price::parse("0.9").unwrap())
        );
        assert_eq!(
            limit(Price::parse("0.0005").unwrap(), tick, 9000),
            Some(tick)
        );
    }
}

#[cfg(test)]
#[path = "etf_market_tests.rs"]
mod tests;
