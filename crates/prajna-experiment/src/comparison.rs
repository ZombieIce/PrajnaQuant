//! Frozen strategy-benchmark comparison definitions. The benchmark is derived only
//! in Rust from the frozen strategy selection; browsers never assemble it.
use std::path::Path;

use chrono::NaiveDate;
use prajna_research::{Panel, StaticUniverse, load_panel_for_venues};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Experiment, ExperimentError,
    selection::{self, Selection},
};

pub const SEMANTIC_VERSION: &str = "strategy-benchmark-comparison@1";
pub const RELATION_VERSION: &str = "strategy-benchmark@1";
pub const CALENDAR_VERSION: &str = "session-close-calendar@1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReinvestmentPolicy {
    /// Reinvest dividends at the strategy's frozen rebalance Session closes.
    StrategyRebalance,
    /// Explicitly declare that dividends are never reinvested.
    None,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub selection: Selection,
    pub reinvestment_calendar: ReinvestmentPolicy,
}

#[derive(Debug, Serialize)]
pub struct ComparisonError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for ComparisonError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

fn error(code: &'static str, message: impl ToString) -> ComparisonError {
    ComparisonError {
        code,
        message: message.to_string(),
    }
}

fn selection_error(failure: selection::SelectionError) -> ComparisonError {
    ComparisonError {
        code: failure.code,
        message: failure.message,
    }
}

fn experiment_error(failure: ExperimentError) -> ComparisonError {
    match failure {
        ExperimentError::InvalidDefinition(message) => {
            error("comparison_benchmark_invalid", message)
        }
        other => error("dataset_read_failed", other),
    }
}

fn strategy_kind(definition: &Value) -> Result<&str, ComparisonError> {
    match definition["strategy"].as_str() {
        Some(kind @ ("single_etf_timing" | "s2_rotation")) => Ok(kind),
        Some(other) => Err(error(
            "comparison_unsupported_strategy",
            format!("strategy {other} has no product holding benchmark"),
        )),
        None => Err(error(
            "comparison_unsupported_strategy",
            "selection has no supported strategy",
        )),
    }
}

fn rebalance_every(run_spec: &Value) -> Result<usize, ComparisonError> {
    run_spec["strategy"]["parameters"]["rebalance_every"]
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            error(
                "comparison_requires_rebalance_schedule",
                "frozen strategy has no positive rebalance_every",
            )
        })
}

/// Session closes after the evaluation start at the strategy's declared cadence.
fn calendar(
    panel: &Panel,
    start: NaiveDate,
    end: NaiveDate,
    every: usize,
    policy: ReinvestmentPolicy,
) -> Result<Value, ComparisonError> {
    let start_index = panel
        .sessions
        .iter()
        .position(|session| session.session_date == start)
        .ok_or_else(|| {
            error(
                "comparison_window_invalid",
                "evaluation start is not a Session",
            )
        })?;
    let end_index = panel
        .sessions
        .iter()
        .position(|session| session.session_date == end)
        .ok_or_else(|| {
            error(
                "comparison_window_invalid",
                "evaluation end is not a Session",
            )
        })?;
    let dates = if policy == ReinvestmentPolicy::None {
        Vec::new()
    } else {
        let mut dates = Vec::new();
        let Some(mut index) = start_index.checked_add(every) else {
            return Ok(
                json!({"semantic_version": CALENDAR_VERSION, "policy": policy, "decision_dates": []}),
            );
        };
        while index <= end_index {
            dates.push(panel.sessions[index].session_date);
            let Some(next) = index.checked_add(every) else {
                break;
            };
            index = next;
        }
        dates
    };
    Ok(json!({
        "semantic_version": CALENDAR_VERSION,
        "policy": policy,
        "decision_dates": dates.iter().map(ToString::to_string).collect::<Vec<_>>(),
    }))
}

