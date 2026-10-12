//! Fast Event L1: close decisions, next-session open market fills, exact subaccounts.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use chrono::NaiveDate;
use prajna_account::{
    AccountValuation, Fill, FillCosts, FillPricing, LedgerError, Order, OrderId,
    ParityFeeSettlement, Side, TradingAccount, VirtualPortfolioId,
};
use prajna_domain::{Currency, FixedPointError, InstrumentId, Notional, Price, Quantity, VenueId};

use crate::{
    ExecutionStatusMap, Panel, PanelError, PanelSession, executable,
    strategy::{Decision, ExecutionEvent, ExecutionState, UnfilledEntry},
};

pub const SEMANTIC_VERSION: &str = "fast_event@1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sizing {
    VectorParity,
    Lot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rebalance {
    FullTarget,
    /// POC-only variant: keep unentered targets pending, including cash-limited entries.
    EntryOnly(EntryBudget),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryBudget {
    FixedInitial,
    RemainingCash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FastEventCosts {
    pub commission_rate: Notional,
    pub buy_slippage_rate: Notional,
    pub sell_slippage_rate: Notional,
    pub buy_tax_rate: Notional,
    pub sell_tax_rate: Notional,
    pub minimum_commission: Notional,
}

impl FastEventCosts {
    pub const ZERO: Self = Self {
        commission_rate: Notional::ZERO,
        buy_slippage_rate: Notional::ZERO,
        sell_slippage_rate: Notional::ZERO,
        buy_tax_rate: Notional::ZERO,
        sell_tax_rate: Notional::ZERO,
        minimum_commission: Notional::ZERO,
    };

    fn fees(self, basis: Notional, side: Side) -> Result<(FillCosts, Notional), FastEventError> {
        let (slip, tax) = match side {
            Side::Buy => (self.buy_slippage_rate, self.buy_tax_rate),
            Side::Sell => (self.sell_slippage_rate, self.sell_tax_rate),
        };
        Ok((
            FillCosts {
                commission: basis.checked_mul_decimal(self.commission_rate)?,
                minimum_commission_top_up: Notional::ZERO,
                tax: basis.checked_mul_decimal(tax)?,
            },
            basis.checked_mul_decimal(slip)?,
        ))
    }
}

#[derive(Debug, Clone)]
pub struct FastEventConfig {
    pub currency: Currency,
    pub venue: VenueId,
    pub initial_cash: Notional,
    pub sizing: Sizing,
    pub costs: FastEventCosts,
    /// Resolved Instrument lot sizes; required in lot mode, never defaulted.
    pub lot_sizes: BTreeMap<InstrumentId, Quantity>,
}

#[derive(Debug, Clone)]
pub struct PortfolioRun {
    pub id: VirtualPortfolioId,
    pub capital: Notional,
    pub decisions: Vec<Decision>,
    pub unfilled_entry: UnfilledEntry,
    pub rebalance: Rebalance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceField {
    Open,
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceSource {
    pub session_date: NaiveDate,
    pub field: PriceField,
}

#[derive(Debug, Clone)]
pub struct FastEventSession {
    pub session: PanelSession,
    pub open_after: AccountValuation,
    pub close: AccountValuation,
    pub open_sources: BTreeMap<InstrumentId, PriceSource>,
    pub close_sources: BTreeMap<InstrumentId, PriceSource>,
}

#[derive(Debug, Clone)]
pub struct FastEventFill {
    pub fill: Fill,
    /// F8 weight-turnover base, which can differ from the actual raw Fill notional.
    pub fee_basis: Notional,
    pub fee_side: Side,
}

#[derive(Debug, Clone)]
pub struct FastEventAttempt {
    pub portfolio_id: VirtualPortfolioId,
    pub event: ExecutionEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantityAdjustmentKind {
    Target,
    BuyOrder,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SizingAdjustmentReason {
    LotRounding,
    InsufficientCash,
}

/// Diagnostic only: explains target lot rounding and cash-limited order reductions.
/// Zero actual quantity is an unsubmitted attempt, never a Domain Order or Fill.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FastEventSizingAdjustment {
    pub portfolio_id: VirtualPortfolioId,
    pub instrument_id: InstrumentId,
    pub attempted_at: prajna_domain::TimestampNs,
    pub quantity_kind: QuantityAdjustmentKind,
    pub reason: SizingAdjustmentReason,
    pub requested_quantity: Quantity,
    pub actual_quantity: Quantity,
}

#[derive(Default)]
struct PlannedExecution {
    fills: Vec<FastEventFill>,
    fee_settlements: Vec<ParityFeeSettlement>,
}

#[derive(Debug, Clone)]
pub struct FastEventResult {
    pub semantic_version: &'static str,
    pub config: FastEventConfig,
    pub sessions: Vec<FastEventSession>,
    pub attempts: Vec<FastEventAttempt>,
    pub fills: Vec<FastEventFill>,
    pub sizing_adjustments: Vec<FastEventSizingAdjustment>,
    pub account: TradingAccount,
    pub assumptions: Vec<&'static str>,
}

#[derive(Debug)]
pub enum FastEventError {
    InvalidInput(String),
    Arithmetic(FixedPointError),
    Ledger(LedgerError),
    Panel(PanelError),
    Polars(polars::error::PolarsError),
}

impl fmt::Display for FastEventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) => write!(f, "invalid Fast Event input: {message}"),
            Self::Arithmetic(error) => write!(f, "Fast Event arithmetic: {error}"),
            Self::Ledger(error) => write!(f, "Fast Event ledger: {error}"),
            Self::Panel(error) => write!(f, "Fast Event panel: {error}"),
            Self::Polars(error) => write!(f, "Fast Event grid: {error}"),
        }
    }
}
impl std::error::Error for FastEventError {}
impl From<FixedPointError> for FastEventError {
    fn from(error: FixedPointError) -> Self {
        Self::Arithmetic(error)
    }
}
impl From<LedgerError> for FastEventError {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error)
    }
}
impl From<PanelError> for FastEventError {
    fn from(error: PanelError) -> Self {
        Self::Panel(error)
    }
}
impl From<polars::error::PolarsError> for FastEventError {
    fn from(error: polars::error::PolarsError) -> Self {
        Self::Polars(error)
    }
}

