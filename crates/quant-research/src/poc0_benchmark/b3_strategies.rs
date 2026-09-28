use super::super::{
    B1SoaProjection, DatasetInput, PreparedDataset, SignalProjection, make_ma_dataset,
    prepare_dataset, replay_event_targets, run, sorted_median, sorted_p95,
};
use super::{BarEvent, StrategyDecision};
use anyhow::{Context, Result, ensure};
use chrono::{Datelike, NaiveDate};
use pyo3::{prelude::*, types::PyModule};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    time::Instant,
};

const WARMUP_RUNS: usize = 1;
const MEASUREMENT_RUNS: usize = 5;
const TARGET_RUNS: usize = 6;
const TARGET_WORKERS: usize = 2;
const TARGET_INSTRUMENTS: usize = 64;
const TARGET_SESSIONS: usize = 252;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StrategyKind {
    Empty,
    S2MomentumRotation,
    S3Ma20_60,
}

impl StrategyKind {
    fn key(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::S2MomentumRotation => "s2",
            Self::S3Ma20_60 => "s3",
        }
    }

    fn python_class(self) -> &'static str {
        match self {
            Self::Empty => "EmptyStrategy",
            Self::S2MomentumRotation => "MomentumRotation",
            Self::S3Ma20_60 => "Ma20_60",
        }
    }
}

#[derive(Clone, Copy)]
struct S2Parameters {
    short_days: usize,
    long_days: usize,
    volatility_window: usize,
    short_weight: f64,
    long_weight: f64,
    volatility_weight: f64,
    top_n: usize,
    rebalance_every: usize,
}

impl From<&DatasetInput> for S2Parameters {
    fn from(input: &DatasetInput) -> Self {
        Self {
            short_days: input.strategy.momentum_short_days,
            long_days: input.strategy.momentum_long_days,
            volatility_window: input.strategy.volatility_window,
            short_weight: input.strategy.short_momentum_weight,
            long_weight: input.strategy.long_momentum_weight,
            volatility_weight: input.strategy.volatility_weight,
            top_n: input.strategy.top_n,
            rebalance_every: input.strategy.rebalance_every.max(1),
        }
    }
}

struct StrategyRun {
    decisions: Vec<StrategyDecision>,
    initialization_ns: u128,
    callback_ns: u128,
}

