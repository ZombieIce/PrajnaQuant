use super::{
    prepare_dataset, run, run_fast_event, run_fast_event_with_targets, sorted_median, sorted_p95,
};
use anyhow::{Context, Result, ensure};
use chrono::NaiveDate;
use pyo3::{prelude::*, types::PyModule};
use serde::Serialize;
use serde_json::Value;
use std::{fs, path::Path, time::Instant};

#[path = "b3_strategies.rs"]
mod strategies;
pub use strategies::run_b3_strategies;

const WARMUP_RUNS: usize = 1;
const MEASUREMENT_RUNS: usize = 5;

#[derive(Clone)]
struct BarEvent {
    date: NaiveDate,
    symbol: String,
    close: f64,
}

#[derive(Serialize)]
struct CallbackMeasurement {
    status: &'static str,
    callbacks_per_run: usize,
    warmup_runs: usize,
    raw_initialization_samples_ns: Vec<u128>,
    initialization_median_ns: Option<u128>,
    raw_samples_ns: Vec<u128>,
    median_ns: Option<u128>,
    p95_ns: Option<u128>,
    raw_end_to_end_samples_ns: Vec<u128>,
    end_to_end_median_ns: Option<u128>,
    end_to_end_p95_ns: Option<u128>,
}

pub fn run_b3(dataset_path: &Path, expected_path: &Path) -> Result<Value> {
    let reference = run(dataset_path, expected_path, "soa")?;
    ensure!(
        reference.correctness_status() == "passed",
        "B3 reference fixture failed its independent correctness checks"
    );
    ensure!(
        reference.b2_fast_event_buy_hold.status == "correctness_passed_and_measured",
        "B3 Rust S1 account reference failed"
    );

    let dataset_bytes = fs::read(dataset_path)?;
    let dataset: super::DatasetInput = serde_json::from_slice(&dataset_bytes)?;
    let prepared = prepare_dataset(dataset)?;
    let mut events = prepared
        .bars
        .iter()
        .map(|bar| BarEvent {
            date: bar.trade_date,
            symbol: bar.symbol.clone(),
            close: bar.close,
        })
        .collect::<Vec<_>>();
    events.sort_by(|left, right| (left.date, &left.symbol).cmp(&(right.date, &right.symbol)));
    ensure!(!events.is_empty(), "B3 event stream is empty");

    let decision_date = prepared.spec.calendar[prepared.spec.calendar.len() - 5];
    let decision_symbol = events
        .iter()
        .filter(|event| event.date == decision_date)
        .map(|event| event.symbol.as_str())
        .max()
        .context("S1 decision session has no bar events")?
        .to_owned();
    let targets = prepared
        .spec
        .instruments
        .iter()
        .map(|instrument| instrument.symbol.clone())
        .collect::<Vec<_>>();

    let (rust_native, _) = measure_rust_s1(&events, decision_date, &decision_symbol, &targets);
    let empty = measure_python(
        &events,
        decision_date,
        &decision_symbol,
        &targets,
        "EmptyStrategy",
    )?;
    let python_s1 = measure_python(
        &events,
        decision_date,
        &decision_symbol,
        &targets,
        "BuyHoldStrategy",
    )?;
    let decision_checks = vec![
        "Rust Native and PyO3 callbacks received the same date/symbol ordered bar stream"
            .to_string(),
        "Rust Native S1 and Python on_bar emitted identical date/symbol/target decision events"
            .to_string(),
        "S1 emitted its decision on the configured bar event".to_string(),
        "Empty callbacks returned no decisions for every event".to_string(),
    ];
    let decisions_match = rust_native == python_s1.decisions;
    let python_decision_matches_expected_event =
        decision_for_event(&python_s1.decisions, decision_date, &decision_symbol)
            .is_ok_and(|decision| decision.targets == targets);
    let rust_decision_matches_expected_event =
        decision_for_event(&rust_native, decision_date, &decision_symbol)
            .is_ok_and(|decision| decision.targets == targets);
    let empty_is_empty = empty.decisions.is_empty();
    let rust_projection = &reference.b2_fast_event_buy_hold.projection;
    let account_checksum = reference.b2_fast_event_buy_hold.checksum_sha256.clone();
    let ledger_invariant_passed = reference.correctness.accounting_checks.is_empty();
    let python_decision =
        decision_for_event(&python_s1.decisions, decision_date, &decision_symbol)?;
    let python_projection = run_fast_event_with_targets(
        &prepared,
        python_decision.date,
        python_decision.targets.clone(),
    )
    .projection;
    let portfolio_projection_matches_rust =
        serde_json::to_value(&python_projection)? == serde_json::to_value(rust_projection)?;
    let correct = decisions_match
        && python_decision_matches_expected_event
        && rust_decision_matches_expected_event
        && empty_is_empty
        && ledger_invariant_passed
        && portfolio_projection_matches_rust;

    let mut empty_raw = Vec::with_capacity(MEASUREMENT_RUNS);
    let mut empty_initialization = Vec::with_capacity(MEASUREMENT_RUNS);
    let mut empty_e2e = Vec::with_capacity(MEASUREMENT_RUNS);
    let mut s1_raw = Vec::with_capacity(MEASUREMENT_RUNS);
    let mut s1_initialization = Vec::with_capacity(MEASUREMENT_RUNS);
    let mut s1_e2e = Vec::with_capacity(MEASUREMENT_RUNS);
    let mut rust_empty_raw = Vec::with_capacity(MEASUREMENT_RUNS);
    let mut rust_empty_e2e = Vec::with_capacity(MEASUREMENT_RUNS);
    let mut rust_s1_raw = Vec::with_capacity(MEASUREMENT_RUNS);
    let mut rust_s1_e2e = Vec::with_capacity(MEASUREMENT_RUNS);
    for _ in 0..WARMUP_RUNS {
        let _ = measure_python(
            &events,
            decision_date,
            &decision_symbol,
            &targets,
            "EmptyStrategy",
        )?;
        let _ = measure_python(
            &events,
            decision_date,
            &decision_symbol,
            &targets,
            "BuyHoldStrategy",
        )?;
        let _ = measure_rust_empty(&events);
        let _ = measure_rust_s1(&events, decision_date, &decision_symbol, &targets);
        let _ = run_fast_event(&prepared);
    }
    if correct {
        for _ in 0..MEASUREMENT_RUNS {
            let empty_started = Instant::now();
            let empty_run = measure_python(
                &events,
                decision_date,
                &decision_symbol,
                &targets,
                "EmptyStrategy",
            )?;
            empty_raw.push(empty_run.callback_ns);
            empty_initialization.push(empty_run.initialization_ns);
            let _rust_account = run_fast_event_with_targets(&prepared, decision_date, Vec::new());
            empty_e2e.push(empty_started.elapsed().as_nanos());

            let s1_started = Instant::now();
            let s1_run = measure_python(
                &events,
                decision_date,
                &decision_symbol,
                &targets,
                "BuyHoldStrategy",
            )?;
            s1_raw.push(s1_run.callback_ns);
            s1_initialization.push(s1_run.initialization_ns);
            let emitted_decision =
                decision_for_event(&s1_run.decisions, decision_date, &decision_symbol)?;
            let _rust_account = run_fast_event_with_targets(
                &prepared,
                emitted_decision.date,
                emitted_decision.targets.clone(),
            );
            s1_e2e.push(s1_started.elapsed().as_nanos());

            let rust_empty_started = Instant::now();
            rust_empty_raw.push(measure_rust_empty(&events));
            let _rust_account = run_fast_event_with_targets(&prepared, decision_date, Vec::new());
            rust_empty_e2e.push(rust_empty_started.elapsed().as_nanos());
            let rust_started = Instant::now();
            let (native_decisions, rust_callback_ns) =
                measure_rust_s1(&events, decision_date, &decision_symbol, &targets);
            rust_s1_raw.push(rust_callback_ns);
            let native_decision =
                decision_for_event(&native_decisions, decision_date, &decision_symbol)?;
            let _rust_account = run_fast_event_with_targets(
                &prepared,
                native_decision.date,
                native_decision.targets.clone(),
            );
            rust_s1_e2e.push(rust_started.elapsed().as_nanos());
        }
    }

    let python_version = Python::attach(|py| -> PyResult<String> {
        py.import("sys")?.getattr("version")?.extract()
    })
    .map_err(|error| anyhow::anyhow!("read embedded Python version: {error}"))?;

    Ok(serde_json::json!({
        "schema_version": "poc0.b3.report.v1",
        "status": if correct { "correctness_passed_and_measured" } else { "correctness_failed_timing_skipped" },
        "engine": "Rust Native + embedded CPython through PyO3",
        "pyo3_version": "0.29.0",
        "python_version": python_version,
        "dataset_version": reference.dataset.version,
        "dataset_content_sha256": reference.dataset.content_sha256,
        "provenance": reference.provenance,
        "event_count_per_run": events.len(),
        "event_order": "ascending trade date, then ascending symbol; missing bars emit no callback",
        "decision_date": decision_date,
        "decision_symbol": decision_symbol,
        "targets": targets,
        "decision_events": rust_native,
        "correctness_checks": decision_checks,
        "correctness": {
            "rust_native_python_s1_decisions_match": decisions_match,
            "python_s1_decision_matches_expected_event": python_decision_matches_expected_event,
            "rust_s1_decision_matches_expected_event": rust_decision_matches_expected_event,
            "empty_callbacks_emit_no_decisions": empty_is_empty,
            "rust_accounting_nav_invariant_passed": ledger_invariant_passed,
            "python_decision_account_projection_matches_rust": portfolio_projection_matches_rust,
            "account_projection_checksum_sha256": account_checksum,
        },
        "portfolio_result": python_projection,
        "measurements": {
            "warmup_runs": WARMUP_RUNS,
            "requested_runs": MEASUREMENT_RUNS,
            "python_empty_callback": callback_measurement(&empty_raw, &empty_initialization, &empty_e2e, events.len()),
            "python_s1_on_bar": callback_measurement(&s1_raw, &s1_initialization, &s1_e2e, events.len()),
            "rust_native_empty_callback": callback_measurement(&rust_empty_raw, &[], &rust_empty_e2e, events.len()),
            "rust_native_s1_callback": callback_measurement(&rust_s1_raw, &[], &rust_s1_e2e, events.len()),
            "timing_scope": "per-run callback loop and callback-plus-Rust-account end-to-end; fixture parsing and report serialization excluded",
            "python_strategy_scope": "Python receives only date/symbol/close and returns the S1 target decision; Rust Fast Event interprets orders, fills, costs, marks, cash, positions, and portfolio result",
            "initialization_scope": "per-run embedded module and strategy object creation; the embedded interpreter itself is initialized before warmup",
            "end_to_end_scope": "Python/Rust empty include their callback loop plus a no-trade Rust account run; S1 includes the callback loop plus Buy & Hold Rust Fast Event accounting; Python per-run module/object setup is included in Python end-to-end and reported separately",
        },
        "conclusion": "unresolved: fixed 3 ETF x 10 session fixture validates callback parity and measures this tiny workload only; it does not establish a scale crossover or parallel/GIL conclusion"
    }))
}

