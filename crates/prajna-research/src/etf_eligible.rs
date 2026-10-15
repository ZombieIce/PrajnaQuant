//! P9 eligible inputs are distinct from PIT membership and from next-open permission.
use crate::{
    etf_market::{DerivedMarket, TradeState, identity},
    etf_momentum::{EtfMomentum, TotalReturnIndex},
    pit_universe::{CatalogError, HistoricalCatalog, UniverseBuildResult},
};
use chrono::NaiveDate;
use prajna_domain::{InstrumentId, Notional};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EligibilityParams {
    pub minimum_listing_sessions: u32,
    pub momentum_sessions: u32,
    pub amount_sessions: u32,
    pub minimum_average_amount: Notional,
}
impl Default for EligibilityParams {
    fn default() -> Self {
        Self {
            minimum_listing_sessions: 61,
            momentum_sessions: 60,
            amount_sessions: 20,
            minimum_average_amount: Notional::parse("10000000").expect("fixed threshold"),
        }
    }
}
/// Identity-bearing fragment for #177 Run Spec/results, including exclusions even
/// for Summary. An empty member set is valid; missing market coverage is not.
#[derive(Debug, Clone, Serialize)]
pub struct EligibleSet {
    pub version: String,
    pub universe: UniverseBuildResult,
    pub market_version: String,
    pub factor_key: String,
    pub params: EligibilityParams,
    pub members: Vec<InstrumentId>,
    pub excluded: BTreeMap<InstrumentId, BTreeSet<String>>,
}
impl EligibleSet {
    pub fn build(
        catalog: &HistoricalCatalog,
        market: &DerivedMarket,
        tri: &TotalReturnIndex,
        date: NaiveDate,
        params: EligibilityParams,
    ) -> Result<Self, CatalogError> {
        let error = |s: &str| CatalogError(s.into());
        if params.minimum_listing_sessions == 0
            || params.amount_sessions == 0
            || params.minimum_average_amount < Notional::ZERO
        {
            return Err(error("invalid eligibility parameters"));
        }
        if catalog.version()? != market.catalog_version() {
            return Err(error("market/Catalog identity mismatch"));
        }
        let universe = catalog.universe(date)?;
        let i = market
            .sessions()
            .iter()
            .position(|s| s.session_date() == date)
            .ok_or_else(|| error("decision outside market coverage"))?;
        let now = market.sessions()[i].ts_close();
        let factor =
            EtfMomentum::new(params.momentum_sessions).map_err(|e| CatalogError(e.to_string()))?;
        let values = factor
            .compute(market, tri, &universe.members)
            .map_err(|e| CatalogError(e.to_string()))?;
        let factor_key = factor
            .cache_key(market, tri, &universe.universe_version, &universe.members)
            .map_err(|e| CatalogError(e.to_string()))?
            .as_str()
            .to_owned();
        let column_error = |e: polars::error::PolarsError| CatalogError(e.to_string());
        let ids = values
            .column("instrument_id")
            .map_err(column_error)?
            .str()
            .map_err(column_error)?;
        let statuses = values
            .column("status")
            .map_err(column_error)?
            .str()
            .map_err(column_error)?;
        let times = values
            .column("available_at")
            .map_err(column_error)?
            .datetime()
            .map_err(column_error)?
            .physical();
        let mut momentum = BTreeMap::new();
        for row in (i..values.height()).step_by(market.sessions().len()) {
            momentum.insert(
                ids.get(row).unwrap_or_default(),
                statuses.get(row) == Some("ok")
                    && times.get(row).is_some_and(|at| at <= now.as_unix_nanos()),
            );
        }
        let mut members = Vec::new();
        let mut excluded = BTreeMap::new();
        for id in &universe.members {
            let mut reasons = BTreeSet::new();
            let life = catalog
                .input()
                .lifecycles
                .iter()
                .find(|l| l.instrument_id().as_ref() == Ok(id))
                .ok_or_else(|| error("unresolved member lifecycle"))?;
            let age = catalog
                .input()
                .sessions
                .iter()
                .filter(|s| {
                    s.session_date() >= life.listing.effective_date && s.session_date() <= date
                })
                .count();
            if age < (params.minimum_listing_sessions as usize) {
                reasons.insert("insufficient_listing_sessions".into());
            }
            if !momentum
                .get(id.to_string().as_str())
                .copied()
                .unwrap_or(false)
            {
                reasons.insert("momentum_unavailable".into());
            }
            if !market
                .cells()
                .get(&(id.clone(), date))
                .is_some_and(|c| c.permits(true) && c.known_by(now))
            {
                reasons.insert("opening_status_unavailable_or_buy_blocked".into());
            }
            let mut amount = 0_i128;
            match (i + 1).checked_sub(params.amount_sessions as usize) {
                None => {
                    reasons.insert("insufficient_amount_window".into());
                }
                Some(start) => {
                    for s in &market.sessions()[start..=i] {
                        match market.cells().get(&(id.clone(), s.session_date())) {
                            Some(c) if c.state != TradeState::Unknown && c.known_by(now) => {
                                let value = if c.state == TradeState::Halted {
                                    Some(Notional::ZERO)
                                } else {
                                    c.amount
                                };
                                match value {
                                    Some(v) if v >= Notional::ZERO => {
                                        amount = amount
                                            .checked_add(v.mantissa())
                                            .ok_or_else(|| error("amount sum overflow"))?;
                                    }
                                    _ => {
                                        reasons.insert("amount_missing_input".into());
                                    }
                                }
                            }
                            _ => {
                                reasons.insert("amount_missing_input".into());
                            }
                        }
                    }
                    // Compare exact sums; equality at 10m qualifies, no f64 rounding.
                    let threshold = params
                        .minimum_average_amount
                        .mantissa()
                        .checked_mul(i128::from(params.amount_sessions))
                        .ok_or_else(|| error("amount threshold overflow"))?;
                    if amount < threshold {
                        reasons.insert("below_average_amount_threshold".into());
                    }
                }
            }
            if reasons.is_empty() {
                members.push(id.clone());
            } else {
                excluded.insert(id.clone(), reasons);
            }
        }
        let version = identity(
            "eligible",
            &(
                "szse-eligible@1",
                &universe,
                market.version(),
                &factor_key,
                &params,
                &members,
                excluded.iter().collect::<Vec<_>>(),
            ),
        )?;
        Ok(Self {
            version,
            universe,
            market_version: market.version().into(),
            factor_key,
            params,
            members,
            excluded,
        })
    }
}
