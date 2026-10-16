//! Independent MVP-6 momentum semantics; legacy close momentum remains unchanged.
//! A price-only 6a adapter is explicit and cannot certify corporate action coverage.
use crate::{
    cache::{
        key::FactorKey,
        store::{CacheError, FactorCache},
    },
    etf_market::{DerivedMarket, MarketCell, TradeState},
    factor::{FactorError, FactorStatus},
};
use chrono::NaiveDate;
use polars::prelude::*;
use prajna_data::{
    CanonicalError, CorporateActionKind, CorporateActionRow, RawStore,
    corporate_actions_from_record_batch, logical_hash, read_manifest, read_parquet,
};
use prajna_domain::{InstrumentId, TimestampNs};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnMode {
    PriceEngineering6a,
    ProviderTotalReturn,
}

/// A provider supplies already adjusted positive index levels plus the latest
/// availability of every corporate-action/price input. Its identity must bind
/// the complete immutable provider data and rules. This is an integration seam,
/// not an implementation or certification of a total-return ledger.
pub trait TotalReturnProvider {
    fn identity(&self) -> Value;
    fn level(&self, id: &InstrumentId, date: NaiveDate) -> Option<IndexPoint>;
}
#[derive(Debug, Clone, Copy)]
pub struct IndexPoint {
    pub level: f64,
    pub available_at: Option<TimestampNs>,
}

/// Explicit 6a assertion: no corporate actions exist in this immutable coverage.
/// A real provider must replace this contract whenever any action is present.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyActionCoverage {
    pub version: String,
    pub instruments: BTreeSet<InstrumentId>,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub source_hashes: BTreeSet<String>,
    pub available_at: Option<TimestampNs>,
}