fn invalid(message: impl Into<String>) -> FastEventError {
    FastEventError::InvalidInput(message.into())
}
fn add(a: Notional, b: Notional) -> Result<Notional, FastEventError> {
    Ok(Notional::from_mantissa(
        a.mantissa()
            .checked_add(b.mantissa())
            .ok_or(FixedPointError::Overflow)?,
    )?)
}
fn sub(a: Notional, b: Notional) -> Result<Notional, FastEventError> {
    add(a, b.checked_mul(-1)?)
}
fn decimal_price(price: Price) -> Result<Notional, FastEventError> {
    Ok(Notional::from_mantissa(price.mantissa())?)
}
fn value(quantity: Quantity, price: Price) -> Result<Notional, FastEventError> {
    Ok(Notional::from_mantissa(quantity.mantissa())?.checked_mul_decimal(decimal_price(price)?)?)
}
fn quantity(budget: Notional, price: Price) -> Result<Quantity, FastEventError> {
    Ok(Quantity::from_mantissa(
        budget
            .checked_div_decimal(decimal_price(price)?)?
            .mantissa(),
    )?)
}
fn weight(input: f64) -> Result<Notional, FastEventError> {
    if !input.is_finite() || !(0.0..=1.0).contains(&input) {
        return Err(invalid("weights must be finite and in [0, 1]"));
    }
    // Decision weights originate in Research f64; prices and ledger never do.
    Ok(input.to_string().parse()?)
}

#[derive(Default)]
struct Marks {
    prices: BTreeMap<InstrumentId, Price>,
    sources: BTreeMap<InstrumentId, PriceSource>,
}

