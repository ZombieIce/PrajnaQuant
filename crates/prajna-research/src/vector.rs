use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use polars::prelude::PolarsError;
use prajna_domain::InstrumentId;
use serde::{Deserialize, Serialize};

use crate::{
    Panel, PanelSession,
    factor::AvailabilityAssumption,
    strategy::{Decision, ExecutionEvent},
};

const RETURN_TOLERANCE: f64 = 1e-12;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VectorCosts {
    pub commission_rate: f64,
    pub buy_slippage_bps: f64,
    pub sell_slippage_bps: f64,
    pub buy_tax_rate: f64,
    pub sell_tax_rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorResult {
    pub sessions: Vec<VectorSession>,
    pub decisions: Vec<VectorDecision>,
    pub executions: Vec<VectorExecution>,
    pub pending_at_end: Option<VectorPending>,
    pub assumptions: Vec<String>,
    pub costs: VectorCosts,
    pub availability_assumption: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorSession {
    pub session_date: String,
    pub target_weights: BTreeMap<InstrumentId, f64>,
    pub weights_after_execution: BTreeMap<InstrumentId, f64>,
    pub turnover: f64,
    pub cost: f64,
    pub gross_return: f64,
    pub net_return: f64,
    pub nav: f64,
    pub valuation_carried: Vec<InstrumentId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorDecision {
    pub decision_session: String,
    pub ranked: Vec<VectorRankedScore>,
    pub targets: BTreeMap<InstrumentId, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorRankedScore {
    pub instrument_id: InstrumentId,
    pub score: f64,
    pub rank: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorExecution {
    pub session_date: String,
    pub kind: VectorExecutionKind,
    pub blocked: BTreeSet<InstrumentId>,
    pub skipped_buys: BTreeSet<InstrumentId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorExecutionKind {
    Executed,
    Deferred,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorPending {
    pub decision_session: String,
    pub targets: BTreeMap<InstrumentId, f64>,
}

#[derive(Debug)]
pub enum VectorError {
    InvalidCosts(String),
    InvalidInput(String),
    Polars(PolarsError),
}

impl fmt::Display for VectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCosts(message) => write!(formatter, "invalid Vector costs: {message}"),
            Self::InvalidInput(message) => write!(formatter, "invalid Vector input: {message}"),
            Self::Polars(error) => write!(formatter, "could not read Vector price panel: {error}"),
        }
    }
}

impl Error for VectorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Polars(error) => Some(error),
            Self::InvalidCosts(_) | Self::InvalidInput(_) => None,
        }
    }
}

impl From<PolarsError> for VectorError {
    fn from(error: PolarsError) -> Self {
        Self::Polars(error)
    }
}

pub fn run_vector(
    panel: &Panel,
    decisions: &[Decision],
    events: &[ExecutionEvent],
    costs: &VectorCosts,
    availability_assumption: AvailabilityAssumption,
) -> Result<VectorResult, VectorError> {
    validate_costs(costs)?;
    let prices = read_prices(panel)?;
    let decision_indices = index_decisions(panel, decisions)?;
    let event_indices = index_events(panel, events)?;
    let decisions_by_index = decision_indices
        .iter()
        .map(|(index, decision)| (*index, *decision))
        .collect::<BTreeMap<_, _>>();
    let events_by_index = event_indices
        .iter()
        .map(|(index, event)| (*index, *event))
        .collect::<BTreeMap<_, _>>();

    let mut last_known_prices = vec![None; panel.instruments.len()];
    let mut marks_by_session = Vec::with_capacity(panel.sessions.len());
    let mut carried_by_session = Vec::with_capacity(panel.sessions.len());
    for (session_index, _) in panel.sessions.iter().enumerate() {
        let mut marks = vec![None; panel.instruments.len()];
        let mut carried = Vec::new();
        for (instrument_index, (instrument, instrument_prices)) in
            panel.instruments.iter().zip(&prices).enumerate()
        {
            if let Some(price) = instrument_prices[session_index] {
                marks[instrument_index] = Some(price);
                last_known_prices[instrument_index] = Some(price);
            } else if let Some(price) = last_known_prices[instrument_index] {
                marks[instrument_index] = Some(price);
                carried.push(instrument.clone());
            }
        }
        marks_by_session.push(marks);
        carried_by_session.push(carried);
    }

    let (buy_rate, sell_rate) = costs.side_rates();
    let mut weights = BTreeMap::new();
    let mut pending_decision: Option<&Decision> = None;
    let mut latest_target_weights = BTreeMap::new();
    let mut nav = 1.0;
    let mut result_sessions = Vec::with_capacity(panel.sessions.len());
    let mut result_executions = Vec::new();

    for (session_index, session) in panel.sessions.iter().enumerate() {
        let event = events_by_index.get(&session_index).copied();
        let mut turnover = 0.0;
        let mut cost = 0.0;

        match (pending_decision, event) {
            (Some(pending), Some(event)) => {
                validate_event_decision(event, pending)?;
                match event {
                    ExecutionEvent::Deferred { blocked, .. } => {
                        if blocked.is_empty()
                            || blocked.iter().any(|instrument| {
                                weights.get(instrument).copied().unwrap_or(0.0) <= 0.0
                            })
                        {
                            return Err(invalid_input(
                                "deferred event must block at least one currently held instrument",
                            ));
                        }
                        result_executions.push(VectorExecution {
                            session_date: session.session_date.to_string(),
                            kind: VectorExecutionKind::Deferred,
                            blocked: blocked.clone(),
                            skipped_buys: BTreeSet::new(),
                        });
                    }
                    ExecutionEvent::Executed {
                        applied,
                        skipped_buys,
                        ..
                    } => {
                        validate_applied_weights(panel, applied)?;
                        validate_skipped_buys(pending, applied, skipped_buys)?;
                        for instrument in applied.keys().chain(weights.keys()) {
                            let instrument_index =
                                panel.instruments.binary_search(instrument).map_err(|_| {
                                    invalid_input(format!("unknown held instrument {instrument}"))
                                })?;
                            if prices[instrument_index][session_index].is_none() {
                                return Err(invalid_input(format!(
                                    "executed event applies {instrument} without an open price on {}",
                                    session.session_date
                                )));
                            }
                        }
                        let mut buys = 0.0;
                        let mut sells = 0.0;
                        for instrument in &panel.instruments {
                            let delta = applied.get(instrument).copied().unwrap_or(0.0)
                                - weights.get(instrument).copied().unwrap_or(0.0);
                            buys += delta.max(0.0);
                            sells += (-delta).max(0.0);
                        }
                        turnover = buys + sells;
                        cost = buys * buy_rate + sells * sell_rate;
                        if !turnover.is_finite() || !cost.is_finite() || cost >= 1.0 {
                            return Err(invalid_costs(
                                "an execution must have finite turnover and cost below 100%",
                            ));
                        }
                        weights = applied
                            .iter()
                            .filter(|(_, weight)| **weight > 0.0)
                            .map(|(instrument, weight)| (instrument.clone(), *weight))
                            .collect();
                        pending_decision = None;
                        result_executions.push(VectorExecution {
                            session_date: session.session_date.to_string(),
                            kind: VectorExecutionKind::Executed,
                            blocked: BTreeSet::new(),
                            skipped_buys: skipped_buys.clone(),
                        });
                    }
                    ExecutionEvent::PendingAtEnd { .. } => unreachable!(),
                }
            }
            (Some(_), None) => {
                return Err(invalid_input(format!(
                    "pending target has no execution event on {}",
                    session.session_date
                )));
            }
            (None, Some(_)) => {
                return Err(invalid_input(format!(
                    "execution event on {} has no pending target",
                    session.session_date
                )));
            }
            (None, None) => {}
        }

        let weights_after_execution = weights.clone();
        let mut gross_return = 0.0;
        let mut price_relatives = BTreeMap::new();
        if let Some(next_marks) = marks_by_session.get(session_index + 1) {
            for (instrument, weight) in &weights {
                let instrument_index = panel
                    .instruments
                    .binary_search(instrument)
                    .map_err(|_| invalid_input(format!("unknown held instrument {instrument}")))?;
                let current_price =
                    marks_by_session[session_index][instrument_index].ok_or_else(|| {
                        invalid_input(format!(
                            "held instrument {instrument} has no current valuation price"
                        ))
                    })?;
                let next_price = next_marks[instrument_index].ok_or_else(|| {
                    invalid_input(format!(
                        "held instrument {instrument} has no next valuation price to carry"
                    ))
                })?;
                let relative = next_price / current_price;
                price_relatives.insert(instrument.clone(), relative);
                gross_return += weight * (relative - 1.0);
            }
            if !gross_return.is_finite() || 1.0 + gross_return <= 0.0 {
                return Err(invalid_input(
                    "portfolio return must be finite and greater than -100%",
                ));
            }
        }

        let net_return = (1.0 - cost) * (1.0 + gross_return) - 1.0;
        nav *= 1.0 - cost;
        nav *= 1.0 + gross_return;
        if !nav.is_finite() || nav <= 0.0 || !net_return.is_finite() {
            return Err(invalid_input(
                "portfolio NAV and net return must remain finite and positive",
            ));
        }

        if session_index + 1 < panel.sessions.len() {
            let return_divisor = 1.0 + gross_return;
            for (instrument, weight) in &mut weights {
                let relative = price_relatives.get(instrument).ok_or_else(|| {
                    invalid_input(format!(
                        "missing price transition for held instrument {instrument}"
                    ))
                })?;
                *weight *= relative / return_divisor;
            }
            validate_applied_weights(panel, &weights)?;
        }

        if let Some(decision) = decisions_by_index.get(&session_index) {
            latest_target_weights = decision.targets.clone();
            pending_decision = Some(decision);
        }

        result_sessions.push(VectorSession {
            session_date: session.session_date.to_string(),
            target_weights: latest_target_weights.clone(),
            weights_after_execution,
            turnover,
            cost,
            gross_return,
            net_return,
            nav,
            valuation_carried: carried_by_session[session_index].clone(),
        });
    }

    let pending_event = event_indices.iter().find_map(|(_, event)| match event {
        ExecutionEvent::PendingAtEnd {
            decision_session,
            targets,
        } => Some((decision_session, targets)),
        ExecutionEvent::Deferred { .. } | ExecutionEvent::Executed { .. } => None,
    });
    let pending_at_end = match (pending_decision, pending_event) {
        (Some(pending), Some((event_session, targets))) => {
            if pending.decision_session != *event_session || pending.targets != *targets {
                return Err(invalid_input(
                    "pending-at-end event does not match the latest target",
                ));
            }
            Some(VectorPending {
                decision_session: pending.decision_session.session_date.to_string(),
                targets: pending.targets.clone(),
            })
        }
        (Some(_), None) => return Err(invalid_input("missing pending-at-end event")),
        (None, Some(_)) => return Err(invalid_input("unexpected pending-at-end event")),
        (None, None) => None,
    };

    let result_decisions = decisions
        .iter()
        .map(|decision| VectorDecision {
            decision_session: decision.decision_session.session_date.to_string(),
            ranked: decision
                .ranked
                .iter()
                .map(|(instrument_id, score, rank)| VectorRankedScore {
                    instrument_id: instrument_id.clone(),
                    score: *score,
                    rank: *rank,
                })
                .collect(),
            targets: decision.targets.clone(),
        })
        .collect();

    Ok(VectorResult {
        sessions: result_sessions,
        decisions: result_decisions,
        executions: result_executions,
        pending_at_end,
        assumptions: assumptions(availability_assumption),
        costs: *costs,
        availability_assumption: availability_assumption.canonical_str().to_owned(),
    })
}

impl VectorCosts {
    fn side_rates(&self) -> (f64, f64) {
        (
            self.commission_rate + self.buy_slippage_bps / 10_000.0 + self.buy_tax_rate,
            self.commission_rate + self.sell_slippage_bps / 10_000.0 + self.sell_tax_rate,
        )
    }
}

fn validate_costs(costs: &VectorCosts) -> Result<(), VectorError> {
    let values = [
        costs.commission_rate,
        costs.buy_slippage_bps,
        costs.sell_slippage_bps,
        costs.buy_tax_rate,
        costs.sell_tax_rate,
    ];
    if values
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err(invalid_costs("all rates must be finite and non-negative"));
    }
    let (buy_rate, sell_rate) = costs.side_rates();
    if !buy_rate.is_finite() || !sell_rate.is_finite() {
        return Err(invalid_costs("combined side rates must be finite"));
    }
    Ok(())
}

