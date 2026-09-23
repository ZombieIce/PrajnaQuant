use crate::core::{Bar, StrategyConfig};
use crate::factor::FactorObservation;
use chrono::NaiveDate;
use std::collections::{BTreeMap, HashMap};

pub fn rotation_scores(bars: &[Bar], config: &StrategyConfig) -> HashMap<(NaiveDate, String), f64> {
    if !config.uses_rotation_score() {
        return HashMap::new();
    }
    let short = config.momentum_short_days.unwrap_or(config.lookback_days);
    let long = config.momentum_long_days.unwrap_or(config.lookback_days);
    let volatility = config.volatility_window.unwrap_or(short.max(2));
    let trend = config.trend_window.unwrap_or(long);
    let required = short.max(long).max(volatility).max(trend);
    let mut by_symbol: BTreeMap<&str, Vec<&Bar>> = BTreeMap::new();
    for bar in bars {
        by_symbol.entry(&bar.symbol).or_default().push(bar);
    }
    let mut scores = HashMap::new();
    for (symbol, mut series) in by_symbol {
        series.sort_by_key(|bar| bar.trade_date);
        for i in required..series.len() {
            let close = series[i].close;
            let short_return = close / series[i - short].close - 1.0;
            let long_return = close / series[i - long].close - 1.0;
            let returns: Vec<f64> = ((i - volatility + 1)..=i)
                .map(|j| series[j].close / series[j - 1].close - 1.0)
                .collect();
            let mean = returns.iter().sum::<f64>() / returns.len() as f64;
            let variance = returns
                .iter()
                .map(|value| (value - mean).powi(2))
                .sum::<f64>()
                / (returns.len() - 1) as f64;
            let trend_ok = !config.use_trend_filter
                || close
                    >= series[(i + 1 - trend)..=i]
                        .iter()
                        .map(|bar| bar.close)
                        .sum::<f64>()
                        / trend as f64;
            let score = config.short_momentum_weight * short_return
                + config.long_momentum_weight * long_return
                - config.volatility_weight * variance.sqrt();
            if trend_ok && score.is_finite() {
                scores.insert((series[i].trade_date, symbol.to_owned()), score);
            }
        }
    }
    scores
}

pub fn feature_key(config: &StrategyConfig) -> String {
    if !config.uses_rotation_score() {
        return format!("momentum:{}", config.lookback_days);
    }
    format!(
        "rotation:{}:{}:{}:{}:{}:{:.12}:{:.12}:{:.12}",
        config.momentum_short_days.unwrap_or(config.lookback_days),
        config.momentum_long_days.unwrap_or(config.lookback_days),
        config.volatility_window.unwrap_or(2),
        config
            .trend_window
            .unwrap_or(config.momentum_long_days.unwrap_or(config.lookback_days)),
        config.use_trend_filter,
        config.short_momentum_weight,
        config.long_momentum_weight,
        config.volatility_weight,
    )
}

pub fn rotation_factor_observations(
    bars: &[Bar],
    config: &StrategyConfig,
    forward: usize,
) -> Vec<FactorObservation> {
    let scores = rotation_scores(bars, config);
    let mut by_symbol: BTreeMap<&str, Vec<&Bar>> = BTreeMap::new();
    for bar in bars {
        by_symbol.entry(&bar.symbol).or_default().push(bar);
    }
    let mut observations = Vec::new();
    for (symbol, mut series) in by_symbol {
        series.sort_by_key(|bar| bar.trade_date);
        for (index, bar) in series.iter().enumerate() {
            let Some(value) = scores.get(&(bar.trade_date, symbol.to_owned())).copied() else {
                continue;
            };
            let forward_return = series
                .get(index + forward)
                .map(|future| future.close / bar.close - 1.0);
            if forward_return.is_none_or(f64::is_finite) {
                observations.push(FactorObservation {
                    date: bar.trade_date,
                    symbol: symbol.to_owned(),
                    value,
                    forward_return,
                });
            }
        }
    }
    observations.sort_by(|left, right| (left.date, &left.symbol).cmp(&(right.date, &right.symbol)));
    observations
}

pub fn select_top_n(
    date: NaiveDate,
    bars: &[&Bar],
    scores: &HashMap<(NaiveDate, String), f64>,
    top_n: usize,
) -> Vec<String> {
    let mut ranked: Vec<(String, f64)> = bars
        .iter()
        .filter_map(|bar| {
            scores
                .get(&(date, bar.symbol.clone()))
                .map(|score| (bar.symbol.clone(), *score))
        })
        .collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked.into_iter().take(top_n).map(|item| item.0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn bar(day: u32, close: f64) -> Bar {
        Bar {
            symbol: "sh510050".into(),
            name: "ETF".into(),
            trade_date: NaiveDate::from_ymd_opt(2025, 1, day).unwrap(),
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
            amount: Some(1.0),
        }
    }

    #[test]
    fn rotation_score_requires_all_windows_before_emitting() {
        let bars = (1..=8).map(|day| bar(day, day as f64)).collect::<Vec<_>>();
        let config = StrategyConfig {
            momentum_short_days: Some(2),
            momentum_long_days: Some(4),
            volatility_window: Some(3),
            ..StrategyConfig::default()
        };
        let scores = rotation_scores(&bars, &config);
        assert_eq!(scores.len(), 4);
    }
}
