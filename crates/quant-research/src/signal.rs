use crate::{
    core::{Bar, ForwardReturnMethod},
    experiment::UniverseReportIdentity,
    factor::{
        FactorObservation, FactorReport, evaluate_observations_with_context,
        forward_return_for_series,
    },
    feature::{pct_change, rolling_drawdown, rolling_mean, rolling_std},
    universe::{
        Capability, CapabilityKind, MembershipSnapshot, UniverseDefinition, market_bar_symbol,
        resolve_members,
    },
};
use anyhow::{Result, bail};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
    #[serde(default)]
    pub label_method: ForwardReturnMethod,
    #[serde(default)]
    pub universe_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub version_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub strict_pit: bool,
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
            label_method: ForwardReturnMethod::NextOpenToForwardOpen,
            universe_id: None,
            version_id: None,
            strict_pit: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalResearchReport {
    pub definition: SignalDefinition,
    #[serde(default)]
    pub calendar_basis: String,
    #[serde(default)]
    pub universe: Option<UniverseReportIdentity>,
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

/// Produces a point-in-time signal panel. Values are normalized so larger values
/// are more favourable; forward labels are selectable and never enter scoring.
pub fn observations(
    bars: &[Bar],
    key: &str,
    forward_days: usize,
) -> Result<Vec<FactorObservation>> {
    let calendar = crate::factor::observed_market_calendar(bars);
    observations_with_calendar(
        bars,
        key,
        forward_days,
        ForwardReturnMethod::CloseToClose,
        &calendar,
    )
}

pub fn observations_with_calendar(
    bars: &[Bar],
    key: &str,
    forward_days: usize,
    label_method: ForwardReturnMethod,
    calendar: &[NaiveDate],
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
            if !crate::factor::has_forward_calendar_window(
                series[index].trade_date,
                forward_days,
                label_method,
                calendar,
            ) {
                continue;
            }
            let forward_return =
                forward_return_for_series(&series, index, forward_days, label_method, calendar);
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

pub fn observations_with_method(
    bars: &[Bar],
    key: &str,
    forward_days: usize,
    label_method: ForwardReturnMethod,
) -> Result<Vec<FactorObservation>> {
    let calendar = crate::factor::observed_market_calendar(bars);
    observations_with_calendar(bars, key, forward_days, label_method, &calendar)
}

pub fn analyze(bars: &[Bar], request: &SignalResearchRequest) -> Result<SignalResearchReport> {
    let mut expected_by_date = BTreeMap::<NaiveDate, usize>::new();
    for bar in bars {
        *expected_by_date.entry(bar.trade_date).or_default() += 1;
    }
    analyze_with_context(bars, request, &expected_by_date)
}

pub fn analyze_with_context(
    bars: &[Bar],
    request: &SignalResearchRequest,
    expected_by_date: &BTreeMap<NaiveDate, usize>,
) -> Result<SignalResearchReport> {
    let calendar = crate::factor::observed_market_calendar(bars);
    analyze_with_evaluation_calendar(
        bars,
        request,
        expected_by_date,
        &calendar,
        "observed_etf_bar_dates",
    )
}

pub fn analyze_with_evaluation_calendar(
    bars: &[Bar],
    request: &SignalResearchRequest,
    expected_by_date: &BTreeMap<NaiveDate, usize>,
    calendar: &[NaiveDate],
    calendar_basis: &str,
) -> Result<SignalResearchReport> {
    analyze_internal(
        bars,
        request,
        expected_by_date,
        calendar,
        calendar_basis,
        None,
        None,
    )
}

pub fn analyze_with_universe(
    bars: &[Bar],
    request: &SignalResearchRequest,
    definition: &UniverseDefinition,
    strict_pit: bool,
    snapshot_hash: &str,
    calendar: &[NaiveDate],
    calendar_basis: &str,
) -> Result<SignalResearchReport> {
    if request.universe_id != Some(definition.universe_id)
        || request.version_id != Some(definition.version_id)
    {
        bail!("universe_version_mismatch");
    }
    let mut allowed = BTreeMap::<NaiveDate, BTreeSet<String>>::new();
    let mut snapshots = Vec::<MembershipSnapshot>::new();
    for date in bars
        .iter()
        .map(|bar| bar.trade_date)
        .collect::<BTreeSet<_>>()
    {
        let snapshot = resolve_members(
            definition,
            date,
            crate::runner::decision_cutoff(date)?,
            strict_pit,
        )?;
        let symbols = snapshot
            .members
            .iter()
            .map(|member| market_bar_symbol(&member.instrument.exchange, &member.instrument.code))
            .collect::<Result<BTreeSet<_>>>()?;
        allowed.insert(date, symbols);
        snapshots.push(snapshot);
    }
    let expected_by_date = allowed
        .iter()
        .map(|(date, symbols)| (*date, symbols.len()))
        .collect::<BTreeMap<_, _>>();
    let identity = UniverseReportIdentity::freeze(
        definition,
        &snapshots,
        vec![
            Capability {
                kind: CapabilityKind::MembershipReady,
                ready: true,
                blockers: Vec::new(),
            },
            Capability {
                kind: CapabilityKind::FactorResearchReady,
                ready: true,
                blockers: Vec::new(),
            },
        ],
        snapshot_hash.to_owned(),
    )?;
    analyze_internal(
        bars,
        request,
        &expected_by_date,
        calendar,
        calendar_basis,
        Some(&allowed),
        Some(identity),
    )
}

fn analyze_internal(
    bars: &[Bar],
    request: &SignalResearchRequest,
    expected_by_date: &BTreeMap<NaiveDate, usize>,
    calendar: &[NaiveDate],
    calendar_basis: &str,
    allowed: Option<&BTreeMap<NaiveDate, BTreeSet<String>>>,
    universe: Option<UniverseReportIdentity>,
) -> Result<SignalResearchReport> {
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
        let mut panel = observations_with_calendar(
            bars,
            &request.signal,
            *period,
            request.label_method,
            calendar,
        )?;
        if let Some(allowed) = allowed {
            panel.retain(|observation| {
                allowed
                    .get(&observation.date)
                    .is_some_and(|members| members.contains(&observation.symbol))
            });
        }
        if *period == request.forward_periods[0] {
            primary_observations = panel.clone();
        }
        let labelable_expected = crate::factor::expected_with_forward_window(
            expected_by_date,
            *period,
            request.label_method,
            calendar,
        );
        reports.push(evaluate_observations_with_context(
            &request.signal,
            definition.lookback,
            *period,
            request.quantiles,
            request.label_method,
            panel,
            &labelable_expected,
        ));
    }
    Ok(SignalResearchReport {
        definition,
        calendar_basis: calendar_basis.into(),
        universe,
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
                .skip(1)
                .map(|value| value.unwrap_or(f64::NAN))
                .collect::<Vec<_>>();
            let standard_deviations = rolling_std(&daily_returns, window);
            let mut aligned = vec![None; closes.len()];
            for (return_index, value) in standard_deviations.into_iter().enumerate() {
                aligned[return_index + 1] = value.filter(|value| value.is_finite());
            }
            aligned
        }
        "drawdown" => rolling_drawdown(closes, window),
        _ => vec![None; closes.len()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(day: u32, open: f64, close: f64) -> Bar {
        Bar {
            symbol: "ETF".into(),
            name: "ETF".into(),
            trade_date: NaiveDate::from_ymd_opt(2025, 1, day).unwrap(),
            open,
            high: open.max(close),
            low: open.min(close),
            close,
            volume: 1.0,
            amount: None,
        }
    }

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

    #[test]
    fn next_open_label_uses_market_calendar_and_missing_bar_stays_missing() {
        let bars = [bar(1, 10.0, 11.0), bar(2, 20.0, 22.0), bar(3, 30.0, 33.0)];
        let calendar = [1, 2, 3, 4].map(|day| NaiveDate::from_ymd_opt(2025, 1, day).unwrap());
        let series = bars.iter().collect::<Vec<_>>();
        let next_open = forward_return_for_series(
            &series,
            0,
            1,
            ForwardReturnMethod::NextOpenToForwardOpen,
            &calendar,
        );
        let close_diagnostic =
            forward_return_for_series(&series, 0, 1, ForwardReturnMethod::CloseToClose, &calendar);
        assert_eq!(next_open, Some(0.5));
        assert_eq!(close_diagnostic, Some(1.0));

        let bars_with_gap = [bar(1, 10.0, 11.0), bar(3, 30.0, 33.0)];
        let gap_series = bars_with_gap.iter().collect::<Vec<_>>();
        assert_eq!(
            forward_return_for_series(
                &gap_series,
                0,
                1,
                ForwardReturnMethod::NextOpenToForwardOpen,
                &calendar,
            ),
            None
        );
    }

    #[test]
    fn volatility_first_window_uses_only_observed_returns() {
        let signal = def(
            "volatility",
            2,
            SignalCategory::Volatility,
            SignalDirection::LowerIsBetter,
            "test",
        );
        let values = signal_values(&signal, &[100.0, 110.0, 99.0, 108.9]);
        assert_eq!(values[0], None);
        assert_eq!(values[1], None);
        assert!((values[2].unwrap() - 0.02_f64.sqrt()).abs() < 1e-12);
    }
}
