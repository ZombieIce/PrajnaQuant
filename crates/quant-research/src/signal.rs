use crate::{
    core::Bar,
    factor::{FactorObservation, FactorReport, evaluate_observations},
    feature::{pct_change, rolling_drawdown, rolling_mean, rolling_std},
};
use anyhow::{Result, bail};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignalCategory {
    Momentum,
    Trend,
    Volatility,
    Drawdown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignalDirection {
    HigherIsBetter,
    LowerIsBetter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalDefinition {
    pub key: String,
    pub display_name: String,
    pub category: SignalCategory,
    pub description: String,
    pub lookback: usize,
    pub direction: SignalDirection,
    pub dependencies: Vec<String>,
}

/// The fixed research contract shared by the API, reports and future Python adapter.
/// A signal is observed at the close of date `t`; its forward return is close(t+n)/close(t)-1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalResearchRequest {
    pub signal: String,
    #[serde(default = "default_forward_periods")]
    pub forward_periods: Vec<usize>,
    #[serde(default = "default_quantiles")]
    pub quantiles: usize,
}

fn default_forward_periods() -> Vec<usize> {
    vec![1, 5, 10, 20, 60]
}

fn default_quantiles() -> usize {
    5
}

impl Default for SignalResearchRequest {
    fn default() -> Self {
        Self {
            signal: "momentum_60".into(),
            forward_periods: default_forward_periods(),
            quantiles: default_quantiles(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalResearchReport {
    pub definition: SignalDefinition,
    pub observation_count: usize,
    pub first_date: Option<NaiveDate>,
    pub last_date: Option<NaiveDate>,
    pub reports: Vec<FactorReport>,
}

pub fn registry() -> Vec<SignalDefinition> {
    let mut values = Vec::new();
    for window in [20, 60, 120] {
        values.push(def(
            "momentum",
            window,
            SignalCategory::Momentum,
            SignalDirection::HigherIsBetter,
            "收盘价区间收益",
        ));
        values.push(def(
            "ma_distance",
            window,
            SignalCategory::Trend,
            SignalDirection::HigherIsBetter,
            "收盘价相对均线距离",
        ));
    }
    for window in [20, 60] {
        values.push(def(
            "volatility",
            window,
            SignalCategory::Volatility,
            SignalDirection::LowerIsBetter,
            "日收益率滚动波动率",
        ));
        values.push(def(
            "drawdown",
            window,
            SignalCategory::Drawdown,
            SignalDirection::HigherIsBetter,
            "窗口内当前回撤",
        ));
    }
    values
}
fn def(
    prefix: &str,
    window: usize,
    category: SignalCategory,
    direction: SignalDirection,
    description: &str,
) -> SignalDefinition {
    SignalDefinition {
        key: format!("{prefix}_{window}"),
        display_name: format!(
            "{} {}日",
            match prefix {
                "ma_distance" => "均线距离",
                "volatility" => "波动率",
                "drawdown" => "回撤",
                _ => "动量",
            },
            window
        ),
        category,
        direction,
        description: description.into(),
        lookback: window,
        dependencies: vec!["close".into()],
    }
}

pub fn definition(key: &str) -> Option<SignalDefinition> {
    registry().into_iter().find(|item| item.key == key)
}

/// Produces an unambiguous, point-in-time signal panel. Values are normalized so
/// that larger always means a more favourable expected return; this makes Q5 and
/// long-short reports comparable across momentum, volatility and drawdown.
pub fn observations(
    bars: &[Bar],
    key: &str,
    forward_days: usize,
) -> Result<Vec<FactorObservation>> {
    let definition = definition(key).ok_or_else(|| anyhow::anyhow!("unknown signal: {key}"))?;
    if forward_days == 0 {
        bail!("forward_days must be positive");
    }
    let mut by_symbol: BTreeMap<&str, Vec<&Bar>> = BTreeMap::new();
    for bar in bars {
        by_symbol.entry(&bar.symbol).or_default().push(bar);
    }
    let mut result = Vec::new();
    for (symbol, mut series) in by_symbol {
        series.sort_by_key(|bar| bar.trade_date);
        let closes: Vec<f64> = series.iter().map(|bar| bar.close).collect();
        let values = signal_values(&definition, &closes);
        for (index, value) in values.into_iter().enumerate() {
            let Some(mut value) = value else { continue };
            let forward_return = series
                .get(index + forward_days)
                .map(|future| future.close / series[index].close - 1.0);
            if definition.direction == SignalDirection::LowerIsBetter {
                value = -value;
            }
            if value.is_finite() && forward_return.is_none_or(f64::is_finite) {
                result.push(FactorObservation {
                    date: series[index].trade_date,
                    symbol: symbol.to_owned(),
                    value,
                    forward_return,
                });
            }
        }
    }
    result.sort_by(|left, right| (left.date, &left.symbol).cmp(&(right.date, &right.symbol)));
    Ok(result)
}

pub fn analyze(bars: &[Bar], request: &SignalResearchRequest) -> Result<SignalResearchReport> {
    let definition = definition(&request.signal)
        .ok_or_else(|| anyhow::anyhow!("unknown signal: {}", request.signal))?;
    if !(2..=20).contains(&request.quantiles) {
        bail!("quantiles must be between 2 and 20");
    }
    if request.forward_periods.is_empty() || request.forward_periods.contains(&0) {
        bail!("forward_periods must contain positive values");
    }
    let mut reports = Vec::new();
    let mut primary_observations = Vec::new();
    for period in &request.forward_periods {
        let panel = observations(bars, &request.signal, *period)?;
        if *period == request.forward_periods[0] {
            primary_observations = panel.clone();
        }
        reports.push(evaluate_observations(
            &request.signal,
            definition.lookback,
            *period,
            request.quantiles,
            panel,
        ));
    }
    Ok(SignalResearchReport {
        definition,
        observation_count: primary_observations.len(),
        first_date: primary_observations.first().map(|row| row.date),
        last_date: primary_observations.last().map(|row| row.date),
        reports,
    })
}

fn signal_values(definition: &SignalDefinition, closes: &[f64]) -> Vec<Option<f64>> {
    let window = definition.lookback;
    match definition.key.split('_').next().unwrap_or_default() {
        "momentum" => pct_change(closes, window),
        "ma" => rolling_mean(closes, window)
            .into_iter()
            .enumerate()
            .map(|(index, average)| average.map(|average| closes[index] / average - 1.0))
            .collect(),
        "volatility" => {
            let daily_returns = pct_change(closes, 1)
                .into_iter()
                .map(|value| value.unwrap_or(0.0))
                .collect::<Vec<_>>();
            rolling_std(&daily_returns, window)
        }
        "drawdown" => rolling_drawdown(closes, window),
        _ => vec![None; closes.len()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_has_required_etf_signals() {
        assert!(definition("momentum_60").is_some());
        assert!(definition("volatility_20").is_some());
        assert_eq!(
            definition("volatility_20").unwrap().direction,
            SignalDirection::LowerIsBetter
        );
    }

    #[test]
    fn lower_is_better_volatility_is_normalized_for_ranking() {
        let signal = definition("volatility_20").unwrap();
        let values = signal_values(&signal, &(1..=25).map(|day| day as f64).collect::<Vec<_>>());
        assert!(values[24].is_some());
        assert_eq!(signal.direction, SignalDirection::LowerIsBetter);
    }
}