#[derive(Debug)]
pub struct TotalReturnIndex {
    points: BTreeMap<(InstrumentId, NaiveDate), IndexPoint>,
    identity: Value,
    version: String,
    market_identity: String,
    actions: Vec<CorporateActionRow>,
    integrity: Vec<InstrumentActionIntegrity>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorporateActionRules {
    pub version: String,
    pub unexplained_jump_threshold_bps: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InstrumentActionIntegrity {
    pub instrument_id: InstrumentId,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub observed_actions: usize,
    pub unapplied_action_dates: Vec<NaiveDate>,
    pub unexplained_price_jumps: Vec<NaiveDate>,
    /// Raw-set presence does not prove complete official announcement coverage.
    pub coverage_status: ActionCoverageStatus,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionCoverageStatus {
    ObservedRawSetOnly,
}

impl TotalReturnIndex {
    pub fn build(
        market: &DerivedMarket,
        provider: Option<&dyn TotalReturnProvider>,
        empty_actions: Option<&EmptyActionCoverage>,
        raw: &RawStore,
    ) -> Result<Self, FactorError> {
        if provider.is_some() == empty_actions.is_some() {
            return Err(FactorError::Evaluation(
                "provide either TRI provider or explicit empty-action coverage".into(),
            ));
        }
        if let Some(coverage) = empty_actions {
            for hash in &coverage.source_hashes {
                raw.get(hash)
                    .map_err(|e| FactorError::Evaluation(e.to_string()))?;
            }
            if coverage.version.trim().is_empty()
                || coverage.source_hashes.is_empty()
                || market.cells().keys().any(|(id, date)| {
                    !coverage.instruments.contains(id)
                        || *date < coverage.start
                        || *date > coverage.end
                })
            {
                return Err(FactorError::Evaluation(
                    "incomplete empty-action coverage".into(),
                ));
            }
        }
        let mode = if provider.is_some() {
            ReturnMode::ProviderTotalReturn
        } else {
            ReturnMode::PriceEngineering6a
        };
        let ids: BTreeSet<_> = market.cells().keys().map(|(id, _)| id.clone()).collect();
        let mut points = BTreeMap::new();
        for id in ids {
            let mut previous: Option<IndexPoint> = None;
            for session in market.sessions() {
                let key = (id.clone(), session.session_date());
                let cell = market.cells().get(&key);
                let current = match cell {
                    None
                    | Some(MarketCell {
                        state: TradeState::Unknown,
                        ..
                    }) => None,
                    Some(cell) => {
                        if let Some(provider) = provider {
                            // Provider is required even during a halt, so an action
                            // cannot be silently hidden by carrying yesterday's level.
                            provider
                                .level(&id, session.session_date())
                                .map(|p| IndexPoint {
                                    level: p.level,
                                    available_at: max_known(p.available_at, cell.available_at),
                                })
                        } else if cell.state == TradeState::Halted {
                            previous.map(|p| IndexPoint {
                                level: p.level,
                                available_at: max_known(
                                    max_known(p.available_at, cell.available_at),
                                    empty_actions.and_then(|c| c.available_at),
                                ),
                            })
                        } else {
                            cell.close.map(|p| IndexPoint {
                                level: p.to_f64(),
                                available_at: max_known(
                                    cell.available_at,
                                    empty_actions.and_then(|c| c.available_at),
                                ),
                            })
                        }
                    }
                };
                let current = current.map(|p| IndexPoint {
                    level: p.level,
                    available_at: p.available_at.map(|t| t.max(session.ts_close())),
                });
                if let Some(p) = current {
                    if !p.level.is_finite() || p.level <= 0.0 {
                        return Err(FactorError::Evaluation(
                            "TRI level must be positive and finite".into(),
                        ));
                    }
                    points.insert(key, p);
                }
                // UNKNOWN resets carry; do not search backward through a gap.
                previous = current;
            }
        }
        let identity = json!({"semantic_version":"etf-tri-input@1","mode":match mode {ReturnMode::PriceEngineering6a=>"price_engineering_6a",ReturnMode::ProviderTotalReturn=>"provider_total_return"},"market":market.version(),"provider":provider.map(TotalReturnProvider::identity),"empty_action_coverage":empty_actions.map(|c|json!({"version":c.version,"instruments":c.instruments,"start":c.start,"end":c.end,"sources":c.source_hashes,"available_at":c.available_at}))});
        // Bind actual provider output too, so stale or incorrectly reused provider IDs
        // cannot alias different levels or availability in the persistent cache.
        let mut identity = identity;
        identity["points"] = json!(points.iter().map(|((id,date),p)|json!({"instrument":id,"date":date,"level_bits":format!("{:016x}",p.level.to_bits()),"available_at":p.available_at})).collect::<Vec<_>>());
        // Validate identity at construction, before it can reach persistent cache.
        let canonical = prajna_data::restricted_jcs(&identity)
            .map_err(|e| FactorError::Evaluation(e.to_string()))?;
        let version = format!("etf-tri-input:sha256:{:x}", Sha256::digest(canonical));
        let integrity = empty_actions.map_or_else(Vec::new, |coverage| {
            coverage
                .instruments
                .iter()
                .map(|instrument_id| InstrumentActionIntegrity {
                    instrument_id: instrument_id.clone(),
                    start: coverage.start,
                    end: coverage.end,
                    observed_actions: 0,
                    unapplied_action_dates: Vec::new(),
                    unexplained_price_jumps: Vec::new(),
                    coverage_status: ActionCoverageStatus::ObservedRawSetOnly,
                })
                .collect()
        });
        Ok(Self {
            points,
            identity,
            version,
            market_identity: market.version().into(),
            actions: Vec::new(),
            integrity,
        })
    }

    /// Builds the 6b TRI from versioned, normalized corporate actions. Actions
    /// affect the ex/effective-date return; their availability remains attached
    /// to the resulting point so late notices cannot be used as-of earlier.
    pub fn build_with_actions(
        market: &DerivedMarket,
        actions: &[CorporateActionRow],
        rules: &CorporateActionRules,
        dataset_version: &str,
        raw: &RawStore,
    ) -> Result<Self, FactorError> {
        if dataset_version != market.dsv()
            || rules.version.trim().is_empty()
            || rules.unexplained_jump_threshold_bps == 0
        {
            return Err(FactorError::Evaluation(
                "invalid corporate action Dataset Version or rules".into(),
            ));
        }
        let mut by_date = BTreeMap::new();
        for action in actions {
            raw.get(&action.source_raw_sha256)
                .map_err(|e| FactorError::Evaluation(e.to_string()))?;
            let (date, ratio, cash) = action_terms(action)?;
            let terminal_settlement = action.kind == CorporateActionKind::Liquidation
                && market
                    .termination_date(&action.instrument_id)
                    .is_some_and(|termination| date >= termination)
                && market
                    .sessions()
                    .iter()
                    .any(|session| session.session_date() == date)
                && market
                    .cells()
                    .keys()
                    .any(|(id, market_date)| id == &action.instrument_id && *market_date < date);
            if action.normalization_version.trim().is_empty()
                || (!market
                    .cells()
                    .contains_key(&(action.instrument_id.clone(), date))
                    && !terminal_settlement)
            {
                return Err(FactorError::Evaluation(
                    "corporate action is outside normalized market coverage".into(),
                ));
            }
            if by_date
                .insert((action.instrument_id.clone(), date), (action, ratio, cash))
                .is_some()
            {
                return Err(FactorError::Evaluation(
                    "multiple same-day corporate actions have unresolved ordering".into(),
                ));
            }
        }
        let ids: BTreeSet<_> = market.cells().keys().map(|(id, _)| id.clone()).collect();
        let mut points = BTreeMap::new();
        let mut integrity = Vec::new();
        for id in ids {
            let dates: Vec<_> = market.sessions().iter().map(|s| s.session_date()).collect();
            let mut previous_close: Option<f64> = None;
            let mut previous_level: Option<IndexPoint> = None;
            let mut suspicious = Vec::new();
            let mut unapplied = Vec::new();
            let mut count = 0usize;
            for session in market.sessions() {
                let date = session.session_date();
                let key = (id.clone(), date);
                let action = by_date.get(&key).copied();
                if let Some((settlement, _, cash)) =
                    action.filter(|(action, _, _)| action.kind == CorporateActionKind::Liquidation)
                {
                    if let (Some(previous_close), Some(previous)) = (previous_close, previous_level)
                    {
                        let point = IndexPoint {
                            level: previous.level * cash / previous_close,
                            available_at: max_known(
                                previous.available_at,
                                Some(settlement.available_at),
                            ),
                        };
                        if point.level.is_finite() && point.level > 0.0 {
                            points.insert(
                                key.clone(),
                                IndexPoint {
                                    available_at: point
                                        .available_at
                                        .map(|at| at.max(session.ts_close())),
                                    ..point
                                },
                            );
                            count += 1;
                        } else {
                            unapplied.push(date);
                        }
                    } else {
                        unapplied.push(date);
                    }
                    previous_close = None;
                    previous_level = None;
                    continue;
                }
                let Some(cell) = market.cells().get(&key) else {
                    let after_termination = market
                        .termination_date(&id)
                        .is_some_and(|termination| date >= termination);
                    if after_termination {
                        continue;
                    }
                    if action.is_some() {
                        unapplied.push(date);
                    }
                    previous_close = None;
                    previous_level = None;
                    continue;
                };
                if cell.state == TradeState::Unknown {
                    if by_date.contains_key(&key) {
                        unapplied.push(date);
                    }
                    previous_close = None;
                    previous_level = None;
                    continue;
                }
                let Some(close) = cell.close.map(|price| price.to_f64()) else {
                    if by_date.contains_key(&key) {
                        unapplied.push(date);
                    }
                    previous_close = None;
                    previous_level = None;
                    continue;
                };
                let mut current_level = None;
                if let (Some(prior_close), Some(prior)) = (previous_close, previous_level) {
                    let (ratio, cash) = action.map_or((1.0, 0.0), |(_, ratio, cash)| (ratio, cash));
                    let adjusted_return_bps =
                        (((ratio * close + cash) / prior_close - 1.0) * 10_000.0).abs();
                    if adjusted_return_bps > f64::from(rules.unexplained_jump_threshold_bps) {
                        suspicious.push(date);
                    } else {
                        let value = prior.level * (ratio * close + cash) / prior_close;
                        let available_at = if let Some((action, _, _)) = action {
                            max_known(
                                prior.available_at,
                                max_known(cell.available_at, Some(action.available_at)),
                            )
                        } else {
                            max_known(prior.available_at, cell.available_at)
                        };
                        current_level = Some(IndexPoint {
                            level: value,
                            available_at,
                        });
                        if action.is_some() {
                            count += 1;
                        }
                    }
                } else if action.is_none() {
                    current_level = Some(IndexPoint {
                        level: close,
                        available_at: cell.available_at,
                    });
                } else {
                    // An event on the first usable point has no prior close to
                    // define its return. Keep it out of TRI and report it.
                    unapplied.push(date);
                }
                if let Some(point) = current_level.filter(|p| p.level.is_finite() && p.level > 0.0)
                {
                    let point = IndexPoint {
                        level: point.level,
                        available_at: point.available_at.map(|at| at.max(session.ts_close())),
                    };
                    points.insert(key, point);
                    previous_level = Some(point);
                    previous_close = Some(close);
                } else {
                    points.remove(&key);
                    previous_level = None;
                    previous_close = None;
                }
            }
            integrity.push(InstrumentActionIntegrity {
                instrument_id: id,
                start: *dates.first().expect("market has sessions"),
                end: *dates.last().expect("market has sessions"),
                observed_actions: count,
                unapplied_action_dates: unapplied,
                unexplained_price_jumps: suspicious,
                coverage_status: ActionCoverageStatus::ObservedRawSetOnly,
            });
        }
        let identity = json!({
            "semantic_version":"etf-total-return-index@2", "market":market.version(),
            "dataset_version":dataset_version, "normalization_version":"szse-corporate-action@1",
            "rules":rules, "actions":actions, "integrity":integrity,
            "points":points.iter().map(|((id,date),p)|json!({"instrument":id,"date":date,"level_bits":format!("{:016x}",p.level.to_bits()),"available_at":p.available_at})).collect::<Vec<_>>()
        });
        let canonical = prajna_data::restricted_jcs(&identity)
            .map_err(|e| FactorError::Evaluation(e.to_string()))?;
        let version = format!("etf-tri-input:sha256:{:x}", Sha256::digest(canonical));
        Ok(Self {
            points,
            identity,
            version,
            market_identity: market.version().into(),
            actions: actions.to_vec(),
            integrity,
        })
    }

    pub fn build_from_dataset(
        market: &DerivedMarket,
        root: &std::path::Path,
        rules: &CorporateActionRules,
        raw: &RawStore,
    ) -> Result<Self, FactorError> {
        let manifest = read_manifest(root, market.dsv())
            .map_err(|e| FactorError::Evaluation(e.to_string()))?;
        let table = manifest
            .core
            .tables
            .iter()
            .find(|table| table.table == "corporate_actions")
            .ok_or_else(|| {
                FactorError::Evaluation(
                    "Dataset Version has no normalized corporate_actions table".into(),
                )
            })?;
        let dsv_hex = market
            .dsv()
            .strip_prefix("dsv:sha256:")
            .ok_or_else(|| FactorError::Evaluation("invalid Dataset Version".into()))?;
        let path = root
            .join("normalized")
            .join("corporate_actions")
            .join(dsv_hex)
            .join("part-00000.parquet");
        let batches = read_parquet(path, "corporate_actions", "1")
            .map_err(|e| FactorError::Evaluation(e.to_string()))?;
        let actual_hash = logical_hash(
            "corporate_actions",
            batches
                .first()
                .ok_or_else(|| {
                    FactorError::Evaluation("corporate_actions table is empty or missing".into())
                })?
                .schema()
                .as_ref(),
            &batches,
            &["announcement_id", "action_index"],
        )
        .map_err(|e| FactorError::Evaluation(e.to_string()))?;
        let row_count: u64 = batches.iter().map(|batch| batch.num_rows() as u64).sum();
        if actual_hash != table.logical_hash || row_count != table.row_count {
            return Err(FactorError::Evaluation(
                "corporate_actions table does not match Dataset Version".into(),
            ));
        }
        let actions = batches
            .iter()
            .map(corporate_actions_from_record_batch)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| FactorError::Evaluation(e.to_string()))?
            .into_iter()
            .flatten()
            .map(|mut action| {
                let date = action.ex_date.or(action.effective_date).ok_or_else(|| {
                    FactorError::Evaluation(
                        "corporate action cannot be assigned to a lifecycle without an ex-date"
                            .into(),
                    )
                })?;
                let native_symbol = action.instrument_id.symbol().to_owned();
                action.instrument_id = if action.kind == CorporateActionKind::Liquidation {
                    market.resolve_settlement_action(&native_symbol, date)
                } else {
                    market.resolve_native_row(&native_symbol, date)
                }
                .map_err(|e| FactorError::Evaluation(e.to_string()))?;
                Ok(action)
            })
            .collect::<Result<Vec<_>, FactorError>>()?;
        Self::build_with_actions(market, &actions, rules, market.dsv(), raw)
    }

    pub fn identity(&self) -> &Value {
        &self.identity
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn market_identity(&self) -> &str {
        &self.market_identity
    }
    pub fn point(&self, id: &InstrumentId, date: NaiveDate) -> Option<IndexPoint> {
        self.points.get(&(id.clone(), date)).copied()
    }
    pub fn integrity(&self) -> &[InstrumentActionIntegrity] {
        &self.integrity
    }

    pub fn actions(&self) -> &[CorporateActionRow] {
        &self.actions
    }
}

fn parse_action_decimal(value: Option<&str>) -> Result<f64, FactorError> {
    let value = value.ok_or_else(|| {
        FactorError::Evaluation("corporate action is missing a required amount or ratio".into())
    })?;
    let parsed = value
        .parse::<f64>()
        .map_err(|_| FactorError::Evaluation("invalid corporate action decimal".into()))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(FactorError::Evaluation(
            "corporate action amount or ratio must be positive".into(),
        ));
    }
    Ok(parsed)
}

fn action_terms(action: &CorporateActionRow) -> Result<(NaiveDate, f64, f64), FactorError> {
    let (date, ratio, cash) = match action.kind {
        CorporateActionKind::Dividend => (
            action.ex_date,
            1.0,
            parse_action_decimal(action.cash_per_share.as_deref())?,
        ),
        CorporateActionKind::ShareConversion => (
            action.effective_date,
            parse_action_decimal(action.share_ratio.as_deref())?,
            0.0,
        ),
        CorporateActionKind::Liquidation => (
            action.effective_date,
            0.0,
            parse_action_decimal(action.liquidation_cash_per_share.as_deref())?,
        ),
    };
    let date = date
        .ok_or_else(|| FactorError::Evaluation("corporate action lacks effective date".into()))?;
    Ok((date, ratio, cash))
}

fn max_known(a: Option<TimestampNs>, b: Option<TimestampNs>) -> Option<TimestampNs> {
    a.zip(b).map(|(a, b)| a.max(b))
}

/// High momentum first, N Session return requiring N+1 consecutive Session
/// levels and states. No forward returns or skip/bar-count windows are used.
#[derive(Debug, Clone, Copy)]
pub struct EtfMomentum {
    n: u32,
}
impl EtfMomentum {
    pub fn new(n: u32) -> Result<Self, FactorError> {
        if n == 0 {
            return Err(FactorError::Evaluation(
                "ETF momentum n must be positive".into(),
            ));
        }
        Ok(Self { n })
    }
    pub fn canonical_json(&self) -> Value {
        json!({"kind":"etf_tri_momentum","version":"1","params":{"n":self.n},"direction":"higher_first","missing":"full_session_window_unknown_missing_halted_carry"})
    }
    pub fn cache_key(
        &self,
        market: &DerivedMarket,
        tri: &TotalReturnIndex,
        universe_version: &str,
        members: &[InstrumentId],
    ) -> Result<FactorKey, CanonicalError> {
        FactorKey::for_etf_input(
            self.canonical_json(),
            market.dsv(),
            universe_version,
            json!({"catalog":market.catalog_version(),"market":market.version(),"tri":tri.version(),"members":members.iter().collect::<BTreeSet<_>>()}),
        )
    }
    pub fn cached(
        &self,
        cache: &FactorCache,
        market: &DerivedMarket,
        tri: &TotalReturnIndex,
        members: &[InstrumentId],
        universe_version: &str,
    ) -> Result<DataFrame, CacheError> {
        self.validate(market, tri, members)?;
        let key = self
            .cache_key(market, tri, universe_version, members)
            .map_err(|e| FactorError::Evaluation(e.to_string()))?;
        cache.get_or_compute_values(&key, || self.compute(market, tri, members))
    }
    pub fn compute(
        &self,
        market: &DerivedMarket,
        tri: &TotalReturnIndex,
        members: &[InstrumentId],
    ) -> Result<DataFrame, FactorError> {
        self.validate(market, tri, members)?;
        let ids: BTreeSet<_> = members.iter().cloned().collect();
        let mut instruments = Vec::new();
        let mut dates = Vec::new();
        let mut values = Vec::new();
        let mut availability = Vec::new();
        let mut statuses = Vec::new();
        let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
        for id in ids {
            for (i, session) in market.sessions().iter().enumerate() {
                let row = self.evaluate(market, tri, &id, i);
                instruments.push(id.to_string());
                dates.push(
                    i32::try_from((session.session_date() - epoch).num_days())
                        .map_err(|_| FactorError::Evaluation("Session date overflow".into()))?,
                );
                values.push(row.0);
                availability.push(row.1.map(TimestampNs::as_unix_nanos));
                statuses.push(row.2.as_str());
            }
        }
        let date = Series::new("session_date".into(), dates).cast(&DataType::Date)?;
        let available =
            Int64Chunked::from_iter_options("available_at".into(), availability.into_iter())
                .into_datetime(TimeUnit::Nanoseconds, Some(TimeZone::UTC))
                .into_series();
        Ok(DataFrame::new(
            instruments.len(),
            vec![
                Series::new("instrument_id".into(), instruments).into(),
                date.into(),
                Series::new("value".into(), values).into(),
                available.into(),
                Series::new("status".into(), statuses).into(),
            ],
        )?)
    }
    fn validate(
        &self,
        market: &DerivedMarket,
        tri: &TotalReturnIndex,
        members: &[InstrumentId],
    ) -> Result<(), FactorError> {
        if tri.market_identity() != market.version() {
            return Err(FactorError::Evaluation(
                "TRI belongs to a different DerivedMarket".into(),
            ));
        }
        let ids: BTreeSet<_> = members.iter().cloned().collect();
        if ids.len() != members.len() {
            return Err(FactorError::Evaluation(
                "duplicate ETF momentum members".into(),
            ));
        }
        if ids
            .iter()
            .any(|id| !market.cells().keys().any(|(known, _)| known == id))
        {
            return Err(FactorError::Evaluation(
                "ETF momentum member absent from market".into(),
            ));
        }
        Ok(())
    }
    fn evaluate(
        &self,
        market: &DerivedMarket,
        tri: &TotalReturnIndex,
        id: &InstrumentId,
        i: usize,
    ) -> (Option<f64>, Option<TimestampNs>, FactorStatus) {
        let Some(start) = i.checked_sub(self.n as usize) else {
            return (None, None, FactorStatus::InsufficientWindow);
        };
        let mut points = Vec::new();
        for session in &market.sessions()[start..=i] {
            if !market
                .cells()
                .get(&(id.clone(), session.session_date()))
                .is_some_and(|c| c.state != TradeState::Unknown)
            {
                return (None, None, FactorStatus::MissingInput);
            }
            let Some(p) = tri.point(id, session.session_date()) else {
                return (None, None, FactorStatus::MissingInput);
            };
            points.push(p);
        }
        if points.iter().any(|p| p.available_at.is_none()) {
            return (None, None, FactorStatus::UnknownAvailability);
        }
        let at = points
            .iter()
            .filter_map(|p| p.available_at)
            .max()
            .map(|t| t.max(market.sessions()[i].ts_close()));
        let value = points.last().expect("nonempty").level / points[0].level - 1.0;
        if !value.is_finite() {
            return (None, None, FactorStatus::MissingInput);
        }
        (Some(value), at, FactorStatus::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn window_zero_rejected_and_version_is_distinct() {
        assert!(EtfMomentum::new(0).is_err());
        let id = EtfMomentum::new(60).unwrap().canonical_json();
        assert_eq!(id["kind"], "etf_tri_momentum");
        assert_eq!(id["params"]["n"], 60);
    }
    #[test]
    fn unknown_dependency_time_is_not_backfilled() {
        let a = TimestampNs::from_unix_nanos(4);
        let b = TimestampNs::from_unix_nanos(9);
        assert_eq!(max_known(Some(a), Some(b)), Some(b));
        assert_eq!(max_known(None, Some(b)), None);
    }
}
