use crate::core::Bar;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorObservation {
    pub date: NaiveDate,
    pub symbol: String,
    pub value: f64,
    pub forward_return: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyFactorResult {
    pub date: NaiveDate,
    pub rank_ic: f64,
    pub count: usize,
    pub quantile_returns: Vec<f64>,
    pub long_short_return: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorReport {
    pub factor: String,
    pub lookback_days: usize,
    pub forward_days: usize,
    pub quantiles: usize,
    pub observation_count: usize,
    pub evaluated_dates: usize,
    pub mean_rank_ic: f64,
    pub rank_ic_stddev: f64,
    pub rank_ic_ir: f64,
    pub rank_ic_positive_rate: f64,
    pub mean_long_short_return: f64,
    pub daily: Vec<DailyFactorResult>,
}

pub fn momentum_observations(
    bars: &[Bar],
    lookback: usize,
    forward: usize,
) -> Vec<FactorObservation> {
    let mut by_symbol: BTreeMap<&str, Vec<&Bar>> = BTreeMap::new();
    for bar in bars {
        by_symbol.entry(&bar.symbol).or_default().push(bar);
    }
    let mut result = Vec::new();
    for (symbol, mut series) in by_symbol {
        series.sort_by_key(|b| b.trade_date);
        for i in lookback..series.len() {
            let base = series[i - lookback].close;
            let value = series[i].close / base - 1.0;
            let forward_return = series
                .get(i + forward)
                .map(|future| future.close / series[i].close - 1.0);
            if value.is_finite() && forward_return.is_none_or(f64::is_finite) {
                result.push(FactorObservation {
                    date: series[i].trade_date,
                    symbol: symbol.into(),
                    value,
                    forward_return,
                });
            }
        }
    }
    result.sort_by(|a, b| (a.date, &a.symbol).cmp(&(b.date, &b.symbol)));
    result
}

pub fn evaluate_momentum(
    bars: &[Bar],
    lookback: usize,
    forward: usize,
    quantiles: usize,
) -> FactorReport {
    let observations = momentum_observations(bars, lookback, forward);
    evaluate_observations(
        "momentum_close_to_close",
        lookback,
        forward,
        quantiles,
        observations,
    )
}

pub fn evaluate_observations(
    factor_name: &str,
    lookback: usize,
    forward: usize,
    quantiles: usize,
    observations: Vec<FactorObservation>,
) -> FactorReport {
    let mut by_date: BTreeMap<NaiveDate, Vec<&FactorObservation>> = BTreeMap::new();
    for value in &observations {
        if value.forward_return.is_some() {
            by_date.entry(value.date).or_default().push(value);
        }
    }
    let mut daily = Vec::new();
    for (date, mut values) in by_date {
        if values.len() < quantiles.max(3) {
            continue;
        }
        let x: Vec<f64> = values.iter().map(|v| v.value).collect();
        let y: Vec<f64> = values
            .iter()
            .map(|v| v.forward_return.unwrap_or_default())
            .collect();
        let rank_ic = pearson(&ranks(&x), &ranks(&y));
        if !rank_ic.is_finite() {
            continue;
        }
        values.sort_by(|a, b| a.value.total_cmp(&b.value));
        let mut sums = vec![0.0; quantiles];
        let mut counts = vec![0usize; quantiles];
        let value_count = values.len();
        for (i, value) in values.into_iter().enumerate() {
            let q = (i * quantiles / value_count).min(quantiles - 1);
            sums[q] += value.forward_return.unwrap_or_default();
            counts[q] += 1;
        }
        let qret: Vec<f64> = sums
            .into_iter()
            .zip(counts)
            .map(|(s, n)| if n == 0 { 0.0 } else { s / n as f64 })
            .collect();
        daily.push(DailyFactorResult {
            date,
            rank_ic,
            count: value_count,
            long_short_return: qret[quantiles - 1] - qret[0],
            quantile_returns: qret,
        });
    }
    let ics: Vec<f64> = daily.iter().map(|d| d.rank_ic).collect();
    let spreads: Vec<f64> = daily.iter().map(|d| d.long_short_return).collect();
    let mean_ic = mean(&ics);
    let sd = stddev(&ics, mean_ic);
    FactorReport {
        factor: factor_name.into(),
        lookback_days: lookback,
        forward_days: forward,
        quantiles,
        observation_count: observations.len(),
        evaluated_dates: daily.len(),
        mean_rank_ic: mean_ic,
        rank_ic_stddev: sd,
        rank_ic_ir: if sd > 0.0 { mean_ic / sd } else { 0.0 },
        rank_ic_positive_rate: if ics.is_empty() {
            0.0
        } else {
            ics.iter().filter(|v| **v > 0.0).count() as f64 / ics.len() as f64
        },
        mean_long_short_return: mean(&spreads),
        daily,
    }
}

pub fn observation_map(values: &[FactorObservation]) -> HashMap<(NaiveDate, String), f64> {
    values
        .iter()
        .map(|v| ((v.date, v.symbol.clone()), v.value))
        .collect()
}

fn ranks(values: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
    let mut result = vec![0.0; values.len()];
    let mut i = 0;
    while i < order.len() {
        let mut j = i + 1;
        while j < order.len() && values[order[i]].total_cmp(&values[order[j]]).is_eq() {
            j += 1;
        }
        let rank = (i + j - 1) as f64 / 2.0;
        for k in i..j {
            result[order[k]] = rank;
        }
        i = j;
    }
    result
}

fn pearson(x: &[f64], y: &[f64]) -> f64 {
    if x.len() != y.len() || x.len() < 2 {
        return f64::NAN;
    }
    let mx = mean(x);
    let my = mean(y);
    let mut covariance = 0.0;
    let mut vx = 0.0;
    let mut vy = 0.0;
    for (a, b) in x.iter().zip(y) {
        let dx = *a - mx;
        let dy = *b - my;
        covariance += dx * dy;
        vx += dx * dx;
        vy += dy * dy;
    }
    if vx == 0.0 || vy == 0.0 {
        f64::NAN
    } else {
        covariance / (vx * vy).sqrt()
    }
}
fn mean(x: &[f64]) -> f64 {
    if x.is_empty() {
        0.0
    } else {
        x.iter().sum::<f64>() / x.len() as f64
    }
}
fn stddev(x: &[f64], mean: f64) -> f64 {
    if x.len() < 2 {
        0.0
    } else {
        (x.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (x.len() - 1) as f64).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tied_ranks_are_averaged() {
        assert_eq!(ranks(&[3.0, 1.0, 1.0]), vec![2.0, 0.5, 0.5]);
    }
    #[test]
    fn correlation_is_directional() {
        assert!((pearson(&[1., 2., 3.], &[3., 2., 1.]) + 1.0).abs() < 1e-12);
    }
}
