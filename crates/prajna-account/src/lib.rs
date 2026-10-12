//! Single-venue, long-only, single-currency ledger; no execution or sizing policy.
//! Domain decimals are authoritative. Failed mutations leave the ledger unchanged.

mod trade;
mod valuation;

pub use trade::{
    Fill, FillCosts, FillPricing, Order, OrderId, ParityFeeSettlement, Side, VirtualPortfolioId,
};
pub use valuation::{AccountValuation, MarkedPosition, PortfolioValuation};

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use prajna_domain::{
    Currency, FixedPointError, InstrumentId, Notional, Quantity, TimestampNs, VenueId,
};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    Arithmetic(FixedPointError),
    NegativeCash,
    InvalidAllocation,
    DuplicatePortfolio(VirtualPortfolioId),
    UnknownPortfolio(VirtualPortfolioId),
    NonPositiveQuantity,
    InsufficientPosition,
    NonPositivePrice,
    NegativeFee,
    FavorableSlippage,
    WrongVenue,
    DuplicateOrder(OrderId),
    UnknownOrder(OrderId),
    OrderMismatch,
    AlreadyFilled(OrderId),
    FillBeforeOrder,
    MissingPrice(InstrumentId),
    InvalidFeeSettlement,
    DuplicateFeeSettlement,
    Invariant(&'static str),
}

impl From<FixedPointError> for LedgerError {
    fn from(error: FixedPointError) -> Self {
        Self::Arithmetic(error)
    }
}
impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for LedgerError {}

pub(crate) fn add(a: Notional, b: Notional) -> Result<Notional, LedgerError> {
    let value = a
        .mantissa()
        .checked_add(b.mantissa())
        .ok_or(FixedPointError::Overflow)?;
    Ok(Notional::from_mantissa(value)?)
}
pub(crate) fn subtract(a: Notional, b: Notional) -> Result<Notional, LedgerError> {
    add(a, b.checked_mul(-1)?)
}
pub(crate) fn change_quantity(a: Quantity, b: Quantity) -> Result<Quantity, LedgerError> {
    let value = a
        .mantissa()
        .checked_add(b.mantissa())
        .ok_or(FixedPointError::Overflow)?;
    Ok(Quantity::from_mantissa(value)?)
}
pub(crate) fn notional(
    price: prajna_domain::Price,
    quantity: Quantity,
) -> Result<Notional, LedgerError> {
    Ok(Notional::from_mantissa(price.mantissa())?
        .checked_mul_decimal(Notional::from_mantissa(quantity.mantissa())?)?)
}

/// Independent subaccount; capital cannot be reassigned after construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VirtualPortfolio {
    initial_capital: Notional,
    cash: Notional,
    positions: BTreeMap<InstrumentId, Quantity>,
}
impl VirtualPortfolio {
    pub fn initial_capital(&self) -> Notional {
        self.initial_capital
    }
    pub fn cash(&self) -> Notional {
        self.cash
    }
    pub fn positions(&self) -> &BTreeMap<InstrumentId, Quantity> {
        &self.positions
    }
}

/// Aggregate balances are updated separately from the VP balances and reconciled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradingAccount {
    currency: Currency,
    venue: VenueId,
    initial_cash: Notional,
    unallocated: Notional,
    cash: Notional,
    positions: BTreeMap<InstrumentId, Quantity>,
    portfolios: BTreeMap<VirtualPortfolioId, VirtualPortfolio>,
    orders: BTreeMap<OrderId, Order>,
    filled: BTreeSet<OrderId>,
    fills: Vec<Fill>,
    settled_fees: BTreeSet<(VirtualPortfolioId, InstrumentId, TimestampNs)>,
    fee_settlements: Vec<ParityFeeSettlement>,
}

