//! Version 1 MA gap: trailing Session windows over close, available at the latest input time.
//! Missing bars invalidate the whole window; a non-usable gap resets crossover comparison.
use std::collections::{BTreeMap, BTreeSet};

use prajna_domain::InstrumentId;

use crate::{
    Panel,
    factor::{AvailabilityAssumption, FactorStatus},
    strategy::{
        Decision, EngineError, MaCrossoverParams, VectorEngine, VectorStrategy, validate_members,
    },
};

#[derive(Debug, Clone, PartialEq)]
pub struct MaGap {
    pub instrument_id: InstrumentId,
    pub session_index: usize,
    pub value: Option<f64>,
    pub available_at: Option<i64>,
    pub status: FactorStatus,
}

pub fn gaps(
    panel: &Panel,
    members: &[InstrumentId],
    params: &MaCrossoverParams,
    assumption: AvailabilityAssumption,
) -> Result<Vec<MaGap>, EngineError> {
    VectorEngine::validate(&VectorStrategy::MaCrossover(params.clone()))?;
    validate_members(panel, members)?;
    let ids = panel.grid.column("instrument_id")?.str()?;
    let indices = panel.grid.column("session_index")?.u32()?;
    let closes = panel.grid.column("close")?.f64()?;
    let has_bar = panel.grid.column("has_bar")?.bool()?;
    let available = panel
        .grid
        .column("close_available_at")?
        .datetime()?
        .physical();
    let member_set = members
        .iter()
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    let mut inputs = BTreeMap::new();
    for row in 0..panel.grid.height() {
        let id = ids.get(row).ok_or_else(|| invalid("null instrument"))?;
        if !member_set.contains(id) {
            continue;
        }
        let index = indices
            .get(row)
            .ok_or_else(|| invalid("null session_index"))? as usize;
        let session = panel
            .sessions
            .get(index)
            .ok_or_else(|| invalid("session_index out of range"))?;
        let bar = has_bar.get(row).ok_or_else(|| invalid("null has_bar"))?;
        let close = closes.get(row);
        if bar && !close.is_some_and(|value| value.is_finite() && value > 0.0) {
            return Err(invalid("bar must have a finite positive close"));
        }
        let time = available.get(row).or_else(|| match assumption {
            AvailabilityAssumption::None => None,
            AvailabilityAssumption::TreatUnknownAsTsClose => Some(session.ts_close.as_unix_nanos()),
        });
        if inputs
            .insert((id.to_owned(), index), (bar, close, time))
            .is_some()
        {
            return Err(invalid("duplicate Panel grid row"));
        }
    }
    if inputs.len() != members.len() * panel.sessions.len() {
        return Err(invalid("incomplete Panel grid"));
    }
    let mut result = Vec::new();
    for id in members.iter().collect::<BTreeSet<_>>() {
        for index in 0..panel.sessions.len() {
            let mut row = MaGap {
                instrument_id: id.clone(),
                session_index: index,
                value: None,
                available_at: None,
                status: FactorStatus::InsufficientWindow,
            };
            let long = params.long_window as usize;
            if index + 1 >= long {
                let window = (index + 1 - long..=index)
                    .map(|i| inputs[&(id.to_string(), i)])
                    .collect::<Vec<_>>();
                row.status = if window.iter().any(|(bar, _, _)| !bar) {
                    FactorStatus::MissingInput
                } else if window.iter().any(|(_, _, time)| time.is_none()) {
                    FactorStatus::UnknownAvailability
                } else {
                    FactorStatus::Ok
                };
                if row.status == FactorStatus::Ok {
                    let short = params.short_window as usize;
                    let long_mean = window
                        .iter()
                        .map(|(_, close, _)| close.unwrap())
                        .sum::<f64>()
                        / long as f64;
                    let short_mean = window[long - short..]
                        .iter()
                        .map(|(_, close, _)| close.unwrap())
                        .sum::<f64>()
                        / short as f64;
                    let gap = short_mean - long_mean;
                    if !gap.is_finite() {
                        return Err(invalid("MA gap is not finite"));
                    }
                    row.value = Some(gap);
                    row.available_at = window.iter().filter_map(|(_, _, time)| *time).max();
                }
            }
            result.push(row);
        }
    }
    Ok(result)
}

pub(crate) fn decide(
    panel: &Panel,
    members: &[InstrumentId],
    params: &MaCrossoverParams,
    assumption: AvailabilityAssumption,
) -> Result<Vec<Decision>, EngineError> {
    let rows = gaps(panel, members, params, assumption)?;
    let mut by_session = vec![Vec::new(); panel.sessions.len()];
    for row in rows {
        by_session[row.session_index].push(row);
    }
    let mut previous = BTreeMap::<InstrumentId, f64>::new();
    let mut held = BTreeSet::new();
    let mut decisions = Vec::new();
    for (index, rows) in by_session.into_iter().enumerate() {
        let old = held.clone();
        for row in rows {
            let current = row.value.filter(|_| {
                row.available_at
                    .is_some_and(|time| time <= panel.sessions[index].ts_close.as_unix_nanos())
            });
            if let Some(gap) = current {
                if let Some(prior) = previous.insert(row.instrument_id.clone(), gap) {
                    if prior <= 0.0 && gap > 0.0 {
                        held.insert(row.instrument_id.clone());
                    }
                    if prior >= 0.0 && gap < 0.0 {
                        held.remove(&row.instrument_id);
                    }
                }
            } else {
                previous.remove(&row.instrument_id);
            }
        }
        if held != old {
            decisions.push(Decision {
                decision_session: panel.sessions[index].clone(),
                ranked: Vec::new(),
                targets: held
                    .iter()
                    .map(|id| (id.clone(), 1.0 / members.len() as f64))
                    .collect(),
            });
        }
    }
    Ok(decisions)
}

fn invalid(message: &str) -> EngineError {
    EngineError::InvalidScoreValues(message.into())
}