pub fn run_b3_strategies() -> Result<Value> {
    let dataset_path = Path::new("poc/poc0-benchmark/fixtures/dataset-v1.json");
    let expected_path = Path::new("poc/poc0-benchmark/fixtures/expected-v1.json");
    let base_bytes = fs::read(dataset_path)?;
    let base_spec: DatasetInput = serde_json::from_slice(&base_bytes)?;
    let base = prepare_dataset(base_spec)?;
    let baseline_report = run(dataset_path, expected_path, "soa")?;
    ensure!(
        baseline_report.correctness_status() == "passed",
        "B3 S2/S3 reference fixture failed independent correctness checks"
    );
    ensure!(
        baseline_report.b2_fast_event_momentum_rotation.status == "correctness_passed_and_measured"
            && baseline_report.b2_fast_event_ma20_60.status == "correctness_passed_and_measured",
        "B3 S2/S3 reference event accounting failed its golden checks"
    );
    let (ma_base, _) = make_ma_dataset(&base.spec)?;
    let s2_stress = make_s2_stress_dataset(&base.spec, TARGET_INSTRUMENTS, TARGET_SESSIONS)?;
    let s3_stress = make_s3_stress_dataset(&ma_base, TARGET_INSTRUMENTS, TARGET_SESSIONS)?;

    let mut strategy_reports = Vec::new();
    for (kind, small, stress, golden_signals) in [
        (
            StrategyKind::S2MomentumRotation,
            &base,
            &s2_stress,
            baseline_report
                .b2_fast_event_momentum_rotation
                .signals
                .clone(),
        ),
        (
            StrategyKind::S3Ma20_60,
            &ma_base,
            &s3_stress,
            baseline_report.b2_fast_event_ma20_60.signals.clone(),
        ),
    ] {
        let small_events = ordered_events(small);
        let stress_events = ordered_events(stress);
        let expected_small = decisions_from_signals(kind, &small_events, &golden_signals)?;
        let native_small = run_native(kind, small, &small_events)?;
        let python_small = run_python(kind, small, &small_events, false)?;
        let python_batch_small = run_python(kind, small, &small_events, true)?;
        let native_small_projection = account_projection(small, &native_small.decisions)?;
        let expected_projection = if matches!(kind, StrategyKind::S2MomentumRotation) {
            serde_json::to_value(&baseline_report.b2_fast_event_momentum_rotation.projection)?
        } else {
            serde_json::to_value(&baseline_report.b2_fast_event_ma20_60.projection)?
        };
        let small_correct = native_small.decisions == expected_small
            && python_small.decisions == expected_small
            && python_batch_small.decisions == expected_small
            && serde_json::to_value(&native_small_projection)? == expected_projection
            && serde_json::to_value(account_projection(small, &python_small.decisions)?)?
                == expected_projection
            && serde_json::to_value(account_projection(small, &python_batch_small.decisions)?)?
                == expected_projection;
        ensure!(
            small_correct,
            "{} callback decisions or ledger differ from independent B2 golden",
            kind.key()
        );

        let expected_stress = run_native(kind, stress, &stress_events)?;
        let stress_projection = account_projection(stress, &expected_stress.decisions)?;
        let stress_checksum = sha256(&serde_json::to_vec(&stress_projection)?);
        let stress_python = run_python(kind, stress, &stress_events, false)?;
        let stress_batch = run_python(kind, stress, &stress_events, true)?;
        let stress_correct = stress_python.decisions == expected_stress.decisions
            && stress_batch.decisions == expected_stress.decisions
            && sha256(&serde_json::to_vec(&account_projection(
                stress,
                &stress_python.decisions,
            )?)?)
                == stress_checksum
            && sha256(&serde_json::to_vec(&account_projection(
                stress,
                &stress_batch.decisions,
            )?)?)
                == stress_checksum;
        ensure!(
            stress_correct,
            "{} stress workload callback or account parity failed",
            kind.key()
        );

        let measurements = measure_candidates(kind, stress, &stress_events, &stress_checksum)?;
        let empty_python_median = measurements["python_empty"]["single_run"]["callback_median_ns"]
            .as_u64()
            .context("Python empty callback median missing")?;
        let empty_rust_median = measurements["rust_empty"]["single_run"]["callback_median_ns"]
            .as_u64()
            .context("Rust empty callback median missing")?;
        strategy_reports.push(json!({
            "strategy": kind,
            "correctness": {
                "status": "passed",
                "small_fixture_matches_independent_golden": small_correct,
                "stress_python_on_bar_matches_rust_native": stress_python.decisions == expected_stress.decisions,
                "stress_python_batch_matches_rust_native": stress_batch.decisions == expected_stress.decisions,
                "account_projection_matches": true,
                "small_fixture_event_count": small_events.len(),
                "stress_event_count": stress_events.len(),
                "small_fixture_checksum_sha256": sha256(&serde_json::to_vec(&native_small_projection)?),
                "stress_checksum_sha256": stress_checksum,
            },
            "small_fixture": {
                "dataset_version": small.spec.dataset_version,
                "dataset_content_sha256": small.content_sha256,
                "signals": expected_small,
            },
            "target_workload": {
                "dataset_version": stress.spec.dataset_version,
                "dataset_content_sha256": stress.content_sha256,
                "seed": stress.spec.seed,
                "instruments": stress.spec.instruments.len(),
                "sessions": stress.spec.calendar.len(),
                "event_count": stress_events.len(),
                "strategy_parameters": serde_json::to_value(&stress.spec.strategy)?,
                "signals": expected_stress.decisions,
                "empty_callback_boundary_delta_median_ns": empty_python_median as i128 - empty_rust_median as i128,
                "measurements": measurements,
            }
        }));
    }

    let peak_rss = process_peak_rss_bytes()?;
    let python_version = Python::attach(|py| -> PyResult<String> {
        py.import("sys")?.getattr("version")?.extract()
    })
    .map_err(|error| anyhow::anyhow!("read embedded Python version: {error}"))?;
    let per_bar_eligible = strategy_reports.iter().all(|report| {
        let candidates = &report["target_workload"]["measurements"];
        let rust = &candidates["rust_native"]["parallel_runs"];
        let python = &candidates["python_on_bar"]["parallel_runs"];
        let rust_median = rust["callback_median_ns"].as_u64().unwrap_or(0) as f64;
        let python_median = python["callback_median_ns"].as_u64().unwrap_or(u64::MAX) as f64;
        let rust_rps = rust["runs_per_second_median"].as_f64().unwrap_or(0.0);
        let python_rps = python["runs_per_second_median"].as_f64().unwrap_or(0.0);
        rust_median > 0.0 && python_median / rust_median <= 2.0 && python_rps >= rust_rps * 0.8
    });
    let batch_eligible = strategy_reports.iter().all(|report| {
        let candidates = &report["target_workload"]["measurements"];
        let rust = &candidates["rust_native"]["parallel_runs"];
        let batch = &candidates["python_batch"]["parallel_runs"];
        let rust_median = rust["callback_median_ns"].as_u64().unwrap_or(0) as f64;
        let batch_median = batch["callback_median_ns"].as_u64().unwrap_or(u64::MAX) as f64;
        let rust_rps = rust["runs_per_second_median"].as_f64().unwrap_or(0.0);
        let batch_rps = batch["runs_per_second_median"].as_f64().unwrap_or(0.0);
        rust_median > 0.0 && batch_median / rust_median <= 2.0 && batch_rps >= rust_rps * 0.8
    });
    let conclusion = if per_bar_eligible {
        "adopt: per-bar Python met the pre-registered target workload latency and throughput gates"
    } else if batch_eligible {
        "defer: per-bar Python missed the target gate; batched Python met it"
    } else {
        "reject: Python candidates missed the pre-registered target workload latency or throughput gates"
    };
    Ok(json!({
        "schema_version": "poc0.b3.strategies.report.v1",
        "status": "correctness_passed_and_measured",
        "engine": "Rust Native + embedded CPython through PyO3",
        "pyo3_version": "0.29.0",
        "python_version": python_version,
        "provenance": super::super::provenance_report(),
        "pre_registered_protocol": {
            "target_workload": format!("{} instruments x {} sessions", TARGET_INSTRUMENTS, TARGET_SESSIONS),
            "warmup_runs": WARMUP_RUNS,
            "single_run_repeats": MEASUREMENT_RUNS,
            "parallel_runs": TARGET_RUNS,
            "parallel_workers": TARGET_WORKERS,
            "acceptance_gate": "per candidate and both S2/S3: parallel callback p95 is reported; callback median no more than 2x Rust Native and parallel Runs/s at least 80% of Rust Native; correctness is mandatory; peak RSS is reported as process high-water but is not a candidate gate because the embedded-interpreter process is shared",
            "decision_method": "adopt per-bar if its gate passes for both strategies; otherwise defer if batch passes both; otherwise reject for this target workload",
        },
        "peak_rss_bytes": peak_rss,
        "peak_rss_scope": "whole benchmark process high-water, including embedded Python and all sequential candidates; not candidate-isolated",
        "parallelism_scope": "independent Runs on two native threads; Python worker threads re-enter CPython per callback and contend on the GIL; batch candidate enters Python once per Run",
        "strategies": strategy_reports,
        "conclusion": conclusion,
        "conclusion_scope": "64x252 synthetic target workload on this host only; does not establish a universal workload crossover or production suitability",
    }))
}