impl Marks {
    fn update(
        &mut self,
        panel: &Panel,
        index: usize,
        field: PriceField,
    ) -> Result<(), FastEventError> {
        let name = match field {
            PriceField::Open => "open_decimal",
            PriceField::Close => "close_decimal",
        };
        let prices = panel.grid.column(name)?.str()?;
        let has_bar = panel.grid.column("has_bar")?.bool()?;
        for (i, instrument) in panel.instruments.iter().enumerate() {
            let row = i * panel.sessions.len() + index;
            let present = has_bar.get(row).ok_or_else(|| invalid("null has_bar"))?;
            match (present, prices.get(row)) {
                (true, Some(raw)) => {
                    let price: Price = raw.parse()?;
                    if price <= Price::ZERO {
                        return Err(invalid("nonpositive valuation price"));
                    }
                    self.prices.insert(instrument.clone(), price);
                    self.sources.insert(
                        instrument.clone(),
                        PriceSource {
                            session_date: panel.sessions[index].session_date,
                            field,
                        },
                    );
                }
                (false, None) => {}
                _ => return Err(invalid("decimal prices inconsistent with has_bar")),
            }
        }
        Ok(())
    }
}

/// Consumes precomputed close decisions (signal generation is outside this seam).
/// Panel prices must include the exact `open_decimal` / `close_decimal` String
/// columns supplied by `load_panel`; there is no f64-price fallback.
/// Errors abort the Run, including any scale-18 aggregation discrepancy.
pub fn run_fast_event(
    panel: &Panel,
    statuses: &ExecutionStatusMap,
    config: &FastEventConfig,
    portfolios: &[PortfolioRun],
) -> Result<FastEventResult, FastEventError> {
    validate(panel, config, portfolios)?;
    let mut account = TradingAccount::new(
        config.currency.clone(),
        config.venue.clone(),
        config.initial_cash,
        portfolios.iter().map(|p| (p.id, p.capital)),
    )?;
    let mut states = portfolios
        .iter()
        .map(|p| ExecutionState::new(p.unfilled_entry))
        .collect::<Vec<_>>();
    let mut open = Marks::default();
    let mut original_decisions: Vec<Option<&Decision>> = vec![None; portfolios.len()];
    let mut close = Marks::default();
    let mut sessions = Vec::with_capacity(panel.sessions.len());
    let mut attempts = Vec::new();
    let mut fills = Vec::new();
    let mut sizing_adjustments = Vec::new();
    for (index, session) in panel.sessions.iter().enumerate() {
        open.update(panel, index, PriceField::Open)?;
        let gates = panel
            .instruments
            .iter()
            .map(|id| executable(panel, statuses, id, session).map(|yes| (id.clone(), yes)))
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let mut session_fills = Vec::new();
        let mut session_fees = Vec::new();
        let mut entry_attempted = BTreeSet::new();
        for (portfolio_index, (portfolio, state)) in portfolios.iter().zip(&mut states).enumerate()
        {
            let held = account.portfolios()[&portfolio.id]
                .positions()
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>();
            if let Some(event) = state.attempt(session, &held, |id, _| gates[id]) {
                let execution = match &event {
                    ExecutionEvent::Executed { applied, .. } => Some((applied, false)),
                    ExecutionEvent::RetryableExecution {
                        applied, is_retry, ..
                    } => Some((applied, *is_retry)),
                    _ => None,
                };
                if let Some((targets, retry)) = execution {
                    let planned = match config.sizing {
                        Sizing::VectorParity => parity_fills(
                            &account,
                            portfolio.id,
                            targets,
                            &open.prices,
                            session,
                            config.costs,
                            retry,
                        )?,
                        Sizing::Lot => PlannedExecution {
                            fills: lot_fills(
                                &account,
                                portfolio,
                                TargetAttempt {
                                    applied: targets,
                                    original: &original_decisions[portfolio_index]
                                        .ok_or_else(|| {
                                            invalid("execution without prior close decision")
                                        })?
                                        .targets,
                                    retry,
                                },
                                &open.prices,
                                session,
                                config,
                                &mut sizing_adjustments,
                            )?,
                            fee_settlements: Vec::new(),
                        },
                    };
                    session_fills.extend(planned.fills);
                    session_fees.extend(planned.fee_settlements);
                    if matches!(portfolio.rebalance, Rebalance::EntryOnly(_)) {
                        entry_attempted.insert(portfolio_index);
                    }
                }
                attempts.push(FastEventAttempt {
                    portfolio_id: portfolio.id,
                    event,
                });
            }
        }
        session_fills.sort_by(|a: &FastEventFill, b: &FastEventFill| {
            let key = |entry: &FastEventFill| {
                (
                    entry.fill.order().side() != Side::Sell,
                    entry.fill.order().instrument_id().clone(),
                    entry.fill.order().portfolio_id(),
                )
            };
            key(a).cmp(&key(b))
        });
        // Planners use provisional ID 0 and ledger fee arithmetic. Only this
        // account-wide settlement assigns IDs after sell/buy and Instrument sorting.
        for mut entry in session_fills {
            let previous = entry.fill.order();
            let order = Order::new(
                next_order(&account, 0)?,
                previous.portfolio_id(),
                previous.instrument_id().clone(),
                previous.side(),
                previous.quantity(),
                session.ts_open,
            )?;
            entry.fill = Fill::new(
                order,
                session.ts_open,
                entry.fill.raw_price(),
                entry.fill.pricing(),
                entry.fill.costs(),
            )?;
            account.submit_order(entry.fill.order().clone())?;
            account.apply_fill(entry.fill.clone())?;
            fills.push(entry);
        }
        session_fees.sort_by(|a: &ParityFeeSettlement, b: &ParityFeeSettlement| {
            (a.instrument_id(), a.portfolio_id()).cmp(&(b.instrument_id(), b.portfolio_id()))
        });
        for settlement in session_fees {
            account.apply_fee_settlement(settlement)?;
        }
        for index in entry_attempted {
            let held = account.portfolios()[&portfolios[index].id]
                .positions()
                .keys()
                .cloned()
                .collect();
            states[index].retain_unentered(
                original_decisions[index]
                    .ok_or_else(|| invalid("entry attempt without decision"))?,
                &held,
            );
        }
        let open_after = account.value(&open.prices)?;
        close.update(panel, index, PriceField::Close)?;
        let close_value = account.value(&close.prices)?;
        sessions.push(FastEventSession {
            session: session.clone(),
            open_after,
            close: close_value,
            open_sources: open.sources.clone(),
            close_sources: close.sources.clone(),
        });
        for (portfolio_index, (portfolio, state)) in portfolios.iter().zip(&mut states).enumerate()
        {
            if let Some(decision) = portfolio
                .decisions
                .iter()
                .find(|d| d.decision_session == *session)
            {
                state.decide(decision);
                original_decisions[portfolio_index] = Some(decision);
            }
        }
    }
    for (portfolio, state) in portfolios.iter().zip(states) {
        if let Some(event) = state.pending_at_end() {
            attempts.push(FastEventAttempt {
                portfolio_id: portfolio.id,
                event,
            });
        }
    }
    Ok(FastEventResult {
        semantic_version: SEMANTIC_VERSION,
        config: config.clone(),
        sessions,
        attempts,
        fills,
        sizing_adjustments,
        account,
        assumptions: vec![
            "Single Venue, single currency, long-only, unlevered; full market fills.",
            "T+1, price limits, currency-unit rounding and corporate actions are not modeled.",
            "Static synthetic Universe evidence does not establish PIT or dividend total return.",
            "Valid raw bars mark even HALTED/UNKNOWN instruments; status gates execution only.",
            "Missing prices carry the last valid price of the same field, never zero.",
        ],
    })
}

