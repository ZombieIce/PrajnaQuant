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
use prajna_data::{CanonicalError, RawStore};
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
        Ok(Self {
            points,
            identity,
            version,
            market_identity: market.version().into(),
        })
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