fn read_prices(panel: &Panel) -> Result<Vec<Vec<Option<f64>>>, VectorError> {
    let session_count = panel.sessions.len();
    let row_count = panel
        .instruments
        .len()
        .checked_mul(session_count)
        .ok_or_else(|| invalid_input("panel dimensions overflow"))?;
    if panel.grid.height() != row_count {
        return Err(invalid_input(
            "panel grid height does not match instruments and sessions",
        ));
    }
    if panel.instruments.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid_input(
            "panel instruments must be strictly sorted and unique",
        ));
    }
    if panel
        .sessions
        .iter()
        .any(|session| session.ts_open >= session.ts_close)
        || panel
            .sessions
            .windows(2)
            .any(|pair| pair[0].session_date >= pair[1].session_date)
    {
        return Err(invalid_input(
            "panel sessions must be ordered with valid open and close times",
        ));
    }

    let ids = panel.grid.column("instrument_id")?.str()?;
    let session_indices = panel.grid.column("session_index")?.u32()?;
    let opens = panel.grid.column("open")?.f64()?;
    let mut prices = vec![vec![None; session_count]; panel.instruments.len()];
    for (instrument_index, (instrument, instrument_prices)) in
        panel.instruments.iter().zip(prices.iter_mut()).enumerate()
    {
        for (session_index, price_slot) in instrument_prices.iter_mut().enumerate() {
            let row = instrument_index * session_count + session_index;
            let instrument_id = ids
                .get(row)
                .ok_or_else(|| invalid_input("panel grid has a null instrument_id"))?;
            if instrument_id != instrument.to_string() {
                return Err(invalid_input(
                    "panel grid instrument order does not match panel instruments",
                ));
            }
            if session_indices.get(row) != u32::try_from(session_index).ok() {
                return Err(invalid_input(
                    "panel grid session order does not match panel sessions",
                ));
            }
            let open = opens.get(row);
            if open.is_some_and(|price| !price.is_finite() || price <= 0.0) {
                return Err(invalid_input(
                    "observed open prices must be finite and positive",
                ));
            }
            *price_slot = open;
        }
    }
    Ok(prices)
}