fn validate(
    panel: &Panel,
    config: &FastEventConfig,
    portfolios: &[PortfolioRun],
) -> Result<(), FastEventError> {
    if panel.sessions.is_empty()
        || panel.instruments.is_empty()
        || panel.instruments.windows(2).any(|p| p[0] >= p[1])
        || panel
            .sessions
            .windows(2)
            .any(|p| p[0].session_date >= p[1].session_date || p[0].ts_close >= p[1].ts_open)
        || panel.sessions.iter().any(|s| s.ts_open >= s.ts_close)
        || panel
            .instruments
            .iter()
            .any(|id| id.venue() != &config.venue)
        || panel.grid.height()
            != panel
                .instruments
                .len()
                .checked_mul(panel.sessions.len())
                .ok_or(FixedPointError::Overflow)?
    {
        return Err(invalid("invalid single-venue panel shape or ordering"));
    }
    let costs = config.costs;
    let one: Notional = "1".parse()?;
    if [
        costs.commission_rate,
        costs.buy_slippage_rate,
        costs.sell_slippage_rate,
        costs.buy_tax_rate,
        costs.sell_tax_rate,
    ]
    .iter()
    .any(|rate| *rate < Notional::ZERO || *rate >= one)
        || costs.minimum_commission < Notional::ZERO
    {
        return Err(invalid("invalid costs"));
    }
    if config.sizing == Sizing::Lot
        && panel.instruments.iter().any(|id| {
            config
                .lot_sizes
                .get(id)
                .is_none_or(|lot| *lot <= Quantity::ZERO)
        })
    {
        return Err(invalid(
            "lot sizing requires a positive Instrument lot size for every instrument",
        ));
    }
    for portfolio in portfolios {
        if config.sizing == Sizing::VectorParity && portfolio.rebalance != Rebalance::FullTarget {
            return Err(invalid(
                "vector_parity requires full_target; entry_only is a lot POC variant",
            ));
        }
        let mut seen = BTreeSet::new();
        for decision in &portfolio.decisions {
            if !panel.sessions.contains(&decision.decision_session)
                || !seen.insert(decision.decision_session.session_date)
            {
                return Err(invalid(
                    "decision Session absent, inconsistent or duplicated",
                ));
            }

            let mut sum = Notional::ZERO;
            for (id, target) in &decision.targets {
                if panel.instruments.binary_search(id).is_err() {
                    return Err(invalid("unknown target instrument"));
                }
                sum = add(sum, weight(*target)?)?;
            }
            if sum > one {
                return Err(invalid("target weights exceed one"));
            }
        }
    }
    Ok(())
}

