use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use polars::prelude::PolarsError;
use prajna_account::Side;
use prajna_domain::{FixedPointError, InstrumentId, Notional, Price, Quantity};
use serde::{Deserialize, Serialize};

use crate::{
    Panel, PanelSession,
    corporate_actions::{
        CorporateActionCompleteness, CorporateActionCoverage, CorporateActionLedgerEntry,
        CorporateActionProcessor, CorporateActionSchedule, ReturnBasis, apply_transition_to_vector,
    },
    factor::AvailabilityAssumption,
    fast_event::{FastEventCosts, Marks, PriceField},
    parity_sizing::{ParityPlan, ParitySizingState, target_plan},
    strategy::{Decision, ExecutionEvent, ExecutionState, UnfilledEntry, execution_decimal},
    terminal_settlement::{TerminalSettlementEntry, TerminalSettlementSchedule},
};

const RETURN_TOLERANCE: f64 = 1e-12;
pub const CORPORATE_ACTION_SEMANTIC_VERSION: &str = "vector@2";
pub const TERMINAL_SETTLEMENT_ENGINE_VERSION: &str = "vector@3";
/// PIT Vector shares the corrected full-target cash-feasible sizing plan.
pub const PIT_SIZING_SEMANTIC_VERSION: &str = "vector@4";
pub const CORPORATE_ACTION_PIT_SIZING_SEMANTIC_VERSION: &str = "vector@5";
pub const TERMINAL_SETTLEMENT_PIT_SIZING_ENGINE_VERSION: &str = "vector@6";

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risk_exits: Vec<crate::pit_strategy::RiskExitAttempt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_risk_exits: Vec<InstrumentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pit_initial_capital: Option<Notional>,
    #[serde(default, skip_serializing_if = "is_price_return")]
    pub return_basis: ReturnBasis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corporate_action_schedule_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corporate_action_dividend_tax_rate: Option<Notional>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corporate_action_coverage: Vec<CorporateActionCoverage>,
    #[serde(default, skip_serializing_if = "is_unreported_completeness")]
    pub corporate_action_completeness: CorporateActionCompleteness,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corporate_action_ledger: Vec<CorporateActionLedgerEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminal_settlements: Vec<TerminalSettlementEntry>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portfolio_components: Option<VectorPortfolioComponents>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portfolio_components_close: Option<VectorPortfolioComponents>,
    pub valuation_carried: Vec<InstrumentId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorPortfolioComponents {
    pub available_cash_weight: f64,
    pub dividend_receivable_weight: f64,
    pub liquidation_receivable_weight: f64,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pit_sizing: Option<VectorPitSizing>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorPitSizing {
    pub equity: Notional,
    pub fees: Notional,
    pub desired_quantities: BTreeMap<InstrumentId, Quantity>,
    pub required_sides: BTreeMap<InstrumentId, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cash_adjustments: Vec<VectorCashAdjustment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cash_residue: Option<Notional>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorCashAdjustment {
    pub instrument_id: InstrumentId,
    pub candidate_quantity: Quantity,
    pub actual_quantity: Quantity,
    pub candidate_principal: Notional,
    pub actual_principal: Notional,
    pub principal_budget: Notional,
    pub reserved_fee: Notional,
    pub cash_residue: Notional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorExecutionKind {
    Executed,
    Deferred,
    EntriesRetried,
    RetryDeferred,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorPending {
    pub decision_session: String,
    pub targets: BTreeMap<InstrumentId, f64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retry_entries: bool,
}

#[derive(Debug)]
pub enum VectorError {
    InvalidCosts(String),
    InvalidInput(String),
    Polars(PolarsError),
}

impl From<FixedPointError> for VectorError {
    fn from(error: FixedPointError) -> Self {
        invalid_input(format!("PIT scale-18 sizing: {error}"))
    }
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
    run_vector_inner(
        panel,
        decisions,
        events,
        costs,
        availability_assumption,
        None,
        None,
        None,
        None,
    )
}

pub fn run_vector_with_policy(
    panel: &Panel,
    decisions: &[Decision],
    unfilled_entry: UnfilledEntry,
    costs: &VectorCosts,
    availability_assumption: AvailabilityAssumption,
    executable: impl Fn(&InstrumentId, &PanelSession) -> bool,
) -> Result<VectorResult, VectorError> {
    let mut state = ExecutionState::new(unfilled_entry);
    let mut generate = |session: &PanelSession,
                        held: &BTreeSet<InstrumentId>,
                        decision: Option<&Decision>,
                        at_end: bool| {
        if let Some(decision) = decision {
            state.decide(decision);
            None
        } else if at_end {
            state.pending_at_end()
        } else {
            state.attempt(session, held, &executable)
        }
    };
    run_vector_inner(
        panel,
        decisions,
        &[],
        costs,
        availability_assumption,
        Some(&mut generate),
        None,
        None,
        None,
    )
}

pub fn run_vector_pit(
    panel: &Panel,
    decisions: &[Decision],
    market: &crate::etf_market::DerivedMarket,
    risk: &crate::pit_strategy::RiskSignals,
    costs: &VectorCosts,
    initial_capital: Notional,
) -> Result<VectorResult, VectorError> {
    if initial_capital <= Notional::ZERO {
        return Err(invalid_input("PIT initial_capital must be positive"));
    }
    crate::pit_strategy::validate_execution_panel(panel, market)
        .map_err(|e| invalid_input(e.to_string()))?;
    run_vector_inner(
        panel,
        decisions,
        &[],
        costs,
        AvailabilityAssumption::None,
        None,
        Some((market, risk, initial_capital)),
        None,
        None,
    )
}

pub fn run_vector_pit_with_actions(
    panel: &Panel,
    decisions: &[Decision],
    market: &crate::etf_market::DerivedMarket,
    risk: &crate::pit_strategy::RiskSignals,
    costs: &VectorCosts,
    initial_capital: Notional,
    actions: &CorporateActionSchedule,
) -> Result<VectorResult, VectorError> {
    if initial_capital <= Notional::ZERO {
        return Err(invalid_input("PIT initial_capital must be positive"));
    }
    crate::pit_strategy::validate_execution_panel(panel, market)
        .map_err(|e| invalid_input(e.to_string()))?;
    run_vector_inner(
        panel,
        decisions,
        &[],
        costs,
        AvailabilityAssumption::None,
        None,
        Some((market, risk, initial_capital)),
        Some(actions),
        None,
    )
}

pub fn run_vector_pit_with_terminal_settlement(
    panel: &Panel,
    decisions: &[Decision],
    market: &crate::etf_market::DerivedMarket,
    risk: &crate::pit_strategy::RiskSignals,
    costs: &VectorCosts,
    initial_capital: Notional,
    actions: Option<&CorporateActionSchedule>,
) -> Result<VectorResult, VectorError> {
    if initial_capital <= Notional::ZERO {
        return Err(invalid_input("PIT initial_capital must be positive"));
    }
    crate::pit_strategy::validate_execution_panel(panel, market)
        .map_err(|e| invalid_input(e.to_string()))?;
    let schedule = TerminalSettlementSchedule::from_market(market, panel).map_err(invalid_input)?;
    run_vector_inner(
        panel,
        decisions,
        &[],
        costs,
        AvailabilityAssumption::None,
        None,
        Some((market, risk, initial_capital)),
        actions,
        Some(&schedule),
    )
}

type EventGenerator<'a> = dyn FnMut(&PanelSession, &BTreeSet<InstrumentId>, Option<&Decision>, bool) -> Option<ExecutionEvent>
    + 'a;

struct SizedVectorTarget {
    weights: BTreeMap<InstrumentId, f64>,
    turnover: f64,
    cost: f64,
    blocked_sells: BTreeSet<InstrumentId>,
    skipped_buys: BTreeSet<InstrumentId>,
    next: Option<ParitySizingState>,
    plan: ParityPlan,
}

fn pit_target_plan(
    panel: &Panel,
    market: &crate::etf_market::DerivedMarket,
    session: &PanelSession,
    current: &ParitySizingState,
    targets: &BTreeMap<InstrumentId, f64>,
    prices: &BTreeMap<InstrumentId, Price>,
    costs: FastEventCosts,
) -> Result<SizedVectorTarget, VectorError> {
    let equity = current.equity(prices).map_err(sizing_error)?;
    let receivables =
        crate::fast_event::add(current.dividend_receivable, current.liquidation_receivable)
            .map_err(sizing_error)?;
    let mut skipped_buys = targets
        .iter()
        .filter(|(id, w)| **w > 0.0 && !prices.contains_key(*id))
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    loop {
        let plan = target_plan(
            equity,
            current.cash,
            receivables,
            &current.positions,
            targets,
            prices,
            costs,
            &skipped_buys,
        )
        .map_err(sizing_error)?;
        let mut new_skipped = BTreeSet::new();
        let mut blocked_sells = BTreeSet::new();
        for leg in &plan.legs {
            match leg.required_side().map_err(sizing_error)? {
                Some(Side::Buy)
                    if !crate::pit_strategy::permits(
                        market,
                        panel,
                        &leg.instrument,
                        session,
                        true,
                    ) =>
                {
                    new_skipped.insert(leg.instrument.clone());
                }
                Some(Side::Sell)
                    if !crate::pit_strategy::permits(
                        market,
                        panel,
                        &leg.instrument,
                        session,
                        false,
                    ) =>
                {
                    blocked_sells.insert(leg.instrument.clone());
                }
                _ => {}
            }
        }
        if !new_skipped.is_empty() {
            skipped_buys.extend(new_skipped);
            continue;
        }
        let next = if blocked_sells.is_empty() {
            Some(current.preview(&plan).map_err(sizing_error)?)
        } else {
            None
        };
        let weights = next
            .as_ref()
            .unwrap_or(current)
            .weights(prices)
            .map_err(sizing_error)?;
        return Ok(SizedVectorTarget {
            weights,
            turnover: decimal_float(plan.turnover)?,
            cost: decimal_float(plan.total_fees)? / decimal_float(equity)?,
            blocked_sells,
            skipped_buys,
            next,
            plan,
        });
    }
}

fn decimal_float(value: Notional) -> Result<f64, VectorError> {
    value
        .to_string()
        .parse()
        .map_err(|error| invalid_input(format!("PIT decimal report: {error}")))
}

fn sizing_error(error: crate::fast_event::FastEventError) -> VectorError {
    invalid_input(format!("PIT monetary sizing: {error}"))
}

fn sizing_diagnostic(plan: &ParityPlan) -> Result<VectorPitSizing, VectorError> {
    let mut sides = BTreeMap::new();
    for leg in &plan.legs {
        if let Some(side) = leg.required_side().map_err(sizing_error)? {
            sides.insert(
                leg.instrument.clone(),
                if side == Side::Buy {
                    "buy".into()
                } else {
                    "sell".into()
                },
            );
        }
    }
    Ok(VectorPitSizing {
        equity: plan.equity,
        fees: plan.total_fees,
        desired_quantities: plan
            .legs
            .iter()
            .map(|leg| (leg.instrument.clone(), leg.desired))
            .collect(),
        required_sides: sides,
        cash_adjustments: plan
            .cash_adjustments
            .iter()
            .map(|adjustment| VectorCashAdjustment {
                instrument_id: adjustment.instrument.clone(),
                candidate_quantity: adjustment.candidate_quantity,
                actual_quantity: adjustment.actual_quantity,
                candidate_principal: adjustment.candidate_principal,
                actual_principal: adjustment.actual_principal,
                principal_budget: adjustment.principal_budget,
                reserved_fee: adjustment.reserved_fee,
                cash_residue: adjustment.cash_residue,
            })
            .collect(),
        cash_residue: (!plan.cash_adjustments.is_empty()).then_some(plan.cash_residue),
    })
}

fn monetary_costs(costs: &VectorCosts) -> Result<FastEventCosts, VectorError> {
    let bps = execution_decimal(10000.0)?;
    Ok(FastEventCosts {
        commission_rate: execution_decimal(costs.commission_rate)?,
        buy_slippage_rate: execution_decimal(costs.buy_slippage_bps)?.checked_div_decimal(bps)?,
        sell_slippage_rate: execution_decimal(costs.sell_slippage_bps)?.checked_div_decimal(bps)?,
        buy_tax_rate: execution_decimal(costs.buy_tax_rate)?,
        sell_tax_rate: execution_decimal(costs.sell_tax_rate)?,
        minimum_commission: Notional::ZERO,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_vector_inner(
    panel: &Panel,
    decisions: &[Decision],
    events: &[ExecutionEvent],
    costs: &VectorCosts,
    availability_assumption: AvailabilityAssumption,
    mut generate: Option<&mut EventGenerator<'_>>,
    pit: Option<(
        &crate::etf_market::DerivedMarket,
        &crate::pit_strategy::RiskSignals,
        Notional,
    )>,
    actions: Option<&CorporateActionSchedule>,
    terminal_settlement: Option<&TerminalSettlementSchedule>,
) -> Result<VectorResult, VectorError> {
    validate_costs(costs)?;
    if (actions.is_some() || terminal_settlement.is_some()) && pit.is_none() {
        return Err(invalid_input(
            "PIT ledger extensions require the PIT Vector Engine entry point",
        ));
    }
    let mut action_processor = actions
        .map(|actions| {
            let processor = if terminal_settlement.is_some() {
                CorporateActionProcessor::new_ignoring_liquidations(actions, panel)
            } else {
                CorporateActionProcessor::new(actions, panel)
            };
            processor.map_err(|e| invalid_input(e.to_string()))
        })
        .transpose()?;
    let ledger_mode = action_processor.is_some() || terminal_settlement.is_some();
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
    let mut pending_targets = BTreeMap::new();
    let mut retrying = false;
    let mut latest_target_weights = BTreeMap::new();
    let mut nav = 1.0;
    let mut result_sessions = Vec::with_capacity(panel.sessions.len());
    let mut terminal_settlements = Vec::new();
    let mut result_executions = Vec::new();
    let mut risk_exits = Vec::new();
    let mut pit_state = pit.map(|_| ExecutionState::new(UnfilledEntry::Skip));
    let mut sizing_state = pit.map(|(_, _, capital)| ParitySizingState::new(capital));
    let sizing_costs = pit.map(|_| monetary_costs(costs)).transpose()?;
    let mut decimal_marks = Marks::default();
    let mut terminal_close_prices = BTreeMap::new();

    for (session_index, session) in panel.sessions.iter().enumerate() {
        let mut risk_turnover = 0.0;
        let mut risk_cost = 0.0;
        if pit.is_some() {
            decimal_marks
                .update(panel, session_index, PriceField::Open)
                .map_err(sizing_error)?;
        }
        if let Some(processor) = &mut action_processor {
            let book = sizing_state
                .as_mut()
                .ok_or_else(|| invalid_input("missing PIT sizing state"))?;
            let fresh_prices = decimal_marks.fresh_ids_on(session.session_date);
            let transitions = processor
                .opening(
                    session.session_date,
                    None,
                    &book.positions,
                    &decimal_marks.prices,
                    &fresh_prices,
                    false,
                )
                .map_err(|e| invalid_input(e.to_string()))?;
            for transition in &transitions {
                apply_transition_to_vector(transition, book).map_err(invalid_input)?;
            }
            processor.record(transitions);
            weights = book.weights(&decimal_marks.prices).map_err(sizing_error)?;
        }
        if let Some((market, risk, _)) = pit {
            let book = sizing_state
                .as_mut()
                .ok_or_else(|| invalid_input("missing PIT sizing state"))?;
            let before = book.equity(&decimal_marks.prices).map_err(sizing_error)?;
            let mut sold = Notional::ZERO;
            let mut fees = Notional::ZERO;
            let active = risk.active_before_open(session.ts_open);
            let terminal = action_processor
                .as_ref()
                .map(|processor| processor.terminal_instruments(session.session_date))
                .unwrap_or_default();
            let mut excluded = active.clone();
            excluded.extend(terminal.iter().cloned());
            if let Some(state) = &mut pit_state {
                state.exclude(&excluded);
            }
            pending_targets.retain(|id, _| !excluded.contains(id));
            for id in book.positions.keys().cloned().collect::<Vec<_>>() {
                if !active.contains(&id) || terminal.contains(&id) {
                    continue;
                }
                let executed = crate::pit_strategy::permits(market, panel, &id, session, false);
                risk_exits.push(crate::pit_strategy::RiskExitAttempt {
                    session_date: session.session_date,
                    instrument_id: id.clone(),
                    executed,
                });
                if executed {
                    let (principal, fee) = book
                        .risk_sale(
                            &id,
                            decimal_marks.prices[&id],
                            sizing_costs
                                .ok_or_else(|| invalid_input("missing PIT monetary costs"))?,
                        )
                        .map_err(sizing_error)?;
                    sold = crate::fast_event::add(sold, principal).map_err(sizing_error)?;
                    fees = crate::fast_event::add(fees, fee).map_err(sizing_error)?;
                }
            }
            risk_turnover = decimal_float(sold)? / decimal_float(before)?;
            risk_cost = decimal_float(fees)? / decimal_float(before)?;
            weights = book.weights(&decimal_marks.prices).map_err(sizing_error)?;
        }
        let mut sized_target = None;
        let generated = if let (Some(state), Some((market, risk, _))) = (&mut pit_state, pit) {
            state.exclude(&risk.active_before_open(session.ts_open));
            if let Some(request) = state.pending_execution() {
                let sized = pit_target_plan(
                    panel,
                    market,
                    session,
                    sizing_state
                        .as_ref()
                        .ok_or_else(|| invalid_input("missing PIT sizing state"))?,
                    &request.targets,
                    &decimal_marks.prices,
                    sizing_costs.ok_or_else(|| invalid_input("missing PIT monetary costs"))?,
                )?;
                let event = state.resolve_sized(
                    session,
                    sized.weights.clone(),
                    sized.blocked_sells.clone(),
                    sized.skipped_buys.clone(),
                );
                sized_target = Some(sized);
                event
            } else {
                None
            }
        } else {
            generate.as_mut().and_then(|generate| {
                generate(session, &weights.keys().cloned().collect(), None, false)
            })
        };
        let event = generated
            .as_ref()
            .or_else(|| events_by_index.get(&session_index).copied());
        let mut turnover = 0.0;
        let mut cost = 0.0;

        match (pending_decision, event) {
            (Some(pending), Some(event)) => {
                validate_event_decision(event, pending)?;
                match event {
                    ExecutionEvent::Deferred { blocked, .. } => {
                        if blocked.is_empty()
                            || blocked.iter().any(|instrument| {
                                sizing_state.as_ref().map_or_else(
                                    || weights.get(instrument).copied().unwrap_or(0.0) <= 0.0,
                                    |state| !state.positions.contains_key(instrument),
                                )
                            })
                        {
                            return Err(invalid_input(
                                "deferred event must block at least one currently held instrument",
                            ));
                        }
                        result_executions.push(VectorExecution {
                            session_date: session.session_date.to_string(),
                            kind: if retrying {
                                VectorExecutionKind::RetryDeferred
                            } else {
                                VectorExecutionKind::Deferred
                            },
                            blocked: blocked.clone(),
                            skipped_buys: BTreeSet::new(),
                            pit_sizing: sized_target
                                .as_ref()
                                .map(|s| sizing_diagnostic(&s.plan))
                                .transpose()?,
                        });
                    }
                    ExecutionEvent::Executed {
                        applied,
                        skipped_buys,
                        ..
                    }
                    | ExecutionEvent::RetryableExecution {
                        applied,
                        skipped_buys,
                        ..
                    } => {
                        let retains_entries =
                            matches!(event, ExecutionEvent::RetryableExecution { .. });
                        let is_retry = matches!(
                            event,
                            ExecutionEvent::RetryableExecution { is_retry: true, .. }
                        );
                        if is_retry != retrying {
                            return Err(invalid_input(
                                "execution mode does not match the pending target",
                            ));
                        }
                        validate_applied_weights(panel, applied)?;
                        if pit.is_none() {
                            validate_skipped_buys(&pending_targets, applied, skipped_buys)?;
                        }
                        if is_retry
                            && applied
                                .keys()
                                .any(|instrument| weights.contains_key(instrument))
                        {
                            return Err(invalid_input(
                                "an entry retry cannot rebalance a held leg",
                            ));
                        }
                        let required_prices = applied
                            .keys()
                            .chain(weights.keys().filter(|_| !is_retry))
                            .filter(|_| pit.is_none());
                        for instrument in required_prices {
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
                        if let Some(sized) = &sized_target {
                            turnover = sized.turnover;
                            cost = sized.cost;
                            sizing_state = sized.next.clone();
                        } else if is_retry {
                            (turnover, cost) = apply_entry_retry(&mut weights, applied, buy_rate)?;
                        } else {
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
                        }
                        if !turnover.is_finite() || !cost.is_finite() || cost >= 1.0 {
                            return Err(invalid_costs(
                                "an execution must have finite turnover and cost below 100%",
                            ));
                        }
                        if !is_retry {
                            weights = applied
                                .iter()
                                .filter(|(_, weight)| **weight > 0.0)
                                .map(|(instrument, weight)| (instrument.clone(), *weight))
                                .collect();
                        }
                        validate_applied_weights(panel, &weights)?;
                        pending_targets = if retains_entries {
                            pending_targets
                                .into_iter()
                                .filter(|(instrument, _)| skipped_buys.contains(instrument))
                                .collect()
                        } else {
                            BTreeMap::new()
                        };
                        retrying = !pending_targets.is_empty();
                        pending_decision = retrying.then_some(pending);
                        result_executions.push(VectorExecution {
                            session_date: session.session_date.to_string(),
                            kind: if is_retry {
                                VectorExecutionKind::EntriesRetried
                            } else {
                                VectorExecutionKind::Executed
                            },
                            blocked: BTreeSet::new(),
                            skipped_buys: skipped_buys.clone(),
                            pit_sizing: sized_target
                                .as_ref()
                                .map(|s| sizing_diagnostic(&s.plan))
                                .transpose()?,
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

        turnover = risk_turnover + turnover * (1.0 - risk_cost);
        cost = risk_cost + cost * (1.0 - risk_cost);
        let weights_after_execution = weights.clone();
        let portfolio_components = if ledger_mode {
            Some(vector_components(
                sizing_state
                    .as_ref()
                    .ok_or_else(|| invalid_input("missing PIT sizing state"))?,
                &decimal_marks.prices,
            )?)
        } else {
            None
        };
        let terminal_open_equity = if terminal_settlement.is_some() {
            Some(
                sizing_state
                    .as_ref()
                    .ok_or_else(|| invalid_input("missing PIT sizing state"))?
                    .equity(&decimal_marks.prices)
                    .map_err(sizing_error)?,
            )
        } else {
            None
        };
        if let Some(processor) = &mut action_processor {
            let book = sizing_state
                .as_mut()
                .ok_or_else(|| invalid_input("missing PIT sizing state"))?;
            let transitions = processor
                .closing(session.session_date, None, &book.positions)
                .map_err(|e| invalid_input(e.to_string()))?;
            for transition in &transitions {
                apply_transition_to_vector(transition, book).map_err(invalid_input)?;
            }
            processor.record(transitions);
        }
        let mut close_marks = Marks::default();
        let mut close_prices = decimal_marks.prices.clone();
        if terminal_settlement.is_some() {
            close_marks
                .update(panel, session_index, PriceField::Close)
                .map_err(sizing_error)?;
            close_prices.extend(close_marks.prices.clone());
            terminal_close_prices.extend(close_marks.prices.clone());
            close_prices.clone_from(&terminal_close_prices);
        }
        if let Some(schedule) = terminal_settlement {
            let book = sizing_state
                .as_mut()
                .ok_or_else(|| invalid_input("missing PIT sizing state"))?;
            let fresh_close = close_marks.fresh_ids_on(session.session_date);
            let instruments = schedule
                .instruments(session.session_date)
                .cloned()
                .unwrap_or_default();
            for instrument in instruments {
                if let Some(quantity) = book.positions.remove(&instrument) {
                    if !fresh_close.contains(&instrument) {
                        return Err(invalid_input(format!(
                            "terminal settlement for {instrument} on {} requires a fresh raw close",
                            session.session_date
                        )));
                    }
                    let price = close_marks.prices[&instrument];
                    let proceeds = crate::terminal_settlement::proceeds(quantity, price)
                        .map_err(invalid_input)?;
                    book.cash =
                        crate::fast_event::add(book.cash, proceeds).map_err(sizing_error)?;
                    terminal_settlements.push(TerminalSettlementEntry {
                        session_date: session.session_date,
                        portfolio_id: None,
                        instrument_id: instrument,
                        quantity,
                        price,
                        proceeds,
                    });
                }
            }
        }
        let portfolio_components_close = if ledger_mode {
            Some(vector_components(
                sizing_state
                    .as_ref()
                    .ok_or_else(|| invalid_input("missing PIT sizing state"))?,
                &close_prices,
            )?)
        } else {
            None
        };
        let mut gross_return = 0.0;
        let mut price_relatives = BTreeMap::new();
        let mut action_next_weights = None;
        if let Some(next_marks) = marks_by_session.get(session_index + 1) {
            if ledger_mode {
                let book = sizing_state
                    .as_ref()
                    .ok_or_else(|| invalid_input("missing PIT sizing state"))?;
                let current_equity = if let Some(equity) = terminal_open_equity {
                    equity
                } else {
                    book.equity(&decimal_marks.prices).map_err(sizing_error)?
                };
                let next_index = session_index + 1;
                let mut next_marks = Marks::default();
                next_marks
                    .update(panel, next_index, PriceField::Open)
                    .map_err(sizing_error)?;
                let mut next_book = book.clone();
                let next_date = panel.sessions[next_index].session_date;
                if let Some(processor) = &action_processor {
                    let mut next_processor = processor.clone();
                    let fresh_prices = next_marks.fresh_ids_on(next_date);
                    let transitions = next_processor
                        .opening(
                            next_date,
                            None,
                            &next_book.positions,
                            &next_marks.prices,
                            &fresh_prices,
                            false,
                        )
                        .map_err(|e| invalid_input(e.to_string()))?;
                    for transition in &transitions {
                        apply_transition_to_vector(transition, &mut next_book)
                            .map_err(invalid_input)?;
                    }
                    next_processor.record(transitions);
                }
                let next_equity = next_book.equity(&next_marks.prices).map_err(sizing_error)?;
                let current = decimal_float(current_equity)?;
                let next = decimal_float(next_equity)?;
                gross_return = next / current - 1.0;
                action_next_weights = Some(
                    next_book
                        .weights(&next_marks.prices)
                        .map_err(sizing_error)?,
                );
            } else {
                for (instrument, weight) in &weights {
                    let instrument_index =
                        panel.instruments.binary_search(instrument).map_err(|_| {
                            invalid_input(format!("unknown held instrument {instrument}"))
                        })?;
                    let current_price = marks_by_session[session_index][instrument_index]
                        .ok_or_else(|| {
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
            if let Some(next_weights) = action_next_weights {
                weights = next_weights;
            } else {
                let return_divisor = 1.0 + gross_return;
                for (instrument, weight) in &mut weights {
                    let relative = price_relatives.get(instrument).ok_or_else(|| {
                        invalid_input(format!(
                            "missing price transition for held instrument {instrument}"
                        ))
                    })?;
                    *weight *= relative / return_divisor;
                }
            }
            validate_applied_weights(panel, &weights)?;
        }

        if let Some(decision) = decisions_by_index.get(&session_index) {
            latest_target_weights = decision.targets.clone();
            pending_decision = Some(decision);
            pending_targets = decision.targets.clone();
            retrying = false;
            if let Some(generate) = &mut generate {
                generate(session, &BTreeSet::new(), Some(decision), false);
            }
            if let Some(state) = &mut pit_state {
                state.decide(decision);
            }
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
            portfolio_components,
            portfolio_components_close,
            valuation_carried: carried_by_session[session_index].clone(),
        });
    }

    let generated_pending = if let Some(state) = &pit_state {
        state.pending_at_end()
    } else if let (Some(generate), Some(last)) = (&mut generate, panel.sessions.last()) {
        generate(last, &BTreeSet::new(), None, true)
    } else {
        None
    };
    let pending_event = generated_pending
        .as_ref()
        .into_iter()
        .chain(event_indices.iter().map(|(_, event)| *event))
        .find_map(|event| match event {
            ExecutionEvent::PendingAtEnd {
                decision_session,
                targets,
            } => Some((decision_session, targets)),
            ExecutionEvent::Deferred { .. }
            | ExecutionEvent::Executed { .. }
            | ExecutionEvent::RetryableExecution { .. } => None,
        });
    let pending_at_end = match (pending_decision, pending_event) {
        (Some(pending), Some((event_session, targets))) => {
            if pending.decision_session != *event_session || pending_targets != *targets {
                return Err(invalid_input(
                    "pending-at-end event does not match the latest target",
                ));
            }
            Some(VectorPending {
                decision_session: pending.decision_session.session_date.to_string(),
                targets: pending_targets,
                retry_entries: retrying,
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

    let pending_risk_exits = if let (Some((_, risk, _)), Some(last)) = (pit, panel.sessions.last())
    {
        let active = risk.active(last.ts_close);
        let terminal = action_processor
            .as_ref()
            .map(|processor| processor.terminal_instruments(last.session_date))
            .unwrap_or_default();
        sizing_state
            .as_ref()
            .ok_or_else(|| invalid_input("missing PIT sizing state"))?
            .positions
            .keys()
            .filter(|id| active.contains(*id) && !terminal.contains(*id))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let (
        return_basis,
        corporate_action_schedule_version,
        corporate_action_dividend_tax_rate,
        corporate_action_coverage,
        corporate_action_completeness,
        corporate_action_ledger,
    ) = if let (Some(schedule), Some(processor)) = (actions, action_processor.as_ref()) {
        (
            ReturnBasis::TotalReturn,
            Some(schedule.version().to_owned()),
            Some(schedule.dividend_tax_rate()),
            schedule.coverage().to_vec(),
            processor.completeness(),
            processor.ledger().to_vec(),
        )
    } else {
        (
            ReturnBasis::PriceReturn,
            None,
            None,
            Vec::new(),
            CorporateActionCompleteness {
                is_complete: false,
                reasons: vec![
                    "corporate-action coverage was not supplied; total return is unknown".into(),
                ],
            },
            Vec::new(),
        )
    };
    let mut assumptions = if pit.is_some() {
        vec![
            "Offline PIT/Eligible/Derived Market evidence; not real-market PIT certification."
                .into(),
        if actions.is_some() {
            "Directional limits and persistent Risk Exit; no lot/minimum commission or T+1."
                .into()
        } else {
            "Directional limits and persistent Risk Exit; no lot/minimum commission, T+1 or corporate actions."
                .into()
        },
        "Vector NAV includes next-open return; compare nav/(1+gross_return) to Fast open-after."
            .into(),
        ]
    } else {
        assumptions(availability_assumption)
    };
    if actions.is_some() {
        assumptions.push(
            "Corporate actions are applied in continuous-share parity mode; lot rounding is excluded."
                .into(),
        );
    }
    if terminal_settlement.is_some() {
        assumptions.push(
            "Normal execution at the final in-lifecycle Bar open remains allowed; remaining Catalog-terminal positions settle at that session's raw Bar close without using actual liquidation cash or date."
                .into(),
        );
    }
    Ok(VectorResult {
        pit_initial_capital: pit.map(|(_, _, capital)| capital),
        sessions: result_sessions,
        decisions: result_decisions,
        executions: result_executions,
        pending_at_end,
        assumptions,
        costs: *costs,
        availability_assumption: availability_assumption.canonical_str().to_owned(),
        risk_exits,
        pending_risk_exits,
        return_basis,
        corporate_action_schedule_version,
        corporate_action_dividend_tax_rate,
        corporate_action_coverage,
        corporate_action_completeness,
        corporate_action_ledger,
        terminal_settlements,
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

fn vector_components(
    book: &ParitySizingState,
    prices: &BTreeMap<InstrumentId, prajna_domain::Price>,
) -> Result<VectorPortfolioComponents, VectorError> {
    let equity = book.equity(prices).map_err(sizing_error)?;
    if equity <= Notional::ZERO {
        return Err(invalid_input(
            "Vector portfolio equity must remain positive",
        ));
    }
    let denominator = decimal_float(equity)?;
    Ok(VectorPortfolioComponents {
        available_cash_weight: decimal_float(book.cash)? / denominator,
        dividend_receivable_weight: decimal_float(book.dividend_receivable)? / denominator,
        liquidation_receivable_weight: decimal_float(book.liquidation_receivable)? / denominator,
    })
}

fn is_price_return(return_basis: &ReturnBasis) -> bool {
    *return_basis == ReturnBasis::PriceReturn
}

fn is_unreported_completeness(completeness: &CorporateActionCompleteness) -> bool {
    !completeness.is_complete
        && completeness.reasons.len() == 1
        && matches!(
            completeness.reasons[0].as_str(),
            "corporate-action coverage was not supplied; total return is unknown"
                | "corporate-action completeness was not supplied"
        )
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
            }
            | ExecutionEvent::RetryableExecution {
                decision_session,
                attempt_session,
                applied,
                skipped_buys,
                ..
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
        }
        | ExecutionEvent::RetryableExecution {
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
    targets: &BTreeMap<InstrumentId, f64>,
    applied: &BTreeMap<InstrumentId, f64>,
    skipped_buys: &BTreeSet<InstrumentId>,
) -> Result<(), VectorError> {
    if applied
        .keys()
        .any(|instrument| skipped_buys.contains(instrument))
        || applied
            .iter()
            .any(|(instrument, weight)| targets.get(instrument) != Some(weight))
        || skipped_buys
            .iter()
            .any(|instrument| targets.get(instrument).is_none_or(|weight| *weight <= 0.0))
    {
        return Err(invalid_input(
            "executed and skipped legs do not match the pending targets",
        ));
    }
    for (instrument, weight) in targets {
        if *weight > 0.0 && !applied.contains_key(instrument) && !skipped_buys.contains(instrument)
        {
            return Err(invalid_input(
                "each positive target must be applied or recorded as a skipped buy",
            ));
        }
    }
    Ok(())
}

fn apply_entry_retry(
    weights: &mut BTreeMap<InstrumentId, f64>,
    applied: &BTreeMap<InstrumentId, f64>,
    buy_rate: f64,
) -> Result<(f64, f64), VectorError> {
    let mut cash_weight = (1.0 - weights.values().sum::<f64>()).max(0.0);
    let mut buys = BTreeMap::new();
    let mut turnover = 0.0;
    for (instrument, target) in applied {
        // Retry budgets use current pre-fee equity; cash includes the buy fees.
        let buy = target.min(cash_weight / (1.0 + buy_rate));
        cash_weight = (cash_weight - buy * (1.0 + buy_rate)).max(0.0);
        if buy > 0.0 {
            buys.insert(instrument.clone(), buy);
        }
        turnover += buy;
    }
    let cost = turnover * buy_rate;
    if !cost.is_finite() || cost >= 1.0 {
        return Err(invalid_costs("an entry retry must have cost below 100%"));
    }
    // Only normalization changes held weights: held notional/quantity stays fixed.
    for weight in weights.values_mut() {
        *weight /= 1.0 - cost;
    }
    for (instrument, buy) in buys {
        weights.insert(instrument, buy / (1.0 - cost));
    }
    Ok((turnover, cost))
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
