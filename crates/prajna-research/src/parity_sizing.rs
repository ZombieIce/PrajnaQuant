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
pub(crate) struct ParityCashAdjustment {
    pub instrument: InstrumentId,
    pub candidate_quantity: Quantity,
    pub actual_quantity: Quantity,
    pub candidate_principal: Notional,
    pub actual_principal: Notional,
    pub principal_budget: Notional,
    pub reserved_fee: Notional,
    pub cash_residue: Notional,
}

#[derive(Debug, Clone)]
pub(crate) struct ParityPlan {
    pub equity: Notional,
    pub total_fees: Notional,
    pub turnover: Notional,
    pub legs: Vec<ParityLeg>,
    /// Quantization-only buy reductions with their checked fixed-point evidence.
    pub cash_adjustments: Vec<ParityCashAdjustment>,
    /// Final cash after sells, all buy fees, buy principal and fee-only fees.
    pub cash_residue: Notional,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn target_plan(
    equity: Notional,
    cash: Notional,
    receivables: Notional,
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
        cash_adjustments: Vec::new(),
        cash_residue: Notional::ZERO,
    };
    if equity == Notional::ZERO {
        return Ok(plan);
    }
    let mut retained_value = Notional::ZERO;
    for instrument in retained {
        if let Some(current) = positions.get(instrument) {
            let price = *prices
                .get(instrument)
                .ok_or_else(|| LedgerError::MissingPrice(instrument.clone()))?;
            retained_value = add(retained_value, value(*current, price)?)?;
        }
    }
    let instruments = positions
        .keys()
        .chain(targets.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut inputs = Vec::new();
    let mut target_weight_sum = Notional::ZERO;
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
        target_weight_sum = add(target_weight_sum, target)?;
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
    settle_cash(
        &mut plan,
        cash,
        after,
        target_weight_sum,
        receivables,
        retained_value,
    )?;
    Ok(plan)
}

/// Exact (unquantized) target amounts leave `after * (1 - sum(targets))` for
/// cash after deducting receivables and retained positions. The scale-18
/// products are compared as 256-bit integers so the proof does not depend on
/// a rounded reference value or a fixed epsilon.
fn rounding_only_feasible(
    after: Notional,
    target_weight_sum: Notional,
    receivables: Notional,
    retained_value: Notional,
) -> Result<bool, FastEventError> {
    let remaining_weight = sub("1".parse()?, target_weight_sum)?;
    if remaining_weight < Notional::ZERO {
        return Ok(false);
    }
    let committed = add(receivables, retained_value)?;
    let scale: u128 = 1_000_000_000_000_000_000;
    let left = wide_mul(
        u128::try_from(after.mantissa()).map_err(|_| FixedPointError::Overflow)?,
        u128::try_from(remaining_weight.mantissa()).map_err(|_| FixedPointError::Overflow)?,
    );
    let right = wide_mul(
        u128::try_from(committed.mantissa()).map_err(|_| FixedPointError::Overflow)?,
        scale,
    );
    Ok(left >= right)
}

fn wide_mul(left: u128, right: u128) -> (u128, u128) {
    let mask = u128::from(u64::MAX);
    let left_low = left & mask;
    let right_low = right & mask;
    let low = left_low * right_low;
    let mut middle = (left >> 64) * right_low + (low >> 64);
    let high = (left >> 64) * (right >> 64) + (middle >> 64);
    middle = left_low * (right >> 64) + (middle & mask);
    (high + (middle >> 64), (middle << 64) | (low & mask))
}

fn settle_cash(
    plan: &mut ParityPlan,
    cash: Notional,
    after: Notional,
    target_weight_sum: Notional,
    receivables: Notional,
    retained_value: Notional,
) -> Result<(), FastEventError> {
    let mut cash_after_sells = cash;
    for leg in &plan.legs {
        let delta = leg.delta()?;
        if delta >= 0 {
            continue;
        }
        cash_after_sells = add(
            cash_after_sells,
            value(Quantity::from_mantissa(-delta)?, leg.price)?,
        )?;
        cash_after_sells = sub(cash_after_sells, leg.total_fees()?)?;
        if cash_after_sells < Notional::ZERO {
            return Err(FastEventError::InvalidInput(
                "full-target sell settlement exceeds available cash".into(),
            ));
        }
    }
    let mut reserved_fees = Notional::ZERO;
    for leg in &plan.legs {
        if leg.delta()? >= 0 {
            let fees = leg.total_fees()?;
            if fees > Notional::ZERO {
                reserved_fees = add(reserved_fees, fees)?;
            }
        }
    }
    let mut budget = sub(cash_after_sells, reserved_fees)?;
    if budget < Notional::ZERO {
        return Err(FastEventError::InvalidInput(
            "unpaid full-target fees exceed cash after sells".into(),
        ));
    }
    let mut constrained = false;
    for leg in &plan.legs {
        let delta = leg.delta()?;
        if delta <= 0 {
            continue;
        }
        let principal = value(Quantity::from_mantissa(delta)?, leg.price)?;
        if principal > budget {
            constrained = true;
            break;
        }
        budget = sub(budget, principal)?;
    }
    if !constrained {
        plan.cash_residue = budget;
        return Ok(());
    }
    if !rounding_only_feasible(after, target_weight_sum, receivables, retained_value)? {
        return Err(FastEventError::InvalidInput(
            "full-target cash shortfall is not attributable to scale-18 rounding".into(),
        ));
    }
    let mut remaining = sub(cash_after_sells, reserved_fees)?;
    for leg in &mut plan.legs {
        let delta = leg.delta()?;
        if delta <= 0 {
            continue;
        }
        let candidate_quantity = Quantity::from_mantissa(delta)?;
        let candidate_principal = value(candidate_quantity, leg.price)?;
        let actual_quantity = if candidate_principal <= remaining {
            candidate_quantity
        } else {
            crate::fast_event::affordable_quantity(candidate_quantity, leg.price, remaining)?
        };
        let actual_principal = value(actual_quantity, leg.price)?;
        if actual_quantity != candidate_quantity {
            leg.desired = Quantity::from_mantissa(
                leg.current
                    .mantissa()
                    .checked_add(actual_quantity.mantissa())
                    .ok_or(FixedPointError::Overflow)?,
            )?;
            plan.cash_adjustments.push(ParityCashAdjustment {
                instrument: leg.instrument.clone(),
                candidate_quantity,
                actual_quantity,
                candidate_principal,
                actual_principal,
                principal_budget: remaining,
                reserved_fee: leg.total_fees()?,
                cash_residue: Notional::ZERO,
            });
        }
        remaining = sub(remaining, actual_principal)?;
    }
    plan.cash_residue = remaining;
    if plan.cash_residue < Notional::ZERO {
        return Err(FastEventError::InvalidInput(
            "checked full-target cash adjustment is negative".into(),
        ));
    }
    for adjustment in &mut plan.cash_adjustments {
        adjustment.cash_residue = plan.cash_residue;
    }
    Ok(())
}

/// Exact carried sizing inputs for Vector. Fast's TradingAccount remains the
/// canonical settlement ledger; this state has no orders, IDs or execution tape.
#[derive(Debug, Clone)]
pub(crate) struct ParitySizingState {
    pub cash: Notional,
    pub dividend_receivable: Notional,
    pub liquidation_receivable: Notional,
    pub positions: BTreeMap<InstrumentId, Quantity>,
}

impl ParitySizingState {
    pub fn new(capital: Notional) -> Self {
        Self {
            cash: capital,
            dividend_receivable: Notional::ZERO,
            liquidation_receivable: Notional::ZERO,
            positions: BTreeMap::new(),
        }
    }

