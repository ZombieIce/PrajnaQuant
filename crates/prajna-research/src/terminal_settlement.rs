//! Opt-in close settlement for instruments with an evidenced Catalog termination.
use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use prajna_domain::{InstrumentId, Notional, Price, Quantity};
use serde::{Deserialize, Serialize};

use crate::{Panel, etf_market::DerivedMarket};

pub const SEMANTIC_VERSION: &str = "mvp6-terminal-settlement@1";

#[derive(Debug, Clone, Default)]
pub struct TerminalSettlementSchedule {
    dates: BTreeMap<NaiveDate, BTreeSet<InstrumentId>>,
}

impl TerminalSettlementSchedule {
    pub fn from_market(market: &DerivedMarket, panel: &Panel) -> Result<Self, String> {
        let Some(last_session) = panel.sessions.last() else {
            return Err("terminal settlement requires at least one session".into());
        };
        let mut dates = BTreeMap::<NaiveDate, BTreeSet<InstrumentId>>::new();
        for instrument in &panel.instruments {
            let Some(termination_date) = market.termination_date(instrument) else {
                continue;
            };
            if termination_date > last_session.session_date {
                continue;
            }
            let settlement_date = require_terminal_bar_date(
                instrument,
                termination_date,
                panel.sessions.iter().filter_map(|session| {
                    market
                        .cells()
                        .get(&(instrument.clone(), session.session_date))
                        .is_some_and(|cell| cell.close.is_some())
                        .then_some(session.session_date)
                }),
            )?;
            dates
                .entry(settlement_date)
                .or_default()
                .insert(instrument.clone());
        }
        Ok(Self { dates })
    }

    pub fn instruments(&self, date: NaiveDate) -> Option<&BTreeSet<InstrumentId>> {
        self.dates.get(&date)
    }

    pub fn contains(&self, date: NaiveDate, instrument: &InstrumentId) -> bool {
        self.dates
            .get(&date)
            .is_some_and(|instruments| instruments.contains(instrument))
    }

    pub fn is_empty(&self) -> bool {
        self.dates.is_empty()
    }
}

fn require_terminal_bar_date(
    instrument: &InstrumentId,
    termination_date: NaiveDate,
    bar_dates: impl Iterator<Item = NaiveDate>,
) -> Result<NaiveDate, String> {
    bar_dates
        .filter(|date| *date < termination_date)
        .max()
        .ok_or_else(|| {
            format!(
                "terminal settlement for {instrument} at {termination_date} has no Bar close in its lifecycle"
            )
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSettlementEntry {
    pub session_date: NaiveDate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portfolio_id: Option<prajna_account::VirtualPortfolioId>,
    pub instrument_id: InstrumentId,
    pub quantity: Quantity,
    pub price: Price,
    pub proceeds: Notional,
}

pub fn proceeds(quantity: Quantity, price: Price) -> Result<Notional, String> {
    if quantity < Quantity::ZERO || price <= Price::ZERO {
        return Err("terminal settlement quantity and price must be nonnegative/positive".into());
    }
    let quantity = Notional::from_mantissa(quantity.mantissa()).map_err(|e| e.to_string())?;
    let price = Notional::from_mantissa(price.mantissa()).map_err(|e| e.to_string())?;
    price
        .checked_mul_decimal(quantity)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminated_lifecycle_without_a_bar_close_is_an_explicit_error() {
        let instrument = InstrumentId::new(
            "159001-20260101-TERMINAL",
            prajna_domain::VenueId::new("SZSE").unwrap(),
        )
        .unwrap();
        let error = require_terminal_bar_date(
            &instrument,
            NaiveDate::from_ymd_opt(2026, 1, 8).unwrap(),
            std::iter::empty(),
        )
        .unwrap_err();
        assert!(error.contains("has no Bar close in its lifecycle"));
    }
}