struct PythonRun {
    decisions: Vec<StrategyDecision>,
    initialization_ns: u128,
    callback_ns: u128,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct StrategyDecision {
    date: NaiveDate,
    symbol: String,
    targets: Vec<String>,
}

fn decision_for_event<'a>(
    decisions: &'a [StrategyDecision],
    date: NaiveDate,
    symbol: &str,
) -> Result<&'a StrategyDecision> {
    decisions
        .iter()
        .find(|decision| decision.date == date && decision.symbol == symbol)
        .context("S1 callback produced no target on the configured date and symbol event")
}

fn measure_python(
    events: &[BarEvent],
    decision_date: NaiveDate,
    decision_symbol: &str,
    targets: &[String],
    class_name: &str,
) -> Result<PythonRun> {
    let (strategy, initialization_ns) = Python::attach(|py| -> PyResult<_> {
        let initialization_started = Instant::now();
        let module = PyModule::from_code(
            py,
            c"class EmptyStrategy:\n    def on_bar(self, date, symbol, close):\n        return None\n\nclass BuyHoldStrategy:\n    def __init__(self, decision_date, decision_symbol, targets):\n        self.decision_date = decision_date\n        self.decision_symbol = decision_symbol\n        self.targets = targets\n    def on_bar(self, date, symbol, close):\n        if date == self.decision_date and symbol == self.decision_symbol:\n            return self.targets\n        return None\n",
            c"poc0_b3_callback.py",
            c"poc0_b3_callback",
        )?;
        let decision_date = decision_date.to_string();
        let decision_symbol = decision_symbol.to_owned();
        let targets = targets.to_vec();
        let strategy_class = module.getattr(class_name)?;
        let strategy = if class_name == "BuyHoldStrategy" {
            strategy_class.call1((decision_date.as_str(), decision_symbol.as_str(), targets))?
        } else {
            strategy_class.call0()?
        };
        let initialization_ns = initialization_started.elapsed().as_nanos();
        Ok((strategy.unbind(), initialization_ns))
    })
    .map_err(|error| anyhow::anyhow!("initialize PyO3 callback: {error}"))?;

    let started = Instant::now();
    let mut decisions = Vec::new();
    for event in events {
        let date = event.date.to_string();
        let action = Python::attach(|py| -> PyResult<Option<Vec<String>>> {
            let result = strategy.bind(py).call_method1(
                "on_bar",
                (date.as_str(), event.symbol.as_str(), event.close),
            )?;
            result.extract::<Option<Vec<String>>>()
        })
        .map_err(|error| anyhow::anyhow!("PyO3 on_bar callback failed: {error}"))?;
        if let Some(action) = action {
            decisions.push(StrategyDecision {
                date: event.date,
                symbol: event.symbol.clone(),
                targets: action,
            });
        }
    }
    Ok(PythonRun {
        decisions,
        initialization_ns,
        callback_ns: started.elapsed().as_nanos(),
    })
}

