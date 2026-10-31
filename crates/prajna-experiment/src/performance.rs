//! Opt-in close-equity performance; legacy E11 output is never rewritten.
use crate::{DefinitionInput, Experiment, ExperimentError, ResultLevel, RunSummary, invalid};
use prajna_domain::Notional;
use prajna_research::{fast_event::FastEventResult, load_panel_for_venues};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

const VERSION: &str = "complete-performance@1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    semantic_version: String,
    #[serde(default)]
    benchmark_definition: Option<Value>,
}
#[derive(Debug, Clone)]
pub(crate) struct Context {
    pub identity: Value,
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
                benchmark: None,
            });
            Some(Box::new(other))
        } else {
            None
        };
        let mut identity = json!({"semantic_version": VERSION});
        if let Some(other) = &benchmark {
            identity["benchmark"] = other.normalized.clone();
        }
        Ok(Some(Self {
            identity,
            benchmark,
        }))
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
}

pub(crate) fn complete(
    summary: &mut RunSummary,
    result: &FastEventResult,
    benchmark: Option<&RunSummary>,
) -> Result<(), String> {
    let number = |n: Notional| n.to_string().parse::<f64>().map_err(|e| e.to_string());
    let initial = number(result.config.initial_cash)?;
    if initial <= 0.0 {
        return Err("nonpositive initial capital".into());
    }
    let mut previous = initial;
    let mut returns = Vec::new();
    for session in &result.sessions {
        session.open_after.validate().map_err(|e| e.to_string())?;
        session.close.validate().map_err(|e| e.to_string())?;
        let equity = number(session.close.account.equity)?;
        if previous <= 0.0 || equity <= 0.0 {
            return Err("nonpositive Summary equity denominator".into());
        }
        returns.push(equity / previous - 1.0);
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
    });
    Ok(())
}
