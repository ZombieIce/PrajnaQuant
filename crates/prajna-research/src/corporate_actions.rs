use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use chrono::NaiveDate;
use prajna_account::{ReceivableKind, VirtualPortfolioId};
use prajna_data::{CorporateActionKind, CorporateActionRow};
use prajna_domain::{DECIMAL_SCALE, InstrumentId, Notional, Price, Quantity};
use serde::{Deserialize, Serialize};

use crate::Panel;

pub const SEMANTIC_VERSION: &str = "corporate_action_ledger@1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReturnBasis {
    #[default]
    PriceReturn,
    TotalReturn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionCoverageStatus {
    /// Explicit caller attestation for the requested instrument/date range.
    Complete,
    /// Rows were observed, but official announcement completeness is unproven.
    ObservedRawSetOnly,
    /// No coverage evidence was supplied for this range.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorporateActionCoverage {
    pub instrument_id: InstrumentId,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub observed_actions: usize,
    pub unapplied_action_dates: Vec<NaiveDate>,
    pub unexplained_price_jumps: Vec<NaiveDate>,
    pub status: ActionCoverageStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorporateActionCompleteness {
    pub is_complete: bool,
    pub reasons: Vec<String>,
}

impl Default for CorporateActionCompleteness {
    fn default() -> Self {
        Self {
            is_complete: false,
            reasons: vec!["corporate-action completeness was not supplied".into()],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorporateActionLedgerKind {
    DividendEntitlement,
    DividendReceivable,
    DividendPaid,
    ShareConversion,
    LiquidationReceivable,
    LiquidationPaid,
    LiquidationAmountUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorporateActionLedgerEntry {
    pub session_date: NaiveDate,
    pub portfolio_id: Option<VirtualPortfolioId>,
    pub announcement_id: String,
    pub action_index: u32,
    pub instrument_id: InstrumentId,
    pub kind: CorporateActionLedgerKind,
    pub quantity_before: Option<Quantity>,
    pub quantity_after: Option<Quantity>,
    pub share_ratio: Option<Notional>,
    pub theoretical_quantity: Option<Quantity>,
    pub residual_quantity: Option<Quantity>,
    pub gross_amount: Notional,
    pub tax_amount: Notional,
    pub net_amount: Notional,
    pub corporate_action_rounding: Option<Notional>,
    pub corporate_action_value_delta: Option<Notional>,
}

#[derive(Debug, Clone)]
pub struct CorporateActionSchedule {
    version: String,
    actions: Vec<CorporateActionRow>,
    coverage: Vec<CorporateActionCoverage>,
    dividend_tax_rate: Notional,
    incomplete_reasons: Vec<String>,
}

impl CorporateActionSchedule {
    pub fn new(
        version: impl Into<String>,
        mut actions: Vec<CorporateActionRow>,
        mut coverage: Vec<CorporateActionCoverage>,
    ) -> Result<Self, CorporateActionError> {
        let version = version.into();
        if version.trim().is_empty() {
            return Err(invalid("corporate-action version must not be empty"));
        }
        coverage.sort_by(|left, right| left.instrument_id.cmp(&right.instrument_id));
        if coverage.windows(2).any(|pair| {
            pair[0].instrument_id == pair[1].instrument_id
                || pair[0].start > pair[0].end
                || pair[1].start > pair[1].end
        }) || coverage.iter().any(|item| item.start > item.end)
        {
            return Err(invalid("invalid or duplicate corporate-action coverage"));
        }
        for item in &mut coverage {
            if !item.unapplied_action_dates.is_empty() || !item.unexplained_price_jumps.is_empty() {
                item.status = ActionCoverageStatus::Unknown;
            }
        }
        let mut identities = BTreeSet::new();
        for action in &actions {
            validate_action(action)?;
            if !identities.insert((action.announcement_id.clone(), action.action_index)) {
                return Err(invalid("duplicate corporate-action identity"));
            }
        }
        actions.sort_by(|left, right| {
            (
                &left.instrument_id,
                opening_date(left),
                &left.announcement_id,
                left.action_index,
            )
                .cmp(&(
                    &right.instrument_id,
                    opening_date(right),
                    &right.announcement_id,
                    right.action_index,
                ))
        });
        for pair in actions.windows(2) {
            if pair[0].instrument_id == pair[1].instrument_id
                && opening_date(&pair[0]).is_some()
                && opening_date(&pair[0]) == opening_date(&pair[1])
            {
                return Err(invalid(
                    "multiple same-instrument opening actions have unresolved ordering",
                ));
            }
        }
        Ok(Self {
            version,
            actions,
            coverage,
            dividend_tax_rate: Notional::ZERO,
            incomplete_reasons: Vec::new(),
        })
    }

    pub fn with_dividend_tax_rate(
        mut self,
        dividend_tax_rate: Notional,
    ) -> Result<Self, CorporateActionError> {
        let one: Notional = "1".parse().expect("one is representable");
        if dividend_tax_rate < Notional::ZERO || dividend_tax_rate >= one {
            return Err(invalid("dividend tax rate must be in [0, 1)"));
        }
        self.dividend_tax_rate = dividend_tax_rate;
        Ok(self)
    }

    pub fn from_total_return_index(
        version: impl Into<String>,
        panel: &Panel,
        index: &crate::etf_momentum::TotalReturnIndex,
        dividend_tax_rate: Notional,
    ) -> Result<Self, CorporateActionError> {
        let Some(start) = panel.sessions.first().map(|session| session.session_date) else {
            return Err(invalid("corporate actions require a non-empty Panel"));
        };
        let end = panel
            .sessions
            .last()
            .expect("non-empty sessions")
            .session_date;
        let coverage = panel
            .instruments
            .iter()
            .map(|instrument_id| {
                let report = index
                    .integrity()
                    .iter()
                    .find(|report| report.instrument_id == *instrument_id);
                let (observed_actions, unapplied, jumps, status) = report.map_or(
                    (0, Vec::new(), Vec::new(), ActionCoverageStatus::Unknown),
                    |report| {
                        let covers_panel = report.start <= start && report.end >= end;
                        (
                            report.observed_actions,
                            report
                                .unapplied_action_dates
                                .iter()
                                .copied()
                                .filter(|date| *date >= start && *date <= end)
                                .collect(),
                            report
                                .unexplained_price_jumps
                                .iter()
                                .copied()
                                .filter(|date| *date >= start && *date <= end)
                                .collect(),
                            if covers_panel {
                                ActionCoverageStatus::ObservedRawSetOnly
                            } else {
                                ActionCoverageStatus::Unknown
                            },
                        )
                    },
                );
                CorporateActionCoverage {
                    instrument_id: instrument_id.clone(),
                    start,
                    end,
                    observed_actions,
                    unapplied_action_dates: unapplied,
                    unexplained_price_jumps: jumps,
                    status,
                }
            })
            .collect();
        Self::new(version, index.actions().to_vec(), coverage)?
            .with_dividend_tax_rate(dividend_tax_rate)
    }

    /// Consumer evidence gaps do not fabricate executable events.
    pub fn with_incomplete_reasons(mut self, reasons: Vec<String>) -> Self {
        self.incomplete_reasons = reasons;
        self
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn actions(&self) -> &[CorporateActionRow] {
        &self.actions
    }

    pub fn coverage(&self) -> &[CorporateActionCoverage] {
        &self.coverage
    }

    pub fn dividend_tax_rate(&self) -> Notional {
        self.dividend_tax_rate
    }

    pub(crate) fn validate_for_panel(&self, panel: &Panel) -> Result<(), CorporateActionError> {
        let Some(first) = panel.sessions.first().map(|s| s.session_date) else {
            return Err(invalid("corporate actions require a non-empty Panel"));
        };
        let last = panel
            .sessions
            .last()
            .expect("non-empty sessions")
            .session_date;
        if self.coverage.len() != panel.instruments.len()
            || self
                .coverage
                .iter()
                .zip(&panel.instruments)
                .any(|(c, id)| &c.instrument_id != id || c.start != first || c.end != last)
        {
            return Err(invalid(
                "corporate-action coverage must match every Panel instrument and Session",
            ));
        }
        let sessions = panel
            .sessions
            .iter()
            .map(|session| session.session_date)
            .collect::<BTreeSet<_>>();
        for coverage in &self.coverage {
            if coverage.status != ActionCoverageStatus::Complete {
                continue;
            }
            let row_count = self
                .actions
                .iter()
                .filter(|action| {
                    action.instrument_id == coverage.instrument_id
                        && action_dates(action)
                            .iter()
                            .any(|date| *date >= coverage.start && *date <= coverage.end)
                })
                .count();
            if row_count != coverage.observed_actions {
                return Err(invalid(format!(
                    "{} complete coverage reports {} actions but the schedule has {row_count}",
                    coverage.instrument_id, coverage.observed_actions
                )));
            }
        }
        for action in &self.actions {
            if !panel.instruments.contains(&action.instrument_id) {
                return Err(invalid(format!(
                    "corporate action references instrument outside Panel: {}",
                    action.instrument_id
                )));
            }
            for date in action_dates(action) {
                if date >= first && date <= last && !sessions.contains(&date) {
                    return Err(invalid(format!(
                        "corporate-action date {date} is not a Panel Session"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorporateActionError(String);

impl fmt::Display for CorporateActionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl Error for CorporateActionError {}

#[derive(Debug, Clone)]
pub(crate) struct ActionTransition {
    pub entry: CorporateActionLedgerEntry,
    pub mutation: Option<ActionMutation>,
}

#[derive(Debug, Clone)]
pub(crate) enum ActionMutation {
    AddReceivable {
        kind: ReceivableKind,
        amount: Notional,
    },
    SetQuantity {
        instrument_id: InstrumentId,
        quantity: Quantity,
    },
    Liquidate {
        instrument_id: InstrumentId,
        amount: Notional,
    },
    SettleReceivable {
        kind: ReceivableKind,
        amount: Notional,
    },
}

#[derive(Debug, Clone)]
struct Entitlement {
    quantity: Quantity,
    gross: Notional,
    tax: Notional,
    net: Notional,
    accrued: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CorporateActionProcessor<'a> {
    schedule: &'a CorporateActionSchedule,
    ignore_liquidations: bool,
    entitlements: BTreeMap<(usize, Option<VirtualPortfolioId>), Entitlement>,
    liquidation_receivables: BTreeMap<(usize, Option<VirtualPortfolioId>), Notional>,
    ledger: Vec<CorporateActionLedgerEntry>,
    completeness: CorporateActionCompleteness,
}

impl<'a> CorporateActionProcessor<'a> {
    pub fn new(
        schedule: &'a CorporateActionSchedule,
        panel: &Panel,
    ) -> Result<Self, CorporateActionError> {
        Self::build(schedule, panel, false)
    }

    pub fn new_ignoring_liquidations(
        schedule: &'a CorporateActionSchedule,
        panel: &Panel,
    ) -> Result<Self, CorporateActionError> {
        Self::build(schedule, panel, true)
    }

    fn build(
        schedule: &'a CorporateActionSchedule,
        panel: &Panel,
        ignore_liquidations: bool,
    ) -> Result<Self, CorporateActionError> {
        schedule.validate_for_panel(panel)?;
        let mut reasons = schedule.incomplete_reasons.clone();
        for coverage in &schedule.coverage {
            match coverage.status {
                ActionCoverageStatus::Complete => {}
                ActionCoverageStatus::ObservedRawSetOnly => reasons.push(format!(
                    "{} corporate-action coverage is observed-raw-set-only",
                    coverage.instrument_id
                )),
                ActionCoverageStatus::Unknown => reasons.push(format!(
                    "{} corporate-action coverage is unknown",
                    coverage.instrument_id
                )),
            }
            if !coverage.unexplained_price_jumps.is_empty() {
                reasons.push(format!(
                    "{} has unexplained price jumps at {:?}",
                    coverage.instrument_id, coverage.unexplained_price_jumps
                ));
            }
            if !coverage.unapplied_action_dates.is_empty() {
                reasons.push(format!(
                    "{} has unapplied corporate actions at {:?}",
                    coverage.instrument_id, coverage.unapplied_action_dates
                ));
            }
        }
        Ok(Self {
            schedule,
            ignore_liquidations,
            entitlements: BTreeMap::new(),
            liquidation_receivables: BTreeMap::new(),
            ledger: Vec::new(),
            completeness: CorporateActionCompleteness {
                is_complete: reasons.is_empty(),
                reasons,
            },
        })
    }

    pub fn opening(
        &mut self,
        date: NaiveDate,
        portfolio_id: Option<VirtualPortfolioId>,
        positions: &BTreeMap<InstrumentId, Quantity>,
        prices: &BTreeMap<InstrumentId, Price>,
        fresh_prices: &BTreeSet<InstrumentId>,
        round_conversions: bool,
    ) -> Result<Vec<ActionTransition>, CorporateActionError> {
        let mut transitions = Vec::new();
        for (index, action) in self.schedule.actions.iter().enumerate() {
            if self.ignore_liquidations && action.kind == CorporateActionKind::Liquidation {
                continue;
            }
            if opening_date(action) != Some(date) {
                continue;
            }
            let quantity = positions
                .get(&action.instrument_id)
                .copied()
                .unwrap_or(Quantity::ZERO);
            match action.kind {
                CorporateActionKind::Dividend => {
                    if let Some(entitlement) = self.entitlements.get_mut(&(index, portfolio_id))
                        && !entitlement.accrued
                    {
                        entitlement.accrued = true;
                        transitions.push(transition(
                            action,
                            date,
                            portfolio_id,
                            CorporateActionLedgerKind::DividendReceivable,
                            Some(entitlement.quantity),
                            Some(entitlement.quantity),
                            entitlement.gross,
                            entitlement.tax,
                            entitlement.net,
                            Notional::ZERO,
                            Some(ActionMutation::AddReceivable {
                                kind: ReceivableKind::Dividend,
                                amount: entitlement.net,
                            }),
                        ));
                    }
                }
                CorporateActionKind::ShareConversion if quantity > Quantity::ZERO => {
                    let ratio = parse_quantity(action.share_ratio.as_deref().expect("validated"))?;
                    let theoretical = quantity.checked_mul_decimal(ratio).map_err(domain_error)?;
                    let actual = if round_conversions {
                        floor_to_whole_share(theoretical)?
                    } else {
                        theoretical
                    };
                    let residual = subtract_quantity(theoretical, actual)?;
                    let stale_rounding = residual != Quantity::ZERO
                        && round_conversions
                        && !fresh_prices.contains(&action.instrument_id);
                    let rounding = if residual == Quantity::ZERO || !round_conversions {
                        Notional::ZERO
                    } else if stale_rounding {
                        self.add_incomplete(format!(
                            "{} conversion rounding cannot be valued at a current open on {date}",
                            action.instrument_id
                        ));
                        Notional::ZERO
                    } else {
                        let price = prices.get(&action.instrument_id).ok_or_else(|| {
                            invalid(format!(
                                "missing valuation price for fractional conversion of {}",
                                action.instrument_id
                            ))
                        })?;
                        money_for(
                            residual,
                            Notional::from_mantissa(price.mantissa()).map_err(domain_error)?,
                        )?
                        .checked_mul(-1)
                        .map_err(domain_error)?
                    };
                    let mut entry = transition(
                        action,
                        date,
                        portfolio_id,
                        CorporateActionLedgerKind::ShareConversion,
                        Some(quantity),
                        Some(actual),
                        Notional::ZERO,
                        Notional::ZERO,
                        Notional::ZERO,
                        rounding,
                        Some(ActionMutation::SetQuantity {
                            instrument_id: action.instrument_id.clone(),
                            quantity: actual,
                        }),
                    );
                    entry.entry.share_ratio =
                        Some(Notional::from_mantissa(ratio.mantissa()).map_err(domain_error)?);
                    entry.entry.theoretical_quantity = Some(theoretical);
                    entry.entry.residual_quantity = Some(residual);
                    if stale_rounding {
                        entry.entry.corporate_action_rounding = None;
                    }
                    transitions.push(entry);
                }
                CorporateActionKind::Liquidation => {
                    self.add_incomplete_if_unknown(action, date);
                    let Some(per_share) = action
                        .liquidation_cash_per_share
                        .as_deref()
                        .map(parse_notional)
                        .transpose()?
                    else {
                        if quantity > Quantity::ZERO {
                            transitions.push(transition(
                                action,
                                date,
                                portfolio_id,
                                CorporateActionLedgerKind::LiquidationAmountUnknown,
                                Some(quantity),
                                Some(quantity),
                                Notional::ZERO,
                                Notional::ZERO,
                                Notional::ZERO,
                                Notional::ZERO,
                                None,
                            ));
                        }
                        continue;
                    };
                    if quantity > Quantity::ZERO {
                        let amount = money_for(quantity, per_share)?;
                        self.liquidation_receivables
                            .insert((index, portfolio_id), amount);
                        let mut entry = transition(
                            action,
                            date,
                            portfolio_id,
                            CorporateActionLedgerKind::LiquidationReceivable,
                            Some(quantity),
                            Some(Quantity::ZERO),
                            amount,
                            Notional::ZERO,
                            amount,
                            Notional::ZERO,
                            Some(ActionMutation::Liquidate {
                                instrument_id: action.instrument_id.clone(),
                                amount,
                            }),
                        );
                        entry.entry.corporate_action_value_delta = if fresh_prices
                            .contains(&action.instrument_id)
                        {
                            let price = prices.get(&action.instrument_id).ok_or_else(|| {
                                invalid(format!(
                                    "missing current valuation price for {}",
                                    action.instrument_id
                                ))
                            })?;
                            let market_value = money_for(
                                quantity,
                                Notional::from_mantissa(price.mantissa()).map_err(domain_error)?,
                            )?;
                            Some(subtract_notional(amount, market_value)?)
                        } else {
                            self.add_incomplete(format!(
                                "{} liquidation value delta has no current open at {date}",
                                action.instrument_id
                            ));
                            None
                        };
                        transitions.push(entry);
                    }
                }
                CorporateActionKind::ShareConversion => {}
            }
        }
        Ok(transitions)
    }

    pub fn closing(
        &mut self,
        date: NaiveDate,
        portfolio_id: Option<VirtualPortfolioId>,
        positions: &BTreeMap<InstrumentId, Quantity>,
    ) -> Result<Vec<ActionTransition>, CorporateActionError> {
        let mut transitions = Vec::new();
        for (index, action) in self.schedule.actions.iter().enumerate() {
            if self.ignore_liquidations && action.kind == CorporateActionKind::Liquidation {
                continue;
            }
            if action.pay_date != Some(date) {
                continue;
            }
            match action.kind {
                CorporateActionKind::Dividend => {
                    if let Some(entitlement) = self.entitlements.get(&(index, portfolio_id))
                        && entitlement.accrued
                    {
                        transitions.push(transition(
                            action,
                            date,
                            portfolio_id,
                            CorporateActionLedgerKind::DividendPaid,
                            Some(entitlement.quantity),
                            Some(entitlement.quantity),
                            entitlement.gross,
                            entitlement.tax,
                            entitlement.net,
                            Notional::ZERO,
                            Some(ActionMutation::SettleReceivable {
                                kind: ReceivableKind::Dividend,
                                amount: entitlement.net,
                            }),
                        ));
                        self.entitlements.remove(&(index, portfolio_id));
                    }
                }
                CorporateActionKind::Liquidation => {
                    if let Some(amount) =
                        self.liquidation_receivables.remove(&(index, portfolio_id))
                    {
                        transitions.push(transition(
                            action,
                            date,
                            portfolio_id,
                            CorporateActionLedgerKind::LiquidationPaid,
                            None,
                            None,
                            amount,
                            Notional::ZERO,
                            amount,
                            Notional::ZERO,
                            Some(ActionMutation::SettleReceivable {
                                kind: ReceivableKind::Liquidation,
                                amount,
                            }),
                        ));
                    }
                }
                CorporateActionKind::ShareConversion => {}
            }
        }
        for (index, action) in self.schedule.actions.iter().enumerate() {
            if action.kind != CorporateActionKind::Dividend || action.record_date != Some(date) {
                continue;
            }
            let quantity = positions
                .get(&action.instrument_id)
                .copied()
                .unwrap_or(Quantity::ZERO);
            let per_share = parse_notional(action.cash_per_share.as_deref().expect("validated"))?;
            let gross = money_for(quantity, per_share)?;
            let tax = gross
                .checked_mul_decimal(self.schedule.dividend_tax_rate)
                .map_err(domain_error)?;
            let net = subtract_notional(gross, tax)?;
            let entitlement = Entitlement {
                quantity,
                gross,
                tax,
                net,
                accrued: false,
            };
            self.entitlements.insert((index, portfolio_id), entitlement);
            transitions.push(transition(
                action,
                date,
                portfolio_id,
                CorporateActionLedgerKind::DividendEntitlement,
                Some(quantity),
                Some(quantity),
                gross,
                tax,
                net,
                Notional::ZERO,
                None,
            ));
        }
        Ok(transitions)
    }

    pub fn record(&mut self, transitions: Vec<ActionTransition>) {
        for transition in &transitions {
            self.ledger.push(transition.entry.clone());
        }
    }

    pub fn terminal_instruments(&self, date: NaiveDate) -> BTreeSet<InstrumentId> {
        self.schedule
            .actions
            .iter()
            .filter(|action| {
                !self.ignore_liquidations
                    && action.kind == CorporateActionKind::Liquidation
                    && action
                        .effective_date
                        .is_some_and(|effective| effective <= date)
            })
            .map(|action| action.instrument_id.clone())
            .collect()
    }

    pub fn ledger(&self) -> &[CorporateActionLedgerEntry] {
        &self.ledger
    }

    pub fn completeness(&self) -> CorporateActionCompleteness {
        CorporateActionCompleteness {
            is_complete: self.completeness.reasons.is_empty(),
            reasons: self.completeness.reasons.clone(),
        }
    }

    fn add_incomplete_if_unknown(&mut self, action: &CorporateActionRow, date: NaiveDate) {
        if action.liquidation_cash_per_share.is_none() {
            let reason = format!(
                "{} liquidation amount is unknown on {date}",
                action.instrument_id
            );
            self.add_incomplete(reason);
        }
    }

    fn add_incomplete(&mut self, reason: String) {
        if !self.completeness.reasons.contains(&reason) {
            self.completeness.reasons.push(reason);
        }
        self.completeness.is_complete = false;
    }
}

pub(crate) fn apply_transition_to_account(
    transition: &ActionTransition,
    account: &mut prajna_account::TradingAccount,
    portfolio_id: VirtualPortfolioId,
) -> Result<(), prajna_account::LedgerError> {
    let Some(mutation) = &transition.mutation else {
        return Ok(());
    };
    match mutation {
        ActionMutation::AddReceivable { kind, amount } => {
            account.add_receivable(portfolio_id, *kind, *amount)
        }
        ActionMutation::SetQuantity {
            instrument_id,
            quantity,
        } => account
            .convert_quantity(portfolio_id, instrument_id, *quantity)
            .map(|_| ()),
        ActionMutation::Liquidate {
            instrument_id,
            amount,
        } => account
            .liquidate_position(portfolio_id, instrument_id, *amount)
            .map(|_| ()),
        ActionMutation::SettleReceivable { kind, amount } => {
            account.settle_receivable(portfolio_id, *kind, *amount)
        }
    }
}

pub(crate) fn apply_transition_to_vector(
    transition: &ActionTransition,
    book: &mut crate::parity_sizing::ParitySizingState,
) -> Result<(), String> {
    let Some(mutation) = &transition.mutation else {
        return Ok(());
    };
    match mutation {
        ActionMutation::AddReceivable { kind, amount } => match kind {
            ReceivableKind::Dividend => {
                book.dividend_receivable = add_notional(book.dividend_receivable, *amount)?;
            }
            ReceivableKind::Liquidation => {
                book.liquidation_receivable = add_notional(book.liquidation_receivable, *amount)?;
            }
        },
        ActionMutation::SetQuantity {
            instrument_id,
            quantity,
        } => set_vector_quantity(book, instrument_id, *quantity),
        ActionMutation::Liquidate {
            instrument_id,
            amount,
        } => {
            book.positions.remove(instrument_id);
            book.liquidation_receivable = add_notional(book.liquidation_receivable, *amount)?;
        }
        ActionMutation::SettleReceivable { kind, amount } => {
            let receivable = match kind {
                ReceivableKind::Dividend => &mut book.dividend_receivable,
                ReceivableKind::Liquidation => &mut book.liquidation_receivable,
            };
            *receivable = subtract_notional(*receivable, *amount).map_err(|e| e.to_string())?;
            book.cash = add_notional(book.cash, *amount)?;
        }
    }
    Ok(())
}

fn set_vector_quantity(
    book: &mut crate::parity_sizing::ParitySizingState,
    instrument: &InstrumentId,
    quantity: Quantity,
) {
    if quantity == Quantity::ZERO {
        book.positions.remove(instrument);
    } else {
        book.positions.insert(instrument.clone(), quantity);
    }
}

fn add_notional(left: Notional, right: Notional) -> Result<Notional, String> {
    let mantissa = left
        .mantissa()
        .checked_add(right.mantissa())
        .ok_or_else(|| "Notional addition overflow".to_owned())?;
    Notional::from_mantissa(mantissa).map_err(|e| e.to_string())
}

fn subtract_notional(left: Notional, right: Notional) -> Result<Notional, CorporateActionError> {
    let mantissa = left
        .mantissa()
        .checked_sub(right.mantissa())
        .ok_or_else(|| invalid("Notional subtraction overflow"))?;
    Notional::from_mantissa(mantissa).map_err(domain_error)
}

fn subtract_quantity(left: Quantity, right: Quantity) -> Result<Quantity, CorporateActionError> {
    let mantissa = left
        .mantissa()
        .checked_sub(right.mantissa())
        .ok_or_else(|| invalid("share quantity subtraction overflow"))?;
    Quantity::from_mantissa(mantissa).map_err(domain_error)
}

#[allow(clippy::too_many_arguments)]
fn transition(
    action: &CorporateActionRow,
    date: NaiveDate,
    portfolio_id: Option<VirtualPortfolioId>,
    kind: CorporateActionLedgerKind,
    quantity_before: Option<Quantity>,
    quantity_after: Option<Quantity>,
    gross_amount: Notional,
    tax_amount: Notional,
    net_amount: Notional,
    corporate_action_rounding: Notional,
    mutation: Option<ActionMutation>,
) -> ActionTransition {
    ActionTransition {
        entry: CorporateActionLedgerEntry {
            session_date: date,
            portfolio_id,
            announcement_id: action.announcement_id.clone(),
            action_index: action.action_index,
            instrument_id: action.instrument_id.clone(),
            kind,
            quantity_before,
            quantity_after,
            share_ratio: None,
            theoretical_quantity: None,
            residual_quantity: None,
            gross_amount,
            tax_amount,
            net_amount,
            corporate_action_rounding: Some(corporate_action_rounding),
            corporate_action_value_delta: None,
        },
        mutation,
    }
}

fn validate_action(action: &CorporateActionRow) -> Result<(), CorporateActionError> {
    if action.announcement_id.trim().is_empty()
        || action.source_raw_sha256.trim().is_empty()
        || action.normalization_version.trim().is_empty()
        || action
            .announced_at
            .is_some_and(|announced_at| action.available_at < announced_at)
    {
        return Err(invalid(
            "invalid corporate-action source identity or availability",
        ));
    }
    match action.kind {
        CorporateActionKind::Dividend => {
            let (Some(record), Some(ex), Some(pay), Some(cash)) = (
                action.record_date,
                action.ex_date,
                action.pay_date,
                action.cash_per_share.as_deref(),
            ) else {
                return Err(invalid(
                    "dividend requires record, ex, pay dates and cash per share",
                ));
            };
            if record >= ex || ex > pay || parse_notional(cash)? < Notional::ZERO {
                return Err(invalid("invalid dividend dates or cash amount"));
            }
        }
        CorporateActionKind::ShareConversion => {
            let (Some(_effective), Some(ratio)) =
                (action.effective_date, action.share_ratio.as_deref())
            else {
                return Err(invalid(
                    "share conversion requires effective date and ratio",
                ));
            };
            let ratio = parse_quantity(ratio)?;
            if ratio <= Quantity::ZERO {
                return Err(invalid("invalid share conversion date or ratio"));
            }
        }
        CorporateActionKind::Liquidation => {
            let Some(effective) = action.effective_date else {
                return Err(invalid("liquidation requires an effective date"));
            };
            if action.pay_date.is_some_and(|pay| pay < effective)
                || action
                    .liquidation_cash_per_share
                    .as_deref()
                    .map(parse_notional)
                    .transpose()?
                    .is_some_and(|amount| amount < Notional::ZERO)
            {
                return Err(invalid("invalid liquidation date or settlement amount"));
            }
        }
    }
    Ok(())
}

fn action_dates(action: &CorporateActionRow) -> Vec<NaiveDate> {
    [
        action.record_date,
        action.ex_date,
        action.pay_date,
        action.effective_date,
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn opening_date(action: &CorporateActionRow) -> Option<NaiveDate> {
    match action.kind {
        CorporateActionKind::Dividend => action.ex_date,
        CorporateActionKind::ShareConversion | CorporateActionKind::Liquidation => {
            action.effective_date
        }
    }
}

fn floor_to_whole_share(quantity: Quantity) -> Result<Quantity, CorporateActionError> {
    let scale = 10_i128.pow(DECIMAL_SCALE);
    let whole = quantity.mantissa() / scale;
    let mantissa = whole
        .checked_mul(scale)
        .ok_or_else(|| invalid("share quantity overflow"))?;
    Quantity::from_mantissa(mantissa).map_err(domain_error)
}

fn money_for(quantity: Quantity, per_share: Notional) -> Result<Notional, CorporateActionError> {
    Notional::from_mantissa(quantity.mantissa())
        .map_err(domain_error)?
        .checked_mul_decimal(per_share)
        .map_err(domain_error)
}

fn parse_notional(value: &str) -> Result<Notional, CorporateActionError> {
    value.parse().map_err(domain_error)
}

fn parse_quantity(value: &str) -> Result<Quantity, CorporateActionError> {
    value.parse().map_err(domain_error)
}

fn domain_error(error: impl fmt::Display) -> CorporateActionError {
    invalid(error.to_string())
}

fn invalid(message: impl Into<String>) -> CorporateActionError {
    CorporateActionError(message.into())
}