fn measure_rust_s1(
    events: &[BarEvent],
    decision_date: NaiveDate,
    decision_symbol: &str,
    targets: &[String],
) -> (Vec<StrategyDecision>, u128) {
    let started = Instant::now();
    let mut decisions = Vec::new();
    for event in events {
        if event.date == decision_date && event.symbol == decision_symbol {
            decisions.push(StrategyDecision {
                date: event.date,
                symbol: event.symbol.clone(),
                targets: targets.to_vec(),
            });
        }
    }
    (decisions, started.elapsed().as_nanos())
}

fn measure_rust_empty(events: &[BarEvent]) -> u128 {
    let started = Instant::now();
    for event in events {
        std::hint::black_box((&event.date, &event.symbol, event.close));
    }
    started.elapsed().as_nanos()
}

fn callback_measurement(
    raw: &[u128],
    initialization: &[u128],
    end_to_end: &[u128],
    events: usize,
) -> CallbackMeasurement {
    CallbackMeasurement {
        status: if raw.len() == MEASUREMENT_RUNS {
            "measured"
        } else {
            "skipped_due_to_correctness_failure"
        },
        callbacks_per_run: events,
        warmup_runs: if raw.is_empty() { 0 } else { WARMUP_RUNS },
        raw_initialization_samples_ns: initialization.to_vec(),
        initialization_median_ns: sorted_median(initialization),
        raw_samples_ns: raw.to_vec(),
        median_ns: sorted_median(raw),
        p95_ns: sorted_p95(raw),
        raw_end_to_end_samples_ns: end_to_end.to_vec(),
        end_to_end_median_ns: sorted_median(end_to_end),
        end_to_end_p95_ns: sorted_p95(end_to_end),
    }
}

#[cfg(test)]
mod tests {
    use super::{StrategyDecision, decision_for_event};
    use chrono::NaiveDate;

    #[test]
    fn decision_on_another_bar_of_the_same_date_does_not_match() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 28).expect("valid date");
        let decisions = [StrategyDecision {
            date,
            symbol: "ETF_B".to_string(),
            targets: vec!["ETF_A".to_string()],
        }];

        assert!(decision_for_event(&decisions, date, "ETF_A").is_err());
        assert!(decision_for_event(&decisions, date, "ETF_B").is_ok());
    }
}