    pub fn equity(
        &self,
        prices: &BTreeMap<InstrumentId, Price>,
    ) -> Result<Notional, FastEventError> {
        let mut equity = add(
            add(self.cash, self.dividend_receivable)?,
            self.liquidation_receivable,
        )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fast_event::FastEventCosts;

    fn price(value: &str) -> Price {
        value.parse().unwrap()
    }
    fn quantity(value: &str) -> Quantity {
        value.parse().unwrap()
    }
    fn instrument(value: &str) -> InstrumentId {
        value.parse().unwrap()
    }

    #[test]
    fn exact_single_leg_and_cash_residue_are_unchanged() {
        let a = instrument("A.SYNTH");
        let prices = BTreeMap::from([(a.clone(), price("1"))]);
        for (target, expected, residue) in [("1", "1000", "0"), ("0.5", "500", "500")] {
            let plan = target_plan(
                "1000".parse().unwrap(),
                "1000".parse().unwrap(),
                Notional::ZERO,
                &BTreeMap::new(),
                &BTreeMap::from([(a.clone(), target.parse().unwrap())]),
                &prices,
                FastEventCosts::ZERO,
                &BTreeSet::new(),
            )
            .unwrap();
            assert!(plan.cash_adjustments.is_empty());
            assert_eq!(plan.legs[0].desired, quantity(expected));
            assert_eq!(plan.cash_residue, residue.parse().unwrap());
        }
    }

    #[test]
    fn tiny_representable_quantity_is_not_clamped_away() {
        let a = instrument("A.SYNTH");
        let prices = BTreeMap::from([(a.clone(), price("1"))]);
        let plan = target_plan(
            "0.000000000000000002".parse().unwrap(),
            "0.000000000000000002".parse().unwrap(),
            Notional::ZERO,
            &BTreeMap::new(),
            &BTreeMap::from([(a, 0.5)]),
            &prices,
            FastEventCosts::ZERO,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(plan.legs[0].desired, quantity("0.000000000000000001"));
        assert_eq!(plan.cash_residue, "0.000000000000000001".parse().unwrap());
        assert!(plan.cash_adjustments.is_empty());
    }

    #[test]
    fn receivables_are_not_spendable_cash_and_fail_before_adjustment() {
        let ids = [instrument("A.SYNTH")];
        let prices = BTreeMap::from([(ids[0].clone(), price("1"))]);
        let targets = BTreeMap::from([(ids[0].clone(), 1.0)]);
        let error = target_plan(
            "1000".parse().unwrap(),
            Notional::ZERO,
            "1000".parse().unwrap(),
            &BTreeMap::new(),
            &targets,
            &prices,
            FastEventCosts::ZERO,
            &BTreeSet::new(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("not attributable to scale-18 rounding"),
            "{error}"
        );
    }

    #[test]
    fn retained_positions_are_not_liquidated_to_fund_other_targets() {
        let a = instrument("A.SYNTH");
        let b = instrument("B.SYNTH");
        let prices = BTreeMap::from([(a.clone(), price("1")), (b.clone(), price("1"))]);
        let positions = BTreeMap::from([(a.clone(), quantity("500"))]);
        let targets = BTreeMap::from([(a.clone(), 0.5), (b.clone(), 0.5)]);
        let error = target_plan(
            "500".parse().unwrap(),
            Notional::ZERO,
            Notional::ZERO,
            &positions,
            &targets,
            &prices,
            FastEventCosts::ZERO,
            &BTreeSet::from([a]),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("not attributable to scale-18 rounding"),
            "{error}"
        );
    }
}
