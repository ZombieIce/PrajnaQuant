use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use chrono::NaiveDate;
use polars::prelude::{DataFrame, PolarsError};
use prajna_domain::InstrumentId;

use crate::{
    Panel, PanelSession,
    factor::AvailabilityAssumption,
    factor::{Factor, FactorStatus, values_schema},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StrategyCapability {
    Vectorizable,
    EventDriven,
    OrderBookDriven,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankDirection {
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weighting {
    EqualWeight,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UnfilledEntry {
    #[default]
    Skip,
    Retry,
}

#[derive(Debug, Clone)]
pub struct TopKRank {
    pub score: Factor,
    pub direction: RankDirection,
    pub top_k: u32,
    pub rebalance_every: u32,
    pub weighting: Weighting,
    pub unfilled_entry: UnfilledEntry,
    pub capabilities: BTreeSet<StrategyCapability>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub decision_session: PanelSession,
    pub ranked: Vec<(InstrumentId, f64, u32)>,
    pub targets: BTreeMap<InstrumentId, f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionEvent {
    Deferred {
        decision_session: PanelSession,
        attempt_session: PanelSession,
        blocked: BTreeSet<InstrumentId>,
    },
    Executed {
        decision_session: PanelSession,
        attempt_session: PanelSession,
        applied: BTreeMap<InstrumentId, f64>,
        skipped_buys: BTreeSet<InstrumentId>,
    },
    RetryableExecution {
        decision_session: PanelSession,
        attempt_session: PanelSession,
        applied: BTreeMap<InstrumentId, f64>,
        skipped_buys: BTreeSet<InstrumentId>,
        is_retry: bool,
    },
    PendingAtEnd {
        decision_session: PanelSession,
        targets: BTreeMap<InstrumentId, f64>,
    },
}

pub fn execute(
    decisions: &[Decision],
    sessions: &[PanelSession],
    executable: impl Fn(&InstrumentId, &PanelSession) -> bool,
) -> Vec<ExecutionEvent> {
    execute_with_policy(decisions, sessions, UnfilledEntry::Skip, executable)
}

/// Target-level projection without cash sizing; engines use `ExecutionState` with realized holdings.
pub fn execute_with_policy(
    decisions: &[Decision],
    sessions: &[PanelSession],
    unfilled_entry: UnfilledEntry,
    executable: impl Fn(&InstrumentId, &PanelSession) -> bool,
) -> Vec<ExecutionEvent> {
    let decisions_by_session = decisions
        .iter()
        .map(|decision| (decision.decision_session.session_date, decision))
        .collect::<BTreeMap<_, _>>();
    let mut held = BTreeSet::new();
    let mut state = ExecutionState::new(unfilled_entry);
    let mut events = Vec::new();

    for session in sessions {
        if let Some(event) = state.attempt(session, &held, &executable) {
            match &event {
                ExecutionEvent::Executed { applied, .. }
                | ExecutionEvent::RetryableExecution {
                    applied,
                    is_retry: false,
                    ..
                } => {
                    held = positive_legs(applied);
                }
                ExecutionEvent::RetryableExecution {
                    applied,
                    is_retry: true,
                    ..
                } => {
                    held.extend(positive_legs(applied));
                }
                _ => {}
            }
            events.push(event);
        }
        if let Some(decision) = decisions_by_session.get(&session.session_date) {
            state.decide(decision);
        }
    }
    if let Some(event) = state.pending_at_end() {
        events.push(event);
    }
    events
}

fn positive_legs(weights: &BTreeMap<InstrumentId, f64>) -> BTreeSet<InstrumentId> {
    weights
        .iter()
        .filter(|(_, weight)| **weight > 0.0)
        .map(|(instrument, _)| instrument.clone())
        .collect()
}

/// Shared decision state. Engines supply realized holdings before each open attempt.
pub struct ExecutionState {
    unfilled_entry: UnfilledEntry,
    pending: Option<Decision>,
    missing_entries: Option<BTreeMap<InstrumentId, f64>>,
}

impl ExecutionState {
    pub fn new(unfilled_entry: UnfilledEntry) -> Self {
        Self {
            unfilled_entry,
            pending: None,
            missing_entries: None,
        }
    }

    pub fn attempt(
        &mut self,
        session: &PanelSession,
        held: &BTreeSet<InstrumentId>,
        executable: impl Fn(&InstrumentId, &PanelSession) -> bool,
    ) -> Option<ExecutionEvent> {
        if let Some(decision) = &self.pending {
            let blocked = held
                .iter()
                .filter(|instrument| !executable(instrument, session))
                .cloned()
                .collect::<BTreeSet<_>>();
            if blocked.is_empty() {
                let mut applied = BTreeMap::new();
                let mut skipped_buys = BTreeSet::new();
                let is_retry = self.missing_entries.is_some();
                let targets = self.missing_entries.as_ref().unwrap_or(&decision.targets);
                for (instrument, weight) in targets {
                    if executable(instrument, session) {
                        applied.insert(instrument.clone(), *weight);
                    } else if *weight > 0.0 || self.unfilled_entry == UnfilledEntry::Skip {
                        skipped_buys.insert(instrument.clone());
                    }
                }
                if self.unfilled_entry == UnfilledEntry::Retry {
                    let remaining = targets
                        .iter()
                        .filter(|(instrument, weight)| {
                            **weight > 0.0 && skipped_buys.contains(*instrument)
                        })
                        .map(|(instrument, weight)| (instrument.clone(), *weight))
                        .collect::<BTreeMap<_, _>>();
                    let event = ExecutionEvent::RetryableExecution {
                        decision_session: decision.decision_session.clone(),
                        attempt_session: session.clone(),
                        applied,
                        skipped_buys,
                        is_retry,
                    };
                    if remaining.is_empty() {
                        self.pending = None;
                        self.missing_entries = None;
                    } else {
                        self.missing_entries = Some(remaining);
                    }
                    Some(event)
                } else {
                    let event = ExecutionEvent::Executed {
                        decision_session: decision.decision_session.clone(),
                        attempt_session: session.clone(),
                        applied,
                        skipped_buys,
                    };
                    self.pending = None;
                    Some(event)
                }
            } else {
                Some(ExecutionEvent::Deferred {
                    decision_session: decision.decision_session.clone(),
                    attempt_session: session.clone(),
                    blocked,
                })
            }
        } else {
            None
        }
    }

    pub fn decide(&mut self, decision: &Decision) {
        self.pending = Some(decision.clone());
        self.missing_entries = None;
    }

    pub fn pending_at_end(&self) -> Option<ExecutionEvent> {
        self.pending
            .as_ref()
            .map(|decision| ExecutionEvent::PendingAtEnd {
                decision_session: decision.decision_session.clone(),
                targets: self
                    .missing_entries
                    .clone()
                    .unwrap_or_else(|| decision.targets.clone()),
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    UnsupportedCapability,
    InvalidTopK,
    InvalidParameters(String),
    InvalidRebalanceEvery,
    InvalidSessions(String),
    InvalidScoreValues(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedCapability => {
                formatter.write_str("strategy does not support the Vector Engine")
            }
            Self::InvalidParameters(message) => {
                write!(formatter, "invalid strategy parameters: {message}")
            }
            Self::InvalidTopK => formatter.write_str("top_k must be at least 1"),
            Self::InvalidRebalanceEvery => {
                formatter.write_str("rebalance_every must be at least 1")
            }
            Self::InvalidSessions(message) => {
                write!(formatter, "invalid Venue Sessions: {message}")
            }
            Self::InvalidScoreValues(message) => {
                write!(formatter, "invalid Factor Values: {message}")
            }
        }
    }
}

impl Error for EngineError {}

impl From<PolarsError> for EngineError {
    fn from(error: PolarsError) -> Self {
        Self::InvalidScoreValues(error.to_string())
    }
}

pub struct VectorEngine;

impl VectorEngine {
    fn validate_top_k(strategy: &TopKRank) -> Result<(), EngineError> {
        if strategy.top_k == 0 {
            return Err(EngineError::InvalidTopK);
        }
        if strategy.rebalance_every == 0 {
            return Err(EngineError::InvalidRebalanceEvery);
        }
        if !strategy
            .capabilities
            .contains(&StrategyCapability::Vectorizable)
        {
            return Err(EngineError::UnsupportedCapability);
        }
        Ok(())
    }
}

impl TopKRank {
    pub fn execute(
        &self,
        decisions: &[Decision],
        sessions: &[PanelSession],
        executable: impl Fn(&InstrumentId, &PanelSession) -> bool,
    ) -> Vec<ExecutionEvent> {
        execute_with_policy(decisions, sessions, self.unfilled_entry, executable)
    }

    /// Rank available Factor Values at each scheduled Venue Session close.
    pub fn decide(
        &self,
        score_values: &DataFrame,
        sessions: &[PanelSession],
    ) -> Result<Vec<Decision>, EngineError> {
        VectorEngine::validate_top_k(self)?;
        if score_values.schema().as_ref() != &values_schema().polars {
            return Err(EngineError::InvalidScoreValues(
                "input must use the Factor Values v1 schema".into(),
            ));
        }

        let mut session_by_date = BTreeMap::new();
        let mut previous_date = None;
        for (index, session) in sessions.iter().enumerate() {
            if session.ts_open >= session.ts_close {
                return Err(EngineError::InvalidSessions(format!(
                    "session {} has an invalid time range",
                    session.session_date
                )));
            }
            if previous_date.is_some_and(|date| date >= session.session_date) {
                return Err(EngineError::InvalidSessions(
                    "sessions must be in strictly increasing date order".into(),
                ));
            }
            previous_date = Some(session.session_date);
            let days = date32(session.session_date)?;
            session_by_date.insert(days, index);
        }

        let ids = score_values.column("instrument_id")?.str()?;
        let dates = score_values.column("session_date")?.date()?.physical();
        let values = score_values.column("value")?.f64()?;
        let available_at = score_values.column("available_at")?.datetime()?.physical();
        let statuses = score_values.column("status")?.str()?;
        let mut seen = BTreeSet::new();
        let mut scores_by_session = vec![Vec::new(); sessions.len()];

        for row in 0..score_values.height() {
            let instrument_id = ids
                .get(row)
                .ok_or_else(|| EngineError::InvalidScoreValues("null instrument_id".into()))?;
            let id = instrument_id
                .parse::<InstrumentId>()
                .map_err(|error| EngineError::InvalidScoreValues(error.to_string()))?;
            let date = dates
                .get(row)
                .ok_or_else(|| EngineError::InvalidScoreValues("null session_date".into()))?;
            let session_index = *session_by_date.get(&date).ok_or_else(|| {
                EngineError::InvalidScoreValues(format!(
                    "session date {date} is not present in the Venue Sessions"
                ))
            })?;
            if !seen.insert((id.clone(), session_index)) {
                return Err(EngineError::InvalidScoreValues(
                    "duplicate instrument/session key".into(),
                ));
            }
            let status = statuses
                .get(row)
                .ok_or_else(|| EngineError::InvalidScoreValues("null status".into()))?
                .parse::<FactorStatus>()
                .map_err(|error| EngineError::InvalidScoreValues(error.to_string()))?;
            let value = values.get(row);
            if status != FactorStatus::Ok {
                if value.is_some() {
                    return Err(EngineError::InvalidScoreValues(
                        "non-ok score must have a null value".into(),
                    ));
                }
                continue;
            }

            let score = value.filter(|score| score.is_finite()).ok_or_else(|| {
                EngineError::InvalidScoreValues("ok score must have a finite value".into())
            })?;
            let available_at = available_at.get(row).ok_or_else(|| {
                EngineError::InvalidScoreValues("ok score must have available_at".into())
            })?;
            // A known score arriving after the close is not usable for that decision.
            if available_at <= sessions[session_index].ts_close.as_unix_nanos() {
                scores_by_session[session_index].push((instrument_id.to_owned(), id, score));
            }
        }

        let Some(first_decision_index) = scores_by_session
            .iter()
            .position(|scores| !scores.is_empty())
        else {
            return Ok(Vec::new());
        };
        let rebalance_every = usize::try_from(self.rebalance_every)
            .map_err(|_| EngineError::InvalidRebalanceEvery)?;
        let mut decisions = Vec::new();

        for (session_index, scores) in scores_by_session
            .into_iter()
            .enumerate()
            .skip(first_decision_index)
        {
            if (session_index - first_decision_index) % rebalance_every != 0 {
                continue;
            }
            let mut scores = scores;
            scores.sort_by(|(left_key, _, left_score), (right_key, _, right_score)| {
                if left_score == right_score {
                    left_key.cmp(right_key)
                } else {
                    right_score.total_cmp(left_score)
                }
            });
            let selected_count = scores.len().min(self.top_k as usize);
            let targets = if selected_count == 0 {
                BTreeMap::new()
            } else {
                let weight = 1.0 / selected_count as f64;
                scores
                    .iter()
                    .take(selected_count)
                    .map(|(_, id, _)| (id.clone(), weight))
                    .collect()
            };
            let ranked = scores
                .into_iter()
                .enumerate()
                .map(|(index, (_, id, score))| {
                    let rank = u32::try_from(index + 1)
                        .map_err(|_| EngineError::InvalidScoreValues("rank exceeds u32".into()))?;
                    Ok((id, score, rank))
                })
                .collect::<Result<_, EngineError>>()?;
            decisions.push(Decision {
                decision_session: sessions[session_index].clone(),
                ranked,
                targets,
            });
        }

        Ok(decisions)
    }
}

fn date32(date: NaiveDate) -> Result<i32, EngineError> {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("Unix epoch is a valid date");
    i32::try_from((date - epoch).num_days()).map_err(|_| {
        EngineError::InvalidSessions(format!("session date {date} exceeds Date32 range"))
    })
}

/// Closed Vector strategy family. Experiment Run Spec integration is a separate contract.
#[derive(Debug, Clone)]
pub enum VectorStrategy {
    TopKRank(TopKRank),
    BuyAndHold(BuyAndHoldParams),
    MaCrossover(MaCrossoverParams),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuyAndHoldParams {
    pub params_version: u32,
}

impl Default for BuyAndHoldParams {
    fn default() -> Self {
        Self { params_version: 1 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaCrossoverParams {
    pub params_version: u32,
    pub short_window: u32,
    pub long_window: u32,
}

impl Default for MaCrossoverParams {
    fn default() -> Self {
        Self {
            params_version: 1,
            short_window: 20,
            long_window: 60,
        }
    }
}

impl VectorEngine {
    pub fn validate(strategy: &VectorStrategy) -> Result<(), EngineError> {
        match strategy {
            VectorStrategy::TopKRank(params) => Self::validate_top_k(params),
            VectorStrategy::BuyAndHold(params) if params.params_version == 1 => Ok(()),
            VectorStrategy::MaCrossover(params)
                if params.params_version == 1
                    && params.short_window > 0
                    && params.short_window < params.long_window =>
            {
                Ok(())
            }
            _ => Err(EngineError::InvalidParameters(
                "unsupported version or MA windows".into(),
            )),
        }
    }
}

impl VectorStrategy {
    pub fn params_version(&self) -> u32 {
        match self {
            Self::TopKRank(_) => 1,
            Self::BuyAndHold(params) => params.params_version,
            Self::MaCrossover(params) => params.params_version,
        }
    }

    pub fn unfilled_entry(&self) -> UnfilledEntry {
        match self {
            Self::TopKRank(params) => params.unfilled_entry,
            Self::BuyAndHold(_) => UnfilledEntry::Retry,
            Self::MaCrossover(_) => UnfilledEntry::Skip,
        }
    }

    pub fn decide(
        &self,
        scores: &DataFrame,
        sessions: &[PanelSession],
    ) -> Result<Vec<Decision>, EngineError> {
        match self {
            Self::TopKRank(params) => params.decide(scores, sessions),
            _ => Err(EngineError::InvalidParameters(
                "this strategy requires decide_panel".into(),
            )),
        }
    }

    /// Signals use only session-close inputs; execution remains next-session open.
    pub fn decide_panel(
        &self,
        panel: &Panel,
        members: &[InstrumentId],
        assumption: AvailabilityAssumption,
    ) -> Result<Vec<Decision>, EngineError> {
        VectorEngine::validate(self)?;
        validate_members(panel, members)?;
        match self {
            Self::TopKRank(params) => {
                let scores = crate::compute(panel, members, &params.score, assumption)
                    .map_err(|error| EngineError::InvalidScoreValues(error.to_string()))?;
                params.decide(&scores, &panel.sessions)
            }
            Self::BuyAndHold(_) => Ok(panel
                .sessions
                .first()
                .map(|session| Decision {
                    decision_session: session.clone(),
                    ranked: Vec::new(),
                    targets: members
                        .iter()
                        .map(|id| (id.clone(), 1.0 / members.len() as f64))
                        .collect(),
                })
                .into_iter()
                .collect()),
            Self::MaCrossover(params) => {
                super::ma_crossover::decide(panel, members, params, assumption)
            }
        }
    }

    pub fn execute(
        &self,
        decisions: &[Decision],
        sessions: &[PanelSession],
        executable: impl Fn(&InstrumentId, &PanelSession) -> bool,
    ) -> Vec<ExecutionEvent> {
        execute_with_policy(decisions, sessions, self.unfilled_entry(), executable)
    }
}

pub(crate) fn validate_members(panel: &Panel, members: &[InstrumentId]) -> Result<(), EngineError> {
    if members.is_empty()
        || members.iter().collect::<BTreeSet<_>>().len() != members.len()
        || members.iter().any(|id| !panel.instruments.contains(id))
    {
        return Err(EngineError::InvalidParameters(
            "Universe must be nonempty, unique and present in Panel".into(),
        ));
    }
    let mut previous = None;
    for session in &panel.sessions {
        if session.ts_open >= session.ts_close
            || previous.is_some_and(|date| date >= session.session_date)
        {
            return Err(EngineError::InvalidSessions(
                "invalid or unordered Sessions".into(),
            ));
        }
        previous = Some(session.session_date);
    }
    Ok(())
}