fn benchmark_definition(
    strategy: &Value,
    kind: &str,
    calendar: &Value,
) -> Result<Value, ComparisonError> {
    let mut benchmark = strategy
        .as_object()
        .ok_or_else(|| {
            error(
                "comparison_definition_invalid",
                "strategy definition is not an object",
            )
        })?
        .clone();
    for forbidden in ["pit", "backend", "seed"] {
        if benchmark.contains_key(forbidden) {
            return Err(error(
                "comparison_unsupported_strategy",
                format!("benchmark comparison does not support {forbidden} definitions"),
            ));
        }
    }
    if benchmark.get("holding_benchmark").is_some() {
        return Err(error(
            "comparison_unsupported_strategy",
            "selection is already a holding benchmark",
        ));
    }
    benchmark.insert("strategy".into(), json!("buy_and_hold"));
    benchmark.insert("unfilled_entry".into(), json!("retry"));
    benchmark.insert(
        "parameter_space".into(),
        json!({"list":[{"params_version":1}]}),
    );
    let fast = benchmark
        .get_mut("fast_event")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            error(
                "comparison_requires_fast_event",
                "strategy selection must freeze Fast Event settings",
            )
        })?;
    fast.insert("rebalance".into(), json!("full_target"));
    fast.remove("entry_budget");
    benchmark.insert(
        "holding_benchmark".into(),
        json!({
            "semantic_version": "holding-benchmark@1",
            "kind": kind,
            "reinvestment_calendar": {
                "semantic_version": calendar["semantic_version"],
                "decision_dates": calendar["decision_dates"],
            },
        }),
    );
    Ok(Value::Object(benchmark))
}

fn require_equal(
    checks: &mut Vec<&'static str>,
    field: &'static str,
    strategy: &Value,
    benchmark: &Value,
) -> Result<(), ComparisonError> {
    if strategy != benchmark {
        return Err(error(
            "comparison_definition_mismatch",
            format!("strategy and benchmark disagree on {field}"),
        ));
    }
    checks.push(field);
    Ok(())
}

fn comparability(
    strategy: &Value,
    benchmark: &Value,
    benchmark_run: &Value,
    strategy_kind: &str,
    benchmark_kind: &str,
    calendar: &Value,
) -> Result<Value, ComparisonError> {
    let mut checks = Vec::new();
    for field in [
        "dsv",
        "universe",
        "venue",
        "venues",
        "engine",
        "availability_assumption",
        "sessions_per_year",
        "costs",
        "adjusted_versions",
        "evaluation",
        "corporate_actions",
    ] {
        require_equal(&mut checks, field, &strategy[field], &benchmark[field])?;
    }
    for field in [
        "currency",
        "initial_cash",
        "sizing",
        "minimum_commission",
        "portfolios",
        "lot_size_source",
    ] {
        require_equal(
            &mut checks,
            field,
            &strategy["fast_event"][field],
            &benchmark["fast_event"][field],
        )?;
    }
    let initial_cash = strategy["fast_event"]["initial_cash"]
        .as_str()
        .ok_or_else(|| error("comparison_definition_invalid", "missing initial cash"))?;
    Ok(json!({
        "semantic_version": RELATION_VERSION,
        "strategy_kind": strategy_kind,
        "benchmark_kind": benchmark_kind,
        "dsv": strategy["dsv"],
        "universe": strategy["universe"],
        "venues": strategy.get("venues").cloned().or_else(|| strategy.get("venue").cloned()).unwrap_or(Value::Null),
        "initial_cash": initial_cash,
        "minimum_commission": strategy["fast_event"]["minimum_commission"],
        "costs": strategy["costs"],
        "sessions_per_year": strategy["sessions_per_year"],
        "availability_assumption": strategy["availability_assumption"],
        "adjusted_versions": strategy["adjusted_versions"],
        "evaluation": strategy["evaluation"],
        "corporate_actions": strategy["corporate_actions"],
        "quantity_price_rules": benchmark_run["holding_benchmark"]["execution_rules"],
        "return_basis": "total_return",
        "reinvestment_calendar": calendar,
        "checks": checks,
    }))
}

