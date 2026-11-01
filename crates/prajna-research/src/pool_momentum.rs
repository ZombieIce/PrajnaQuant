//! Pool momentum Top-K rotation, reusing the versioned Momentum factor and the
//! existing TopKRank decision/execution contract.
//!
//! Signals use only the bound adjusted research closes; the ledger keeps raw
//! prices. A scheduled close Decision may list fewer candidates than `top_k`
//! (equal weight over the actual count) or no candidate at all (all cash).
use std::collections::BTreeSet;

use chrono::NaiveDate;
use prajna_domain::InstrumentId;

use crate::{
    Panel,
    factor::{AvailabilityAssumption, Factor, FactorKind, MomentumParams},
    strategy::{
        Decision, EngineError, RankDirection, StrategyCapability, TopKRank, UnfilledEntry,
        VectorEngine, VectorStrategy, Weighting, validate_members,
    },
};

/// Version 1 parameters of the pool momentum Top-K template.
///
/// `lookback` is the number of Sessions between the two Momentum closes
/// (`close_t / close_(t-n) - 1`), so the first evaluable Session is the
/// `lookback`-th zero-based Session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolMomentumTopKParams {
    pub params_version: u32,
    pub lookback: u32,
    pub top_k: u32,
    pub rebalance_every: u32,
}

/// Rank available momentum values at each scheduled Session close.
///
/// When `start` is given the cadence is anchored at that Session (the frozen
/// evaluation start), not at the first mathematically ready Session. That keeps
/// the frozen window and the actual Decision schedule identical. Ties break by
/// ascending `InstrumentId`, matching the existing `TopKRank` contract.
pub fn decide(
    panel: &Panel,
    members: &[InstrumentId],
    params: &PoolMomentumTopKParams,
    assumption: AvailabilityAssumption,
    start: Option<NaiveDate>,
) -> Result<Vec<Decision>, EngineError> {
    VectorEngine::validate(&VectorStrategy::PoolMomentumTopK(params.clone()))?;
    validate_members(panel, members)?;
    let factor = Factor::new(FactorKind::Momentum, MomentumParams { n: params.lookback })
        .map_err(|error| EngineError::InvalidScoreValues(error.to_string()))?;
    let values = crate::compute_base(panel, members, &factor, assumption)
        .map_err(|error| EngineError::InvalidScoreValues(error.to_string()))?;
    let ranker = TopKRank {
        score: factor,
        direction: RankDirection::Descending,
        top_k: params.top_k,
        rebalance_every: params.rebalance_every,
        weighting: Weighting::EqualWeight,
        unfilled_entry: UnfilledEntry::Skip,
        capabilities: BTreeSet::from([StrategyCapability::Vectorizable]),
    };
    ranker.decide_validated_from(&values, &panel.sessions, start)
}
