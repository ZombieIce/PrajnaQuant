//! Shared F8 money/quantity sizing; no orders, settlement ledger or performance engine.
use std::collections::{BTreeMap, BTreeSet};

use prajna_account::{FillCosts, LedgerError, Side};
use prajna_domain::{FixedPointError, InstrumentId, Notional, Price, Quantity};

use crate::fast_event::{FastEventCosts, FastEventError, add, quantity, sub, value, weight};

#[derive(Debug, Clone)]
pub(crate) struct ParityLeg {
    pub instrument: InstrumentId,
    pub price: Price,
    pub current: Quantity,
    pub desired: Quantity,
    pub basis: Notional,
    pub fee_side: Side,
    pub fees: FillCosts,
    pub slip: Notional,
}

impl ParityLeg {
    pub fn delta(&self) -> Result<i128, FastEventError> {
        Ok(self
            .desired
            .mantissa()
            .checked_sub(self.current.mantissa())
            .ok_or(FixedPointError::Overflow)?)
    }

    pub fn total_fees(&self) -> Result<Notional, FastEventError> {
        add(add(self.fees.commission, self.fees.tax)?, self.slip)
    }

    pub fn required_side(&self) -> Result<Option<Side>, FastEventError> {
        let delta = self.delta()?;
        Ok(if delta > 0 {
            Some(Side::Buy)
        } else if delta < 0 {
            Some(Side::Sell)
        } else if self.total_fees()? > Notional::ZERO {
            Some(self.fee_side)
        } else {
            None
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ParityPlan {
    pub equity: Notional,
    pub total_fees: Notional,
    pub turnover: Notional,
    pub legs: Vec<ParityLeg>,
}

pub(crate) fn target_plan(
    equity: Notional,
    positions: &BTreeMap<InstrumentId, Quantity>,
    targets: &BTreeMap<InstrumentId, f64>,
    prices: &BTreeMap<InstrumentId, Price>,
    costs: FastEventCosts,
    retained: &BTreeSet<InstrumentId>,
) -> Result<ParityPlan, FastEventError> {
    let mut plan = ParityPlan {
        equity,
        total_fees: Notional::ZERO,
        turnover: Notional::ZERO,
        legs: Vec::new(),
    };
    if equity == Notional::ZERO {
        return Ok(plan);
    }
    let instruments = positions
        .keys()
        .chain(targets.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut inputs = Vec::new();
    for instrument in instruments {
        if retained.contains(&instrument) {
            continue;
        }
        let price = *prices
            .get(&instrument)
            .ok_or_else(|| LedgerError::MissingPrice(instrument.clone()))?;
        let current = positions
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
        let absolute = Notional::from_mantissa(delta.mantissa().abs())?;
        let basis = equity.checked_mul_decimal(absolute)?;
        let (fees, slip) = costs.fees(basis, side)?;
        plan.turnover = add(plan.turnover, absolute)?;
        plan.total_fees = add(plan.total_fees, add(add(fees.commission, fees.tax)?, slip)?)?;
        inputs.push((instrument, price, current, target, basis, side, fees, slip));
    }
    let after = sub(equity, plan.total_fees)?;
    if after <= Notional::ZERO {
        return Err(FastEventError::InvalidInput(
            "costs exhaust pre-open equity".into(),
        ));
    }
    for (instrument, price, current, target, basis, fee_side, fees, slip) in inputs {
        plan.legs.push(ParityLeg {
            instrument,
            price,
            current,
            desired: quantity(target.checked_mul_decimal(after)?, price)?,
            basis,
            fee_side,
            fees,
            slip,
        });
    }
    Ok(plan)
}

/// Exact carried sizing inputs for Vector. Fast's TradingAccount remains the
/// canonical settlement ledger; this state has no orders, IDs or execution tape.
#[derive(Debug, Clone)]
pub(crate) struct ParitySizingState {
    pub cash: Notional,
    pub positions: BTreeMap<InstrumentId, Quantity>,
}

impl ParitySizingState {
    pub fn new(capital: Notional) -> Self {
        Self {
            cash: capital,
            positions: BTreeMap::new(),
        }
    }

    pub fn equity(
        &self,
        prices: &BTreeMap<InstrumentId, Price>,
    ) -> Result<Notional, FastEventError> {
        let mut equity = self.cash;
        for (id, q) in &self.positions {
            let price = *prices
                .get(id)
                .ok_or_else(|| LedgerError::MissingPrice(id.clone()))?;
            equity = add(equity, value(*q, price)?)?;
        }
        Ok(equity)
    }

    pub fn weights(
        &self,
        prices: &BTreeMap<InstrumentId, Price>,
    ) -> Result<BTreeMap<InstrumentId, f64>, FastEventError> {
        let equity = self.equity(prices)?;
        self.positions
            .iter()
            .map(|(id, q)| {
                let v = value(
                    *q,
                    *prices
                        .get(id)
                        .ok_or_else(|| LedgerError::MissingPrice(id.clone()))?,
                )?;
                let parse = |x: Notional| {
                    x.to_string()
                        .parse::<f64>()
                        .map_err(|e| FastEventError::InvalidInput(e.to_string()))
                };
                let w = parse(v)? / parse(equity)?;
                Ok((id.clone(), w))
            })
            .collect()
    }

    pub fn preview(&self, plan: &ParityPlan) -> Result<Self, FastEventError> {
        let mut next = self.clone();
        // Fast settles sells, then buys, then fee-only settlements.
        for side in [Some(Side::Sell), Some(Side::Buy), None] {
            for leg in &plan.legs {
                let delta = leg.delta()?;
                let actual = if delta > 0 {
                    Some(Side::Buy)
                } else if delta < 0 {
                    Some(Side::Sell)
                } else {
                    None
                };
                if actual != side {
                    continue;
                }
                let principal = value(Quantity::from_mantissa(delta.abs())?, leg.price)?;
                let signed = if actual == Some(Side::Buy) {
                    principal.checked_mul(-1)?
                } else {
                    principal
                };
                next.cash = add(next.cash, sub(signed, leg.total_fees()?)?)?;
                if next.cash < Notional::ZERO {
                    return Err(FastEventError::InvalidInput(
                        "sized parity cash is negative".into(),
                    ));
                }
                if leg.desired == Quantity::ZERO {
                    next.positions.remove(&leg.instrument);
                } else {
                    next.positions.insert(leg.instrument.clone(), leg.desired);
                }
            }
        }
        Ok(next)
    }

    pub fn risk_sale(
        &mut self,
        id: &InstrumentId,
        price: Price,
        costs: FastEventCosts,
    ) -> Result<(Notional, Notional), FastEventError> {
        let principal = value(self.positions[id], price)?;
        let (fees, slip) = costs.fees(principal, Side::Sell)?;
        let total = add(add(fees.commission, fees.tax)?, slip)?;
        let cash = add(self.cash, sub(principal, total)?)?;
        if cash < Notional::ZERO {
            return Err(FastEventError::InvalidInput(
                "risk sale exhausts cash".into(),
            ));
        }
        self.cash = cash;
        self.positions.remove(id);
        Ok((principal, total))
    }
}