impl TradingAccount {
    /// Fixed allocation at Run start; no capital transfer API is exposed.
    pub fn new(
        currency: Currency,
        venue: VenueId,
        initial_cash: Notional,
        allocations: impl IntoIterator<Item = (VirtualPortfolioId, Notional)>,
    ) -> Result<Self, LedgerError> {
        if initial_cash < Notional::ZERO {
            return Err(LedgerError::NegativeCash);
        }
        let mut portfolios = BTreeMap::new();
        let mut allocated = Notional::ZERO;
        for (id, capital) in allocations {
            if capital < Notional::ZERO {
                return Err(LedgerError::InvalidAllocation);
            }
            if portfolios.contains_key(&id) {
                return Err(LedgerError::DuplicatePortfolio(id));
            }
            allocated = add(allocated, capital)?;
            if allocated > initial_cash {
                return Err(LedgerError::InvalidAllocation);
            }
            portfolios.insert(
                id,
                VirtualPortfolio {
                    initial_capital: capital,
                    cash: capital,
                    positions: BTreeMap::new(),
                },
            );
        }
        let account = Self {
            currency,
            venue,
            initial_cash,
            unallocated: subtract(initial_cash, allocated)?,
            cash: initial_cash,
            positions: BTreeMap::new(),
            portfolios,
            orders: BTreeMap::new(),
            filled: BTreeSet::new(),
            fills: Vec::new(),
            settled_fees: BTreeSet::new(),
            fee_settlements: Vec::new(),
        };
        account.validate_balances()?;
        Ok(account)
    }
    pub fn currency(&self) -> &Currency {
        &self.currency
    }
    pub fn venue(&self) -> &VenueId {
        &self.venue
    }
    pub fn initial_cash(&self) -> Notional {
        self.initial_cash
    }
    pub fn unallocated_capital(&self) -> Notional {
        self.unallocated
    }
    pub fn cash(&self) -> Notional {
        self.cash
    }
    pub fn positions(&self) -> &BTreeMap<InstrumentId, Quantity> {
        &self.positions
    }
    pub fn portfolios(&self) -> &BTreeMap<VirtualPortfolioId, VirtualPortfolio> {
        &self.portfolios
    }
    pub fn orders(&self) -> &BTreeMap<OrderId, Order> {
        &self.orders
    }
    pub fn fills(&self) -> &[Fill] {
        &self.fills
    }
    pub fn fee_settlements(&self) -> &[ParityFeeSettlement] {
        &self.fee_settlements
    }

    /// Atomically charge an F8 fee-only leg. Quantities, Orders and Fills do not change.
    /// A VP/instrument/open key may be settled once; retries cannot double-charge it.
    pub fn apply_fee_settlement(
        &mut self,
        settlement: ParityFeeSettlement,
    ) -> Result<(), LedgerError> {
        let id = settlement.portfolio_id();
        let mut portfolio = self
            .portfolios
            .get(&id)
            .ok_or(LedgerError::UnknownPortfolio(id))?
            .clone();
        if settlement.instrument_id().venue() != &self.venue {
            return Err(LedgerError::WrongVenue);
        }
        let key = (
            id,
            settlement.instrument_id().clone(),
            settlement.executed_at(),
        );
        if self.settled_fees.contains(&key) {
            return Err(LedgerError::DuplicateFeeSettlement);
        }
        self.validate_balances()?;
        let cash = add(self.cash, settlement.cash_delta())?;
        portfolio.cash = add(portfolio.cash, settlement.cash_delta())?;
        if cash < Notional::ZERO || portfolio.cash < Notional::ZERO {
            return Err(LedgerError::NegativeCash);
        }
        let mut portfolios = self.portfolios.clone();
        portfolios.insert(id, portfolio);
        reconcile(cash, &self.positions, self.unallocated, &portfolios)?;
        self.cash = cash;
        self.portfolios = portfolios;
        self.settled_fees.insert(key);
        self.fee_settlements.push(settlement);
        Ok(())
    }

