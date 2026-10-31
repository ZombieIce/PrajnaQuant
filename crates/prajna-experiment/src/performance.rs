//! Opt-in close-equity performance; legacy E11 output is never rewritten.
use crate::{DefinitionInput, Experiment, ExperimentError, ResultLevel, RunSummary, invalid};
use chrono::{Datelike, Months, NaiveDate};
use prajna_domain::Notional;
use prajna_research::{fast_event::FastEventResult, load_panel_for_venues};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

pub(crate) const VERSION: &str = "complete-performance@1";
pub const CALENDAR_VERSION: &str = "calendar-performance@1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    semantic_version: String,
    #[serde(default)]
    benchmark_definition: Option<Value>,
    #[serde(default)]
    calendar: Option<String>,
}
#[derive(Debug, Clone)]
pub(crate) struct Context {
    pub identity: Value,
    calendar: bool,
    benchmark: Option<Box<Experiment>>,
}
impl Context {
    pub fn load(
        input: &DefinitionInput,
        raw: &Value,
        root: &Path,
    ) -> Result<Option<Self>, ExperimentError> {
        let Some(binding) = &input.summary else {
            return Ok(None);
        };
        if binding.semantic_version != VERSION || input.sessions_per_year != 252 {
            return Err(invalid(
                "complete Summary requires complete-performance@1 and 252 Sessions/year",
            ));
        }
        let calendar = match binding.calendar.as_deref() {
            None => false,
            Some(CALENDAR_VERSION) => true,
            Some(_) => {
                return Err(invalid(
                    "complete Summary calendar requires calendar-performance@1",
                ));
            }
        };
        if input.engine != "fast_event"
            || input.pit.is_some()
            || input.corporate_actions.is_none()
            || input.evaluation.is_none()
        {
            return Err(invalid(
                "complete Summary requires frozen Static Fast Event evaluation and corporate actions",
            ));
        }
        let benchmark = if let Some(definition) = &binding.benchmark_definition {
            if definition.get("summary").is_some() {
                return Err(invalid("nested Summary selection is forbidden"));
            }
            // Compare normalized definitions below; raw inputs need not share spelling.
            let mut other = Experiment::parse(&definition.to_string(), root)?;
            if other.holding_benchmark.is_none()
                || other.venues.len() != 1
                || input.holding_benchmark.is_some()
            {
                return Err(invalid(
                    "Summary benchmark must be a single-Venue frozen holding benchmark",
                ));
            }
            // Parse the parent without its Summary to reuse all existing validation.
            let mut parent = raw.clone();
            parent.as_object_mut().unwrap().remove("summary");
            let parent = Experiment::parse(&parent.to_string(), root)?;
            for field in [
                "dsv",
                "universe",
                "venue",
                "venues",
                "costs",
                "sessions_per_year",
                "availability_assumption",
                "adjusted_versions",
                "evaluation",
                "corporate_actions",
            ] {
                if parent.normalized[field] != other.normalized[field] {
                    return Err(invalid(format!("Summary benchmark mismatch: {field}")));
                }
            }
            for field in [
                "currency",
                "initial_cash",
                "sizing",
                "minimum_commission",
                "portfolios",
                "lot_size_source",
            ] {
                if parent.normalized["fast_event"][field] != other.normalized["fast_event"][field] {
                    return Err(invalid(format!(
                        "Summary benchmark account mismatch: {field}"
                    )));
                }
            }
            if parent.expand(root)?.len() != 1 || other.expand(root)?.len() != 1 {
                return Err(invalid(
                    "Summary benchmark requires exactly one Run per role",
                ));
            }
            let panel = load_panel_for_venues(
                root,
                &parent.dsv,
                &parent.venues.iter().cloned().collect::<Vec<_>>(),
            )
            .map_err(|e| invalid(e.to_string()))?;
            let evaluation = parent.evaluation().unwrap();
            parent
                .engine
                .validate_comparison_prices(
                    &panel
                        .window(evaluation.start(), evaluation.end())
                        .map_err(|e| invalid(e.to_string()))?,
                    &other.holding_benchmark.as_ref().unwrap().price_ticks,
                )
                .map_err(invalid)?;
            other.performance = Some(Self {
                identity: json!({"semantic_version": VERSION}),
                calendar: false,
                benchmark: None,
            });
            Some(Box::new(other))
        } else {
            None
        };
        let mut identity = json!({"semantic_version": VERSION});
        if calendar {
            identity["calendar"] = json!(CALENDAR_VERSION);
        }
        if let Some(other) = &benchmark {
            identity["benchmark"] = other.normalized.clone();
        }
        Ok(Some(Self {
            identity,
            calendar,
            benchmark,
        }))
    }
    pub fn calendar(&self) -> bool {
        self.calendar
    }
    pub fn execute_benchmark(&self, root: &Path) -> Result<Option<RunSummary>, ExperimentError> {
        let Some(benchmark) = &self.benchmark else {
            return Ok(None);
        };
        let report = benchmark.execute_level(root, 1, ResultLevel::Summary)?;
        let run = &report.runs[0];
        if let Some(error) = &run.error {
            return Err(invalid(format!("Summary benchmark failed: {error}")));
        }
        run.summary
            .clone()
            .map(Some)
            .ok_or_else(|| invalid("Summary benchmark has no Summary"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    pub value: Option<f64>,
    pub reason: Option<String>,
}
impl Metric {
    fn new(value: Option<f64>, reason: &str) -> Self {
        let reason = if value.is_some_and(|v| !v.is_finite()) {
            "nonfinite_result"
        } else {
            reason
        };
        let value = value.filter(|v| v.is_finite());
        Self {
            value,
            reason: value.is_none().then(|| reason.into()),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompleteSummary {
    pub semantic_version: String,
    pub currency: String,
    pub evaluation_start: Option<String>,
    pub evaluation_end: Option<String>,
    pub metrics: BTreeMap<String, Metric>,
    /// Scale-18 account-currency amounts, never cumulative rates.
    pub amounts: BTreeMap<String, String>,
    pub fill_count: usize,
    pub fee_settlement_count: usize,
    /// Opt-in `calendar-performance@1` report over the same close series.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<CalendarReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalendarReport {
    pub semantic_version: String,
    pub valuation_point: String,
    pub initial_capital: String,
    pub evaluation_start: Option<String>,
    pub evaluation_end: Option<String>,
    pub sessions: usize,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_drawdown: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawdown_peak_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawdown_trough_session: Option<String>,
    pub points: Vec<CalendarPoint>,
    pub monthly: Vec<CalendarPeriod>,
    pub annual: Vec<CalendarPeriod>,
    pub missing_months: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalendarPoint {
    pub session_date: String,
    /// Scale-18 close account equity; NAV = cash + holdings + receivables.
    pub equity: String,
    pub nav: f64,
    pub drawdown: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalendarPeriod {
    pub label: String,
    /// `initial_capital` for the first segment, then `previous_close`.
    pub start_basis: String,
    pub start_session: Option<String>,
    pub end_session: String,
    pub return_value: f64,
}

pub(crate) fn complete(
    summary: &mut RunSummary,
    result: &FastEventResult,
    benchmark: Option<&RunSummary>,
    with_calendar: bool,
) -> Result<(), String> {
    let number = |n: Notional| n.to_string().parse::<f64>().map_err(|e| e.to_string());
    let initial = number(result.config.initial_cash)?;
    if initial <= 0.0 {
        return Err("nonpositive initial capital".into());
    }
    let mut previous = initial;
    let mut returns = Vec::new();
    let mut closes = Vec::with_capacity(result.sessions.len());
    for session in &result.sessions {
        session.open_after.validate().map_err(|e| e.to_string())?;
        session.close.validate().map_err(|e| e.to_string())?;
        let equity = number(session.close.account.equity)?;
        if previous <= 0.0 || equity <= 0.0 {
            return Err("nonpositive Summary equity denominator".into());
        }
        returns.push(equity / previous - 1.0);
        closes.push((session.session.session_date, session.close.account.equity));
        previous = equity;
    }
    let n = returns.len();
    let total = (n > 0).then_some(previous / initial - 1.0);
    let annual = total
        .map(|r| (1.0 + r).powf(252.0 / n as f64) - 1.0)
        .filter(|v| v.is_finite());
    let std = (n >= 2).then(|| crate::summary::sample_std(&returns));
    let mean = (n > 0).then(|| returns.iter().sum::<f64>() / n as f64);
    let vol = std.map(|v| v * 252f64.sqrt());
    let sharpe = std
        .filter(|s| *s > 0.0)
        .zip(mean)
        .map(|(s, m)| m / s * 252f64.sqrt());
    summary.total_return = total;
    summary.annualized_return = annual;
    summary.n_returns = n;
    summary.mean_return = mean;
    summary.std_return = std;
    summary.annualized_volatility = vol;
    summary.sharpe = sharpe;
    summary.insufficient_sessions = n < 2;
    summary.zero_volatility = std == Some(0.0);
    if summary.drawdown_peak_session.as_ref()
        == result
            .sessions
            .first()
            .map(|s| s.session.session_date.to_string())
            .as_ref()
        && result
            .sessions
            .first()
            .is_some_and(|s| number(s.close.account.equity).unwrap_or(0.0) < initial)
    {
        summary.drawdown_peak_session = Some("initial_capital".into());
    }
    let mut metrics = BTreeMap::new();
    for (name, value, reason) in [
        ("total_return", total, "empty_evaluation"),
        (
            "annualized_return",
            annual,
            if n == 0 {
                "empty_evaluation"
            } else {
                "nonfinite_result"
            },
        ),
        ("annualized_volatility", vol, "insufficient_returns"),
        (
            "sharpe",
            sharpe,
            if n < 2 {
                "insufficient_returns"
            } else {
                "zero_volatility"
            },
        ),
        ("max_drawdown", summary.max_drawdown, "empty_evaluation"),
        (
            "calmar",
            annual
                .zip(summary.max_drawdown)
                .filter(|(_, d)| *d < 0.0)
                .map(|(a, d)| a / -d),
            if n == 0 {
                "empty_evaluation"
            } else if annual.is_none() {
                "annualized_return_unavailable"
            } else {
                "zero_drawdown"
            },
        ),
        (
            "cumulative_cost_rate",
            Some(summary.total_cost),
            "unavailable",
        ),
        (
            "cumulative_turnover_rate",
            Some(summary.total_turnover),
            "unavailable",
        ),
    ] {
        metrics.insert(name.into(), Metric::new(value, reason));
    }
    let complete_actions = result.return_basis
        == prajna_research::corporate_actions::ReturnBasis::TotalReturn
        && result.corporate_action_completeness.is_complete;
    if !complete_actions {
        for name in [
            "total_return",
            "annualized_return",
            "annualized_volatility",
            "sharpe",
            "max_drawdown",
            "calmar",
        ] {
            metrics.insert(
                name.into(),
                Metric::new(None, "corporate_actions_incomplete"),
            );
        }
        summary.total_return = None;
        summary.annualized_return = None;
        summary.annualized_volatility = None;
        summary.sharpe = None;
        summary.max_drawdown = None;
        summary.mean_return = None;
        summary.std_return = None;
        summary.drawdown_peak_session = None;
        summary.drawdown_trough_session = None;
    }
    let benchmark_return = benchmark
        .and_then(|b| b.complete.as_ref())
        .and_then(|b| b.metrics.get("total_return"))
        .and_then(|m| m.value);
    let reason = if benchmark.is_none() {
        "benchmark_not_bound"
    } else {
        "benchmark_or_strategy_incomplete"
    };
    metrics.insert(
        "benchmark_return".into(),
        Metric::new(benchmark_return, reason),
    );
    metrics.insert(
        "excess_return".into(),
        Metric::new(
            metrics["total_return"]
                .value
                .zip(benchmark_return)
                .map(|(s, b)| s - b),
            reason,
        ),
    );
    let mut sums = [0i128; 4];
    for (costs, slip) in result
        .fills
        .iter()
        .map(|f| (f.fill.costs(), f.fill.slippage()))
        .chain(
            result
                .account
                .fee_settlements()
                .iter()
                .map(|f| (f.costs(), f.slippage())),
        )
    {
        for (sum, value) in sums.iter_mut().zip([
            costs.commission,
            costs.minimum_commission_top_up,
            costs.tax,
            slip,
        ]) {
            *sum = sum
                .checked_add(value.mantissa())
                .ok_or("cost amount overflow")?;
        }
    }
    let mut amounts = BTreeMap::new();
    let mut total_amount = 0i128;
    for (name, amount) in ["commission", "minimum_commission_top_up", "tax", "slippage"]
        .into_iter()
        .zip(sums)
    {
        total_amount = total_amount
            .checked_add(amount)
            .ok_or("total cost overflow")?;
        amounts.insert(
            name.into(),
            Notional::from_mantissa(amount)
                .map_err(|e| e.to_string())?
                .to_string(),
        );
    }
    amounts.insert(
        "total_execution_cost".into(),
        Notional::from_mantissa(total_amount)
            .map_err(|e| e.to_string())?
            .to_string(),
    );
    amounts.insert(
        "initial_capital".into(),
        result.config.initial_cash.to_string(),
    );
    summary.complete = Some(CompleteSummary {
        semantic_version: VERSION.into(),
        currency: result.config.currency.to_string(),
        evaluation_start: result
            .sessions
            .first()
            .map(|s| s.session.session_date.to_string()),
        evaluation_end: result
            .sessions
            .last()
            .map(|s| s.session.session_date.to_string()),
        metrics,
        amounts,
        fill_count: result.fills.len(),
        fee_settlement_count: result.account.fee_settlements().len(),
        calendar: with_calendar
            .then(|| build_calendar(result.config.initial_cash, &closes, complete_actions)),
    });
    Ok(())
}

/// Build the calendar report from the same close series as the complete Summary.
/// The first segment starts from initial capital; months without a Session are
/// listed in `missing_months` instead of becoming zero returns.
fn build_calendar(
    initial: Notional,
    closes: &[(NaiveDate, Notional)],
    actions_complete: bool,
) -> CalendarReport {
    let evaluation_start = closes.first().map(|(date, _)| date.to_string());
    let evaluation_end = closes.last().map(|(date, _)| date.to_string());
    let unavailable = |reason: &str| CalendarReport {
        semantic_version: CALENDAR_VERSION.into(),
        valuation_point: "close".into(),
        initial_capital: initial.to_string(),
        evaluation_start: evaluation_start.clone(),
        evaluation_end: evaluation_end.clone(),
        sessions: closes.len(),
        available: false,
        reason: Some(reason.into()),
        max_drawdown: None,
        drawdown_peak_session: None,
        drawdown_trough_session: None,
        points: Vec::new(),
        monthly: Vec::new(),
        annual: Vec::new(),
        missing_months: Vec::new(),
    };
    if closes.is_empty() {
        return unavailable("empty_evaluation");
    }
    if !actions_complete {
        return unavailable("corporate_actions_incomplete");
    }
    let initial_value = initial
        .to_string()
        .parse::<f64>()
        .expect("complete Summary validated initial capital");
    let first_date = closes[0].0.to_string();
    let mut peak = 1.0_f64;
    let mut peak_session = first_date.clone();
    let mut max_drawdown = 0.0_f64;
    let mut drawdown_peak_session: Option<String> = None;
    let mut drawdown_trough_session: Option<String> = None;
    let mut points = Vec::with_capacity(closes.len());
    let mut series = Vec::with_capacity(closes.len());
    for (date, equity) in closes {
        let value = equity
            .to_string()
            .parse::<f64>()
            .expect("complete Summary validated close equity");
        let nav = value / initial_value;
        if nav > peak {
            peak = nav;
            peak_session = date.to_string();
        }
        let drawdown = nav / peak - 1.0;
        if drawdown < max_drawdown {
            max_drawdown = drawdown;
            drawdown_peak_session = Some(peak_session.clone());
            drawdown_trough_session = Some(date.to_string());
        }
        points.push(CalendarPoint {
            session_date: date.to_string(),
            equity: equity.to_string(),
            nav,
            drawdown,
        });
        series.push((*date, value));
    }
    if max_drawdown == 0.0 {
        drawdown_peak_session = None;
        drawdown_trough_session = None;
    }
    if drawdown_peak_session.as_deref() == Some(first_date.as_str())
        && series[0].1 / initial_value < 1.0
    {
        drawdown_peak_session = Some("initial_capital".into());
    }
    let monthly = periods(&series, initial_value, |date| {
        format!("{:04}-{:02}", date.year(), date.month())
    });
    let annual = periods(&series, initial_value, |date| format!("{:04}", date.year()));
    let present = monthly
        .iter()
        .map(|period| period.label.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut missing_months = Vec::new();
    let last = series.last().expect("non-empty close series").0;
    let mut cursor = NaiveDate::from_ymd_opt(series[0].0.year(), series[0].0.month(), 1)
        .expect("valid first Session month");
    let limit = NaiveDate::from_ymd_opt(last.year(), last.month(), 1).expect("valid last month");
    while cursor <= limit {
        let label = format!("{:04}-{:02}", cursor.year(), cursor.month());
        if !present.contains(label.as_str()) {
            missing_months.push(label);
        }
        cursor = cursor
            .checked_add_months(Months::new(1))
            .expect("month iteration bounded by evaluation dates");
    }
    CalendarReport {
        semantic_version: CALENDAR_VERSION.into(),
        valuation_point: "close".into(),
        initial_capital: initial.to_string(),
        evaluation_start,
        evaluation_end,
        sessions: closes.len(),
        available: true,
        reason: None,
        max_drawdown: Some(max_drawdown),
        drawdown_peak_session,
        drawdown_trough_session,
        points,
        monthly,
        annual,
        missing_months,
    }
}

/// Split the close series into periods; the first starts from initial capital.
fn periods(
    series: &[(NaiveDate, f64)],
    initial_value: f64,
    label: impl Fn(NaiveDate) -> String,
) -> Vec<CalendarPeriod> {
    let mut periods = Vec::new();
    let mut group = label(series[0].0);
    let mut start_basis = "initial_capital";
    let mut start_session: Option<String> = None;
    let mut start_value = initial_value;
    let mut last_value = initial_value;
    let mut last_date = series[0].0;
    for (date, value) in series {
        let current = label(*date);
        if current != group {
            periods.push(CalendarPeriod {
                label: group,
                start_basis: start_basis.into(),
                start_session: start_session.clone(),
                end_session: last_date.to_string(),
                return_value: last_value / start_value - 1.0,
            });
            group = current;
            start_basis = "previous_close";
            start_session = Some(last_date.to_string());
            start_value = last_value;
        }
        last_value = *value;
        last_date = *date;
    }
    periods.push(CalendarPeriod {
        label: group,
        start_basis: start_basis.into(),
        start_session,
        end_session: last_date.to_string(),
        return_value: last_value / start_value - 1.0,
    });
    periods
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(value: &str) -> Notional {
        value.parse().unwrap()
    }
    fn d(value: &str) -> NaiveDate {
        value.parse().unwrap()
    }
    fn near(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-12, "{actual} != {expected}");
    }

    #[test]
    fn calendar_golden_spans_months_and_years_without_filling_gaps() {
        let closes = [
            (d("2026-01-30"), n("985")),
            (d("2026-02-27"), n("990")),
            (d("2026-12-31"), n("1100")),
            (d("2027-01-29"), n("1045")),
        ];
        let report = build_calendar(n("1000"), &closes, true);
        assert_eq!(report.semantic_version, CALENDAR_VERSION);
        assert_eq!(report.valuation_point, "close");
        assert_eq!(report.initial_capital, "1000");
        assert_eq!(report.evaluation_start.as_deref(), Some("2026-01-30"));
        assert_eq!(report.evaluation_end.as_deref(), Some("2027-01-29"));
        assert_eq!(report.sessions, 4);
        assert!(report.available);
        assert_eq!(report.reason, None);
        assert_eq!(report.points.len(), 4);
        near(report.points[0].nav, 0.985);
        near(report.points[0].drawdown, -0.015);
        near(report.points[2].nav, 1.1);
        near(report.points[3].drawdown, 1.045 / 1.1 - 1.0);
        assert_eq!(report.points[0].equity, "985");

        let monthly = &report.monthly;
        assert_eq!(
            monthly.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(),
            ["2026-01", "2026-02", "2026-12", "2027-01"]
        );
        assert_eq!(monthly[0].start_basis, "initial_capital");
        assert_eq!(monthly[0].start_session, None);
        assert_eq!(monthly[0].end_session, "2026-01-30");
        near(monthly[0].return_value, -0.015);
        assert_eq!(monthly[1].start_basis, "previous_close");
        assert_eq!(monthly[1].start_session.as_deref(), Some("2026-01-30"));
        near(monthly[1].return_value, 990.0 / 985.0 - 1.0);
        near(monthly[2].return_value, 1100.0 / 990.0 - 1.0);
        near(monthly[3].return_value, 1045.0 / 1100.0 - 1.0);

        let annual = &report.annual;
        assert_eq!(
            annual.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(),
            ["2026", "2027"]
        );
        assert_eq!(annual[0].start_basis, "initial_capital");
        assert_eq!(annual[0].start_session, None);
        near(annual[0].return_value, 0.1);
        assert_eq!(annual[1].start_session.as_deref(), Some("2026-12-31"));
        near(annual[1].return_value, 1045.0 / 1100.0 - 1.0);

        near(report.max_drawdown.unwrap(), 1.045 / 1.1 - 1.0);
        assert_eq!(report.drawdown_peak_session.as_deref(), Some("2026-12-31"));
        assert_eq!(
            report.drawdown_trough_session.as_deref(),
            Some("2027-01-29")
        );
        assert_eq!(
            report.missing_months,
            (3..=11)
                .map(|month| format!("2026-{month:02}"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn calendar_marks_an_initial_capital_peak_and_keeps_zero_drawdown_explicit() {
        let closes = [
            (d("2026-01-30"), n("985")),
            (d("2026-02-02"), n("1000")),
            (d("2026-02-03"), n("950")),
        ];
        let report = build_calendar(n("1000"), &closes, true);
        near(report.max_drawdown.unwrap(), -0.05);
        assert_eq!(
            report.drawdown_peak_session.as_deref(),
            Some("initial_capital")
        );
        assert_eq!(
            report.drawdown_trough_session.as_deref(),
            Some("2026-02-03")
        );

        let flat = build_calendar(
            n("1000"),
            &[(d("2026-01-30"), n("1000")), (d("2026-01-31"), n("1010"))],
            true,
        );
        assert_eq!(flat.max_drawdown, Some(0.0));
        assert_eq!(flat.drawdown_peak_session, None);
        assert_eq!(flat.drawdown_trough_session, None);

        let rising = build_calendar(
            n("1000"),
            &[(d("2026-01-30"), n("1010")), (d("2026-01-31"), n("1000"))],
            true,
        );
        assert_eq!(rising.drawdown_peak_session.as_deref(), Some("2026-01-30"));
        assert_eq!(
            rising.drawdown_trough_session.as_deref(),
            Some("2026-01-31")
        );
    }

    #[test]
    fn calendar_unavailable_reasons_never_become_zero_performance() {
        let single = build_calendar(n("1000"), &[(d("2026-01-30"), n("985"))], true);
        assert!(single.available);
        assert_eq!(single.monthly.len(), 1);
        near(single.monthly[0].return_value, -0.015);
        near(single.annual[0].return_value, -0.015);

        let empty = build_calendar(n("1000"), &[], true);
        assert!(!empty.available);
        assert_eq!(empty.reason.as_deref(), Some("empty_evaluation"));
        assert_eq!(empty.sessions, 0);
        assert_eq!(empty.evaluation_start, None);
        assert!(empty.points.is_empty() && empty.monthly.is_empty() && empty.annual.is_empty());
        assert_eq!(empty.max_drawdown, None);

        let incomplete = build_calendar(n("1000"), &[(d("2026-01-30"), n("985"))], false);
        assert!(!incomplete.available);
        assert_eq!(
            incomplete.reason.as_deref(),
            Some("corporate_actions_incomplete")
        );
        assert_eq!(incomplete.evaluation_start.as_deref(), Some("2026-01-30"));
        assert!(incomplete.points.is_empty() && incomplete.monthly.is_empty());
        assert_eq!(incomplete.max_drawdown, None);
        assert_eq!(incomplete.missing_months, Vec::<String>::new());
    }
}