/// Freezes the strategy through the existing selection boundary, derives exactly
/// one holding benchmark, and verifies the shared comparison inputs.
pub fn freeze(root: &Path, request: &Request) -> Result<Value, ComparisonError> {
    let selection = selection::freeze(root, &request.selection).map_err(selection_error)?;
    let definition = selection["data"]["definition"].clone();
    let strategy_kind = strategy_kind(&definition)?;
    if definition["corporate_actions"].is_null() {
        return Err(error(
            "comparison_requires_corporate_actions",
            "a total-return holding benchmark requires a frozen corporate-actions version",
        ));
    }
    if definition["evaluation"].is_null() {
        return Err(error(
            "comparison_requires_research_window",
            "a comparison requires the frozen common evaluation window",
        ));
    }
    if definition["engine"] != "fast_event" {
        return Err(error(
            "comparison_requires_fast_event",
            "holding benchmarks require Fast Event",
        ));
    }
    let strategy = Experiment::parse(&definition.to_string(), root).map_err(experiment_error)?;
    if strategy.venues().len() != 1 {
        return Err(error(
            "comparison_requires_single_venue",
            "the holding benchmark supports one real Venue only",
        ));
    }
    let evaluation = strategy
        .evaluation()
        .ok_or_else(|| error("comparison_requires_research_window", "missing evaluation"))?;
    let strategy_runs = strategy.run_definitions(root).map_err(experiment_error)?;
    if strategy_runs.len() != 1 {
        return Err(error(
            "comparison_requires_single_run",
            "freeze exactly one strategy parameter combination for a comparison",
        ));
    }
    let every = rebalance_every(&strategy_runs[0].run_spec)?;
    let dsv = definition["dsv"]
        .as_str()
        .ok_or_else(|| error("comparison_definition_invalid", "missing DSV"))?;
    let venues = strategy.venues().iter().cloned().collect::<Vec<_>>();
    let panel = load_panel_for_venues(root, dsv, &venues)
        .map_err(|failure| error("dataset_read_failed", failure))?;
    let calendar = calendar(
        &panel,
        evaluation.start(),
        evaluation.end(),
        every,
        request.reinvestment_calendar,
    )?;
    let universe = StaticUniverse::load(root, strategy.universe())
        .map_err(|failure| error("dataset_read_failed", failure))?;
    let benchmark_kind = if strategy_kind == "single_etf_timing" {
        if universe.members().len() != 1 {
            return Err(error(
                "comparison_definition_mismatch",
                "single ETF timing requires one Universe member",
            ));
        }
        "single_etf"
    } else {
        "equal_weight_pool"
    };
    let benchmark_definition = benchmark_definition(&definition, benchmark_kind, &calendar)?;
    let benchmark =
        Experiment::parse(&benchmark_definition.to_string(), root).map_err(experiment_error)?;
    let benchmark_runs = benchmark.run_definitions(root).map_err(experiment_error)?;
    if benchmark_runs.len() != 1 {
        return Err(error(
            "comparison_benchmark_invalid",
            "derived benchmark must expand to exactly one Run",
        ));
    }
    let comparability = comparability(
        &definition,
        &benchmark_definition,
        &benchmark_runs[0].run_spec,
        strategy_kind,
        benchmark_kind,
        &calendar,
    )?;
    Ok(json!({"schema_version": 1, "data": {
        "semantic_version": SEMANTIC_VERSION,
        "strategy": {
            "role": "strategy", "result_level": "standard", "definition": definition,
            "experiment": strategy.id().map_err(experiment_error)?, "run": strategy_runs[0].run_id,
        },
        "benchmark": {
            "role": "benchmark", "result_level": "standard", "kind": benchmark_kind,
            "definition": benchmark_definition,
            "experiment": benchmark.id().map_err(experiment_error)?, "run": benchmark_runs[0].run_id,
        },
        "comparability": comparability,
    }}))
}