struct TargetAttempt<'a> {
    applied: &'a BTreeMap<InstrumentId, f64>,
    original: &'a BTreeMap<InstrumentId, f64>,
    retry: bool,
}

fn lot_fills(
    account: &TradingAccount,
    run: &PortfolioRun,
    attempt: TargetAttempt<'_>,
    prices: &BTreeMap<InstrumentId, Price>,
    session: &PanelSession,
    config: &FastEventConfig,
    adjustments: &mut Vec<FastEventSizingAdjustment>,
) -> Result<Vec<FastEventFill>, FastEventError> {
    let portfolio = &account.portfolios()[&run.id];
    let mut desired = if attempt.retry && run.rebalance == Rebalance::FullTarget {
        portfolio.positions().clone()
    } else {
        BTreeMap::new()
    };
    if run.rebalance == Rebalance::FullTarget {
        let equity = account.value(prices)?.portfolios[&run.id].equity;
        for (instrument, target) in attempt.applied {
            let price = *prices
                .get(instrument)
                .ok_or_else(|| LedgerError::MissingPrice(instrument.clone()))?;
            let quantity = quantity(weight(*target)?.checked_mul_decimal(equity)?, price)?;
            let rounded = floor_lot(quantity, config.lot_sizes[instrument])?;
            record_adjustment(
                adjustments,
                run.id,
                instrument,
                session,
                QuantityAdjustmentKind::Target,
                SizingAdjustmentReason::LotRounding,
                quantity,
                rounded,
            );
            desired.insert(instrument.clone(), rounded);
        }
    } else {
        desired = portfolio
            .positions()
            .iter()
            .map(|(id, q)| {
                (
                    id.clone(),
                    if attempt.original.get(id).is_some_and(|w| *w > 0.0) {
                        *q
                    } else {
                        Quantity::ZERO
                    },
                )
            })
            .chain(
                attempt
                    .applied
                    .iter()
                    .filter(|(id, w)| **w > 0.0 && !portfolio.positions().contains_key(*id))
                    .map(|(id, _)| (id.clone(), Quantity::ZERO)),
            )
            .collect();
    }
    let instruments = desired
        .keys()
        .chain(portfolio.positions().keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut cash = portfolio.cash();
    let mut fills = Vec::new();
    for side in [Side::Sell, Side::Buy] {
        let entry_budget = if side == Side::Buy {
            match run.rebalance {
                Rebalance::FullTarget => None,
                Rebalance::EntryOnly(mode) => {
                    let count = attempt
                        .original
                        .iter()
                        .filter(|(id, w)| {
                            **w > 0.0
                                && (mode == EntryBudget::FixedInitial
                                    || !portfolio.positions().contains_key(*id))
                        })
                        .count();
                    if count == 0 {
                        None
                    } else {
                        let base = if mode == EntryBudget::FixedInitial {
                            portfolio.initial_capital()
                        } else {
                            cash
                        };
                        Some(base.checked_div_decimal(Notional::parse("1")?.checked_mul(
                            i128::try_from(count).map_err(|_| invalid("too many targets"))?,
                        )?)?)
                    }
                }
            }
        } else {
            None
        };
        for instrument in &instruments {
            let current = portfolio
                .positions()
                .get(instrument)
                .copied()
                .unwrap_or(Quantity::ZERO);
            let mut target = desired.get(instrument).copied().unwrap_or(Quantity::ZERO);
            if let Some(budget) = entry_budget {
                if current == Quantity::ZERO
                    && attempt.applied.get(instrument).is_some_and(|w| *w > 0.0)
                {
                    let price = *prices
                        .get(instrument)
                        .ok_or_else(|| LedgerError::MissingPrice(instrument.clone()))?;
                    let requested = quantity(budget, price)?;
                    target = floor_lot(requested, config.lot_sizes[instrument])?;
                    record_adjustment(
                        adjustments,
                        run.id,
                        instrument,
                        session,
                        QuantityAdjustmentKind::Target,
                        SizingAdjustmentReason::LotRounding,
                        requested,
                        target,
                    );
                }
            }
            let delta = target
                .mantissa()
                .checked_sub(current.mantissa())
                .ok_or(FixedPointError::Overflow)?;
            if (side == Side::Buy && delta <= 0) || (side == Side::Sell && delta >= 0) {
                continue;
            }
            let mut q = Quantity::from_mantissa(delta.abs())?;
            let requested = q;
            let price = *prices
                .get(instrument)
                .ok_or_else(|| LedgerError::MissingPrice(instrument.clone()))?;
            while q > Quantity::ZERO {
                let order = Order::new(
                    OrderId(0),
                    run.id,
                    instrument.clone(),
                    side,
                    q,
                    session.ts_open,
                )?;
                let fill = lot_fill(order, price, config.costs)?;
                let next_cash = add(cash, fill.cash_delta())?;
                if side == Side::Sell || next_cash >= Notional::ZERO {
                    cash = next_cash;
                    fills.push(FastEventFill {
                        fee_basis: fill.trade_notional(),
                        fee_side: side,
                        fill,
                    });
                    break;
                }
                q = Quantity::from_mantissa(
                    q.mantissa()
                        .checked_sub(config.lot_sizes[instrument].mantissa())
                        .ok_or(FixedPointError::Overflow)?,
                )?;
            }
            record_adjustment(
                adjustments,
                run.id,
                instrument,
                session,
                QuantityAdjustmentKind::BuyOrder,
                SizingAdjustmentReason::InsufficientCash,
                requested,
                q,
            );
        }
    }
    Ok(fills)
}

#[allow(clippy::too_many_arguments)]
fn record_adjustment(
    adjustments: &mut Vec<FastEventSizingAdjustment>,
    portfolio_id: VirtualPortfolioId,
    instrument: &InstrumentId,
    session: &PanelSession,
    quantity_kind: QuantityAdjustmentKind,
    reason: SizingAdjustmentReason,
    requested_quantity: Quantity,
    actual_quantity: Quantity,
) {
    if requested_quantity != actual_quantity {
        adjustments.push(FastEventSizingAdjustment {
            portfolio_id,
            instrument_id: instrument.clone(),
            attempted_at: session.ts_open,
            quantity_kind,
            reason,
            requested_quantity,
            actual_quantity,
        });
    }
}

fn floor_lot(quantity: Quantity, lot: Quantity) -> Result<Quantity, FastEventError> {
    Ok(Quantity::from_mantissa(
        (quantity.mantissa() / lot.mantissa())
            .checked_mul(lot.mantissa())
            .ok_or(FixedPointError::Overflow)?,
    )?)
}

fn next_order(account: &TradingAccount, offset: usize) -> Result<OrderId, FastEventError> {
    let number = account
        .orders()
        .len()
        .checked_add(offset)
        .and_then(|n| n.checked_add(1))
        .ok_or(FixedPointError::Overflow)?;
    Ok(OrderId(
        u64::try_from(number).map_err(|_| invalid("too many orders"))?,
    ))
}

fn lot_fill(order: Order, raw: Price, costs: FastEventCosts) -> Result<Fill, FastEventError> {
    let side = order.side();
    let rate = match side {
        Side::Buy => costs.buy_slippage_rate,
        Side::Sell => costs.sell_slippage_rate,
    };
    let one: Notional = "1".parse()?;
    let multiplier = if side == Side::Buy {
        add(one, rate)?
    } else {
        sub(one, rate)?
    };
    let price = Price::from_mantissa(
        decimal_price(raw)?
            .checked_mul_decimal(multiplier)?
            .mantissa(),
    )?;
    let notional = value(order.quantity(), price)?;
    let (mut fees, _) = costs.fees(notional, side)?;
    fees.minimum_commission_top_up = if fees.commission < costs.minimum_commission {
        sub(costs.minimum_commission, fees.commission)?
    } else {
        Notional::ZERO
    };
    Ok(Fill::new(
        order.clone(),
        order.submitted_at(),
        raw,
        FillPricing::EmbeddedSlippage {
            execution_price: price,
        },
        fees,
    )?)
}
fn parity_fills(
    account: &TradingAccount,
    id: VirtualPortfolioId,
    targets: &BTreeMap<InstrumentId, f64>,
    prices: &BTreeMap<InstrumentId, Price>,
    session: &PanelSession,
    costs: FastEventCosts,
    retry: bool,
) -> Result<PlannedExecution, FastEventError> {
    if retry {
        return Ok(PlannedExecution {
            fills: parity_retry_fills(account, id, targets, prices, session, costs)?,
            fee_settlements: Vec::new(),
        });
    }
    let portfolio = &account.portfolios()[&id];
    let equity = account.value(prices)?.portfolios[&id].equity;
    if equity == Notional::ZERO {
        return Ok(PlannedExecution::default());
    }
    let instruments = portfolio
        .positions()
        .keys()
        .chain(targets.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut plans = Vec::new();
    let mut total = Notional::ZERO;
    for instrument in instruments {
        let price = *prices
            .get(&instrument)
            .ok_or_else(|| LedgerError::MissingPrice(instrument.clone()))?;
        let current = portfolio
            .positions()
            .get(&instrument)
            .copied()
            .unwrap_or(Quantity::ZERO);
        let target = weight(targets.get(&instrument).copied().unwrap_or(0.0))?;
        let drift = value(current, price)?.checked_div_decimal(equity)?;
        let delta = sub(target, drift)?;
        let side = if delta >= Notional::ZERO {
            Side::Buy
        } else {
            Side::Sell
        };
        let abs = Notional::from_mantissa(delta.mantissa().abs())?;
        let basis = equity.checked_mul_decimal(abs)?;
        let (fees, slip) = costs.fees(basis, side)?;
        total = add(total, add(add(fees.commission, fees.tax)?, slip)?)?;
        plans.push((instrument, price, current, target, basis, side, fees, slip));
    }
    let after = sub(equity, total)?;
    if after <= Notional::ZERO {
        return Err(invalid("costs exhaust pre-open equity"));
    }
    let mut planned = PlannedExecution::default();
    for (instrument, price, current, target, basis, fee_side, fees, slip) in plans {
        let desired = quantity(target.checked_mul_decimal(after)?, price)?;
        let delta = desired
            .mantissa()
            .checked_sub(current.mantissa())
            .ok_or(FixedPointError::Overflow)?;
        let q = Quantity::from_mantissa(delta.abs())?;
        if q == Quantity::ZERO {
            if add(add(fees.commission, fees.tax)?, slip)? > Notional::ZERO {
                planned.fee_settlements.push(ParityFeeSettlement::new(
                    id,
                    instrument,
                    session.ts_open,
                    basis,
                    fee_side,
                    fees,
                    slip,
                )?);
            }
            continue;
        }
        let side = if delta > 0 { Side::Buy } else { Side::Sell };
        let order = Order::new(OrderId(0), id, instrument, side, q, session.ts_open)?;
        planned.fills.push(FastEventFill {
            fill: Fill::new(
                order,
                session.ts_open,
                price,
                FillPricing::SeparateSlippage { slippage: slip },
                fees,
            )?,
            fee_basis: basis,
            fee_side,
        });
    }
    Ok(planned)
}

fn parity_retry_fills(
    account: &TradingAccount,
    id: VirtualPortfolioId,
    targets: &BTreeMap<InstrumentId, f64>,
    prices: &BTreeMap<InstrumentId, Price>,
    session: &PanelSession,
    costs: FastEventCosts,
) -> Result<Vec<FastEventFill>, FastEventError> {
    let equity = account.value(prices)?.portfolios[&id].equity;
    let mut cash = account.portfolios()[&id].cash();
    let rate = add(
        add(costs.commission_rate, costs.buy_slippage_rate)?,
        costs.buy_tax_rate,
    )?;
    let multiplier = add("1".parse()?, rate)?;
    let mut fills = Vec::new();
    for (instrument, target) in targets {
        if *target == 0.0 || cash == Notional::ZERO {
            continue;
        }
        let price = *prices
            .get(instrument)
            .ok_or_else(|| LedgerError::MissingPrice(instrument.clone()))?;
        let budget = weight(*target)?
            .checked_mul_decimal(equity)?
            .min(cash.checked_div_decimal(multiplier)?);
        let (fees, slip) = costs.fees(budget, Side::Buy)?;
        let fees_total = add(add(fees.commission, fees.tax)?, slip)?;
        let available = sub(cash, fees_total)?;
        if available < Notional::ZERO {
            return Err(invalid("rounded retry fees exceed cash"));
        }
        let q = affordable_quantity(quantity(budget, price)?, price, available)?;
        if q > Quantity::ZERO {
            let order = Order::new(
                OrderId(0),
                id,
                instrument.clone(),
                Side::Buy,
                q,
                session.ts_open,
            )?;
            let fill = Fill::new(
                order,
                session.ts_open,
                price,
                FillPricing::SeparateSlippage { slippage: slip },
                fees,
            )?;
            let next_cash = add(cash, fill.cash_delta())?;
            cash = next_cash;
            fills.push(FastEventFill {
                fill,
                fee_basis: budget,
                fee_side: Side::Buy,
            });
        }
    }
    Ok(fills)
}

fn affordable_quantity(
    maximum: Quantity,
    price: Price,
    available: Notional,
) -> Result<Quantity, FastEventError> {
    if value(maximum, price)? <= available {
        return Ok(maximum);
    }
    // A tiny price can require far more than one quantity quantum to reduce
    // rounded principal. Binary search bounds cash-cut work to at most 127 steps.
    let mut low = 0;
    let mut high = maximum.mantissa();
    while low < high {
        let middle = low + (high - low) / 2 + 1;
        if value(Quantity::from_mantissa(middle)?, price)? <= available {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    Ok(Quantity::from_mantissa(low)?)
}