fn ordered_events(dataset: &PreparedDataset) -> Vec<BarEvent> {
    let mut events = dataset
        .bars
        .iter()
        .map(|bar| BarEvent {
            date: bar.trade_date,
            symbol: bar.symbol.clone(),
            close: bar.close,
        })
        .collect::<Vec<_>>();
    events.sort_by(|left, right| (left.date, &left.symbol).cmp(&(right.date, &right.symbol)));
    events
}

fn decisions_from_signals(
    kind: StrategyKind,
    events: &[BarEvent],
    signals: &[SignalProjection],
) -> Result<Vec<StrategyDecision>> {
    signals
        .iter()
        .map(|signal| {
            let event = match kind {
                StrategyKind::Empty => None,
                StrategyKind::S2MomentumRotation => events
                    .iter()
                    .filter(|event| event.date == signal.date)
                    .max_by(|left, right| left.symbol.cmp(&right.symbol)),
                StrategyKind::S3Ma20_60 => events
                    .iter()
                    .find(|event| event.date == signal.date && event.symbol == "A"),
            }
            .with_context(|| format!("no {} decision event on {}", kind.key(), signal.date))?;
            Ok(StrategyDecision {
                date: signal.date,
                symbol: event.symbol.clone(),
                targets: signal.target_symbols.clone(),
            })
        })
        .collect()
}

fn run_native(
    kind: StrategyKind,
    dataset: &PreparedDataset,
    events: &[BarEvent],
) -> Result<StrategyRun> {
    let initialization_started = Instant::now();
    let (parameters, symbols, last_symbols) = if matches!(kind, StrategyKind::S2MomentumRotation) {
        let mut symbols = dataset
            .spec
            .instruments
            .iter()
            .map(|instrument| instrument.symbol.clone())
            .collect::<Vec<_>>();
        symbols.sort();
        (
            S2Parameters::from(&dataset.spec),
            symbols,
            last_symbol_by_date(events),
        )
    } else {
        (
            S2Parameters::from(&dataset.spec),
            Vec::new(),
            BTreeMap::new(),
        )
    };
    let initialization_ns = initialization_started.elapsed().as_nanos();
    let started = Instant::now();
    let decisions = match kind {
        StrategyKind::Empty => {
            for event in events {
                std::hint::black_box((&event.date, &event.symbol, event.close));
            }
            Vec::new()
        }
        StrategyKind::S2MomentumRotation => native_s2(events, parameters, &symbols, &last_symbols),
        StrategyKind::S3Ma20_60 => native_s3(events, &dataset.spec.calendar),
    };
    Ok(StrategyRun {
        decisions,
        initialization_ns,
        callback_ns: started.elapsed().as_nanos(),
    })
}