fn index_decisions<'a>(
    panel: &Panel,
    decisions: &'a [Decision],
) -> Result<Vec<(usize, &'a Decision)>, VectorError> {
    let mut indexed = Vec::with_capacity(decisions.len());
    let mut previous_index = None;
    for decision in decisions {
        let index = session_index(panel, &decision.decision_session)?;
        if previous_index.is_some_and(|previous| previous >= index) {
            return Err(invalid_input(
                "decisions must be in strictly increasing session order",
            ));
        }
        validate_target_weights(panel, &decision.targets)?;
        let mut ranked_ids = BTreeSet::new();
        for (instrument, score, rank) in &decision.ranked {
            if !panel.instruments.contains(instrument)
                || !score.is_finite()
                || *rank == 0
                || !ranked_ids.insert(instrument)
            {
                return Err(invalid_input(
                    "ranked decisions need known instruments, finite scores, positive ranks, and unique instruments",
                ));
            }
        }
        indexed.push((index, decision));
        previous_index = Some(index);
    }
    Ok(indexed)
}

fn index_events<'a>(
    panel: &Panel,
    events: &'a [ExecutionEvent],
) -> Result<Vec<(usize, &'a ExecutionEvent)>, VectorError> {
    let mut indexed = Vec::with_capacity(events.len());
    let mut attempt_indices = BTreeSet::new();
    let mut pending_at_end_count = 0;
    for event in events {
        match event {
            ExecutionEvent::Deferred {
                decision_session,
                attempt_session,
                blocked,
            } => {
                validate_event_sessions(panel, decision_session, attempt_session)?;
                let index = session_index(panel, attempt_session)?;
                if !attempt_indices.insert(index)
                    || blocked
                        .iter()
                        .any(|instrument| !panel.instruments.contains(instrument))
                {
                    return Err(invalid_input(
                        "execution events must have unique valid sessions",
                    ));
                }
                indexed.push((index, event));
            }
            ExecutionEvent::Executed {
                decision_session,
                attempt_session,
                applied,
                skipped_buys,
            } => {
                validate_event_sessions(panel, decision_session, attempt_session)?;
                let index = session_index(panel, attempt_session)?;
                if !attempt_indices.insert(index)
                    || skipped_buys
                        .iter()
                        .any(|instrument| !panel.instruments.contains(instrument))
                {
                    return Err(invalid_input(
                        "execution events must have unique valid sessions",
                    ));
                }
                validate_applied_weights(panel, applied)?;
                indexed.push((index, event));
            }
            ExecutionEvent::PendingAtEnd {
                decision_session,
                targets,
            } => {
                pending_at_end_count += 1;
                session_index(panel, decision_session)?;
                validate_target_weights(panel, targets)?;
                if pending_at_end_count > 1 {
                    return Err(invalid_input(
                        "there can be at most one pending-at-end event",
                    ));
                }
                indexed.push((panel.sessions.len(), event));
            }
        }
    }
    indexed.sort_by_key(|(index, _)| *index);
    Ok(indexed)
}

