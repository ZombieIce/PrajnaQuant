use crate::{LedgerError, add, notional, subtract};
use prajna_domain::{InstrumentId, Notional, Price, Quantity, TimestampNs};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct VirtualPortfolioId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct OrderId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Order {
    id: OrderId,
    portfolio_id: VirtualPortfolioId,
    instrument_id: InstrumentId,
    side: Side,
    quantity: Quantity,
    submitted_at: TimestampNs,
}
impl Order {
    pub fn new(
        id: OrderId,
        portfolio_id: VirtualPortfolioId,
        instrument_id: InstrumentId,
        side: Side,
        quantity: Quantity,
        submitted_at: TimestampNs,
    ) -> Result<Self, LedgerError> {
        if quantity <= Quantity::ZERO {
            return Err(LedgerError::NonPositiveQuantity);
        }
        Ok(Self {
            id,
            portfolio_id,
            instrument_id,
            side,
            quantity,
            submitted_at,
        })
    }
    pub fn id(&self) -> OrderId {
        self.id
    }
    pub fn portfolio_id(&self) -> VirtualPortfolioId {
        self.portfolio_id
    }
    pub fn instrument_id(&self) -> &InstrumentId {
        &self.instrument_id
    }
    pub fn side(&self) -> Side {
        self.side
    }
    pub fn quantity(&self) -> Quantity {
        self.quantity
    }
    pub fn submitted_at(&self) -> TimestampNs {
        self.submitted_at
    }
}

/// Commission is the proportional component; top-up is additional, not included twice.
/// Rates, sizing and minimum commission calculation belong to the execution engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FillCosts {
    pub commission: Notional,
    pub minimum_commission_top_up: Notional,
    pub tax: Notional,
}
impl FillCosts {
    pub const ZERO: Self = Self {
        commission: Notional::ZERO,
        minimum_commission_top_up: Notional::ZERO,
        tax: Notional::ZERO,
    };
    pub fn total_commission(self) -> Result<Notional, LedgerError> {
        add(self.commission, self.minimum_commission_top_up)
    }
    fn validate(self) -> Result<(), LedgerError> {
        if [self.commission, self.minimum_commission_top_up, self.tax]
            .iter()
            .any(|fee| *fee < Notional::ZERO)
        {
            return Err(LedgerError::NegativeFee);
        }
        Ok(())
    }
}

/// Explicitly distinguishes raw-price fills from fills whose price embeds slippage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum FillPricing {
    /// Vector parity: raw open price; supplied slippage is an additional cash fee.
    SeparateSlippage { slippage: Notional },
    /// Lot mode: adverse execution price; slippage is derived and is not charged twice.
    EmbeddedSlippage { execution_price: Price },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fill {
    order: Order,
    executed_at: TimestampNs,
    raw_price: Price,
    price: Price,
    pricing: FillPricing,
    raw_notional: Notional,
    trade_notional: Notional,
    costs: FillCosts,
    slippage: Notional,
    total_fees: Notional,
    cash_delta: Notional,
}
impl Fill {
    pub fn new(
        order: Order,
        executed_at: TimestampNs,
        raw_price: Price,
        pricing: FillPricing,
        costs: FillCosts,
    ) -> Result<Self, LedgerError> {
        if executed_at < order.submitted_at {
            return Err(LedgerError::FillBeforeOrder);
        }
        if raw_price <= Price::ZERO {
            return Err(LedgerError::NonPositivePrice);
        }
        costs.validate()?;
        let raw_notional = notional(raw_price, order.quantity)?;
        let (price, trade_notional, slippage) = match pricing {
            FillPricing::SeparateSlippage { slippage } => {
                if slippage < Notional::ZERO {
                    return Err(LedgerError::NegativeFee);
                }
                (raw_price, raw_notional, slippage)
            }
            FillPricing::EmbeddedSlippage { execution_price } => {
                if execution_price <= Price::ZERO {
                    return Err(LedgerError::NonPositivePrice);
                }
                if (order.side == Side::Buy && execution_price < raw_price)
                    || (order.side == Side::Sell && execution_price > raw_price)
                {
                    return Err(LedgerError::FavorableSlippage);
                }
                let trade_notional = notional(execution_price, order.quantity)?;
                let slippage = if order.side == Side::Buy {
                    subtract(trade_notional, raw_notional)?
                } else {
                    subtract(raw_notional, trade_notional)?
                };
                (execution_price, trade_notional, slippage)
            }
        };
        let total_fees = add(add(costs.total_commission()?, costs.tax)?, slippage)?;
        let signed_raw = raw_notional.checked_mul(if order.side == Side::Buy { -1 } else { 1 })?;
        let cash_delta = subtract(signed_raw, total_fees)?;
        Ok(Self {
            order,
            executed_at,
            raw_price,
            price,
            pricing,
            raw_notional,
            trade_notional,
            costs,
            slippage,
            total_fees,
            cash_delta,
        })
    }
    pub fn order(&self) -> &Order {
        &self.order
    }
    pub fn executed_at(&self) -> TimestampNs {
        self.executed_at
    }
    pub fn raw_price(&self) -> Price {
        self.raw_price
    }
    pub fn price(&self) -> Price {
        self.price
    }
    pub fn pricing(&self) -> FillPricing {
        self.pricing
    }
    pub fn raw_notional(&self) -> Notional {
        self.raw_notional
    }
    pub fn trade_notional(&self) -> Notional {
        self.trade_notional
    }
    pub fn costs(&self) -> FillCosts {
        self.costs
    }
    pub fn slippage(&self) -> Notional {
        self.slippage
    }
    pub fn total_fees(&self) -> Notional {
        self.total_fees
    }
    /// Signed raw-price principal minus every cost component (including slippage).
    pub fn cash_delta(&self) -> Notional {
        self.cash_delta
    }
}