fn native_s2(
    events: &[BarEvent],
    parameters: S2Parameters,
    symbols: &[String],
    last_symbols: &BTreeMap<NaiveDate, String>,
) -> Vec<StrategyDecision> {
    let mut histories: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut seen_today = BTreeSet::new();
    let mut active_date = None;
    let mut eligible_dates = 0;
    let mut decisions = Vec::new();
    for event in events {
        if active_date != Some(event.date) {
            active_date = Some(event.date);
            seen_today.clear();
        }
        histories
            .entry(event.symbol.clone())
            .or_default()
            .push(event.close);
        seen_today.insert(event.symbol.clone());
        if last_symbols.get(&event.date) != Some(&event.symbol) {
            continue;
        }
        let mut ranked = symbols
            .iter()
            .filter(|symbol| seen_today.contains(*symbol))
            .filter_map(|symbol| {
                histories
                    .get(symbol)
                    .and_then(|history| s2_score(history, parameters))
                    .map(|score| (symbol.clone(), score))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        if ranked.is_empty() {
            continue;
        }
        let should_rebalance = eligible_dates % parameters.rebalance_every == 0;
        eligible_dates += 1;
        if should_rebalance {
            decisions.push(StrategyDecision {
                date: event.date,
                symbol: event.symbol.clone(),
                targets: ranked
                    .iter()
                    .take(parameters.top_n)
                    .map(|(symbol, _)| symbol.clone())
                    .collect(),
            });
        }
    }
    decisions
}

fn s2_score(history: &[f64], parameters: S2Parameters) -> Option<f64> {
    let required = parameters
        .short_days
        .max(parameters.long_days)
        .max(parameters.volatility_window);
    if parameters.volatility_window <= 1 || history.len() <= required {
        return None;
    }
    let now = history[history.len() - 1];
    let short = now / history[history.len() - 1 - parameters.short_days] - 1.0;
    let long = now / history[history.len() - 1 - parameters.long_days] - 1.0;
    let returns = (history.len() - parameters.volatility_window..history.len())
        .map(|index| history[index] / history[index - 1] - 1.0)
        .collect::<Vec<_>>();
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (returns.len() - 1) as f64;
    let score = parameters.short_weight * short + parameters.long_weight * long
        - parameters.volatility_weight * variance.sqrt();
    score.is_finite().then_some(score)
}

fn native_s3(events: &[BarEvent], calendar: &[NaiveDate]) -> Vec<StrategyDecision> {
    let mut closes = Vec::new();
    let mut decisions = Vec::new();
    let mut held = false;
    let mut previous_gap: Option<f64> = None;
    for event in events.iter().filter(|event| event.symbol == "A") {
        closes.push(event.close);
        let index = calendar
            .binary_search(&event.date)
            .expect("bar date belongs to calendar");
        if index < 59 || closes.len() < 60 {
            continue;
        }
        let short = closes[closes.len() - 20..].iter().sum::<f64>() / 20.0;
        let long = closes[closes.len() - 60..].iter().sum::<f64>() / 60.0;
        let gap = short - long;
        if let Some(previous) = previous_gap {
            if !held && previous <= 0.0 && gap > 0.0 {
                held = true;
                decisions.push(StrategyDecision {
                    date: event.date,
                    symbol: event.symbol.clone(),
                    targets: vec!["A".to_string()],
                });
            } else if held && previous >= 0.0 && gap < 0.0 {
                held = false;
                decisions.push(StrategyDecision {
                    date: event.date,
                    symbol: event.symbol.clone(),
                    targets: Vec::new(),
                });
            }
        }
        previous_gap = Some(gap);
    }
    decisions
}

fn run_python(
    kind: StrategyKind,
    dataset: &PreparedDataset,
    events: &[BarEvent],
    batch: bool,
) -> Result<StrategyRun> {
    let parameters = S2Parameters::from(&dataset.spec);
    let (symbols, last_symbols) = if matches!(kind, StrategyKind::S2MomentumRotation) {
        let symbols = dataset
            .spec
            .instruments
            .iter()
            .map(|instrument| instrument.symbol.clone())
            .collect::<Vec<_>>();
        let last_symbols = last_symbol_by_date(events)
            .into_iter()
            .map(|(date, symbol)| (date.to_string(), symbol))
            .collect::<BTreeMap<_, _>>();
        (symbols, last_symbols)
    } else {
        (Vec::new(), BTreeMap::new())
    };
    let (strategy, module, initialization_ns) = Python::attach(|py| -> PyResult<_> {
        let started = Instant::now();
        let source = std::ffi::CString::new(STRATEGY_MODULE).expect("valid Python source");
        let module =
            PyModule::from_code(py, &source, c"poc0_b3_strategies.py", c"poc0_b3_strategies")?;
        let class = module.getattr(kind.python_class())?;
        let strategy = match kind {
            StrategyKind::Empty | StrategyKind::S3Ma20_60 => class.call0()?,
            StrategyKind::S2MomentumRotation => class.call1((
                symbols,
                last_symbols,
                parameters.short_days,
                parameters.long_days,
                parameters.volatility_window,
                parameters.short_weight,
                parameters.long_weight,
                parameters.volatility_weight,
                parameters.top_n,
                parameters.rebalance_every,
            ))?,
        };
        Ok((
            strategy.unbind(),
            module.unbind(),
            started.elapsed().as_nanos(),
        ))
    })
    .map_err(|error| anyhow::anyhow!("initialize Python {} strategy: {error}", kind.key()))?;
    let started = Instant::now();
    let decisions = if batch {
        let batch_events = events
            .iter()
            .map(|event| (event.date.to_string(), event.symbol.clone(), event.close))
            .collect::<Vec<_>>();
        Python::attach(|py| -> PyResult<Vec<StrategyDecision>> {
            let rows = module
                .bind(py)
                .getattr("run_batch")?
                .call1((strategy.bind(py), batch_events))?
                .extract::<Vec<(String, String, Vec<String>)>>()?;
            rows.into_iter()
                .map(|(date, symbol, targets)| {
                    Ok(StrategyDecision {
                        date: NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(|error| {
                            pyo3::exceptions::PyValueError::new_err(error.to_string())
                        })?,
                        symbol,
                        targets,
                    })
                })
                .collect()
        })
        .map_err(|error| anyhow::anyhow!("Python batch strategy failed: {error}"))?
    } else {
        let mut decisions = Vec::new();
        for event in events {
            let date = event.date.to_string();
            let targets = Python::attach(|py| -> PyResult<Option<Vec<String>>> {
                strategy
                    .bind(py)
                    .call_method1(
                        "on_bar",
                        (date.as_str(), event.symbol.as_str(), event.close),
                    )?
                    .extract()
            })
            .map_err(|error| anyhow::anyhow!("Python {} on_bar failed: {error}", kind.key()))?;
            if let Some(targets) = targets {
                decisions.push(StrategyDecision {
                    date: event.date,
                    symbol: event.symbol.clone(),
                    targets,
                });
            }
        }
        decisions
    };
    Ok(StrategyRun {
        decisions,
        initialization_ns,
        callback_ns: started.elapsed().as_nanos(),
    })
}

fn account_projection(
    dataset: &PreparedDataset,
    decisions: &[StrategyDecision],
) -> Result<super::super::FastEventProjection> {
    let signals = decisions
        .iter()
        .map(|decision| SignalProjection {
            date: decision.date,
            target_symbols: decision.targets.clone(),
        })
        .collect();
    Ok(replay_event_targets(
        dataset,
        B1SoaProjection {
            rankings: Vec::new(),
            targets: signals,
            target_weights: Vec::new(),
            portfolio_returns: Vec::new(),
        },
    )
    .projection)
}

fn measure_candidates(
    kind: StrategyKind,
    dataset: &PreparedDataset,
    events: &[BarEvent],
    expected_checksum: &str,
) -> Result<Value> {
    let mut candidates = serde_json::Map::new();
    for (name, candidate_kind, batch) in [
        ("rust_empty", StrategyKind::Empty, None),
        ("python_empty", StrategyKind::Empty, Some(false)),
        ("rust_native", kind, None),
        ("python_on_bar", kind, Some(false)),
        ("python_batch", kind, Some(true)),
    ] {
        let candidate_checksum = if matches!(candidate_kind, StrategyKind::Empty) {
            sha256(&serde_json::to_vec(&account_projection(dataset, &[])?)?)
        } else {
            expected_checksum.to_string()
        };
        let run_once = || -> Result<StrategyRun> {
            match batch {
                None => run_native(candidate_kind, dataset, events),
                Some(use_batch) => run_python(candidate_kind, dataset, events, use_batch),
            }
        };
        for _ in 0..WARMUP_RUNS {
            let warm = run_once()?;
            let checksum = sha256(&serde_json::to_vec(&account_projection(
                dataset,
                &warm.decisions,
            )?)?);
            ensure!(
                checksum == candidate_checksum,
                "{name} warmup account mismatch"
            );
        }
        let mut callback = Vec::with_capacity(MEASUREMENT_RUNS);
        let mut initialization = Vec::with_capacity(MEASUREMENT_RUNS);
        let mut account_samples = Vec::with_capacity(MEASUREMENT_RUNS);
        let mut end_to_end = Vec::with_capacity(MEASUREMENT_RUNS);
        for _ in 0..MEASUREMENT_RUNS {
            let started = Instant::now();
            let measured = run_once()?;
            callback.push(measured.callback_ns);
            initialization.push(measured.initialization_ns);
            let account_started = Instant::now();
            let projection = account_projection(dataset, &measured.decisions)?;
            account_samples.push(account_started.elapsed().as_nanos());
            ensure!(
                sha256(&serde_json::to_vec(&projection)?) == candidate_checksum,
                "{name} measured account mismatch"
            );
            end_to_end.push(started.elapsed().as_nanos());
        }
        for _ in 0..WARMUP_RUNS {
            std::thread::scope(|scope| -> Result<()> {
                let workers = (0..TARGET_WORKERS)
                    .map(|_| {
                        let run_once = &run_once;
                        let candidate_checksum = candidate_checksum.clone();
                        scope.spawn(move || -> Result<()> {
                            let warm = run_once()?;
                            let checksum = sha256(&serde_json::to_vec(&account_projection(
                                dataset,
                                &warm.decisions,
                            )?)?);
                            ensure!(
                                checksum == candidate_checksum,
                                "{name} parallel warmup account mismatch"
                            );
                            Ok(())
                        })
                    })
                    .collect::<Vec<_>>();
                for worker in workers {
                    worker
                        .join()
                        .map_err(|_| anyhow::anyhow!("B3 parallel warmup worker panicked"))??;
                }
                Ok(())
            })?;
        }
        let mut parallel_raw = Vec::with_capacity(MEASUREMENT_RUNS);
        let mut parallel_initialization = Vec::with_capacity(MEASUREMENT_RUNS);
        let mut parallel_account = Vec::with_capacity(MEASUREMENT_RUNS);
        let mut parallel_walls = Vec::with_capacity(MEASUREMENT_RUNS);
        for _ in 0..MEASUREMENT_RUNS {
            let started = Instant::now();
            let raw = std::thread::scope(|scope| {
                let workers = (0..TARGET_WORKERS)
                    .map(|worker| {
                        let run_once = &run_once;
                        let candidate_checksum = candidate_checksum.clone();
                        scope.spawn(move || -> Result<Vec<(u128, u128, u128)>> {
                            let mut samples = Vec::new();
                            for _ in (worker..TARGET_RUNS).step_by(TARGET_WORKERS) {
                                let measured = run_once()?;
                                let account_started = Instant::now();
                                let projection = account_projection(dataset, &measured.decisions)?;
                                let account_ns = account_started.elapsed().as_nanos();
                                ensure!(
                                    sha256(&serde_json::to_vec(&projection)?) == candidate_checksum,
                                    "{name} parallel account mismatch"
                                );
                                samples.push((
                                    measured.initialization_ns,
                                    measured.callback_ns,
                                    account_ns,
                                ));
                            }
                            Ok(samples)
                        })
                    })
                    .collect::<Vec<_>>();
                workers.into_iter().try_fold(Vec::new(), |mut all, worker| {
                    let mut samples = worker
                        .join()
                        .map_err(|_| anyhow::anyhow!("B3 worker panicked"))??;
                    all.append(&mut samples);
                    Ok::<_, anyhow::Error>(all)
                })
            })?;
            parallel_initialization.extend(
                raw.iter()
                    .map(|(initialization_ns, _, _)| *initialization_ns),
            );
            parallel_raw.extend(raw.iter().map(|(_, callback_ns, _)| *callback_ns));
            parallel_account.extend(raw.iter().map(|(_, _, account_ns)| *account_ns));
            parallel_walls.push(started.elapsed().as_nanos());
        }
        let parallel_wall_median_ns = sorted_median(&parallel_walls);
        let single_callback_median_ns = sorted_median(&callback);
        let parallel_callback_median_ns = sorted_median(&parallel_raw);
        let single_end_to_end_median_ns = sorted_median(&end_to_end);
        let candidate = json!({
            "single_run": {
                "warmup_runs": WARMUP_RUNS,
                "raw_callback_samples_ns": callback,
                "callback_median_ns": sorted_median(&callback),
                "callback_p95_ns": sorted_p95(&callback),
                "raw_initialization_samples_ns": initialization,
                "initialization_median_ns": sorted_median(&initialization),
                "raw_account_samples_ns": account_samples,
                "account_median_ns": sorted_median(&account_samples),
                "account_p95_ns": sorted_p95(&account_samples),
                "raw_end_to_end_samples_ns": end_to_end,
                "end_to_end_median_ns": sorted_median(&end_to_end),
                "end_to_end_p95_ns": sorted_p95(&end_to_end),
            },
            "parallel_runs": {
                "workers": TARGET_WORKERS,
                "requested_runs": TARGET_RUNS,
                "warmup_runs": WARMUP_RUNS * TARGET_WORKERS,
                "raw_initialization_samples_ns": parallel_initialization,
                "raw_callback_samples_ns": parallel_raw,
                "callback_median_ns": parallel_callback_median_ns,
                "callback_p95_ns": sorted_p95(&parallel_raw),
                "raw_account_samples_ns": parallel_account,
                "account_median_ns": sorted_median(&parallel_account),
                "raw_parallel_wall_samples_ns": parallel_walls,
                "parallel_wall_median_ns": parallel_wall_median_ns,
                "runs_per_second_median": parallel_wall_median_ns.map(|ns| TARGET_RUNS as f64 * 1e9 / ns as f64),
                "callback_median_vs_single_ratio": match (single_callback_median_ns, parallel_callback_median_ns) { (Some(single), Some(parallel)) if single > 0 => Some(parallel as f64 / single as f64), _ => None },
                "throughput_speedup_vs_single": match (single_end_to_end_median_ns, parallel_wall_median_ns) { (Some(single), Some(parallel)) if single > 0 && parallel > 0 => Some(TARGET_RUNS as f64 * single as f64 / parallel as f64), _ => None },
            },
            "timing_scope": if candidate_kind == StrategyKind::Empty && batch.is_none() { "Rust empty event loop" } else if candidate_kind == StrategyKind::Empty { "Python empty on_bar callback; one GIL reacquisition per event" } else if batch == Some(true) { "Python strategy initialization excluded from callback; one PyO3 boundary per Run; Python processes the complete event list" } else if batch == Some(false) { "Python strategy initialization excluded from callback; one GIL reacquisition and on_bar call per event" } else { "Rust Native event callback loop; initialization is reported separately" },
            "account_scope": "same Rust Fast Event target replay after the strategy emits date/target decisions; separately included in end-to-end and parallel Runs/s",
        });
        candidates.insert(name.to_string(), candidate);
    }
    Ok(Value::Object(candidates))
}

fn last_symbol_by_date(events: &[BarEvent]) -> BTreeMap<NaiveDate, String> {
    let mut result = BTreeMap::new();
    for event in events {
        result.insert(event.date, event.symbol.clone());
    }
    result
}

fn make_s2_stress_dataset(
    base: &DatasetInput,
    instruments: usize,
    sessions: usize,
) -> Result<PreparedDataset> {
    let mut input = serde_json::to_value(base)?;
    let calendar = business_calendar(NaiveDate::from_ymd_opt(2025, 1, 6).unwrap(), sessions);
    let symbols = (0..instruments)
        .map(|index| format!("ETF{index:03}"))
        .collect::<Vec<_>>();
    input["dataset_version"] = json!("poc0.b3.s2-scale-64x252.v1");
    input["source_identity"] = json!("poc0-b3-deterministic-scale-generator-v1");
    input["calendar_basis"] = json!("fixed_synthetic_weekday_sessions");
    input["calendar"] = serde_json::to_value(&calendar)?;
    input["seed"] = json!(20260928_u64);
    input["seed_semantics"] = json!("deterministic arithmetic price paths, not random sampling");
    input["strategy"]["momentum_short_days"] = json!(20);
    input["strategy"]["momentum_long_days"] = json!(60);
    input["strategy"]["volatility_window"] = json!(20);
    input["strategy"]["volatility_weight"] = json!(1.0);
    input["strategy"]["top_n"] = json!(5);
    input["strategy"]["rebalance_every"] = json!(5);
    input["missing_bars"] = json!([{
        "symbol": symbols[1],
        "date": calendar[sessions / 2],
        "reason": "deterministic_scale_fixture_missing_bar"
    }]);
    input["execution_status_overrides"] = json!([{
        "symbol": symbols[2],
        "date": calendar[sessions / 2 + 1],
        "trade_status": "HALTED",
        "is_tradable": false,
        "sources": "poc0-b3-scale-fixture",
        "available_at": "08:50:00+08:00"
    }]);
    input["instruments"] = serde_json::to_value(
        symbols
            .iter()
            .enumerate()
            .map(|(symbol_index, symbol)| {
                let closes = (0..sessions)
                    .map(|day| {
                        100.0
                            + (((symbol_index * 17 + day * (symbol_index % 13 + 1)) % 101) as f64
                                - 50.0)
                                * 0.1
                    })
                    .collect::<Vec<_>>();
                json!({
                    "symbol": symbol,
                    "currency": "CNY",
                    "price_precision": 2,
                    "quantity_precision": 0,
                    "closes": closes
                })
            })
            .collect::<Vec<_>>(),
    )?;
    prepare_dataset(serde_json::from_value(input)?)
}

fn make_s3_stress_dataset(
    base: &PreparedDataset,
    instruments: usize,
    sessions: usize,
) -> Result<PreparedDataset> {
    let mut input = serde_json::to_value(&base.spec)?;
    let original_calendar = &base.spec.calendar;
    ensure!(
        sessions >= original_calendar.len(),
        "S3 stress sessions too short"
    );
    let mut calendar = original_calendar.clone();
    if sessions > calendar.len() {
        let last = *calendar.last().context("S3 base calendar empty")?;
        let mut extension = business_calendar(
            last.succ_opt().context("S3 stress calendar overflow")?,
            sessions - calendar.len(),
        );
        calendar.append(&mut extension);
    }
    let mut old_closes = BTreeMap::new();
    for instrument in input["instruments"].as_array().context("S3 instruments")? {
        old_closes.insert(
            instrument["symbol"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            instrument["closes"]
                .as_array()
                .context("S3 close series")?
                .clone(),
        );
    }
    let mut instruments_out = Vec::with_capacity(instruments);
    for index in 0..instruments {
        let symbol = if index == 0 {
            "A".to_string()
        } else {
            format!("ETF{index:03}")
        };
        let old = old_closes.get(&symbol);
        let last = old
            .and_then(|closes| closes.last())
            .and_then(Value::as_f64)
            .unwrap_or(100.0);
        let mut closes = old
            .map(|values| values.iter().map(|value| value.as_f64().unwrap()).collect())
            .unwrap_or_else(|| vec![100.0; original_calendar.len()]);
        closes.resize(sessions, last);
        instruments_out.push(json!({
            "symbol": symbol,
            "currency": "CNY",
            "price_precision": 2,
            "quantity_precision": 0,
            "closes": closes,
        }));
    }
    input["dataset_version"] = json!("poc0.b3.s3-scale-64x252.v1");
    input["source_identity"] = json!("poc0-b3-s3-deterministic-scale-generator-v1");
    input["calendar_basis"] = json!("fixed_synthetic_weekday_sessions");
    input["calendar"] = serde_json::to_value(&calendar)?;
    input["seed"] = json!(20260928_u64);
    input["seed_semantics"] =
        json!("fixed MA20/60 golden price path extended with flat symbols/sessions");
    input["instruments"] = json!(instruments_out);
    input["missing_bars"] = json!([]);
    input["execution_status_overrides"] = json!([]);
    prepare_dataset(serde_json::from_value(input)?)
}

fn business_calendar(start: NaiveDate, sessions: usize) -> Vec<NaiveDate> {
    let mut calendar = Vec::with_capacity(sessions);
    let mut date = start;
    while calendar.len() < sessions {
        if date.weekday().number_from_monday() <= 5 {
            calendar.push(date);
        }
        date = date.succ_opt().expect("business calendar date overflow");
    }
    calendar
}

fn process_peak_rss_bytes() -> Result<u64> {
    let value = Python::attach(|py| -> PyResult<u64> {
        let resource = py.import("resource")?;
        let usage = resource
            .getattr("getrusage")?
            .call1((resource.getattr("RUSAGE_SELF")?,))?;
        let peak = usage.getattr("ru_maxrss")?.extract::<u64>()?;
        let multiplier = if cfg!(target_os = "macos") { 1 } else { 1024 };
        Ok(peak.saturating_mul(multiplier))
    })
    .map_err(|error| anyhow::anyhow!("read process peak RSS: {error}"))?;
    Ok(value)
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(bytes);
    format!("{:x}", hash.finalize())
}

const STRATEGY_MODULE: &str = r#"
class MomentumRotation:
    def __init__(self, symbols, last_symbols, short_days, long_days, vol_window,
                 short_weight, long_weight, vol_weight, top_n, rebalance_every):
        self.symbols = list(symbols)
        self.last_symbols = last_symbols
        self.short_days = short_days
        self.long_days = long_days
        self.vol_window = vol_window
        self.short_weight = short_weight
        self.long_weight = long_weight
        self.vol_weight = vol_weight
        self.top_n = top_n
        self.rebalance_every = max(1, rebalance_every)
        self.history = {}
        self.seen_today = set()
        self.active_date = None
        self.eligible_dates = 0

    def on_bar(self, date, symbol, close):
        if self.active_date != date:
            self.active_date = date
            self.seen_today = set()
        self.history.setdefault(symbol, []).append(close)
        self.seen_today.add(symbol)
        if symbol != self.last_symbols[date]:
            return None
        ranked = []
        required = max(self.short_days, self.long_days, self.vol_window)
        for candidate in self.symbols:
            values = self.history.get(candidate, [])
            if candidate not in self.seen_today or len(values) <= required or self.vol_window <= 1:
                continue
            short = values[-1] / values[-1 - self.short_days] - 1.0
            long = values[-1] / values[-1 - self.long_days] - 1.0
            returns = [values[index] / values[index - 1] - 1.0
                       for index in range(len(values) - self.vol_window, len(values))]
            mean = sum(returns) / len(returns)
            variance = sum((value - mean) ** 2 for value in returns) / (len(returns) - 1)
            score = self.short_weight * short + self.long_weight * long - self.vol_weight * variance ** 0.5
            ranked.append((candidate, score))
        ranked.sort(key=lambda row: (-row[1], row[0]))
        if not ranked:
            return None
        rebalance = self.eligible_dates % self.rebalance_every == 0
        self.eligible_dates += 1
        if rebalance:
            return [symbol for symbol, _ in ranked[:self.top_n]]
        return None

class Ma20_60:
    def __init__(self):
        self.closes = []
        self.previous_gap = None
        self.held = False

    def on_bar(self, date, symbol, close):
        if symbol != "A":
            return None
        self.closes.append(close)
        if len(self.closes) < 60:
            return None
        short = sum(self.closes[-20:]) / 20.0
        long = sum(self.closes[-60:]) / 60.0
        gap = short - long
        decision = None
        if self.previous_gap is not None:
            if not self.held and self.previous_gap <= 0.0 and gap > 0.0:
                self.held = True
                decision = ["A"]
            elif self.held and self.previous_gap >= 0.0 and gap < 0.0:
                self.held = False
                decision = []
        self.previous_gap = gap
        return decision

class EmptyStrategy:
    def on_bar(self, date, symbol, close):
        return None

def run_batch(strategy, events):
    decisions = []
    for date, symbol, close in events:
        targets = strategy.on_bar(date, symbol, close)
        if targets is not None:
            decisions.append((date, symbol, targets))
    return decisions
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s2_s3_python_callbacks_match_fixed_golden_signals_and_accounts() {
        let dataset_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("poc/poc0-benchmark/fixtures/dataset-v1.json");
        let expected_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("poc/poc0-benchmark/fixtures/expected-v1.json");
        let base: DatasetInput =
            serde_json::from_slice(&fs::read(&dataset_path).expect("read fixed dataset"))
                .expect("parse fixed dataset");
        let prepared = prepare_dataset(base).expect("prepare fixed S2 dataset");
        let report = run(&dataset_path, &expected_path, "soa").expect("run independent golden");
        let s2_events = ordered_events(&prepared);
        let expected_s2 = decisions_from_signals(
            StrategyKind::S2MomentumRotation,
            &s2_events,
            &report.b2_fast_event_momentum_rotation.signals,
        )
        .expect("map S2 golden events");
        assert_eq!(
            run_native(StrategyKind::S2MomentumRotation, &prepared, &s2_events)
                .expect("run native S2")
                .decisions,
            expected_s2
        );
        for batch in [false, true] {
            let python = run_python(
                StrategyKind::S2MomentumRotation,
                &prepared,
                &s2_events,
                batch,
            )
            .expect("run Python S2");
            assert_eq!(python.decisions, expected_s2);
            assert_eq!(
                serde_json::to_value(account_projection(&prepared, &python.decisions).unwrap())
                    .unwrap(),
                serde_json::to_value(&report.b2_fast_event_momentum_rotation.projection).unwrap()
            );
        }

        let (ma_dataset, _) = make_ma_dataset(&prepared.spec).expect("make fixed S3 fixture");
        let s3_events = ordered_events(&ma_dataset);
        let expected_s3 = decisions_from_signals(
            StrategyKind::S3Ma20_60,
            &s3_events,
            &report.b2_fast_event_ma20_60.signals,
        )
        .expect("map S3 golden events");
        assert_eq!(
            run_native(StrategyKind::S3Ma20_60, &ma_dataset, &s3_events)
                .expect("run native S3")
                .decisions,
            expected_s3
        );
        for batch in [false, true] {
            let python = run_python(StrategyKind::S3Ma20_60, &ma_dataset, &s3_events, batch)
                .expect("run Python S3");
            assert_eq!(python.decisions, expected_s3);
            assert_eq!(
                serde_json::to_value(account_projection(&ma_dataset, &python.decisions).unwrap())
                    .unwrap(),
                serde_json::to_value(&report.b2_fast_event_ma20_60.projection).unwrap()
            );
        }
    }
}