fn validate_event_sessions(
    panel: &Panel,
    decision_session: &PanelSession,
    attempt_session: &PanelSession,
) -> Result<(), VectorError> {
    let decision_index = session_index(panel, decision_session)?;
    let attempt_index = session_index(panel, attempt_session)?;
    if attempt_index <= decision_index {
        return Err(invalid_input(
            "an execution attempt must follow its decision session",
        ));
    }
    Ok(())
}

fn validate_event_decision(event: &ExecutionEvent, pending: &Decision) -> Result<(), VectorError> {
    let decision_session = match event {
        ExecutionEvent::Deferred {
            decision_session, ..
        }
        | ExecutionEvent::Executed {
            decision_session, ..
        } => decision_session,
        ExecutionEvent::PendingAtEnd { .. } => {
            return Err(invalid_input(
                "pending-at-end is not a session execution event",
            ));
        }
    };
    if decision_session != &pending.decision_session {
        return Err(invalid_input(
            "execution event does not match the pending decision",
        ));
    }
    Ok(())
}

fn validate_skipped_buys(
    pending: &Decision,
    applied: &BTreeMap<InstrumentId, f64>,
    skipped_buys: &BTreeSet<InstrumentId>,
) -> Result<(), VectorError> {
    if applied
        .keys()
        .any(|instrument| skipped_buys.contains(instrument))
        || applied
            .iter()
            .any(|(instrument, weight)| pending.targets.get(instrument) != Some(weight))
        || skipped_buys.iter().any(|instrument| {
            pending
                .targets
                .get(instrument)
                .is_none_or(|weight| *weight <= 0.0)
        })
    {
        return Err(invalid_input(
            "executed and skipped legs do not match the pending targets",
        ));
    }
    for (instrument, weight) in &pending.targets {
        if *weight > 0.0 && !applied.contains_key(instrument) && !skipped_buys.contains(instrument)
        {
            return Err(invalid_input(
                "each positive target must be applied or recorded as a skipped buy",
            ));
        }
    }
    Ok(())
}