    /// Register one market order owned by exactly one VP. No cross-VP netting.
    pub fn submit_order(&mut self, order: Order) -> Result<(), LedgerError> {
        if !self.portfolios.contains_key(&order.portfolio_id()) {
            return Err(LedgerError::UnknownPortfolio(order.portfolio_id()));
        }
        if order.instrument_id().venue() != &self.venue {
            return Err(LedgerError::WrongVenue);
        }
        if self.orders.contains_key(&order.id()) {
            return Err(LedgerError::DuplicateOrder(order.id()));
        }
        self.orders.insert(order.id(), order);
        Ok(())
    }

    /// Settle one full Fill atomically. Partial fills, shorts and borrowing are unsupported.
    pub fn apply_fill(&mut self, fill: Fill) -> Result<(), LedgerError> {
        let order = self
            .orders
            .get(&fill.order().id())
            .ok_or(LedgerError::UnknownOrder(fill.order().id()))?;
        if order != fill.order() {
            return Err(LedgerError::OrderMismatch);
        }
        if self.filled.contains(&order.id()) {
            return Err(LedgerError::AlreadyFilled(order.id()));
        }
        self.validate_balances()?;
        let mut portfolio = self.portfolios[&order.portfolio_id()].clone();
        let cash = add(self.cash, fill.cash_delta())?;
        portfolio.cash = add(portfolio.cash, fill.cash_delta())?;
        if cash < Notional::ZERO || portfolio.cash < Notional::ZERO {
            return Err(LedgerError::NegativeCash);
        }
        let delta = order
            .quantity()
            .checked_mul(if order.side() == Side::Buy { 1 } else { -1 })?;
        let mut positions = self.positions.clone();
        update_position(&mut portfolio.positions, order.instrument_id(), delta)?;
        update_position(&mut positions, order.instrument_id(), delta)?;
        // Validate the candidate before publishing any mutation.
        let mut portfolios = self.portfolios.clone();
        portfolios.insert(order.portfolio_id(), portfolio);
        reconcile(cash, &positions, self.unallocated, &portfolios)?;
        self.cash = cash;
        self.positions = positions;
        self.portfolios = portfolios;
        self.filled.insert(order.id());
        self.fills.push(fill);
        Ok(())
    }

    pub fn validate_balances(&self) -> Result<(), LedgerError> {
        reconcile(
            self.cash,
            &self.positions,
            self.unallocated,
            &self.portfolios,
        )
    }
}
fn update_position(
    positions: &mut BTreeMap<InstrumentId, Quantity>,
    id: &InstrumentId,
    delta: Quantity,
) -> Result<(), LedgerError> {
    let quantity = change_quantity(positions.get(id).copied().unwrap_or(Quantity::ZERO), delta)?;
    if quantity < Quantity::ZERO {
        return Err(LedgerError::InsufficientPosition);
    }
    if quantity == Quantity::ZERO {
        positions.remove(id);
    } else {
        positions.insert(id.clone(), quantity);
    }
    Ok(())
}
fn reconcile(
    cash: Notional,
    positions: &BTreeMap<InstrumentId, Quantity>,
    unallocated: Notional,
    portfolios: &BTreeMap<VirtualPortfolioId, VirtualPortfolio>,
) -> Result<(), LedgerError> {
    if cash < Notional::ZERO || unallocated < Notional::ZERO {
        return Err(LedgerError::NegativeCash);
    }
    let mut total_cash = unallocated;
    let mut total_positions = BTreeMap::new();
    for portfolio in portfolios.values() {
        if portfolio.cash < Notional::ZERO {
            return Err(LedgerError::NegativeCash);
        }
        total_cash = add(total_cash, portfolio.cash)?;
        for (id, quantity) in &portfolio.positions {
            if *quantity <= Quantity::ZERO {
                return Err(LedgerError::NonPositiveQuantity);
            }
            update_position(&mut total_positions, id, *quantity)?;
        }
    }
    if cash != total_cash {
        return Err(LedgerError::Invariant(
            "account cash != VP cash + unallocated",
        ));
    }
    if positions != &total_positions {
        return Err(LedgerError::Invariant(
            "account quantities != VP quantities",
        ));
    }
    Ok(())
}
