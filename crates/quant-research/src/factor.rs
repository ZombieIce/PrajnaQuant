use crate::core::{Bar, ForwardReturnMethod};
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
    pub rank_ic: Option<f64>,
    #[serde(default)]
    pub pearson_ic: Option<f64>,
    pub count: usize,
    #[serde(default)]
    pub expected_count: usize,
    #[serde(default)]
    pub factor_count: usize,
    #[serde(default)]
    pub missing_factor_count: usize,
    #[serde(default)]
    pub missing_label_count: usize,
    pub quantile_returns: Vec<f64>,
    pub long_short_return: Option<f64>,
    #[serde(default)]
    pub factor_autocorrelation_lag1: Option<f64>,
    #[serde(default)]
    pub top_quantile_turnover: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FactorEvaluationStatus {
    Evaluated,
    InsufficientCrossSection,
    #[default]
    NoValidIc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorReport {
    pub factor: String,
    pub lookback_days: usize,
    pub forward_days: usize,
    pub quantiles: usize,
    #[serde(default)]
    pub label_method: ForwardReturnMethod,
    #[serde(default)]
    pub evaluation_status: FactorEvaluationStatus,
    pub observation_count: usize,
    #[serde(default)]
    pub labeled_observation_count: usize,
    #[serde(default)]
    pub missing_label_count: usize,
    #[serde(default)]
    pub missing_factor_count: usize,
    #[serde(default)]
    pub expected_observation_count: usize,
    #[serde(default)]
    pub coverage: Option<f64>,
    #[serde(default)]
    pub minimum_cross_section: usize,
    pub evaluated_dates: usize,
    #[serde(default)]
    pub mean_pearson_ic: Option<f64>,
    pub mean_rank_ic: Option<f64>,
    pub rank_ic_stddev: Option<f64>,
    pub rank_ic_ir: Option<f64>,
    pub rank_ic_positive_rate: Option<f64>,
    pub mean_long_short_return: Option<f64>,
    #[serde(default)]
    pub mean_factor_autocorrelation_lag1: Option<f64>,
    #[serde(default)]
    pub mean_top_quantile_turnover: Option<f64>,
    pub daily: Vec<DailyFactorResult>,
}

pub fn momentum_observations(
    bars: &[Bar],
    lookback: usize,
    forward: usize,
) -> Vec<FactorObservation> {
    momentum_observations_with_method(bars, lookback, forward, ForwardReturnMethod::CloseToClose)
}

pub fn observed_market_calendar(bars: &[Bar]) -> Vec<NaiveDate> {
    bars.iter()
        .map(|bar| bar.trade_date)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn forward_return_for_series(
    series: &[&Bar],
    index: usize,
    forward: usize,
    method: ForwardReturnMethod,
    calendar: &[NaiveDate],
) -> Option<f64> {
    match method {
        ForwardReturnMethod::CloseToClose => series
            .get(index + forward)
            .map(|future| future.close / series[index].close - 1.0),
        ForwardReturnMethod::NextOpenToForwardOpen => {
            let calendar_index = calendar.binary_search(&series[index].trade_date).ok()?;
            let entry_date = *calendar.get(calendar_index + 1)?;
            let exit_date = *calendar.get(calendar_index + 1 + forward)?;
            let entry = series.iter().find(|bar| bar.trade_date == entry_date)?.open;
            let exit = series.iter().find(|bar| bar.trade_date == exit_date)?.open;
            Some(exit / entry - 1.0)
        }
    }
}

pub fn has_forward_calendar_window(
    date: NaiveDate,
    forward: usize,
    method: ForwardReturnMethod,
    calendar: &[NaiveDate],
) -> bool {
    let Ok(index) = calendar.binary_search(&date) else {
        return false;
    };
    let exit_offset = match method {
        ForwardReturnMethod::CloseToClose => forward,
        ForwardReturnMethod::NextOpenToForwardOpen => forward + 1,
    };
    index + exit_offset < calendar.len()
}

pub fn expected_with_forward_window(
    expected_by_date: &BTreeMap<NaiveDate, usize>,
    forward: usize,
    method: ForwardReturnMethod,
    calendar: &[NaiveDate],
) -> BTreeMap<NaiveDate, usize> {
    expected_by_date
        .iter()
        .filter(|(date, _)| has_forward_calendar_window(**date, forward, method, calendar))
        .map(|(date, count)| (*date, *count))
        .collect()
}

pub fn momentum_observations_with_method(
    bars: &[Bar],
    lookback: usize,
    forward: usize,
    method: ForwardReturnMethod,
) -> Vec<FactorObservation> {
    let calendar = observed_market_calendar(bars);
    momentum_observations_with_calendar(bars, lookback, forward, method, &calendar)
}

pub fn momentum_observations_with_calendar(
    bars: &[Bar],
    lookback: usize,
    forward: usize,
    method: ForwardReturnMethod,
    calendar: &[NaiveDate],
) -> Vec<FactorObservation> {
    let mut by_symbol: BTreeMap<&str, Vec<&Bar>> = BTreeMap::new();
    for bar in bars {
        by_symbol.entry(&bar.symbol).or_default().push(bar);
    }
    let mut result = Vec::new();
    for (symbol, mut series) in by_symbol {
        series.sort_by_key(|b| b.trade_date);
        for i in lookback..series.len() {
            if !has_forward_calendar_window(series[i].trade_date, forward, method, calendar) {
                continue;
            }
            let base = series[i - lookback].close;
            let value = series[i].close / base - 1.0;
            let forward_return = forward_return_for_series(&series, i, forward, method, calendar);
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
    let mut inferred_expected = BTreeMap::<NaiveDate, usize>::new();
    for observation in &observations {
        *inferred_expected.entry(observation.date).or_default() += 1;
    }
    evaluate_observations_with_context(
        factor_name,
        lookback,
        forward,
        quantiles,
        ForwardReturnMethod::CloseToClose,
        observations,
        &inferred_expected,
    )
}

pub fn evaluate_observations_with_context(
    factor_name: &str,
    lookback: usize,
    forward: usize,
    quantiles: usize,
    label_method: ForwardReturnMethod,
    observations: Vec<FactorObservation>,
    expected_by_date: &BTreeMap<NaiveDate, usize>,
) -> FactorReport {
    let minimum_cross_section = quantiles.max(3);
    let mut by_date: BTreeMap<NaiveDate, Vec<&FactorObservation>> = BTreeMap::new();
    for value in &observations {
        by_date.entry(value.date).or_default().push(value);
    }
    for date in expected_by_date.keys() {
        by_date.entry(*date).or_default();
    }
    let mut by_symbol_date = BTreeMap::<NaiveDate, BTreeMap<&str, f64>>::new();
    for observation in &observations {
        by_symbol_date
            .entry(observation.date)
            .or_default()
            .insert(&observation.symbol, observation.value);
    }
    let mut daily = Vec::new();
    let mut previous_factor_values: Option<BTreeMap<&str, f64>> = None;
    let mut previous_top: Option<std::collections::BTreeSet<&str>> = None;
    for (date, mut values) in by_date {
        values.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        let labeled: Vec<_> = values
            .iter()
            .copied()
            .filter(|v| v.forward_return.is_some_and(f64::is_finite))
            .collect();
        let rank_ic = if labeled.len() >= minimum_cross_section {
            let x: Vec<f64> = labeled.iter().map(|v| v.value).collect();
            let y: Vec<f64> = labeled.iter().map(|v| v.forward_return.unwrap()).collect();
            let value = pearson(&ranks(&x), &ranks(&y));
            value.is_finite().then_some(value)
        } else {
            None
        };
        let pearson_ic = if labeled.len() >= minimum_cross_section {
            let x: Vec<f64> = labeled.iter().map(|v| v.value).collect();
            let y: Vec<f64> = labeled.iter().map(|v| v.forward_return.unwrap()).collect();
            let value = pearson(&x, &y);
            value.is_finite().then_some(value)
        } else {
            None
        };
        let (quantile_returns, long_short_return) = if labeled.len() >= minimum_cross_section {
            let mut ranked = labeled.clone();
            ranked.sort_by(|a, b| {
                a.value
                    .total_cmp(&b.value)
                    .then_with(|| a.symbol.cmp(&b.symbol))
            });
            let mut sums = vec![0.0; quantiles];
            let mut counts = vec![0usize; quantiles];
            let value_count = ranked.len();
            for (i, value) in ranked.into_iter().enumerate() {
                let q = (i * quantiles / value_count).min(quantiles - 1);
                sums[q] += value.forward_return.unwrap();
                counts[q] += 1;
            }
            let qret: Vec<f64> = sums
                .into_iter()
                .zip(counts)
                .map(|(sum, count)| sum / count as f64)
                .collect();
            let spread = qret[quantiles - 1] - qret[0];
            (qret, Some(spread))
        } else {
            (Vec::new(), None)
        };
        let current_factor_values = by_symbol_date.remove(&date).unwrap_or_default();
        let factor_autocorrelation_lag1 = previous_factor_values.as_ref().and_then(|previous| {
            let (x, y): (Vec<_>, Vec<_>) = current_factor_values
                .iter()
                .filter_map(|(symbol, value)| {
                    previous
                        .get(symbol)
                        .map(|previous_value| (*previous_value, *value))
                })
                .unzip();
            (x.len() >= 3)
                .then(|| pearson(&x, &y))
                .filter(|value| value.is_finite())
        });
        let mut ranked_factor_values: Vec<_> = current_factor_values.iter().collect();
        ranked_factor_values.sort_by(|(left_symbol, left), (right_symbol, right)| {
            left.total_cmp(right)
                .then_with(|| left_symbol.cmp(right_symbol))
        });
        let top_count = ranked_factor_values.len().div_ceil(quantiles).max(1);
        let current_top: std::collections::BTreeSet<_> = ranked_factor_values
            .iter()
            .rev()
            .take(top_count)
            .map(|(symbol, _)| **symbol)
            .collect();
        let top_quantile_turnover = previous_top.as_ref().map(|previous| {
            if current_top.is_empty() {
                0.0
            } else {
                1.0 - current_top.intersection(previous).count() as f64 / current_top.len() as f64
            }
        });
        previous_factor_values = Some(current_factor_values);
        previous_top = Some(current_top);
        let factor_count = values.len();
        let labeled_count = labeled.len();
        let expected_count = expected_by_date.get(&date).copied().unwrap_or(factor_count);
        daily.push(DailyFactorResult {
            date,
            rank_ic,
            pearson_ic,
            count: labeled_count,
            expected_count,
            factor_count,
            missing_factor_count: expected_count.saturating_sub(factor_count),
            missing_label_count: factor_count.saturating_sub(labeled_count),
            quantile_returns,
            long_short_return,
            factor_autocorrelation_lag1,
            top_quantile_turnover,
        });
    }
    let ics: Vec<f64> = daily.iter().filter_map(|d| d.rank_ic).collect();
    let pearson_ics: Vec<f64> = daily.iter().filter_map(|d| d.pearson_ic).collect();
    let spreads: Vec<f64> = daily.iter().filter_map(|d| d.long_short_return).collect();
    let autocorrelations: Vec<f64> = daily
        .iter()
        .filter_map(|d| d.factor_autocorrelation_lag1)
        .collect();
    let turnovers: Vec<f64> = daily
        .iter()
        .filter_map(|d| d.top_quantile_turnover)
        .collect();
    let mean_ic = mean(&ics);
    let sd = stddev(&ics, mean_ic);
    let labeled_observation_count: usize = daily.iter().map(|d| d.count).sum();
    let missing_label_count: usize = daily.iter().map(|d| d.missing_label_count).sum();
    let missing_factor_count: usize = daily.iter().map(|d| d.missing_factor_count).sum();
    let expected_observation_count: usize = daily.iter().map(|d| d.expected_count).sum();
    let evaluation_status = if !ics.is_empty() {
        FactorEvaluationStatus::Evaluated
    } else if daily.iter().all(|d| d.count < minimum_cross_section) {
        FactorEvaluationStatus::InsufficientCrossSection
    } else {
        FactorEvaluationStatus::NoValidIc
    };
    FactorReport {
        factor: factor_name.into(),
        lookback_days: lookback,
        forward_days: forward,
        quantiles,
        label_method,
        evaluation_status,
        observation_count: observations.len(),
        labeled_observation_count,
        missing_label_count,
        missing_factor_count,
        expected_observation_count,
        coverage: (expected_observation_count > 0)
            .then_some(labeled_observation_count as f64 / expected_observation_count as f64),
        minimum_cross_section,
        evaluated_dates: daily.len(),
        mean_pearson_ic: (!pearson_ics.is_empty()).then(|| mean(&pearson_ics)),
        mean_rank_ic: (!ics.is_empty()).then_some(mean_ic),
        rank_ic_stddev: (ics.len() >= 2).then_some(sd),
        rank_ic_ir: (ics.len() >= 2 && sd > 0.0).then_some(mean_ic / sd),
        rank_ic_positive_rate: (!ics.is_empty())
            .then(|| ics.iter().filter(|value| **value > 0.0).count() as f64 / ics.len() as f64),
        mean_long_short_return: (!spreads.is_empty()).then(|| mean(&spreads)),
        mean_factor_autocorrelation_lag1: (!autocorrelations.is_empty())
            .then(|| mean(&autocorrelations)),
        mean_top_quantile_turnover: (!turnovers.is_empty()).then(|| mean(&turnovers)),
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

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2025, 1, day).unwrap()
    }

    fn observation(day: u32, symbol: &str, value: f64, label: Option<f64>) -> FactorObservation {
        FactorObservation {
            date: date(day),
            symbol: symbol.into(),
            value,
            forward_return: label,
        }
    }

    #[test]
    fn tied_ranks_are_averaged() {
        assert_eq!(ranks(&[3.0, 1.0, 1.0]), vec![2.0, 0.5, 0.5]);
    }
    #[test]
    fn correlation_is_directional() {
        assert!((pearson(&[1., 2., 3.], &[3., 2., 1.]) + 1.0).abs() < 1e-12);
    }

    #[test]
    fn evaluation_reports_hand_calculated_pearson_rank_ic_and_coverage() {
        let observations = vec![
            observation(2, "A", 1.0, Some(1.0)),
            observation(2, "B", 2.0, Some(4.0)),
            observation(2, "C", 3.0, Some(9.0)),
            observation(2, "D", 4.0, Some(16.0)),
            observation(2, "E", 5.0, Some(25.0)),
            observation(2, "F", 6.0, None),
        ];
        let expected = BTreeMap::from([(date(2), 8)]);
        let report = evaluate_observations_with_context(
            "test",
            1,
            1,
            2,
            ForwardReturnMethod::CloseToClose,
            observations,
            &expected,
        );

        // For x=1..5 and y=x², Pearson is 60 / sqrt(10*374), while the rank
        // vectors are identical and therefore have Rank IC 1.
        assert!((report.daily[0].pearson_ic.unwrap() - 60.0 / (3740.0_f64).sqrt()).abs() < 1e-12);
        assert!((report.daily[0].rank_ic.unwrap() - 1.0).abs() < 1e-12);
        assert_eq!(report.daily[0].count, 5);
        assert_eq!(report.daily[0].factor_count, 6);
        assert_eq!(report.daily[0].missing_label_count, 1);
        assert_eq!(report.daily[0].missing_factor_count, 2);
        assert_eq!(report.expected_observation_count, 8);
        assert_eq!(report.labeled_observation_count, 5);
        assert_eq!(report.coverage, Some(5.0 / 8.0));
        assert_eq!(report.evaluation_status, FactorEvaluationStatus::Evaluated);
    }

    #[test]
    fn insufficient_cross_section_is_not_reported_as_zero_ic() {
        let expected = BTreeMap::from([(date(2), 3)]);
        let report = evaluate_observations_with_context(
            "test",
            1,
            1,
            5,
            ForwardReturnMethod::CloseToClose,
            vec![
                observation(2, "A", 1.0, Some(0.01)),
                observation(2, "B", 2.0, Some(0.02)),
                observation(2, "C", 3.0, Some(0.03)),
            ],
            &expected,
        );

        assert_eq!(
            report.evaluation_status,
            FactorEvaluationStatus::InsufficientCrossSection
        );
        assert_eq!(report.mean_rank_ic, None);
        assert_eq!(report.mean_pearson_ic, None);
        assert_eq!(report.mean_long_short_return, None);
        assert!(report.daily[0].quantile_returns.is_empty());
    }
}
