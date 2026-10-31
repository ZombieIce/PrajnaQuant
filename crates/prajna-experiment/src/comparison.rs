//! Frozen strategy-benchmark comparison definitions. The benchmark is derived only
//! in Rust from the frozen strategy selection; browsers never assemble it.
use std::path::Path;

use prajna_research::{Panel, StaticUniverse, cache::store::FactorCache, load_panel_for_venues};
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

/// Reuse execution's full-panel scoring, cadence and evaluation filtering.
fn calendar(
    root: &Path,
    strategy: &Experiment,
    panel: &Panel,
    universe: &StaticUniverse,
    policy: ReinvestmentPolicy,
) -> Result<Value, ComparisonError> {
    let mut dates = Vec::new();
    if policy == ReinvestmentPolicy::StrategyRebalance {
        let run = strategy.expand(root).map_err(experiment_error)?.remove(0);
        let assumption = strategy
            .availability_assumption()
            .map_err(experiment_error)?;
        let mut factors = std::collections::HashMap::new();
        if let Some(factor) = &run.factor {
            let values = FactorCache::open(root)
                .and_then(|cache| {
                    cache.get_or_compute(
                        panel,
                        universe.members(),
                        strategy.universe(),
                        &strategy.dsv,
                        factor,
                        assumption,
                    )
                })
                .map_err(|e| error("comparison_calendar_failed", e))?;
            factors.insert(factor.canonical_json().to_string(), values);
        }
        let research = if strategy.strategy_kind == "single_etf_timing" {
            Some(crate::timing::research_panel(strategy, root, panel).map_err(experiment_error)?)
        } else {
            None
        };
        let evaluation = strategy.evaluation().unwrap();
        let ledger = panel
            .window(evaluation.start(), evaluation.end())
            .map_err(|e| error("comparison_window_invalid", e))?;
        dates = crate::engine::windowed_decisions(
            strategy,
            strategy.engine.selection(),
            &run,
            &factors,
            research.as_ref().unwrap_or(panel),
            &ledger,
            universe.members(),
            assumption,
        )
        .map_err(|e| error("comparison_calendar_failed", e))?
        .into_iter()
        .map(|d| d.decision_session.session_date)
        // Entry already has its own Decision at the first evaluation close.
        .filter(|date| *date > evaluation.start())
        .collect();
    }
    Ok(json!({"semantic_version": CALENDAR_VERSION, "policy": policy, "decision_dates": dates}))
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

/// Both comparison roles opt into the complete Summary and its calendar report.
/// The strategy Summary binds the frozen holding benchmark so benchmark/excess
/// returns are produced by Rust instead of the browser.
fn performance_binding(benchmark: Option<Value>) -> Value {
    let mut summary = json!({
        "semantic_version": crate::performance::VERSION,
        "calendar": crate::performance::CALENDAR_VERSION,
    });
    if let Some(benchmark) = benchmark {
        summary["benchmark_definition"] = benchmark;
    }
    summary
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
    for field in ["semantic_version", "calendar"] {
        if strategy["summary"][field] != benchmark["summary"][field] {
            return Err(error(
                "comparison_definition_mismatch",
                format!("strategy and benchmark disagree on summary.{field}"),
            ));
        }
    }
    if strategy["summary"]["benchmark_definition"]["holding_benchmark"].is_null() {
        return Err(error(
            "comparison_definition_mismatch",
            "strategy Summary must bind the frozen holding benchmark",
        ));
    }
    checks.push("summary");
    let initial_cash = strategy["fast_event"]["initial_cash"]
        .as_str()
        .ok_or_else(|| error("comparison_definition_invalid", "missing initial cash"))?;
    Ok(json!({
        "semantic_version": RELATION_VERSION,
        "validation_version": "effective-rules-calendar@2",
        "strategy_kind": strategy_kind,
        "benchmark_kind": benchmark_kind,
        "performance": {
            "summary": strategy["summary"]["semantic_version"],
            "calendar": strategy["summary"]["calendar"],
        },
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
    let strategy_kind = strategy_kind(&definition)?.to_owned();
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
    let dsv = definition["dsv"]
        .as_str()
        .ok_or_else(|| error("comparison_definition_invalid", "missing DSV"))?;
    let venues = strategy.venues().iter().cloned().collect::<Vec<_>>();
    let panel = load_panel_for_venues(root, dsv, &venues)
        .map_err(|failure| error("dataset_read_failed", failure))?;
    let universe = StaticUniverse::load(root, strategy.universe())
        .map_err(|failure| error("dataset_read_failed", failure))?;
    let calendar = calendar(
        root,
        &strategy,
        &panel,
        &universe,
        request.reinvestment_calendar,
    )?;
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
    strategy
        .engine
        .validate_comparison_prices(
            &panel
                .window(evaluation.start(), evaluation.end())
                .map_err(|e| error("comparison_window_invalid", e))?,
            &benchmark.holding_benchmark.as_ref().unwrap().price_ticks,
        )
        .map_err(|e| error("comparison_price_rules_mismatch", e))?;
    // The role definitions are the ones persisted/executed: both opt into the
    // complete Summary and calendar report, and only the strategy Summary binds
    // the derived holding benchmark.
    let mut benchmark_role_definition = benchmark_definition.clone();
    benchmark_role_definition["summary"] = performance_binding(None);
    let mut strategy_role_definition = definition;
    strategy_role_definition["summary"] = performance_binding(Some(benchmark_definition));
    let strategy_role =
        Experiment::parse(&strategy_role_definition.to_string(), root).map_err(experiment_error)?;
    let benchmark_role = Experiment::parse(&benchmark_role_definition.to_string(), root)
        .map_err(experiment_error)?;
    let strategy_role_runs = strategy_role
        .run_definitions(root)
        .map_err(experiment_error)?;
    let benchmark_role_runs = benchmark_role
        .run_definitions(root)
        .map_err(experiment_error)?;
    if strategy_role_runs.len() != 1 || benchmark_role_runs.len() != 1 {
        return Err(error(
            "comparison_benchmark_invalid",
            "each comparison role must expand to exactly one Run",
        ));
    }
    let comparability = comparability(
        &strategy_role_definition,
        &benchmark_role_definition,
        &benchmark_role_runs[0].run_spec,
        &strategy_kind,
        benchmark_kind,
        &calendar,
    )?;
    Ok(json!({"schema_version": 1, "data": {
        "semantic_version": SEMANTIC_VERSION,
        "strategy": {
            "role": "strategy", "result_level": "standard", "definition": strategy_role_definition,
            "experiment": strategy_role.id().map_err(experiment_error)?, "run": strategy_role_runs[0].run_id,
        },
        "benchmark": {
            "role": "benchmark", "result_level": "standard", "kind": benchmark_kind,
            "definition": benchmark_role_definition,
            "experiment": benchmark_role.id().map_err(experiment_error)?, "run": benchmark_role_runs[0].run_id,
        },
        "comparability": comparability,
    }}))
}
