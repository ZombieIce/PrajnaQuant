use crate::{LedgerError, TradingAccount, VirtualPortfolioId, add, change_quantity, notional};
use prajna_domain::{InstrumentId, Notional, Price, Quantity};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MarkedPosition {
    pub quantity: Quantity,
    pub price: Price,
    /// Account marks sum the rounded VP marks, rather than rounding again.
    pub market_value: Notional,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortfolioValuation {
    pub cash: Notional,
    pub positions: BTreeMap<InstrumentId, MarkedPosition>,
    pub equity: Notional,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountValuation {
    pub unallocated_capital: Notional,
    pub account: PortfolioValuation,
    pub portfolios: BTreeMap<VirtualPortfolioId, PortfolioValuation>,
}
impl TradingAccount {
    /// Prices must be present and positive for every held instrument. The caller
    /// chooses the valuation point and supplies carried prices for missing bars;
    /// this method never infers execution eligibility from a valuation price.
    pub fn value(
        &self,
        prices: &BTreeMap<InstrumentId, Price>,
    ) -> Result<AccountValuation, LedgerError> {
        self.validate_balances()?;
        let mut portfolios = BTreeMap::new();
        let mut account_positions: BTreeMap<InstrumentId, MarkedPosition> = BTreeMap::new();
        for (id, portfolio) in self.portfolios() {
            let mut positions = BTreeMap::new();
            let mut equity = portfolio.cash();
            for (instrument, quantity) in portfolio.positions() {
                let price = *prices
                    .get(instrument)
                    .ok_or_else(|| LedgerError::MissingPrice(instrument.clone()))?;
                if price <= Price::ZERO {
                    return Err(LedgerError::NonPositivePrice);
                }
                let market_value = notional(price, *quantity)?;
                equity = add(equity, market_value)?;
                positions.insert(
                    instrument.clone(),
                    MarkedPosition {
                        quantity: *quantity,
                        price,
                        market_value,
                    },
                );
                let aggregate =
                    account_positions
                        .entry(instrument.clone())
                        .or_insert(MarkedPosition {
                            quantity: Quantity::ZERO,
                            price,
                            market_value: Notional::ZERO,
                        });
                aggregate.quantity = change_quantity(aggregate.quantity, *quantity)?;
                aggregate.market_value = add(aggregate.market_value, market_value)?;
            }
            portfolios.insert(
                *id,
                PortfolioValuation {
                    cash: portfolio.cash(),
                    positions,
                    equity,
                },
            );
        }
        let mut equity = self.cash();
        for position in account_positions.values() {
            equity = add(equity, position.market_value)?;
        }
        let result = AccountValuation {
            unallocated_capital: self.unallocated_capital(),
            account: PortfolioValuation {
                cash: self.cash(),
                positions: account_positions,
                equity,
            },
            portfolios,
        };
        result.validate()?;
        Ok(result)
    }
}
impl AccountValuation {
    /// Validates an exported snapshot with exact equality, including each VP mark.
    pub fn validate(&self) -> Result<(), LedgerError> {
        if self.unallocated_capital < Notional::ZERO || self.account.cash < Notional::ZERO {
            return Err(LedgerError::NegativeCash);
        }
        let mut cash = self.unallocated_capital;
        let mut equity = self.unallocated_capital;
        let mut positions: BTreeMap<InstrumentId, MarkedPosition> = BTreeMap::new();
        for portfolio in self.portfolios.values() {
            if portfolio.cash < Notional::ZERO {
                return Err(LedgerError::NegativeCash);
            }
            cash = add(cash, portfolio.cash)?;
            let mut vp_equity = portfolio.cash;
            for (id, position) in &portfolio.positions {
                if position.quantity <= Quantity::ZERO {
                    return Err(LedgerError::NonPositiveQuantity);
                }
                if position.price <= Price::ZERO {
                    return Err(LedgerError::NonPositivePrice);
                }
                if position.market_value != notional(position.price, position.quantity)? {
                    return Err(LedgerError::Invariant(
                        "VP market value != rounded quantity * price",
                    ));
                }
                vp_equity = add(vp_equity, position.market_value)?;
                let aggregate = positions.entry(id.clone()).or_insert(MarkedPosition {
                    quantity: Quantity::ZERO,
                    price: position.price,
                    market_value: Notional::ZERO,
                });
                if aggregate.price != position.price {
                    return Err(LedgerError::Invariant("VP valuation prices differ"));
                }
                aggregate.quantity = change_quantity(aggregate.quantity, position.quantity)?;
                aggregate.market_value = add(aggregate.market_value, position.market_value)?;
            }
            if portfolio.equity != vp_equity {
                return Err(LedgerError::Invariant("VP equity != cash + market value"));
            }
            equity = add(equity, portfolio.equity)?;
        }
        if self.account.cash != cash {
            return Err(LedgerError::Invariant(
                "account cash != VP cash + unallocated",
            ));
        }
        if self.account.positions != positions {
            return Err(LedgerError::Invariant("account positions != VP positions"));
        }
        let mut marked_equity = self.account.cash;
        for position in self.account.positions.values() {
            marked_equity = add(marked_equity, position.market_value)?;
        }
        if self.account.equity != equity || self.account.equity != marked_equity {
            return Err(LedgerError::Invariant(
                "account equity != VP equity + unallocated",
            ));
        }
        Ok(())
    }
}
