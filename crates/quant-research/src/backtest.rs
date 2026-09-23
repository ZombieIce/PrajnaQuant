use crate::{
    core::{Bar, ExperimentConfig},
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
    pub trades: Vec<Trade>,
    pub final_positions: BTreeMap<String, i64>,
    #[serde(default)]
    pub benchmark_name: Option<String>,
    #[serde(default)]
    pub benchmark_curve: Vec<BenchmarkPoint>,
}

struct Account {
    cash: f64,
    holdings: BTreeMap<String, i64>,
    trades: Vec<Trade>,
}

pub fn run_momentum(
    bars: &[Bar],
    benchmark_bars: &[Bar],
    config: &ExperimentConfig,
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
    run_with_scores(bars, benchmark_bars, config, &factor)
}

pub fn run_with_scores(
    bars: &[Bar],
    benchmark_bars: &[Bar],
    config: &ExperimentConfig,
    factor: &HashMap<(NaiveDate, String), f64>,
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
    let mut pending: Option<Vec<String>> = None;
    let mut curve = Vec::new();
    let mut peak = config.initial_cash;
    let mut eligible_index = 0usize;
    for (date, day_bars) in &days {
        let open_map: HashMap<&str, f64> = day_bars
            .iter()
            .map(|b| (b.symbol.as_str(), b.open))
            .collect();
        if let Some(targets) = pending.take() {
            rebalance(
                *date,
                &targets,
                &open_map,
                &last_prices,
                &mut account,
                config,
            );
        }
        for bar in day_bars {
            last_prices.insert(bar.symbol.clone(), bar.close);
        }
        let equity = portfolio_value(account.cash, &account.holdings, &last_prices);
        peak = peak.max(equity);
        curve.push(EquityPoint {
            date: *date,
            equity,
            cash: account.cash,
            positions: account.holdings.values().filter(|q| **q > 0).count(),
            drawdown: equity / peak - 1.0,
        });
        let targets = select_top_n(*date, day_bars, factor, config.strategy.top_n);
        if !targets.is_empty() {
            if eligible_index % config.strategy.rebalance_every == 0 {
                pending = Some(targets);
            }
            eligible_index += 1;
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
    BacktestReport {
        metrics,
        equity_curve: curve,
        trades: account.trades,
        final_positions: account.holdings,
        benchmark_name: Some("沪深300".into()),
        benchmark_curve,
    }
}

fn rebalance(
    date: NaiveDate,
    targets: &[String],
    prices: &HashMap<&str, f64>,
    last_prices: &HashMap<String, f64>,
    account: &mut Account,
    cfg: &ExperimentConfig,
) {
    let target_set: BTreeSet<&str> = targets.iter().map(String::as_str).collect();
    let equity = account.cash
        + account
            .holdings
            .iter()
            .map(|(s, q)| {
                let price = prices
                    .get(s.as_str())
                    .copied()
                    .or_else(|| last_prices.get(s).copied())
                    .unwrap_or(0.0);
                *q as f64 * price
            })
            .sum::<f64>();
    let target_value = if targets.is_empty() {
        0.0
    } else {
        equity / targets.len() as f64
    };
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
            continue;
        };
        let current = account.holdings.get(symbol).copied().unwrap_or(0);
        let desired = round_lot(target_value / price, cfg.lot_size);
        if desired > current {
            execute_buy(date, symbol, desired - current, price, account, cfg);
        }
    }
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
    use crate::core::CostConfig;
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
