use crate::{
    core::{Bar, ExecutionStatusMap, ExperimentConfig},
    factor::{momentum_observations, observation_map},
    strategy::{rotation_scores, select_top_n},
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub date: NaiveDate,
    pub symbol: String,
    pub side: String,
    pub quantity: i64,
    pub reference_price: f64,
    pub fill_price: f64,
    pub gross_value: f64,
    pub commission: f64,
    pub tax: f64,
    pub slippage_cost: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub date: NaiveDate,
    pub equity: f64,
    pub cash: f64,
    pub positions: usize,
    pub drawdown: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionHolding {
    pub symbol: String,
    pub quantity: i64,
    pub mark_price: f64,
    pub market_value: f64,
    #[serde(default)]
    pub mark_date: Option<NaiveDate>,
    #[serde(default)]
    pub stale_calendar_days: Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionPoint {
    pub date: NaiveDate,
    pub cash: f64,
    pub invested_value: f64,
    /// Long market value divided by total equity, in the range 0..=1 for a
    /// non-negative-cash long-only portfolio.
    pub capital_utilization: f64,
    pub holdings: Vec<PositionHolding>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstrumentPerformance {
    pub symbol: String,
    pub buy_quantity: i64,
    pub sell_quantity: i64,
    pub trade_count: usize,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub total_pnl: f64,
    pub final_quantity: i64,
    pub mark_price: Option<f64>,
    pub market_value: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkPoint {
    pub date: NaiveDate,
    pub close: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metrics {
    pub initial_cash: f64,
    pub final_equity: f64,
    pub total_return: f64,
    pub annualized_return: f64,
    pub annualized_volatility: f64,
    pub sharpe_ratio: f64,
    #[serde(default)]
    pub calmar_ratio: f64,
    pub max_drawdown: f64,
    pub turnover: f64,
    pub trade_count: usize,
    pub commission: f64,
    pub tax: f64,
    pub slippage_cost: f64,
    pub total_cost: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestReport {
    pub metrics: Metrics,
    pub equity_curve: Vec<EquityPoint>,
    #[serde(default)]
    pub position_curve: Vec<PositionPoint>,
    pub trades: Vec<Trade>,
    pub final_positions: BTreeMap<String, i64>,
    #[serde(default)]
    pub instrument_performance: Vec<InstrumentPerformance>,
    #[serde(default)]
    pub rebalance_deferrals: Vec<RebalanceDeferral>,
    #[serde(default)]
    pub unexecuted_orders: Vec<UnexecutedOrder>,
    /// None means an older report did not record its execution input mode.
    #[serde(default)]
    pub execution_status_mode: Option<String>,
    #[serde(default)]
    pub benchmark_name: Option<String>,
    #[serde(default)]
    pub benchmark_curve: Vec<BenchmarkPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebalanceDeferral {
    pub decision_date: NaiveDate,
    pub attempt_date: NaiveDate,
    pub target_symbols: Vec<String>,
    pub blocked_symbols: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnexecutedOrder {
    pub decision_date: NaiveDate,
    pub attempt_date: NaiveDate,
    pub symbol: String,
    pub side: String,
    #[serde(default)]
    pub desired_quantity: Option<i64>,
    #[serde(default)]
    pub trade_status: Option<String>,
    #[serde(default)]
    pub status_sources: Option<String>,
    pub reason: String,
}

struct Account {
    cash: f64,
    holdings: BTreeMap<String, i64>,
    trades: Vec<Trade>,
}

struct PendingRebalance {
    decision_date: NaiveDate,
    targets: Vec<String>,
}

struct RebalanceInput<'a> {
    date: NaiveDate,
    decision_date: NaiveDate,
    targets: &'a [String],
    prices: &'a HashMap<&'a str, f64>,
    last_prices: &'a HashMap<String, f64>,
    config: &'a ExperimentConfig,
    statuses: Option<&'a ExecutionStatusMap>,
}

pub fn run_momentum(
    bars: &[Bar],
    benchmark_bars: &[Bar],
    config: &ExperimentConfig,
) -> BacktestReport {
    run_momentum_with_statuses(bars, benchmark_bars, config, None)
}

pub fn run_momentum_with_statuses(
    bars: &[Bar],
    benchmark_bars: &[Bar],
    config: &ExperimentConfig,
    statuses: Option<&ExecutionStatusMap>,
) -> BacktestReport {
    let factor = if config.strategy.uses_rotation_score() {
        rotation_scores(bars, &config.strategy)
    } else {
        observation_map(&momentum_observations(
            bars,
            config.strategy.lookback_days,
            0,
        ))
    };
    run_with_scores_and_statuses(bars, benchmark_bars, config, &factor, statuses)
}

pub fn run_with_scores(
    bars: &[Bar],
    benchmark_bars: &[Bar],
    config: &ExperimentConfig,
    factor: &HashMap<(NaiveDate, String), f64>,
) -> BacktestReport {
    run_with_scores_and_statuses(bars, benchmark_bars, config, factor, None)
}

pub fn run_with_scores_and_statuses(
    bars: &[Bar],
    benchmark_bars: &[Bar],
    config: &ExperimentConfig,
    factor: &HashMap<(NaiveDate, String), f64>,
    statuses: Option<&ExecutionStatusMap>,
) -> BacktestReport {
    run_with_scores_and_empty_membership_days_and_statuses(
        bars,
        benchmark_bars,
        config,
        factor,
        None,
        statuses,
    )
}

/// Run with explicit dates where the resolved universe membership is known to
/// be empty. A missing score for a non-empty membership set is not an empty
/// universe and is intentionally not inferred here.
pub fn run_with_scores_and_empty_membership_days(
    bars: &[Bar],
    benchmark_bars: &[Bar],
    config: &ExperimentConfig,
    factor: &HashMap<(NaiveDate, String), f64>,
    empty_membership_days: Option<&BTreeSet<NaiveDate>>,
) -> BacktestReport {
    run_with_scores_and_empty_membership_days_and_statuses(
        bars,
        benchmark_bars,
        config,
        factor,
        empty_membership_days,
        None,
    )
}

pub fn run_with_scores_and_empty_membership_days_and_statuses(
    bars: &[Bar],
    benchmark_bars: &[Bar],
    config: &ExperimentConfig,
    factor: &HashMap<(NaiveDate, String), f64>,
    empty_membership_days: Option<&BTreeSet<NaiveDate>>,
    statuses: Option<&ExecutionStatusMap>,
) -> BacktestReport {
    let mut days: BTreeMap<NaiveDate, Vec<&Bar>> = BTreeMap::new();
    for bar in bars {
        days.entry(bar.trade_date).or_default().push(bar);
    }
    let mut account = Account {
        cash: config.initial_cash,
        holdings: BTreeMap::new(),
        trades: Vec::new(),
    };
    let mut last_prices: HashMap<String, f64> = HashMap::new();
    let mut mark_dates: HashMap<String, NaiveDate> = HashMap::new();
    let mut pending: Option<PendingRebalance> = None;
    let mut rebalance_deferrals = Vec::new();
    let mut unexecuted_orders = Vec::new();
    let mut curve = Vec::new();
    let mut position_curve = Vec::new();
    let mut peak = config.initial_cash;
    let mut eligible_index = 0usize;
    for (date, day_bars) in &days {
        let open_map: HashMap<&str, f64> = day_bars
            .iter()
            .map(|b| (b.symbol.as_str(), b.open))
            .collect();
        if let Some(rebalance_request) = pending.take() {
            match rebalance(
                RebalanceInput {
                    date: *date,
                    decision_date: rebalance_request.decision_date,
                    targets: &rebalance_request.targets,
                    prices: &open_map,
                    last_prices: &last_prices,
                    config,
                    statuses,
                },
                &mut account,
                &mut unexecuted_orders,
            ) {
                Ok(()) => {}
                Err(blocked_symbols) => {
                    let reason = if blocked_symbols
                        .iter()
                        .all(|symbol| !open_map.contains_key(symbol.as_str()))
                    {
                        "held_position_missing_open"
                    } else {
                        "held_position_not_tradable"
                    };
                    rebalance_deferrals.push(RebalanceDeferral {
                        decision_date: rebalance_request.decision_date,
                        attempt_date: *date,
                        target_symbols: rebalance_request.targets.clone(),
                        blocked_symbols,
                        reason: reason.into(),
                    });
                    pending = Some(rebalance_request);
                }
            }
        }
        for bar in day_bars {
            last_prices.insert(bar.symbol.clone(), bar.close);
            mark_dates.insert(bar.symbol.clone(), *date);
        }
        let equity = portfolio_value(account.cash, &account.holdings, &last_prices);
        peak = peak.max(equity);
        position_curve.push(position_point(
            *date,
            account.cash,
            equity,
            &account.holdings,
            &last_prices,
            &mark_dates,
        ));
        curve.push(EquityPoint {
            date: *date,
            equity,
            cash: account.cash,
            positions: account.holdings.values().filter(|q| **q > 0).count(),
            drawdown: equity / peak - 1.0,
        });
        let targets = select_top_n(*date, day_bars, factor, config.strategy.top_n);
        let explicitly_empty = empty_membership_days.is_some_and(|dates| dates.contains(date));
        if !targets.is_empty() || explicitly_empty {
            if eligible_index % config.strategy.rebalance_every == 0 {
                pending = Some(PendingRebalance {
                    decision_date: *date,
                    targets,
                });
            }
            eligible_index += 1;
        }
    }
    if let (Some(last_date), Some(rebalance_request)) = (days.keys().next_back(), pending.as_ref())
    {
        for symbol in &rebalance_request.targets {
            unexecuted_orders.push(UnexecutedOrder {
                decision_date: rebalance_request.decision_date,
                // This is the report cutoff, not an attempted fill date.
                attempt_date: *last_date,
                symbol: symbol.clone(),
                side: "TARGET".into(),
                desired_quantity: None,
                trade_status: status_facts(*last_date, symbol, statuses).0,
                status_sources: status_facts(*last_date, symbol, statuses).1,
                reason: "no_future_execution_session".into(),
            });
        }
    }
    let metrics = metrics(config.initial_cash, &curve, &account.trades);
    let benchmark_curve = benchmark_bars
        .iter()
        .map(|bar| BenchmarkPoint {
            date: bar.trade_date,
            close: bar.close,
        })
        .collect();
    let instrument_performance =
        instrument_performance(&account.trades, &account.holdings, &last_prices);
    BacktestReport {
        metrics,
        equity_curve: curve,
        position_curve,
        trades: account.trades,
        final_positions: account.holdings,
        instrument_performance,
        rebalance_deferrals,
        unexecuted_orders,
        execution_status_mode: Some(if statuses.is_some() {
            "status_gated".into()
        } else {
            "legacy_bar_only".into()
        }),
        benchmark_name: (!benchmark_bars.is_empty()).then(|| "沪深300".into()),
        benchmark_curve,
    }
}

fn rebalance(
    input: RebalanceInput<'_>,
    account: &mut Account,
    unexecuted_orders: &mut Vec<UnexecutedOrder>,
) -> Result<(), Vec<String>> {
    let RebalanceInput {
        date,
        decision_date,
        targets,
        prices,
        last_prices,
        config: cfg,
        statuses,
    } = input;
    let target_set: BTreeSet<&str> = targets.iter().map(String::as_str).collect();
    let equity = account.cash
        + account
            .holdings
            .iter()
            .map(|(symbol, quantity)| {
                let price = prices
                    .get(symbol.as_str())
                    .copied()
                    .or_else(|| last_prices.get(symbol).copied())
                    .unwrap_or(0.0);
                *quantity as f64 * price
            })
            .sum::<f64>();
    let target_value = if targets.is_empty() {
        0.0
    } else {
        equity / targets.len() as f64
    };
    let planned_sales: BTreeSet<String> = account
        .holdings
        .iter()
        .filter_map(|(symbol, quantity)| {
            if *quantity <= 0 {
                return None;
            }
            if !target_set.contains(symbol.as_str()) {
                return Some(symbol.clone());
            }
            // A held target without an execution open might need a reduction.
            // Block the whole rebalance rather than silently buying other legs.
            let Some(price) = prices.get(symbol.as_str()).copied() else {
                return Some(symbol.clone());
            };
            let desired = round_lot(target_value / price, cfg.lot_size);
            (*quantity > desired).then(|| symbol.clone())
        })
        .collect();
    let blocked_sales: Vec<(String, String)> = planned_sales
        .iter()
        .filter_map(|symbol| {
            execution_block_reason(date, symbol, prices, statuses)
                .map(|reason| (symbol.clone(), reason))
        })
        .collect();
    if !blocked_sales.is_empty() {
        for (symbol, reason) in &blocked_sales {
            unexecuted_orders.push(UnexecutedOrder {
                decision_date,
                attempt_date: date,
                symbol: symbol.clone(),
                side: "SELL".into(),
                desired_quantity: account.holdings.get(symbol).copied(),
                trade_status: status_facts(date, symbol, statuses).0,
                status_sources: status_facts(date, symbol, statuses).1,
                reason: reason.clone(),
            });
        }
        return Err(blocked_sales
            .into_iter()
            .map(|(symbol, _)| symbol)
            .collect());
    }
    let existing: Vec<String> = account.holdings.keys().cloned().collect();
    for symbol in existing {
        let Some(price) = prices.get(symbol.as_str()).copied() else {
            continue;
        };
        let current = account.holdings.get(&symbol).copied().unwrap_or(0);
        let desired = if target_set.contains(symbol.as_str()) {
            round_lot(target_value / price, cfg.lot_size)
        } else {
            0
        };
        if current > desired {
            execute_sell(date, &symbol, current - desired, price, account, cfg);
        }
    }
    for symbol in targets {
        let Some(price) = prices.get(symbol.as_str()).copied() else {
            unexecuted_orders.push(UnexecutedOrder {
                decision_date,
                attempt_date: date,
                symbol: symbol.clone(),
                side: "BUY".into(),
                desired_quantity: None,
                trade_status: status_facts(date, symbol, statuses).0,
                status_sources: status_facts(date, symbol, statuses).1,
                reason: "missing_open".into(),
            });
            continue;
        };
        let current = account.holdings.get(symbol).copied().unwrap_or(0);
        let desired = round_lot(target_value / price, cfg.lot_size);
        if desired <= current {
            continue;
        }
        if let Some(reason) = status_block_reason(date, symbol, statuses) {
            unexecuted_orders.push(UnexecutedOrder {
                decision_date,
                attempt_date: date,
                symbol: symbol.clone(),
                side: "BUY".into(),
                desired_quantity: Some(desired - current),
                trade_status: status_facts(date, symbol, statuses).0,
                status_sources: status_facts(date, symbol, statuses).1,
                reason,
            });
            continue;
        }
        execute_buy(date, symbol, desired - current, price, account, cfg);
    }
    Ok(())
}

fn execution_block_reason(
    date: NaiveDate,
    symbol: &str,
    prices: &HashMap<&str, f64>,
    statuses: Option<&ExecutionStatusMap>,
) -> Option<String> {
    if !prices.contains_key(symbol) {
        return Some("missing_open".into());
    }
    status_block_reason(date, symbol, statuses)
}

fn status_block_reason(
    date: NaiveDate,
    symbol: &str,
    statuses: Option<&ExecutionStatusMap>,
) -> Option<String> {
    let statuses = statuses?;
    let Some(status) = statuses.get(&(date, symbol.to_owned())) else {
        return Some("missing_status".into());
    };
    if status.is_tradable
        && status.trade_status.as_deref() == Some("TRADABLE")
        && status
            .sources
            .as_deref()
            .is_some_and(|source| !source.is_empty())
    {
        return None;
    }
    Some(match status.trade_status.as_deref() {
        Some("HALTED") => "halted".into(),
        Some("UNKNOWN") => "unknown_status".into(),
        Some("CONFLICT") => "conflicting_status_sources".into(),
        Some(_) => "not_tradable".into(),
        None => "missing_status".into(),
    })
}

fn status_facts(
    date: NaiveDate,
    symbol: &str,
    statuses: Option<&ExecutionStatusMap>,
) -> (Option<String>, Option<String>) {
    statuses
        .and_then(|map| map.get(&(date, symbol.to_owned())))
        .map(|status| (status.trade_status.clone(), status.sources.clone()))
        .unwrap_or((None, None))
}
fn execute_buy(
    date: NaiveDate,
    symbol: &str,
    mut quantity: i64,
    reference: f64,
    account: &mut Account,
    cfg: &ExperimentConfig,
) {
    let fill = reference * (1.0 + cfg.costs.buy_slippage_bps / 10_000.0);
    while quantity > 0 {
        let gross = fill * quantity as f64;
        let commission = (gross * cfg.costs.commission_rate).max(cfg.costs.minimum_commission);
        let tax = gross * cfg.costs.buy_tax_rate;
        if gross + commission + tax <= account.cash + 1e-8 {
            let slip = (fill - reference) * quantity as f64;
            account.cash -= gross + commission + tax;
            *account.holdings.entry(symbol.into()).or_default() += quantity;
            account.trades.push(Trade {
                date,
                symbol: symbol.into(),
                side: "BUY".into(),
                quantity,
                reference_price: reference,
                fill_price: fill,
                gross_value: gross,
                commission,
                tax,
                slippage_cost: slip,
            });
            return;
        }
        quantity -= cfg.lot_size;
    }
}
fn execute_sell(
    date: NaiveDate,
    symbol: &str,
    quantity: i64,
    reference: f64,
    account: &mut Account,
    cfg: &ExperimentConfig,
) {
    if quantity <= 0 {
        return;
    }
    let fill = reference * (1.0 - cfg.costs.sell_slippage_bps / 10_000.0);
    let gross = fill * quantity as f64;
    let commission = (gross * cfg.costs.commission_rate).max(cfg.costs.minimum_commission);
    let tax = gross * cfg.costs.sell_tax_rate;
    let slip = (reference - fill) * quantity as f64;
    account.cash += gross - commission - tax;
    *account.holdings.entry(symbol.into()).or_default() -= quantity;
    account.trades.push(Trade {
        date,
        symbol: symbol.into(),
        side: "SELL".into(),
        quantity,
        reference_price: reference,
        fill_price: fill,
        gross_value: gross,
        commission,
        tax,
        slippage_cost: slip,
    });
}
fn round_lot(quantity: f64, lot: i64) -> i64 {
    ((quantity / lot as f64).floor().max(0.0) as i64) * lot
}
fn portfolio_value(
    cash: f64,
    holdings: &BTreeMap<String, i64>,
    prices: &HashMap<String, f64>,
) -> f64 {
    cash + holdings
        .iter()
        .map(|(s, q)| *q as f64 * prices.get(s).copied().unwrap_or(0.0))
        .sum::<f64>()
}

fn position_point(
    date: NaiveDate,
    cash: f64,
    equity: f64,
    holdings: &BTreeMap<String, i64>,
    prices: &HashMap<String, f64>,
    mark_dates: &HashMap<String, NaiveDate>,
) -> PositionPoint {
    let holdings: Vec<PositionHolding> = holdings
        .iter()
        .filter(|(_, quantity)| **quantity > 0)
        .filter_map(|(symbol, quantity)| {
            prices.get(symbol).map(|mark_price| PositionHolding {
                symbol: symbol.clone(),
                quantity: *quantity,
                mark_price: *mark_price,
                market_value: *quantity as f64 * *mark_price,
                mark_date: mark_dates.get(symbol).copied(),
                stale_calendar_days: mark_dates
                    .get(symbol)
                    .map(|mark_date| (date - *mark_date).num_days()),
            })
        })
        .collect();
    let invested_value = holdings.iter().map(|holding| holding.market_value).sum();
    PositionPoint {
        date,
        cash,
        invested_value,
        capital_utilization: if equity > 0.0 {
            invested_value / equity
        } else {
            0.0
        },
        holdings,
    }
}

/// Attributes realized and terminal unrealized P&L to each symbol using a
/// moving weighted-average cost basis. Fill prices already include slippage;
/// commissions and taxes are included in buy basis or deducted from sell proceeds.
fn instrument_performance(
    trades: &[Trade],
    final_positions: &BTreeMap<String, i64>,
    last_prices: &HashMap<String, f64>,
) -> Vec<InstrumentPerformance> {
    #[derive(Default)]
    struct Ledger {
        quantity: i64,
        cost_basis: f64,
        buy_quantity: i64,
        sell_quantity: i64,
        trade_count: usize,
        realized_pnl: f64,
    }

    let mut ledgers = BTreeMap::<String, Ledger>::new();
    for trade in trades {
        let ledger = ledgers.entry(trade.symbol.clone()).or_default();
        ledger.trade_count += 1;
        match trade.side.as_str() {
            "BUY" => {
                ledger.quantity += trade.quantity;
                ledger.buy_quantity += trade.quantity;
                ledger.cost_basis += trade.gross_value + trade.commission + trade.tax;
            }
            "SELL" => {
                debug_assert!(trade.quantity >= 0 && trade.quantity <= ledger.quantity);
                let sold_quantity = trade.quantity;
                let average_cost = if ledger.quantity > 0 {
                    ledger.cost_basis / ledger.quantity as f64
                } else {
                    0.0
                };
                let allocated_cost = average_cost * sold_quantity as f64;
                ledger.realized_pnl +=
                    trade.gross_value - trade.commission - trade.tax - allocated_cost;
                ledger.quantity -= sold_quantity;
                ledger.sell_quantity += sold_quantity;
                ledger.cost_basis -= allocated_cost;
            }
            _ => {}
        }
    }

    ledgers
        .into_iter()
        .map(|(symbol, ledger)| {
            let final_quantity = final_positions.get(&symbol).copied().unwrap_or(0).max(0);
            let mark_price = last_prices.get(&symbol).copied();
            let market_value = mark_price.map_or(0.0, |price| final_quantity as f64 * price);
            let unrealized_pnl = market_value - ledger.cost_basis;
            let total_pnl = ledger.realized_pnl + unrealized_pnl;
            InstrumentPerformance {
                symbol,
                buy_quantity: ledger.buy_quantity,
                sell_quantity: ledger.sell_quantity,
                trade_count: ledger.trade_count,
                realized_pnl: ledger.realized_pnl,
                unrealized_pnl,
                total_pnl,
                final_quantity,
                mark_price,
                market_value,
            }
        })
        .collect()
}
fn metrics(initial: f64, curve: &[EquityPoint], trades: &[Trade]) -> Metrics {
    let final_equity = curve.last().map_or(initial, |p| p.equity);
    let total_return = final_equity / initial - 1.0;
    let daily: Vec<f64> = curve
        .windows(2)
        .filter_map(|w| {
            let r = w[1].equity / w[0].equity - 1.0;
            r.is_finite().then_some(r)
        })
        .collect();
    let years = (daily.len() as f64 / 252.0).max(1.0 / 252.0);
    let annualized_return = (final_equity / initial).max(0.0).powf(1.0 / years) - 1.0;
    let mean = if daily.is_empty() {
        0.0
    } else {
        daily.iter().sum::<f64>() / daily.len() as f64
    };
    let sd = if daily.len() < 2 {
        0.0
    } else {
        (daily.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (daily.len() - 1) as f64).sqrt()
    };
    let commission = trades.iter().map(|t| t.commission).sum::<f64>();
    let tax = trades.iter().map(|t| t.tax).sum::<f64>();
    let slippage_cost = trades.iter().map(|t| t.slippage_cost).sum::<f64>();
    let gross = trades.iter().map(|t| t.gross_value).sum::<f64>();
    let avg = if curve.is_empty() {
        initial
    } else {
        curve.iter().map(|p| p.equity).sum::<f64>() / curve.len() as f64
    };
    let max_drawdown = curve.iter().map(|p| p.drawdown).fold(0.0, f64::min);
    Metrics {
        initial_cash: initial,
        final_equity,
        total_return,
        annualized_return,
        annualized_volatility: sd * 252f64.sqrt(),
        sharpe_ratio: if sd > 0.0 {
            mean / sd * 252f64.sqrt()
        } else {
            0.0
        },
        calmar_ratio: if max_drawdown < 0.0 {
            annualized_return / max_drawdown.abs()
        } else {
            0.0
        },
        max_drawdown,
        turnover: if avg > 0.0 { gross / avg } else { 0.0 },
        trade_count: trades.len(),
        commission,
        tax,
        slippage_cost,
        total_cost: commission + tax + slippage_cost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CostConfig, ExecutionStatus, ExecutionStatusMap, StrategyConfig};

    fn date(day: u32) -> NaiveDate {
        let weekday_index = day - 1;
        NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()
            + chrono::Duration::days((weekday_index / 5 * 7 + weekday_index % 5) as i64)
    }

    fn rotation_bars(missing: Option<(&str, u32)>) -> Vec<Bar> {
        let closes = [
            (
                "A",
                [
                    100.0_f64, 101.0, 102.0, 103.0, 99.0, 98.0, 100.0, 101.0, 102.0, 103.0,
                ],
            ),
            (
                "B",
                [
                    100.0, 99.0, 98.0, 99.0, 102.0, 103.0, 101.0, 100.0, 99.0, 98.0,
                ],
            ),
            (
                "C",
                [
                    100.0, 100.0, 101.0, 100.0, 101.0, 102.0, 103.0, 104.0, 103.0, 105.0,
                ],
            ),
        ];
        closes
            .into_iter()
            .flat_map(|(symbol, series)| {
                series
                    .into_iter()
                    .enumerate()
                    .filter_map(move |(index, close)| {
                        let day = index as u32 + 1;
                        if missing == Some((symbol, day)) {
                            return None;
                        }
                        Some(Bar {
                            symbol: symbol.into(),
                            name: symbol.into(),
                            trade_date: date(day),
                            open: 100.0,
                            high: close.max(100.0),
                            low: close.min(100.0),
                            close,
                            volume: 1_000_000.0,
                            amount: Some(100_000_000.0),
                        })
                    })
            })
            .collect()
    }

    fn top_n_deferral_bars() -> Vec<Bar> {
        [("A", 60.0), ("B", 20.0), ("C", 40.0)]
            .into_iter()
            .flat_map(|(symbol, price)| {
                (1..=4).filter_map(move |day| {
                    if symbol == "A" && day == 3 {
                        return None;
                    }
                    Some(Bar {
                        symbol: symbol.into(),
                        name: symbol.into(),
                        trade_date: date(day),
                        open: price,
                        high: price,
                        low: price,
                        close: price,
                        volume: 1_000_000.0,
                        amount: Some(100_000_000.0),
                    })
                })
            })
            .collect()
    }

    fn golden_config() -> ExperimentConfig {
        ExperimentConfig {
            name: "3 ETF × 10 日轮动金标准".into(),
            initial_cash: 100_000.0,
            lot_size: 100,
            strategy: StrategyConfig {
                lookback_days: 1,
                top_n: 1,
                rebalance_every: 1,
                momentum_short_days: Some(1),
                momentum_long_days: Some(1),
                volatility_window: Some(2),
                short_momentum_weight: 1.0,
                long_momentum_weight: 1.0,
                volatility_weight: 0.0,
                ..StrategyConfig::default()
            },
            costs: CostConfig {
                commission_rate: 0.001,
                minimum_commission: 100.0,
                buy_tax_rate: 0.0,
                sell_tax_rate: 0.0,
                buy_slippage_bps: 10.0,
                sell_slippage_bps: 10.0,
            },
            ..ExperimentConfig::default()
        }
    }

    /// Independently hand-calculated ledger for `golden_config` (all opens are 100):
    ///
    /// Score is 2 × the latest close-to-close return. Full descending ranks on D3..D10:
    /// C>A>B; B>A>C; B>C>A; C>B>A; A>C>B; A>C>B; A>C>B; C>A>B.
    /// Thus Top-1 leaders are C, B, B, C, A, A, A, C; each decision executes next day.
    /// D4 BUY C 900: fill 100.1, gross 90,090, min commission 100, slip 90, cash 9,810.
    /// D5 SELL C 900 then BUY B 900: sell gross 89,910 / commission 100 / slip 90;
    /// buy gross 90,090 / commission 100 / slip 90; cash 9,430.
    /// D7 SELL B 900 then BUY C 900: same proceeds/costs; cash 9,050.
    /// D8 SELL C 900 then BUY A 900: same proceeds/costs; cash 8,670.
    /// No other trade occurs; D10's C signal remains pending at the end.
    ///
    /// End-of-day hand ledger (cash; held symbol × quantity; closing mark; equity):
    /// D1-D3 100,000; flat; --; 100,000 each.
    /// D4 9,810; C×900; 100; 99,810.
    /// D5 9,430; B×900; 102; 101,230.
    /// D6 9,430; B×900; 103; 102,130.
    /// D7 9,050; C×900; 103; 101,750.
    /// D8 8,670; A×900; 101; 99,570.
    /// D9 8,670; A×900; 102; 100,470.
    /// D10 8,670; A×900; 103; 101,370.
    /// Aggregate: 4 buys + 3 sells; commission 700, tax 0, slippage 630,
    /// total cost 1,330. This table is fixed here, not calculated from the report.
    #[test]
    fn rotation_backtest_matches_independent_three_etf_ten_day_ledger() {
        let bars = rotation_bars(None);
        let cfg = golden_config();
        let benchmark = vec![Bar {
            symbol: "sh000300".into(),
            name: "benchmark".into(),
            trade_date: date(11),
            open: 4_000.0,
            high: 4_010.0,
            low: 3_990.0,
            close: 4_005.0,
            volume: 1.0,
            amount: Some(1.0),
        }];

        // Exercises real rotation scoring and selection, followed by the backtest event loop.
        let report = run_momentum(&bars, &benchmark, &cfg);

        let expected = [
            100_000.0, 100_000.0, 100_000.0, 99_810.0, 101_230.0, 102_130.0, 101_750.0, 99_570.0,
            100_470.0, 101_370.0,
        ];
        assert_eq!(report.equity_curve.len(), 10);
        for (point, expected_equity) in report.equity_curve.iter().zip(expected) {
            assert!(
                (point.equity - expected_equity).abs() < 1e-8,
                "{}",
                point.date
            );
        }
        assert_eq!(report.equity_curve[0].cash, 100_000.0);
        assert_eq!(report.equity_curve[3].cash, 9_810.0);
        assert_eq!(report.equity_curve[9].cash, 8_670.0);
        assert_eq!(report.equity_curve[9].positions, 1);
        assert_eq!(report.position_curve.len(), report.equity_curve.len());
        for (equity, position) in report.equity_curve.iter().zip(&report.position_curve) {
            let marked_value = position
                .holdings
                .iter()
                .map(|holding| holding.quantity as f64 * holding.mark_price)
                .sum::<f64>();
            assert_eq!(position.date, equity.date);
            assert!((position.invested_value - marked_value).abs() < 1e-8);
            assert!((position.cash + position.invested_value - equity.equity).abs() < 1e-8);
            assert!((position.capital_utilization - marked_value / equity.equity).abs() < 1e-8);
        }
        assert!(report.position_curve[0].holdings.is_empty());
        assert_eq!(report.position_curve[3].holdings[0].symbol, "C");
        assert_eq!(report.position_curve[3].holdings[0].quantity, 900);
        assert!((report.position_curve[3].capital_utilization - 90_000.0 / 99_810.0).abs() < 1e-10);
        assert!(report.unexecuted_orders.iter().any(|order| {
            order.decision_date == date(10)
                && order.attempt_date == date(10)
                && order.reason == "no_future_execution_session"
        }));
        assert_eq!(
            report
                .final_positions
                .iter()
                .filter(|(_, quantity)| **quantity > 0)
                .map(|(symbol, quantity)| (symbol.clone(), *quantity))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([("A".into(), 900)])
        );

        let trades: Vec<_> = report
            .trades
            .iter()
            .map(|trade| {
                (
                    trade.date,
                    trade.symbol.as_str(),
                    trade.side.as_str(),
                    trade.quantity,
                )
            })
            .collect();
        assert_eq!(
            trades,
            vec![
                (date(4), "C", "BUY", 900),
                (date(5), "C", "SELL", 900),
                (date(5), "B", "BUY", 900),
                (date(7), "B", "SELL", 900),
                (date(7), "C", "BUY", 900),
                (date(8), "C", "SELL", 900),
                (date(8), "A", "BUY", 900),
            ]
        );
        assert!(report.trades.iter().all(|trade| {
            (trade.reference_price - 100.0).abs() < 1e-10
                && (trade.fill_price - if trade.side == "BUY" { 100.1 } else { 99.9 }).abs() < 1e-10
                && (trade.commission - 100.0).abs() < 1e-10
                && trade.tax == 0.0
                && (trade.slippage_cost - 90.0).abs() < 1e-8
        }));
        assert!((report.metrics.commission - 700.0).abs() < 1e-8);
        assert_eq!(report.metrics.tax, 0.0);
        assert!((report.metrics.slippage_cost - 630.0).abs() < 1e-8);
        assert!((report.metrics.total_cost - 1_330.0).abs() < 1e-8);
        assert_eq!(report.metrics.trade_count, 7);
        let pnl_by_symbol: BTreeMap<_, _> = report
            .instrument_performance
            .iter()
            .map(|item| (item.symbol.as_str(), item.total_pnl))
            .collect();
        assert!((pnl_by_symbol["A"] - 2_510.0).abs() < 1e-8);
        assert!((pnl_by_symbol["B"] + 380.0).abs() < 1e-8);
        assert!((pnl_by_symbol["C"] + 760.0).abs() < 1e-8);
        assert!(
            (report
                .instrument_performance
                .iter()
                .map(|item| item.total_pnl)
                .sum::<f64>()
                - (report.metrics.final_equity - cfg.initial_cash))
                .abs()
                < 1e-8
        );
        let final_a = report
            .instrument_performance
            .iter()
            .find(|item| item.symbol == "A")
            .unwrap();
        assert_eq!(final_a.final_quantity, 900);
        assert!((final_a.market_value - 92_700.0).abs() < 1e-8);
        assert!((final_a.unrealized_pnl - 2_510.0).abs() < 1e-8);
        // The supplied benchmark point is preserved as-is; no aligned benchmark return exists.
        assert_eq!(report.benchmark_curve.len(), 1);
        assert_eq!(report.benchmark_curve[0].date, date(11));

        // Rebuild cash and positions from the fills; value each holding at its latest close,
        // matching the engine's documented missing-close mark policy.
        let mut cash = cfg.initial_cash;
        let mut holdings = BTreeMap::<String, i64>::new();
        let mut latest_close = HashMap::<String, f64>::new();
        let mut trade_index = 0;
        for point in &report.equity_curve {
            for trade in report.trades[trade_index..]
                .iter()
                .take_while(|trade| trade.date == point.date)
            {
                let quantity = if trade.side == "BUY" {
                    trade.quantity
                } else {
                    -trade.quantity
                };
                cash += if trade.side == "BUY" {
                    -(trade.gross_value + trade.commission + trade.tax)
                } else {
                    trade.gross_value - trade.commission - trade.tax
                };
                *holdings.entry(trade.symbol.clone()).or_default() += quantity;
                trade_index += 1;
            }
            for bar in bars.iter().filter(|bar| bar.trade_date == point.date) {
                latest_close.insert(bar.symbol.clone(), bar.close);
            }
            holdings.retain(|_, quantity| *quantity != 0);
            assert!(cash >= 0.0, "negative cash on {}", point.date);
            assert!(holdings.values().all(|quantity| *quantity >= 0));
            let market_value: f64 = holdings
                .iter()
                .map(|(symbol, quantity)| *quantity as f64 * latest_close.get(symbol).unwrap())
                .sum();
            assert!((cash - point.cash).abs() < 1e-8, "cash on {}", point.date);
            assert!(
                (cash + market_value - point.equity).abs() < 1e-8,
                "NAV on {}",
                point.date
            );
        }
        assert_eq!(trade_index, report.trades.len());
        assert_eq!(
            holdings,
            report
                .final_positions
                .iter()
                .filter(|(_, quantity)| **quantity > 0)
                .map(|(symbol, quantity)| (symbol.clone(), *quantity))
                .collect()
        );
    }

    #[test]
    fn missing_bar_skips_sale_and_retains_last_close_mark() {
        let bars = rotation_bars(Some(("C", 5)));
        let report = run_momentum(&bars, &[], &golden_config());

        // D4 bought C. On D5 its absent open prevents the pending sale, while B is
        // unaffordable from the remaining cash; the engine silently keeps C and its D4 close.
        assert_eq!(report.trades[0].side, "BUY");
        assert_eq!(report.trades[0].symbol, "C");
        assert!(!report.trades.iter().any(|trade| trade.date == date(5)));
        assert_eq!(report.equity_curve[4].cash, 9_810.0);
        assert_eq!(report.equity_curve[4].positions, 1);
        assert!((report.equity_curve[4].equity - 99_810.0).abs() < 1e-8);
        let stale_holding = report.position_curve[4]
            .holdings
            .iter()
            .find(|holding| holding.symbol == "C")
            .unwrap();
        assert_eq!(stale_holding.mark_price, 100.0);
        assert_eq!(stale_holding.mark_date, Some(date(4)));
        assert_eq!(stale_holding.stale_calendar_days, Some(1));
        assert_eq!(report.final_positions.get("C"), Some(&0));
        assert!(
            report
                .trades
                .iter()
                .any(|trade| trade.date == date(8) && trade.symbol == "C" && trade.side == "SELL")
        );
        assert!(report.unexecuted_orders.iter().any(|order| {
            order.attempt_date == date(5)
                && order.symbol == "C"
                && order.side == "SELL"
                && order.reason == "missing_open"
        }));
    }

    #[test]
    fn missing_target_open_is_audited_and_a_later_signal_can_reenter() {
        let bars = ["A", "B", "C"]
            .into_iter()
            .flat_map(|symbol| {
                (1..=4).filter_map(move |day| {
                    if symbol == "B" && day == 2 {
                        return None;
                    }
                    Some(Bar {
                        symbol: symbol.into(),
                        name: symbol.into(),
                        trade_date: date(day),
                        open: 50.0,
                        high: 50.0,
                        low: 50.0,
                        close: 50.0,
                        volume: 100_000.0,
                        amount: Some(5_000_000.0),
                    })
                })
            })
            .collect::<Vec<_>>();
        let config = ExperimentConfig {
            initial_cash: 10_000.0,
            lot_size: 100,
            strategy: StrategyConfig {
                lookback_days: 1,
                top_n: 1,
                rebalance_every: 1,
                ..StrategyConfig::default()
            },
            costs: CostConfig {
                commission_rate: 0.0,
                minimum_commission: 0.0,
                buy_tax_rate: 0.0,
                sell_tax_rate: 0.0,
                buy_slippage_bps: 0.0,
                sell_slippage_bps: 0.0,
            },
            ..ExperimentConfig::default()
        };
        let scores = HashMap::from([
            ((date(1), "B".into()), 3.0),
            ((date(1), "A".into()), 2.0),
            ((date(3), "B".into()), 3.0),
            ((date(3), "A".into()), 2.0),
        ]);

        // D1's B target has no D2 open and is skipped with an explicit reason.
        // ADR 0005's retry behavior applies to blocked old-holding exits, not an
        // isolated missing buy leg; a fresh D3 signal schedules B for D4 instead.
        let report = run_with_scores(&bars, &[], &config, &scores);
        assert_eq!(report.unexecuted_orders[0].decision_date, date(1));
        assert_eq!(report.unexecuted_orders[0].attempt_date, date(2));
        assert_eq!(report.unexecuted_orders[0].symbol, "B");
        assert_eq!(report.unexecuted_orders[0].side, "BUY");
        assert_eq!(report.unexecuted_orders[0].reason, "missing_open");
        assert_eq!(report.trades.len(), 1);
        assert_eq!(report.trades[0].date, date(4));
        assert_eq!(report.trades[0].symbol, "B");
        assert_eq!(report.trades[0].quantity, 200);
        assert_eq!(report.equity_curve[1].cash, 10_000.0);
        assert_eq!(report.equity_curve[3].cash, 0.0);
        assert!(report.equity_curve.iter().all(|point| point.positions <= 1));
        for (point, positions) in report.equity_curve.iter().zip(&report.position_curve) {
            assert!((point.equity - positions.cash - positions.invested_value).abs() < 1e-8);
        }
    }

    #[test]
    fn non_tradable_target_reduction_defers_before_any_partial_fill() {
        let bars = ["A", "B"]
            .into_iter()
            .flat_map(|symbol| {
                (1..=4).map(move |day| {
                    let price = if symbol == "A" && day >= 3 {
                        100.0
                    } else {
                        50.0
                    };
                    Bar {
                        symbol: symbol.into(),
                        name: symbol.into(),
                        trade_date: date(day),
                        open: price,
                        high: price,
                        low: price,
                        close: price,
                        volume: 100_000.0,
                        amount: Some(5_000_000.0),
                    }
                })
            })
            .collect::<Vec<_>>();
        let config = ExperimentConfig {
            initial_cash: 20_000.0,
            lot_size: 100,
            strategy: StrategyConfig {
                lookback_days: 1,
                top_n: 2,
                rebalance_every: 1,
                ..StrategyConfig::default()
            },
            costs: CostConfig {
                commission_rate: 0.0,
                minimum_commission: 0.0,
                buy_tax_rate: 0.0,
                sell_tax_rate: 0.0,
                buy_slippage_bps: 0.0,
                sell_slippage_bps: 0.0,
            },
            ..ExperimentConfig::default()
        };
        let scores = HashMap::from([
            ((date(1), "A".into()), 2.0),
            ((date(1), "B".into()), 1.0),
            ((date(2), "A".into()), 2.0),
            ((date(2), "B".into()), 1.0),
        ]);
        let mut statuses: ExecutionStatusMap = bars
            .iter()
            .map(|bar| {
                (
                    (bar.trade_date, bar.symbol.clone()),
                    ExecutionStatus {
                        trade_status: Some("TRADABLE".into()),
                        is_tradable: true,
                        sources: Some("fixture".into()),
                    },
                )
            })
            .collect();
        statuses.insert(
            (date(3), "A".into()),
            ExecutionStatus {
                trade_status: Some("HALTED".into()),
                is_tradable: false,
                sources: Some("fixture".into()),
            },
        );

        // D2 buys A×200 and B×200 at 50. D3's A open doubles; the equal-value
        // target requires selling A×100 and buying B×100. HALTED blocks the
        // whole rebalance. D4 retries and completes both legs.
        let report = run_with_scores_and_statuses(&bars, &[], &config, &scores, Some(&statuses));
        assert_eq!(report.trades.len(), 4);
        assert_eq!(report.trades[0].date, date(2));
        assert_eq!(report.trades[0].side, "BUY");
        assert_eq!(report.trades[0].quantity, 200);
        assert_eq!(report.trades[2].date, date(4));
        assert_eq!(report.trades[2].side, "SELL");
        assert_eq!(report.trades[2].symbol, "A");
        assert_eq!(report.trades[2].quantity, 100);
        assert_eq!(report.trades[3].symbol, "B");
        assert_eq!(report.trades[3].side, "BUY");
        assert_eq!(
            report.execution_status_mode.as_deref(),
            Some("status_gated")
        );

        // If the still-targeted A lacks its D3 open, its required reduction
        // cannot be safely ruled out. No B leg may consume cash on D3.
        let without_a_d3 = bars
            .iter()
            .filter(|bar| !(bar.symbol == "A" && bar.trade_date == date(3)))
            .cloned()
            .collect::<Vec<_>>();
        let missing_open =
            run_with_scores_and_statuses(&without_a_d3, &[], &config, &scores, Some(&statuses));
        assert!(
            missing_open
                .trades
                .iter()
                .all(|trade| trade.date != date(3))
        );
        assert_eq!(
            missing_open.rebalance_deferrals[0].blocked_symbols,
            vec!["A"]
        );
        assert_eq!(
            missing_open.rebalance_deferrals[0].reason,
            "held_position_missing_open"
        );
        assert!(missing_open.unexecuted_orders.iter().any(|order| {
            order.symbol == "A" && order.side == "SELL" && order.reason == "missing_open"
        }));
        assert_eq!(report.trades[3].quantity, 100);
        assert_eq!(report.rebalance_deferrals.len(), 1);
        assert_eq!(report.rebalance_deferrals[0].attempt_date, date(3));
        assert_eq!(report.rebalance_deferrals[0].blocked_symbols, vec!["A"]);
        assert!(report.unexecuted_orders.iter().any(|order| {
            order.attempt_date == date(3)
                && order.symbol == "A"
                && order.side == "SELL"
                && order.reason == "halted"
        }));
        assert_eq!(report.equity_curve[1].positions, 2);
        assert_eq!(report.equity_curve[1].cash, 0.0);
        assert_eq!(report.equity_curve[2].positions, 2);
        assert_eq!(report.equity_curve[2].cash, 0.0);
        assert_eq!(report.equity_curve[2].equity, 30_000.0);
        assert_eq!(report.equity_curve[3].equity, 30_000.0);
    }

    #[test]
    fn inconsistent_or_unsourced_tradable_flag_fails_closed() {
        let mut statuses = ExecutionStatusMap::new();
        statuses.insert(
            (date(1), "A".into()),
            ExecutionStatus {
                trade_status: Some("UNKNOWN".into()),
                is_tradable: true,
                sources: Some("fixture".into()),
            },
        );
        assert_eq!(
            status_block_reason(date(1), "A", Some(&statuses)),
            Some("unknown_status".into())
        );
        statuses
            .get_mut(&(date(1), "A".into()))
            .unwrap()
            .trade_status = Some("TRADABLE".into());
        statuses.get_mut(&(date(1), "A".into())).unwrap().sources = None;
        assert_eq!(
            status_block_reason(date(1), "A", Some(&statuses)),
            Some("not_tradable".into())
        );
    }

    #[test]
    fn unchanged_target_holding_does_not_create_zero_quantity_rejection() {
        let config = ExperimentConfig {
            initial_cash: 10_000.0,
            lot_size: 100,
            ..ExperimentConfig::default()
        };
        let mut account = Account {
            cash: 0.0,
            holdings: BTreeMap::from([("A".into(), 100)]),
            trades: Vec::new(),
        };
        let prices = HashMap::from([("A", 100.0)]);
        let last_prices = HashMap::from([("A".into(), 100.0)]);
        let statuses = ExecutionStatusMap::from([(
            (date(2), "A".into()),
            ExecutionStatus {
                trade_status: Some("HALTED".into()),
                is_tradable: false,
                sources: Some("fixture".into()),
            },
        )]);
        let targets = vec!["A".into()];
        let mut rejected = Vec::new();
        let result = rebalance(
            RebalanceInput {
                date: date(2),
                decision_date: date(1),
                targets: &targets,
                prices: &prices,
                last_prices: &last_prices,
                config: &config,
                statuses: Some(&statuses),
            },
            &mut account,
            &mut rejected,
        );
        assert!(result.is_ok());
        assert!(rejected.is_empty());
        assert!(account.trades.is_empty());
    }

    #[test]
    fn rotation_backtest_gates_halted_and_unknown_orders_with_a_hand_ledger() {
        let bars = rotation_bars(None);
        let config = golden_config();
        let mut statuses: ExecutionStatusMap = bars
            .iter()
            .map(|bar| {
                (
                    (bar.trade_date, bar.symbol.clone()),
                    ExecutionStatus {
                        trade_status: Some("TRADABLE".into()),
                        is_tradable: true,
                        sources: Some("fixture".into()),
                    },
                )
            })
            .collect();
        statuses.insert(
            (date(4), "C".into()),
            ExecutionStatus {
                trade_status: Some("UNKNOWN".into()),
                is_tradable: false,
                sources: Some("fixture".into()),
            },
        );
        statuses.insert(
            (date(7), "B".into()),
            ExecutionStatus {
                trade_status: Some("HALTED".into()),
                is_tradable: false,
                sources: Some("fixture".into()),
            },
        );

        // Independent ledger: D4's C buy is blocked by UNKNOWN status. D5 buys
        // B×900, leaving 9,810 cash. D7's intended B sale is blocked by HALTED,
        // so neither leg of that rebalance executes. D7's close signal replaces
        // the old target with A; D8 sells B×900 then buys A×900. Final cash is
        // 9,430. Costs: 3 × minimum commission 100 and 3 × 90 slippage = 570.
        // Closing NAVs D1-D10: 100,000; 100,000; 100,000; 100,000; 101,610;
        // 102,510; 100,710; 100,330; 101,230; 102,130.
        let report = run_momentum_with_statuses(&bars, &[], &config, Some(&statuses));

        let expected_equity = [
            100_000.0, 100_000.0, 100_000.0, 100_000.0, 101_610.0, 102_510.0, 100_710.0, 100_330.0,
            101_230.0, 102_130.0,
        ];
        for (point, expected) in report.equity_curve.iter().zip(expected_equity) {
            assert!((point.equity - expected).abs() < 1e-8, "{}", point.date);
        }
        assert_eq!(report.trades.len(), 3);
        assert_eq!(report.trades[0].date, date(5));
        assert_eq!(report.trades[0].symbol, "B");
        assert_eq!(report.trades[0].side, "BUY");
        assert_eq!(report.trades[0].quantity, 900);
        assert_eq!(report.trades[1].date, date(8));
        assert_eq!(report.trades[1].symbol, "B");
        assert_eq!(report.trades[1].side, "SELL");
        assert_eq!(report.trades[2].symbol, "A");
        assert_eq!(report.trades[2].side, "BUY");
        assert_eq!(report.trades[2].quantity, 900);
        assert_eq!(report.equity_curve[6].cash, 9_810.0);
        assert_eq!(report.equity_curve[7].cash, 9_430.0);
        assert_eq!(report.final_positions.get("A"), Some(&900));
        assert!((report.metrics.commission - 300.0).abs() < 1e-8);
        assert!((report.metrics.slippage_cost - 270.0).abs() < 1e-8);
        assert!((report.metrics.total_cost - 570.0).abs() < 1e-8);
        assert_eq!(report.rebalance_deferrals.len(), 1);
        assert_eq!(report.rebalance_deferrals[0].blocked_symbols, vec!["B"]);
        assert_eq!(
            report.rebalance_deferrals[0].reason,
            "held_position_not_tradable"
        );
        assert_eq!(
            report
                .unexecuted_orders
                .iter()
                .filter(|order| order.reason != "no_future_execution_session")
                .count(),
            2
        );
        assert_eq!(report.unexecuted_orders[0].symbol, "C");
        assert_eq!(report.unexecuted_orders[0].side, "BUY");
        assert_eq!(report.unexecuted_orders[0].desired_quantity, Some(1_000));
        assert_eq!(report.unexecuted_orders[0].reason, "unknown_status");
        assert_eq!(
            report.unexecuted_orders[0].trade_status.as_deref(),
            Some("UNKNOWN")
        );
        assert_eq!(
            report.unexecuted_orders[0].status_sources.as_deref(),
            Some("fixture")
        );
        assert_eq!(report.unexecuted_orders[1].symbol, "B");
        assert_eq!(report.unexecuted_orders[1].side, "SELL");
        assert_eq!(report.unexecuted_orders[1].desired_quantity, Some(900));
        assert_eq!(report.unexecuted_orders[1].reason, "halted");
        assert_eq!(
            report.unexecuted_orders[1].trade_status.as_deref(),
            Some("HALTED")
        );
        for (equity, position) in report.equity_curve.iter().zip(&report.position_curve) {
            let marked_value = position
                .holdings
                .iter()
                .map(|holding| holding.quantity as f64 * holding.mark_price)
                .sum::<f64>();
            assert!((position.cash + marked_value - equity.equity).abs() < 1e-8);
            assert!(position.cash >= 0.0);
            assert!(
                position
                    .holdings
                    .iter()
                    .all(|holding| holding.quantity >= 0)
            );
        }

        // A missing status row is fail-closed too, and the leg-level reason is retained.
        statuses.remove(&(date(5), "B".into()));
        let missing_status = run_momentum_with_statuses(&bars, &[], &config, Some(&statuses));
        assert!(
            missing_status
                .trades
                .iter()
                .all(|trade| !(trade.date == date(5) && trade.symbol == "B"))
        );
        assert!(missing_status.unexecuted_orders.iter().any(|order| {
            order.symbol == "B" && order.side == "BUY" && order.reason == "missing_status"
        }));
    }

    #[test]
    fn unavailable_holding_sale_defers_rebalance_instead_of_exceeding_top_n() {
        let bars = top_n_deferral_bars();
        let config = ExperimentConfig {
            initial_cash: 10_000.0,
            lot_size: 100,
            strategy: StrategyConfig {
                lookback_days: 1,
                top_n: 1,
                rebalance_every: 1,
                ..StrategyConfig::default()
            },
            costs: CostConfig {
                commission_rate: 0.0,
                minimum_commission: 0.0,
                buy_tax_rate: 0.0,
                sell_tax_rate: 0.0,
                buy_slippage_bps: 0.0,
                sell_slippage_bps: 0.0,
            },
            ..ExperimentConfig::default()
        };
        let scores = HashMap::from([
            ((date(1), "A".into()), 3.0),
            ((date(1), "B".into()), 2.0),
            ((date(1), "C".into()), 1.0),
            ((date(2), "B".into()), 3.0),
            ((date(2), "A".into()), 2.0),
            ((date(2), "C".into()), 1.0),
            ((date(3), "C".into()), 3.0),
            ((date(3), "B".into()), 2.0),
            ((date(3), "A".into()), 1.0),
            ((date(4), "C".into()), 3.0),
            ((date(4), "B".into()), 2.0),
            ((date(4), "A".into()), 1.0),
        ]);

        // Independent ledger, with zero fees to isolate missing-order handling:
        // D1 flat; D2 buy A×100 for 6,000, leaving 4,000 cash.
        // D3 A has no open. The whole B rebalance must defer: cash 4,000,
        // A×100 marked at 60, NAV 10,000, and still one position. D3's new
        // close signal selects C, replacing the deferred B target.
        // D4 sell A×100 for 6,000 then buy C×200 for 8,000 (100-share lots);
        // cash 2,000, NAV 10,000, one position. No partial B buy is allowed on D3.
        let report = run_with_scores(&bars, &[], &config, &scores);

        assert_eq!(report.equity_curve.len(), 4);
        assert_eq!(
            report
                .equity_curve
                .iter()
                .map(|point| point.cash)
                .collect::<Vec<_>>(),
            vec![10_000.0, 4_000.0, 4_000.0, 2_000.0]
        );
        assert_eq!(
            report
                .equity_curve
                .iter()
                .map(|point| point.positions)
                .collect::<Vec<_>>(),
            vec![0, 1, 1, 1]
        );
        assert!(
            report
                .equity_curve
                .iter()
                .all(|point| (point.equity - 10_000.0).abs() < 1e-8)
        );
        let mut ledger_cash = config.initial_cash;
        let mut ledger_holdings = BTreeMap::<String, i64>::new();
        let mut last_close = HashMap::<String, f64>::new();
        for point in &report.equity_curve {
            for trade in report
                .trades
                .iter()
                .filter(|trade| trade.date == point.date)
            {
                let quantity = if trade.side == "BUY" {
                    ledger_cash -= trade.gross_value + trade.commission + trade.tax;
                    trade.quantity
                } else {
                    ledger_cash += trade.gross_value - trade.commission - trade.tax;
                    -trade.quantity
                };
                *ledger_holdings.entry(trade.symbol.clone()).or_default() += quantity;
            }
            for bar in bars.iter().filter(|bar| bar.trade_date == point.date) {
                last_close.insert(bar.symbol.clone(), bar.close);
            }
            ledger_holdings.retain(|_, quantity| *quantity != 0);
            let market_value = ledger_holdings
                .iter()
                .map(|(symbol, quantity)| {
                    *quantity as f64 * last_close.get(symbol).copied().unwrap()
                })
                .sum::<f64>();
            assert!(ledger_cash >= 0.0);
            assert!(ledger_holdings.values().all(|quantity| *quantity >= 0));
            assert!((ledger_cash - point.cash).abs() < 1e-8);
            assert!((ledger_cash + market_value - point.equity).abs() < 1e-8);
            assert!(ledger_holdings.len() <= config.strategy.top_n);
        }
        assert_eq!(report.metrics.total_cost, 0.0);
        assert_eq!(
            report
                .trades
                .iter()
                .map(|trade| (
                    trade.date,
                    trade.symbol.as_str(),
                    trade.side.as_str(),
                    trade.quantity
                ))
                .collect::<Vec<_>>(),
            vec![
                (date(2), "A", "BUY", 100),
                (date(4), "A", "SELL", 100),
                (date(4), "C", "BUY", 200),
            ]
        );
        assert_eq!(
            report
                .final_positions
                .iter()
                .filter(|(_, quantity)| **quantity > 0)
                .map(|(symbol, quantity)| (symbol.clone(), *quantity))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([("C".into(), 200)])
        );
        assert_eq!(report.rebalance_deferrals.len(), 1);
        assert_eq!(report.rebalance_deferrals[0].decision_date, date(2));
        assert_eq!(report.rebalance_deferrals[0].attempt_date, date(3));
        assert_eq!(report.rebalance_deferrals[0].target_symbols, vec!["B"]);
        assert_eq!(report.rebalance_deferrals[0].blocked_symbols, vec!["A"]);
        assert_eq!(
            report.rebalance_deferrals[0].reason,
            "held_position_missing_open"
        );
        let mut legacy_json = serde_json::to_value(&report).unwrap();
        legacy_json
            .as_object_mut()
            .unwrap()
            .remove("rebalance_deferrals");
        legacy_json
            .as_object_mut()
            .unwrap()
            .remove("position_curve");
        legacy_json
            .as_object_mut()
            .unwrap()
            .remove("instrument_performance");
        legacy_json
            .as_object_mut()
            .unwrap()
            .remove("unexecuted_orders");
        let legacy_report: BacktestReport = serde_json::from_value(legacy_json).unwrap();
        assert!(legacy_report.rebalance_deferrals.is_empty());
        assert!(legacy_report.unexecuted_orders.is_empty());
        assert!(legacy_report.position_curve.is_empty());
        assert!(legacy_report.instrument_performance.is_empty());
    }

    #[test]
    fn resolved_empty_universe_schedules_liquidation_but_missing_scores_do_not() {
        let bars = rotation_bars(None);
        let scores = HashMap::from([((date(1), "C".into()), 1.0)]);
        let config = golden_config();

        let explicit_empty = run_with_scores_and_empty_membership_days(
            &bars,
            &[],
            &config,
            &scores,
            Some(&BTreeSet::from([date(2)])),
        );
        assert!(
            explicit_empty.trades.iter().any(|trade| {
                trade.date == date(2) && trade.symbol == "C" && trade.side == "BUY"
            })
        );
        assert!(
            explicit_empty.trades.iter().any(|trade| {
                trade.date == date(3) && trade.symbol == "C" && trade.side == "SELL"
            })
        );
        assert_eq!(explicit_empty.final_positions.get("C"), Some(&0));

        let missing_scores = run_with_scores(&bars, &[], &config, &scores);
        assert!(
            missing_scores.trades.iter().any(|trade| {
                trade.date == date(2) && trade.symbol == "C" && trade.side == "BUY"
            })
        );
        assert!(
            !missing_scores.trades.iter().any(|trade| {
                trade.date == date(3) && trade.symbol == "C" && trade.side == "SELL"
            })
        );
        assert_eq!(missing_scores.final_positions.get("C"), Some(&900));
    }

    #[test]
    fn lots_round_down() {
        assert_eq!(round_lot(199.9, 100), 100);
        assert_eq!(round_lot(99.9, 100), 0);
    }

    #[test]
    fn configured_costs_are_charged_to_cash() {
        let cfg = ExperimentConfig {
            costs: CostConfig {
                commission_rate: 0.001,
                minimum_commission: 5.0,
                buy_tax_rate: 0.0,
                sell_tax_rate: 0.001,
                buy_slippage_bps: 10.0,
                sell_slippage_bps: 10.0,
            },
            ..ExperimentConfig::default()
        };
        let mut account = Account {
            cash: 20_000.0,
            holdings: BTreeMap::new(),
            trades: Vec::new(),
        };
        execute_buy(
            NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(),
            "sh510050",
            100,
            10.0,
            &mut account,
            &cfg,
        );
        execute_sell(
            NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(),
            "sh510050",
            100,
            10.0,
            &mut account,
            &cfg,
        );
        assert_eq!(account.trades.len(), 2);
        assert!((account.trades[0].fill_price - 10.01).abs() < 1e-10);
        assert!((account.trades[1].fill_price - 9.99).abs() < 1e-10);
        assert!((account.trades.iter().map(|t| t.commission).sum::<f64>() - 10.0).abs() < 1e-10);
        assert!((account.trades[1].tax - 0.999).abs() < 1e-10);
        assert!((account.cash - 19_987.001).abs() < 1e-9);
    }
}