fn validate_target_weights(
    panel: &Panel,
    targets: &BTreeMap<InstrumentId, f64>,
) -> Result<(), VectorError> {
    validate_applied_weights(panel, targets)?;
    Ok(())
}

fn validate_applied_weights(
    panel: &Panel,
    weights: &BTreeMap<InstrumentId, f64>,
) -> Result<(), VectorError> {
    if weights.iter().any(|(instrument, weight)| {
        !panel.instruments.contains(instrument) || !weight.is_finite() || *weight < 0.0
    }) {
        return Err(invalid_input(
            "weights must have known instruments and finite non-negative values",
        ));
    }
    let total = weights.values().sum::<f64>();
    if !total.is_finite() || total > 1.0 + RETURN_TOLERANCE {
        return Err(invalid_input("portfolio weights must sum to at most 1"));
    }
    Ok(())
}

fn session_index(panel: &Panel, session: &PanelSession) -> Result<usize, VectorError> {
    let index = panel
        .sessions
        .binary_search_by_key(&session.session_date, |candidate| candidate.session_date)
        .map_err(|_| invalid_input(format!("unknown session {}", session.session_date)))?;
    if panel.sessions[index] != *session {
        return Err(invalid_input(format!(
            "session {} has inconsistent timestamps",
            session.session_date
        )));
    }
    Ok(index)
}

fn assumptions(availability_assumption: AvailabilityAssumption) -> Vec<String> {
    [
        "proportional transaction costs",
        "no minimum commission or lot size",
        "raw open-to-open returns",
        "Static Universe is not point-in-time",
        "conservative deferral when a held instrument is unavailable",
        "weight-based Vector NAV is not a cash-and-quantity account ledger",
        "long-only unlevered weights; residual cash earns zero return",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain([format!(
        "availability assumption: {}",
        availability_assumption.canonical_str()
    )])
    .collect()
}

fn invalid_input(message: impl Into<String>) -> VectorError {
    VectorError::InvalidInput(message.into())
}

fn invalid_costs(message: impl Into<String>) -> VectorError {
    VectorError::InvalidCosts(message.into())
}
