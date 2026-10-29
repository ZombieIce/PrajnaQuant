//! Strict single-ETF timing, using the versioned T10 factor and shared execution.
use crate::{
    Panel,
    factor::{AvailabilityAssumption, Factor, FactorKind, TrendFilterStrictParams},
    strategy::{Decision, EngineError, VectorEngine, VectorStrategy, validate_members},
};
use chrono::NaiveDate;
use prajna_domain::InstrumentId;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimingParams {
    pub params_version: u32,
    pub ma_window: u32,
    pub rebalance_every: u32,
}

/// Evaluate at fixed Session intervals from the first evaluation Session (or
/// first mathematically ready Session). Unavailable signals create no Decision;
/// a valid false/equal signal creates an empty cash target. Missing Sessions do
/// not shorten the calendar or advance the next scheduled rebalance.
pub fn decide(
    panel: &Panel,
    members: &[InstrumentId],
    params: &TimingParams,
    assumption: AvailabilityAssumption,
    start: Option<NaiveDate>,
) -> Result<Vec<Decision>, EngineError> {
    VectorEngine::validate(&VectorStrategy::SingleEtfTiming(params.clone()))?;
    validate_members(panel, members)?;
    if members.len() != 1 {
        return Err(EngineError::InvalidParameters(
            "single_etf_timing requires one member".into(),
        ));
    }
    let factor = Factor::new(
        FactorKind::TrendFilterStrict,
        TrendFilterStrictParams {
            n: params.ma_window,
        },
    )
    .map_err(|e| EngineError::InvalidScoreValues(e.to_string()))?;
    let values = crate::compute_base(panel, members, &factor, assumption)
        .map_err(|e| EngineError::InvalidScoreValues(e.to_string()))?;
    let value = values.column("value")?.f64()?;
    let status = values.column("status")?.str()?;
    let available = values.column("available_at")?.datetime()?.physical();
    let first = match start {
        Some(date) => panel
            .sessions
            .iter()
            .position(|s| s.session_date == date)
            .ok_or_else(|| {
                EngineError::InvalidParameters("evaluation start is not a Panel Session".into())
            })?,
        None => (params.ma_window - 1) as usize,
    };
    let mut decisions = Vec::new();
    // compute_base sorts by member, then Session; this strategy has one member.
    for index in (first..panel.sessions.len()).step_by(params.rebalance_every as usize) {
        if status.get(index) != Some("ok")
            || !available
                .get(index)
                .is_some_and(|time| time <= panel.sessions[index].ts_close.as_unix_nanos())
        {
            continue;
        }
        let Some(signal) = value.get(index) else {
            continue;
        };
        decisions.push(Decision {
            decision_session: panel.sessions[index].clone(),
            ranked: Vec::new(),
            targets: if signal == 1.0 {
                BTreeMap::from([(members[0].clone(), 1.0)])
            } else {
                BTreeMap::new()
            },
        });
    }
    Ok(decisions)
}
