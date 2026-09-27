use crate::{
    backtest::{self, BacktestReport},
    core::{
        Bar, CostConfig, ExecutionStatus, ExecutionStatusMap, ExperimentConfig, StrategyConfig,
    },
    strategy::rotation_scores,
};
use anyhow::{Context, Result, ensure};
use arrow_array::{Array, Float64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use chrono::{DateTime, Datelike, NaiveDate};
use polars::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Instant,
};

mod parquet;
mod sweep;
pub use parquet::{ParquetOptions, run_parquet};
pub use sweep::{SweepOptions, run_sweep};

const WARMUP_RUNS: usize = 1;
const MEASUREMENT_RUNS: usize = 5;
const FLOAT_TOLERANCE: f64 = 1e-8;

#[derive(Debug, Deserialize, Serialize)]
struct DatasetInput {
    dataset_version: String,
    seed: u64,
    seed_semantics: String,
    source_identity: String,
    calendar_basis: String,
    timezone: String,
    calendar: Vec<NaiveDate>,
    bar_defaults: BarDefaults,
    instruments: Vec<InstrumentInput>,
    missing_bars: Vec<MissingBarInput>,
    execution_status_default: StatusInput,
    execution_status_overrides: Vec<StatusOverrideInput>,
    time_model: TimeModelInput,
    result_model_boundary: String,
    account: AccountInput,
    strategy: StrategyInput,
    costs: CostConfig,
}

#[derive(Debug, Deserialize, Serialize)]
struct BarDefaults {
    open: f64,
    volume: f64,
    amount: f64,
    high_rule: String,
    low_rule: String,
    open_available_at: String,
    close_available_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct InstrumentInput {
    symbol: String,
    currency: String,
    price_precision: u8,
    quantity_precision: u8,
    closes: Vec<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MissingBarInput {
    symbol: String,
    date: NaiveDate,
    reason: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct StatusInput {
    trade_status: String,
    is_tradable: bool,
    sources: String,
    available_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct StatusOverrideInput {
    symbol: String,
    date: NaiveDate,
    trade_status: String,
    is_tradable: bool,
    sources: String,
    available_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct TimeModelInput {
    signal: String,
    execution: String,
    execution_status: String,
    final_session: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct AccountInput {
    initial_cash: f64,
    lot_size: i64,
}

#[derive(Debug, Deserialize, Serialize)]
struct StrategyInput {
    name: String,
    version: String,
    momentum_short_days: usize,
    momentum_long_days: usize,
    volatility_window: usize,
    short_momentum_weight: f64,
    long_momentum_weight: f64,
    volatility_weight: f64,
    trend_filter: bool,
    top_n: usize,
    rebalance_every: usize,
    tie_break: String,
}

#[derive(Debug, Deserialize)]
struct ExpectedFile {
    expected_version: String,
    dataset_content_sha256: Option<String>,
    projection: Value,
}

#[derive(Debug, Serialize)]
struct TimedBar {
    symbol: String,
    date: NaiveDate,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    amount: f64,
    open_available_at: String,
    close_available_at: String,
}

#[derive(Debug, Serialize)]
struct TimedStatus {
    symbol: String,
    date: NaiveDate,
    trade_status: String,
    is_tradable: bool,
    sources: String,
    available_at: String,
}

#[derive(Debug, Serialize)]
struct DatasetContent {
    dataset_version: String,
    seed: u64,
    seed_semantics: String,
    source_identity: String,
    calendar_basis: String,
    timezone: String,
    calendar: Vec<NaiveDate>,
    bar_defaults: BarDefaults,
    instruments: Vec<InstrumentIdentity>,
    bars: Vec<TimedBar>,
    missing_bars: Vec<MissingBarInput>,
    execution_statuses: Vec<TimedStatus>,
    time_model: TimeModelInput,
    result_model_boundary: String,
    account: AccountInput,
    strategy: StrategyInput,
    costs: CostConfig,
}

#[derive(Debug, Serialize)]
struct InstrumentIdentity {
    symbol: String,
    currency: String,
    price_precision: u8,
    quantity_precision: u8,
}

struct PreparedDataset {
    spec: DatasetInput,
    bars: Vec<Bar>,
    statuses: ExecutionStatusMap,
    status_rows: Vec<TimedStatus>,
    content_sha256: String,
    bar_count: usize,
    instrument_identities: Vec<InstrumentIdentity>,
}

#[derive(Debug, Serialize)]
struct CandidateConfig {
    name: String,
    version: String,
    initial_cash: f64,
    lot_size: i64,
    strategy: StrategyConfig,
    costs: CostConfig,
    future_returns_used_for_decisions: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct RankingCandidate {
    symbol: String,
    score: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct RankingProjection {
    date: NaiveDate,
    candidates: Vec<RankingCandidate>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct SignalProjection {
    date: NaiveDate,
    target_symbols: Vec<String>,
}

#[derive(Debug, Serialize)]
struct FillProjection {
    date: NaiveDate,
    symbol: String,
    side: String,
    quantity: i64,
    reference_price: f64,
    fill_price: f64,
    gross_value: f64,
    commission: f64,
    tax: f64,
    slippage_cost: f64,
}

#[derive(Debug, Serialize)]
struct UnexecutedOrderProjection {
    decision_date: NaiveDate,
    attempt_date: NaiveDate,
    symbol: String,
    side: String,
    desired_quantity: Option<i64>,
    trade_status: Option<String>,
    status_sources: Option<String>,
    reason: String,
}

#[derive(Debug, Serialize)]
struct HoldingProjection {
    symbol: String,
    quantity: i64,
    mark_price: f64,
}

#[derive(Debug, Serialize)]
struct LedgerProjection {
    date: NaiveDate,
    cash: f64,
    holdings: Vec<HoldingProjection>,
    nav: f64,
}

#[derive(Debug, Serialize)]
struct SummaryProjection {
    initial_cash: f64,
    final_equity: f64,
    final_positions: BTreeMap<String, i64>,
    trade_count: usize,
    commission: f64,
    tax: f64,
    slippage_cost: f64,
    total_cost: f64,
}

#[derive(Debug, Serialize)]
struct ResultProjection {
    rankings: Vec<RankingProjection>,
    signals: Vec<SignalProjection>,
    fills: Vec<FillProjection>,
    unexecuted_orders: Vec<UnexecutedOrderProjection>,
    ledger: Vec<LedgerProjection>,
    summary: SummaryProjection,
}

#[derive(Debug, Serialize)]
struct Difference {
    path: String,
    expected: Value,
    actual: Value,
}

#[derive(Debug, Serialize)]
struct CorrectnessReport {
    status: &'static str,
    tolerance: f64,
    differences: Vec<Difference>,
    accounting_checks: Vec<String>,
    time_checks: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DatasetReport {
    version: String,
    seed: u64,
    seed_semantics: String,
    source_identity: String,
    calendar_basis: String,
    timezone: String,
    calendar: Vec<NaiveDate>,
    bar_defaults: BarDefaults,
    instrument_count: usize,
    instruments: Vec<InstrumentIdentity>,
    bar_count: usize,
    missing_bars: Vec<MissingBarInput>,
    status_override_count: usize,
    execution_statuses: Vec<TimedStatus>,
    input_path: String,
    raw_input_sha256: String,
    content_sha256: String,
    time_model: TimeModelInput,
    result_model_boundary: String,
}

#[derive(Debug, Serialize)]
struct MeasurementReport {
    status: &'static str,
    warmup_runs: usize,
    requested_runs: usize,
    raw_samples_ns: Vec<u128>,
    median_ns: Option<u128>,
    p95_ns: Option<u128>,
    peak_rss_bytes: Option<u64>,
    resource_measurement_status: &'static str,
    measurement_scope: &'static str,
}

#[derive(Debug, Serialize)]
struct ProvenanceReport {
    git_revision: Option<String>,
    working_tree_dirty: Option<bool>,
    working_tree_complete: bool,
    tracked_diff_sha256: Option<String>,
    tracked_diff_summary: Option<String>,
    changed_paths: Vec<String>,
    rustc: Option<String>,
    build_profile: &'static str,
    operating_system: &'static str,
    architecture: &'static str,
    logical_cpu_count: Option<usize>,
    cpu_model: Option<String>,
    cargo_lock_sha256: Option<String>,
    physical_memory_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct Poc0Report {
    schema_version: &'static str,
    run_id: String,
    candidate: String,
    strategy: CandidateConfig,
    dataset: DatasetReport,
    expected_input: String,
    expected_input_sha256: String,
    provenance: ProvenanceReport,
    correctness: CorrectnessReport,
    measurements: MeasurementReport,
    result: ReportResult,
    b1_soa: B1SoaReport,
    b1_arrow: B1ArrowReport,
    b1_polars: B1PolarsReport,
    b2_fast_event_buy_hold: FastEventReport,
    conclusion: &'static str,
}

#[derive(Debug, Serialize)]
struct FastEventReport {
    status: &'static str,
    correctness_checks: Vec<String>,
    engine: &'static str,
    version: &'static str,
    semantics: &'static str,
    checksum_sha256: String,
    projection: FastEventProjection,
    warmup_runs: usize,
    raw_samples_ns: Vec<u128>,
    median_ns: Option<u128>,
    p95_ns: Option<u128>,
    initialization_samples_ns: Vec<u128>,
    event_processing_samples_ns: Vec<u128>,
    end_to_end_samples_ns: Vec<u128>,
    measurement_scope: &'static str,
}

struct FastEventRun {
    projection: FastEventProjection,
    initialization_ns: u128,
    event_processing_ns: u128,
    end_to_end_ns: u128,
}

#[derive(Debug, Serialize, PartialEq)]
struct FastEventProjection {
    orders: Vec<FastEventOrder>,
    fills: Vec<FastEventFill>,
    ledger: Vec<FastEventLedger>,
    unexecuted_targets: Vec<FastEventPendingTarget>,
    summary: FastEventSummary,
}
#[derive(Debug, Serialize, PartialEq)]
struct FastEventOrder {
    decision_date: NaiveDate,
    attempt_date: NaiveDate,
    symbol: String,
    side: String,
    quantity: i64,
    reason: Option<String>,
}
#[derive(Debug, Serialize, PartialEq)]
struct FastEventFill {
    date: NaiveDate,
    symbol: String,
    side: String,
    quantity: i64,
    reference_price: f64,
    fill_price: f64,
    gross_value: f64,
    commission: f64,
    tax: f64,
    slippage_cost: f64,
}
#[derive(Debug, Serialize, PartialEq)]
struct FastEventHolding {
    symbol: String,
    quantity: i64,
    mark_price: f64,
}
#[derive(Debug, Serialize, PartialEq)]
struct FastEventLedger {
    date: NaiveDate,
    cash: f64,
    holdings: Vec<FastEventHolding>,
    nav: f64,
}
#[derive(Debug, Serialize, PartialEq)]
struct FastEventPendingTarget {
    decision_date: NaiveDate,
    attempt_date: NaiveDate,
    symbol: String,
    reason: String,
}
#[derive(Debug, Serialize, PartialEq)]
struct FastEventSummary {
    initial_cash: f64,
    final_equity: f64,
    final_positions: BTreeMap<String, i64>,
    commission: f64,
    tax: f64,
    slippage_cost: f64,
    total_cost: f64,
}

#[derive(Debug, Serialize)]
struct B1SoaReport {
    correctness_status: &'static str,
    checksum_sha256: String,
    projection: B1SoaProjection,
    warmup_runs: usize,
    raw_samples_ns: Vec<u128>,
    median_ns: Option<u128>,
    p95_ns: Option<u128>,
    phase_samples_ns: B1PhaseSamples,
    measurement_scope: &'static str,
    model_boundary: &'static str,
}

#[derive(Debug, Serialize)]
struct B1ArrowReport {
    correctness_status: &'static str,
    checksum_sha256: String,
    projection: B1SoaProjection,
    warmup_runs: usize,
    raw_samples_ns: Vec<u128>,
    median_ns: Option<u128>,
    p95_ns: Option<u128>,
    phase_samples_ns: B1PhaseSamples,
    conversion_samples_ns: Vec<u128>,
    measurement_scope: &'static str,
    model_boundary: &'static str,
}

#[derive(Debug, Serialize)]
struct B1PolarsReport {
    correctness_status: &'static str,
    checksum_sha256: String,
    projection: B1SoaProjection,
    warmup_runs: usize,
    raw_samples_ns: Vec<u128>,
    median_ns: Option<u128>,
    p95_ns: Option<u128>,
    phase_samples_ns: B1PhaseSamples,
    conversion_samples_ns: Vec<u128>,
    measurement_scope: &'static str,
    model_boundary: &'static str,
}

#[derive(Debug, Default, Serialize)]
struct B1PhaseSamples {
    factor_ns: Vec<u128>,
    ranking_topk_weights_ns: Vec<u128>,
    return_projection_ns: Vec<u128>,
}

#[derive(Debug, Serialize, PartialEq)]
struct B1SoaProjection {
    rankings: Vec<RankingProjection>,
    targets: Vec<SignalProjection>,
    target_weights: Vec<WeightedTarget>,
    portfolio_returns: Vec<PortfolioReturn>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct WeightedTarget {
    date: NaiveDate,
    symbol: String,
    weight: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct PortfolioReturn {
    from: NaiveDate,
    to: NaiveDate,
    return_pct: Option<f64>,
}

impl Poc0Report {
    pub fn correctness_status(&self) -> &'static str {
        self.correctness.status
    }
}

#[derive(Debug, Serialize)]
struct ReportResult {
    checksum_sha256: String,
    projection: ResultProjection,
}

/// Load one fixed fixture, run the reference implementation, check an independent golden
/// projection, then measure only after every correctness and accounting gate passes.
pub fn run(
    dataset_path: &Path,
    expected_path: &Path,
    selected_candidate: &str,
) -> Result<Poc0Report> {
    ensure!(
        matches!(selected_candidate, "reference" | "soa" | "arrow" | "polars"),
        "unknown POC-0 candidate {selected_candidate}"
    );
    let dataset_bytes = fs::read(dataset_path)
        .with_context(|| format!("read POC-0 dataset {}", dataset_path.display()))?;
    let expected_bytes = fs::read(expected_path)
        .with_context(|| format!("read POC-0 golden {}", expected_path.display()))?;
    let spec: DatasetInput = serde_json::from_slice(&dataset_bytes)
        .with_context(|| format!("parse POC-0 dataset {}", dataset_path.display()))?;
    let expected: ExpectedFile = serde_json::from_slice(&expected_bytes)
        .with_context(|| format!("parse POC-0 golden {}", expected_path.display()))?;
    let prepared = prepare_dataset(spec)?;
    let config = experiment_config(&prepared.spec);
    config.validate()?;
    ensure!(
        expected.expected_version == "poc0-golden-ledger.v1",
        "unsupported POC-0 expected version {}",
        expected.expected_version
    );

    let report = run_candidate(&prepared, &config);
    let projection = project_result(&prepared, &config, &report);
    let b1_projection = run_soa(&prepared, &config, false).0;
    let (arrow_projection, _, _, _) = run_arrow(&prepared, &config, false);
    let (polars_projection, _, _, _) = run_polars(&prepared, &config, false)?;
    let expected_targets = projection
        .signals
        .iter()
        .enumerate()
        .filter(|(i, _)| *i % prepared.spec.strategy.rebalance_every.max(1) == 0)
        .map(|(_, signal)| signal)
        .cloned()
        .collect::<Vec<_>>();
    let b1_correct =
        b1_projection.rankings == projection.rankings && b1_projection.targets == expected_targets;
    let arrow_correct = arrow_projection.rankings == b1_projection.rankings
        && arrow_projection.targets == b1_projection.targets
        && projections_match(&arrow_projection, &b1_projection);
    let polars_correct = polars_projection.rankings == b1_projection.rankings
        && polars_projection.targets == b1_projection.targets
        && projections_match(&polars_projection, &b1_projection);
    let mut differences = compare_values(
        &expected.projection,
        &serde_json::to_value(&projection)?,
        "$".into(),
    );
    if let Some(expected_hash) = &expected.dataset_content_sha256
        && expected_hash != &prepared.content_sha256
    {
        differences.push(Difference {
            path: "$.dataset_content_sha256".into(),
            expected: Value::String(expected_hash.clone()),
            actual: Value::String(prepared.content_sha256.clone()),
        });
    }
    let accounting_checks = accounting_checks(&report);
    let time_checks = time_checks(&report, &projection, &prepared.spec.calendar);
    let correctness_passed = differences.is_empty()
        && accounting_checks.is_empty()
        && time_checks.is_empty()
        && b1_correct
        && arrow_correct
        && polars_correct;
    let checksum_sha256 = sha256(&serde_json::to_vec(&projection)?);

    let (measurements, measurement_differences) = if correctness_passed
        && selected_candidate == "reference"
    {
        measure_candidate(&prepared, &config, &expected.projection)?
    } else {
        (
            MeasurementReport {
                status: if correctness_passed {
                    "not_selected"
                } else {
                    "skipped_correctness_failure"
                },
                warmup_runs: 0,
                requested_runs: MEASUREMENT_RUNS,
                raw_samples_ns: Vec::new(),
                median_ns: None,
                p95_ns: None,
                peak_rss_bytes: None,
                resource_measurement_status: "not_measured_in_ticket_01",
                measurement_scope: "reference backtest call; excludes fixture parsing and report projection",
            },
            Vec::new(),
        )
    };
    let (b1_ns, b1_phase_samples) = if correctness_passed && selected_candidate == "soa" {
        let (_, samples, phases) = run_soa(&prepared, &config, true);
        (samples, phases)
    } else {
        (Vec::new(), B1PhaseSamples::default())
    };
    let (arrow_ns, arrow_phase_samples, arrow_conversion_samples) =
        if correctness_passed && selected_candidate == "arrow" {
            let (_, samples, phases, conversion) = run_arrow(&prepared, &config, true);
            (samples, phases, conversion)
        } else {
            (Vec::new(), B1PhaseSamples::default(), Vec::new())
        };
    let (polars_ns, polars_phase_samples, polars_conversion_samples) =
        if correctness_passed && selected_candidate == "polars" {
            let (_, samples, phases, conversion) = run_polars(&prepared, &config, true)?;
            (samples, phases, conversion)
        } else {
            (Vec::new(), B1PhaseSamples::default(), Vec::new())
        };
    let fast_event_run = run_fast_event(&prepared);
    let fast_event_checksum = sha256(&serde_json::to_vec(&fast_event_run.projection)?);
    let repeated_projection = run_fast_event(&prepared).projection;
    let fast_event_checks =
        fast_event_checks(&prepared, &fast_event_run.projection, &repeated_projection)?;
    let fast_event_correct = fast_event_checks.is_empty();
    let mut fast_event_runs = Vec::new();
    if fast_event_correct {
        for _ in 0..WARMUP_RUNS {
            let _ = run_fast_event(&prepared);
        }
        for _ in 0..MEASUREMENT_RUNS {
            fast_event_runs.push(run_fast_event(&prepared));
        }
    }
    let initialization_samples_ns = fast_event_runs
        .iter()
        .map(|r| r.initialization_ns)
        .collect::<Vec<_>>();
    let event_processing_samples_ns = fast_event_runs
        .iter()
        .map(|r| r.event_processing_ns)
        .collect::<Vec<_>>();
    let end_to_end_samples_ns = fast_event_runs
        .iter()
        .map(|r| r.end_to_end_ns)
        .collect::<Vec<_>>();
    let b1_median = sorted_median(&b1_ns);
    let b1_p95 = sorted_p95(&b1_ns);
    differences.extend(measurement_differences);
    let candidate_measurement_passed = if selected_candidate == "reference" {
        measurements.status == "measured"
    } else if selected_candidate == "soa" {
        b1_ns.len() == MEASUREMENT_RUNS
    } else if selected_candidate == "arrow" {
        arrow_ns.len() == MEASUREMENT_RUNS
    } else {
        polars_ns.len() == MEASUREMENT_RUNS
    };
    let correctness_passed = correctness_passed && candidate_measurement_passed;
    let provenance = provenance_report();
    let strategy = candidate_config(&prepared.spec, &config);
    let dataset_report = DatasetReport {
        version: prepared.spec.dataset_version.clone(),
        seed: prepared.spec.seed,
        seed_semantics: prepared.spec.seed_semantics.clone(),
        source_identity: prepared.spec.source_identity.clone(),
        calendar_basis: prepared.spec.calendar_basis.clone(),
        timezone: prepared.spec.timezone.clone(),
        calendar: prepared.spec.calendar.clone(),
        bar_defaults: BarDefaults {
            open: prepared.spec.bar_defaults.open,
            volume: prepared.spec.bar_defaults.volume,
            amount: prepared.spec.bar_defaults.amount,
            high_rule: prepared.spec.bar_defaults.high_rule.clone(),
            low_rule: prepared.spec.bar_defaults.low_rule.clone(),
            open_available_at: prepared.spec.bar_defaults.open_available_at.clone(),
            close_available_at: prepared.spec.bar_defaults.close_available_at.clone(),
        },
        instrument_count: prepared.instrument_identities.len(),
        instruments: prepared.instrument_identities,
        bar_count: prepared.bar_count,
        missing_bars: prepared.spec.missing_bars.clone(),
        status_override_count: prepared.spec.execution_status_overrides.len(),
        execution_statuses: prepared.status_rows,
        input_path: dataset_path.display().to_string(),
        raw_input_sha256: sha256(&dataset_bytes),
        content_sha256: prepared.content_sha256.clone(),
        time_model: prepared.spec.time_model,
        result_model_boundary: prepared.spec.result_model_boundary,
    };
    let config_sha256 = sha256(&serde_json::to_vec(&strategy)?);
    let run_id = sha256(&serde_json::to_vec(&(
        &dataset_report.content_sha256,
        &config_sha256,
        &provenance.git_revision,
        &provenance.tracked_diff_sha256,
    ))?);

    Ok(Poc0Report {
        schema_version: "poc0.report.v1",
        run_id,
        candidate: match selected_candidate {
            "soa" => "custom-soa-momentum-rotation",
            "arrow" => "apache-arrow-column-batch-momentum-rotation",
            "polars" => "polars-expression-momentum-rotation",
            _ => "existing-rust-etf-backtest-reference",
        }
        .into(),
        strategy,
        dataset: dataset_report,
        expected_input: expected_path.display().to_string(),
        expected_input_sha256: sha256(&expected_bytes),
        provenance,
        correctness: CorrectnessReport {
            status: if correctness_passed {
                "passed"
            } else {
                "failed"
            },
            tolerance: FLOAT_TOLERANCE,
            differences,
            accounting_checks,
            time_checks,
        },
        measurements,
        result: ReportResult {
            checksum_sha256,
            projection,
        },
        b1_soa: B1SoaReport {
            correctness_status: if b1_correct { "passed" } else { "failed" },
            checksum_sha256: sha256(&serde_json::to_vec(&b1_projection)?),
            projection: b1_projection,
            warmup_runs: if b1_ns.is_empty() { 0 } else { WARMUP_RUNS },
            raw_samples_ns: b1_ns,
            median_ns: b1_median,
            p95_ns: b1_p95,
            phase_samples_ns: b1_phase_samples,
            measurement_scope: "symbol-major SoA factor windows, cross-sectional rank/top-k, equal weights, and next-session return projection; excludes parsing and fixture construction",
            model_boundary: "equal-weight target-weight × next-session close-to-close return; missing selected bar makes that interval unavailable; no cash, fees, fills, or event-account NAV",
        },
        b1_arrow: B1ArrowReport {
            correctness_status: if arrow_correct { "passed" } else { "failed" },
            checksum_sha256: sha256(&serde_json::to_vec(&arrow_projection)?),
            projection: arrow_projection,
            warmup_runs: if arrow_ns.is_empty() { 0 } else { WARMUP_RUNS },
            raw_samples_ns: arrow_ns.clone(),
            median_ns: sorted_median(&arrow_ns),
            p95_ns: sorted_p95(&arrow_ns),
            phase_samples_ns: arrow_phase_samples,
            conversion_samples_ns: arrow_conversion_samples,
            measurement_scope: "Arrow RecordBatch construction and symbol-column S2 factor windows, cross-sectional rank/top-k, equal weights, and next-session return projection; excludes fixture parsing and JSON serialization",
            model_boundary: "equal-weight target-weight × next-session close-to-close return; missing selected bar makes that interval unavailable; no cash, fees, fills, or event-account NAV",
        },
        b1_polars: B1PolarsReport {
            correctness_status: if polars_correct { "passed" } else { "failed" },
            checksum_sha256: sha256(&serde_json::to_vec(&polars_projection)?),
            projection: polars_projection,
            warmup_runs: if polars_ns.is_empty() { 0 } else { WARMUP_RUNS },
            median_ns: sorted_median(&polars_ns),
            p95_ns: sorted_p95(&polars_ns),
            raw_samples_ns: polars_ns,
            phase_samples_ns: polars_phase_samples,
            conversion_samples_ns: polars_conversion_samples,
            measurement_scope: "Polars lazy expressions for factor score, ordered rank/TopK, equal weights, and next-session return projection; excludes fixture parsing",
            model_boundary: "same simplified equal-weight next-session close-to-close return labels as SoA; no cash, fees, fills, or event-account NAV",
        },
        b2_fast_event_buy_hold: FastEventReport {
            status: if fast_event_correct {
                "correctness_passed_and_measured"
            } else {
                "correctness_failed_timing_skipped"
            },
            correctness_checks: fast_event_checks,
            engine: "poc0-fast-event",
            version: "0.1",
            semantics: "single venue; long only; market orders; L1 next-session open fills; fixed commission, tax, and slippage; buy-and-hold equal weight; stale last close marks missing bars; not a production engine",
            checksum_sha256: fast_event_checksum,
            projection: fast_event_run.projection,
            warmup_runs: if fast_event_correct { WARMUP_RUNS } else { 0 },
            raw_samples_ns: end_to_end_samples_ns.clone(),
            median_ns: sorted_median(&end_to_end_samples_ns),
            p95_ns: sorted_p95(&end_to_end_samples_ns),
            initialization_samples_ns,
            event_processing_samples_ns,
            end_to_end_samples_ns,
            measurement_scope: "initialization (day index construction), event processing (orders, fills, marks and ledger), and end-to-end (both phases plus summary); excludes dataset parsing and JSON report serialization",
        },
        conclusion: "unresolved: one fixture validates the harness and reference path; no architecture performance choice is supported",
    })
}

fn sorted_median(samples: &[u128]) -> Option<u128> {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    (!sorted.is_empty()).then(|| sorted[sorted.len() / 2])
}
fn sorted_p95(samples: &[u128]) -> Option<u128> {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    (!sorted.is_empty()).then(|| {
        sorted[(sorted.len() * 95)
            .div_ceil(100)
            .saturating_sub(1)
            .min(sorted.len() - 1)]
    })
}

fn fast_event_checks(
    dataset: &PreparedDataset,
    projection: &FastEventProjection,
    repeated: &FastEventProjection,
) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    if projection != repeated {
        failures.push("repeated run ledger differs".into());
    }
    for row in &projection.ledger {
        let calculated = row.cash
            + row
                .holdings
                .iter()
                .map(|holding| holding.quantity as f64 * holding.mark_price)
                .sum::<f64>();
        if (row.nav - calculated).abs() > FLOAT_TOLERANCE {
            failures.push(format!("NAV identity failed on {}", row.date));
        }
    }
    if projection
        .orders
        .iter()
        .any(|order| order.attempt_date <= order.decision_date)
    {
        failures.push("an order did not execute after its signal date".into());
    }
    let first_execution_date = dataset
        .spec
        .calendar
        .get(dataset.spec.calendar.len().saturating_sub(4))
        .copied();
    let saw_halt = projection.orders.iter().any(|order| {
        Some(order.attempt_date) == first_execution_date
            && order.symbol == "B"
            && order.reason.as_deref() == Some("HALTED")
    });
    if !saw_halt {
        failures.push("fixture HALTED order was not rejected".into());
    }
    let final_row = projection
        .ledger
        .last()
        .context("Fast Event ledger is empty")?;
    let prior_row = projection
        .ledger
        .get(projection.ledger.len().saturating_sub(2))
        .context("Fast Event ledger has fewer than two rows")?;
    let stale_b_mark = final_row
        .holdings
        .iter()
        .find(|holding| holding.symbol == "B")
        .map(|holding| holding.mark_price);
    let prior_b_mark = prior_row
        .holdings
        .iter()
        .find(|holding| holding.symbol == "B")
        .map(|holding| holding.mark_price);
    if stale_b_mark != prior_b_mark {
        failures.push("missing final B bar did not retain the last observed mark".into());
    }
    if projection.fills.is_empty() || projection.summary.total_cost <= 0.0 {
        failures.push("fills or non-zero fixed transaction costs are missing".into());
    }
    if projection
        .ledger
        .iter()
        .any(|row| row.cash < -FLOAT_TOLERANCE)
    {
        failures.push("account cash became negative".into());
    }
    Ok(failures)
}

/// Minimal, independent L1 event path for S1. The first close creates an equal-weight
/// target across the fixture universe; the following sessions submit market buys at
/// their opens. Targets blocked by status or missing bars remain pending. Holdings are
/// marked at the latest observed close, never at a fabricated zero price.
fn run_fast_event(dataset: &PreparedDataset) -> FastEventRun {
    let end_to_end_started = Instant::now();
    let calendar = &dataset.spec.calendar;
    let initial_cash = dataset.spec.account.initial_cash;
    let targets = dataset
        .spec
        .instruments
        .iter()
        .map(|i| i.symbol.clone())
        .collect::<Vec<_>>();
    let decision_index = calendar.len().saturating_sub(5);
    let decision_date = calendar[decision_index];
    let budget = initial_cash / targets.len() as f64;
    let lot = dataset.spec.account.lot_size;
    let initialization_started = Instant::now();
    let mut by_day: BTreeMap<NaiveDate, BTreeMap<&str, &Bar>> = BTreeMap::new();
    for bar in &dataset.bars {
        by_day
            .entry(bar.trade_date)
            .or_default()
            .insert(&bar.symbol, bar);
    }
    let initialization_ns = initialization_started.elapsed().as_nanos();
    let event_processing_started = Instant::now();
    let mut cash = initial_cash;
    let mut positions: BTreeMap<String, i64> = BTreeMap::new();
    let mut marks: BTreeMap<String, f64> = BTreeMap::new();
    let mut orders = Vec::new();
    let mut fills = Vec::new();
    let mut ledger = Vec::new();
    let mut pending = Vec::new();
    let mut commission_total = 0.0;
    let mut tax_total = 0.0;
    let mut slippage_total = 0.0;
    for (day_index, date) in calendar.iter().enumerate() {
        let day_bars = by_day.get(date);
        if day_index > decision_index {
            for symbol in &targets {
                if positions.contains_key(symbol) {
                    continue;
                }
                let order = FastEventOrder {
                    decision_date,
                    attempt_date: *date,
                    symbol: symbol.clone(),
                    side: "BUY".into(),
                    quantity: 0,
                    reason: None,
                };
                let Some(bar) = day_bars.and_then(|bars| bars.get(symbol.as_str())) else {
                    let mut blocked = order;
                    blocked.reason = Some("missing_open".into());
                    orders.push(blocked);
                    continue;
                };
                if dataset
                    .statuses
                    .get(&(*date, symbol.clone()))
                    .is_none_or(|s| {
                        !s.is_tradable
                            || s.trade_status.as_deref() != Some("TRADABLE")
                            || s.sources.as_deref().is_none_or(str::is_empty)
                    })
                {
                    let mut blocked = order;
                    blocked.reason = Some(
                        dataset
                            .statuses
                            .get(&(*date, symbol.clone()))
                            .and_then(|s| s.trade_status.clone())
                            .unwrap_or_else(|| "missing_status".into()),
                    );
                    orders.push(blocked);
                    continue;
                }
                let costs = &dataset.spec.costs;
                let fill_price = bar.open * (1.0 + costs.buy_slippage_bps / 10_000.0);
                let mut quantity = ((budget / fill_price) as i64 / lot) * lot;
                let commission_for = |qty: i64| {
                    (fill_price * qty as f64 * costs.commission_rate).max(costs.minimum_commission)
                };
                while quantity > 0
                    && fill_price * quantity as f64
                        + commission_for(quantity)
                        + fill_price * quantity as f64 * costs.buy_tax_rate
                        > cash
                {
                    quantity -= lot;
                }
                if quantity > 0 {
                    let gross = fill_price * quantity as f64;
                    let commission = commission_for(quantity);
                    let tax = gross * costs.buy_tax_rate;
                    let slippage_cost = (fill_price - bar.open) * quantity as f64;
                    cash -= gross + commission + tax;
                    positions.insert(symbol.clone(), quantity);
                    fills.push(FastEventFill {
                        date: *date,
                        symbol: symbol.clone(),
                        side: "BUY".into(),
                        quantity,
                        reference_price: bar.open,
                        fill_price,
                        gross_value: gross,
                        commission,
                        tax,
                        slippage_cost,
                    });
                    orders.push(FastEventOrder { quantity, ..order });
                    commission_total += commission;
                    tax_total += tax;
                    slippage_total += slippage_cost;
                }
            }
        }
        if let Some(bars) = day_bars {
            for (symbol, bar) in bars {
                marks.insert((*symbol).to_owned(), bar.close);
            }
        }
        let holdings = positions
            .iter()
            .map(|(symbol, quantity)| FastEventHolding {
                symbol: symbol.clone(),
                quantity: *quantity,
                mark_price: marks[symbol],
            })
            .collect::<Vec<_>>();
        let nav = cash
            + holdings
                .iter()
                .map(|h| h.quantity as f64 * h.mark_price)
                .sum::<f64>();
        ledger.push(FastEventLedger {
            date: *date,
            cash,
            holdings,
            nav,
        });
    }
    for symbol in &targets {
        if !positions.contains_key(symbol) {
            pending.push(FastEventPendingTarget {
                decision_date,
                attempt_date: *calendar.last().expect("validated non-empty calendar"),
                symbol: symbol.clone(),
                reason: "no_successful_open_fill_before_dataset_end".into(),
            });
        }
    }
    let final_equity = ledger.last().map_or(initial_cash, |row| row.nav);
    let projection = FastEventProjection {
        orders,
        fills,
        ledger,
        unexecuted_targets: pending,
        summary: FastEventSummary {
            initial_cash,
            final_equity,
            final_positions: positions,
            commission: commission_total,
            tax: tax_total,
            slippage_cost: slippage_total,
            total_cost: commission_total + tax_total + slippage_total,
        },
    };
    let event_processing_ns = event_processing_started.elapsed().as_nanos();
    FastEventRun {
        projection,
        initialization_ns,
        event_processing_ns,
        end_to_end_ns: end_to_end_started.elapsed().as_nanos(),
    }
}

/// Complete small S2 vector path using symbol-major structure-of-arrays storage.
/// Signal rows use close(T) and prior observed closes only; returns are evaluation labels.
fn run_soa(
    dataset: &PreparedDataset,
    config: &ExperimentConfig,
    measure: bool,
) -> (B1SoaProjection, Vec<u128>, B1PhaseSamples) {
    let mut symbols = dataset
        .spec
        .instruments
        .iter()
        .map(|i| i.symbol.clone())
        .collect::<Vec<_>>();
    symbols.sort();
    let days = dataset.spec.calendar.len();
    let mut closes = vec![f64::NAN; symbols.len() * days];
    for bar in &dataset.bars {
        let si = symbols.binary_search(&bar.symbol).expect("sorted symbols");
        let di = dataset
            .spec
            .calendar
            .binary_search(&bar.trade_date)
            .unwrap();
        closes[si * days + di] = bar.close;
    }
    let calculate = || {
        let mut rankings = Vec::new();
        let mut targets = Vec::new();
        let mut target_weights = Vec::new();
        let mut phases = B1PhaseSamples::default();
        let mut eligible_dates = 0usize;
        for di in 0..days {
            let factor_started = Instant::now();
            let mut candidates = Vec::new();
            for (si, symbol) in symbols.iter().enumerate() {
                if closes[si * days + di].is_nan() {
                    continue;
                }
                let observed = (0..=di)
                    .filter(|day| !closes[si * days + day].is_nan())
                    .collect::<Vec<_>>();
                if let Some(score) = soa_factor_score(
                    &closes[si * days..(si + 1) * days],
                    &observed,
                    di,
                    &config.strategy,
                ) {
                    candidates.push(RankingCandidate {
                        symbol: symbol.clone(),
                        score,
                    });
                }
            }
            phases.factor_ns.push(factor_started.elapsed().as_nanos());
            let rank_started = Instant::now();
            candidates.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then_with(|| a.symbol.cmp(&b.symbol))
            });
            if !candidates.is_empty() {
                let rebalance = config.strategy.rebalance_every.max(1);
                let should_rebalance = eligible_dates % rebalance == 0;
                eligible_dates += 1;
                let date = dataset.spec.calendar[di];
                if should_rebalance {
                    let chosen = candidates
                        .iter()
                        .take(config.strategy.top_n)
                        .map(|c| c.symbol.clone())
                        .collect::<Vec<_>>();
                    target_weights.extend(chosen.iter().map(|symbol| WeightedTarget {
                        date,
                        symbol: symbol.clone(),
                        weight: 1.0 / chosen.len() as f64,
                    }));
                    targets.push(SignalProjection {
                        date,
                        target_symbols: chosen,
                    });
                }
                rankings.push(RankingProjection { date, candidates });
            }
            phases
                .ranking_topk_weights_ns
                .push(rank_started.elapsed().as_nanos());
        }
        let return_started = Instant::now();
        let mut portfolio_returns = Vec::new();
        for signal in &targets {
            let Some(di) = dataset.spec.calendar.iter().position(|d| *d == signal.date) else {
                continue;
            };
            if di + 1 >= days {
                continue;
            }
            let mut values = Vec::new();
            for sym in &signal.target_symbols {
                let si = symbols.binary_search(sym).unwrap();
                let a = closes[si * days + di];
                let b = closes[si * days + di + 1];
                if !a.is_nan() && !b.is_nan() {
                    values.push(b / a - 1.0)
                }
            }
            let return_pct = if values.len() == signal.target_symbols.len() && !values.is_empty() {
                Some(values.iter().sum::<f64>() / values.len() as f64)
            } else {
                None
            };
            portfolio_returns.push(PortfolioReturn {
                from: signal.date,
                to: dataset.spec.calendar[di + 1],
                return_pct,
            });
        }
        phases
            .return_projection_ns
            .push(return_started.elapsed().as_nanos());
        (
            B1SoaProjection {
                rankings,
                targets,
                target_weights,
                portfolio_returns,
            },
            phases,
        )
    };
    let mut samples = Vec::with_capacity(MEASUREMENT_RUNS);
    let mut phase_samples = B1PhaseSamples::default();
    let (mut projection, _) = calculate();
    if measure {
        let _ = calculate();
        for _ in 0..MEASUREMENT_RUNS {
            let now = Instant::now();
            let (measured_projection, phases) = std::hint::black_box(calculate());
            projection = measured_projection;
            samples.push(now.elapsed().as_nanos());
            phase_samples.factor_ns.push(phases.factor_ns.iter().sum());
            phase_samples
                .ranking_topk_weights_ns
                .push(phases.ranking_topk_weights_ns.iter().sum());
            phase_samples
                .return_projection_ns
                .push(phases.return_projection_ns.iter().sum());
        }
    }
    (projection, samples, phase_samples)
}

/// S2 over symbol columns in an Arrow RecordBatch. Batch construction is included in
/// end-to-end timings and also reported separately so conversion cost stays visible.
fn run_arrow(
    dataset: &PreparedDataset,
    config: &ExperimentConfig,
    measure: bool,
) -> (B1SoaProjection, Vec<u128>, B1PhaseSamples, Vec<u128>) {
    let mut symbols = dataset
        .spec
        .instruments
        .iter()
        .map(|i| i.symbol.clone())
        .collect::<Vec<_>>();
    symbols.sort();
    let days = dataset.spec.calendar.len();
    let schema = Arc::new(Schema::new(
        symbols
            .iter()
            .map(|symbol| Field::new(symbol, DataType::Float64, true))
            .collect::<Vec<_>>(),
    ));
    let mut aligned = vec![vec![None; days]; symbols.len()];
    for bar in &dataset.bars {
        let si = symbols.binary_search(&bar.symbol).expect("sorted symbols");
        let di = dataset
            .spec
            .calendar
            .binary_search(&bar.trade_date)
            .unwrap();
        aligned[si][di] = Some(bar.close);
    }
    let batch = arrow_batch(&schema, &aligned);
    let calculate = |batch: &RecordBatch| {
        let mut rankings = Vec::new();
        let mut targets = Vec::new();
        let mut target_weights = Vec::new();
        let mut phases = B1PhaseSamples::default();
        let mut eligible_dates = 0usize;
        for di in 0..days {
            let factor_started = Instant::now();
            let mut candidates = Vec::new();
            for (si, symbol) in symbols.iter().enumerate() {
                let close = batch
                    .column(si)
                    .as_any()
                    .downcast_ref::<Float64Array>()
                    .unwrap();
                if !close.is_valid(di) {
                    continue;
                }
                let observed = (0..=di)
                    .filter(|day| close.is_valid(*day))
                    .collect::<Vec<_>>();
                if let Some(score) = arrow_factor_score(close, &observed, di, &config.strategy) {
                    candidates.push(RankingCandidate {
                        symbol: symbol.clone(),
                        score,
                    });
                }
            }
            phases.factor_ns.push(factor_started.elapsed().as_nanos());
            let rank_started = Instant::now();
            candidates.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then_with(|| a.symbol.cmp(&b.symbol))
            });
            if !candidates.is_empty() {
                let rebalance = config.strategy.rebalance_every.max(1);
                let should_rebalance = eligible_dates % rebalance == 0;
                eligible_dates += 1;
                let date = dataset.spec.calendar[di];
                if should_rebalance {
                    let chosen = candidates
                        .iter()
                        .take(config.strategy.top_n)
                        .map(|c| c.symbol.clone())
                        .collect::<Vec<_>>();
                    target_weights.extend(chosen.iter().map(|symbol| WeightedTarget {
                        date,
                        symbol: symbol.clone(),
                        weight: 1.0 / chosen.len() as f64,
                    }));
                    targets.push(SignalProjection {
                        date,
                        target_symbols: chosen,
                    });
                }
                rankings.push(RankingProjection { date, candidates });
            }
            phases
                .ranking_topk_weights_ns
                .push(rank_started.elapsed().as_nanos());
        }
        let return_started = Instant::now();
        let mut portfolio_returns = Vec::new();
        for signal in &targets {
            let di = dataset.spec.calendar.binary_search(&signal.date).unwrap();
            if di + 1 >= days {
                continue;
            }
            let mut values = Vec::new();
            for symbol in &signal.target_symbols {
                let si = symbols.binary_search(symbol).unwrap();
                let close = batch
                    .column(si)
                    .as_any()
                    .downcast_ref::<Float64Array>()
                    .unwrap();
                if close.is_valid(di) && close.is_valid(di + 1) {
                    values.push(close.value(di + 1) / close.value(di) - 1.0);
                }
            }
            let return_pct = if values.len() == signal.target_symbols.len() && !values.is_empty() {
                Some(values.iter().sum::<f64>() / values.len() as f64)
            } else {
                None
            };
            portfolio_returns.push(PortfolioReturn {
                from: signal.date,
                to: dataset.spec.calendar[di + 1],
                return_pct,
            });
        }
        phases
            .return_projection_ns
            .push(return_started.elapsed().as_nanos());
        (
            B1SoaProjection {
                rankings,
                targets,
                target_weights,
                portfolio_returns,
            },
            phases,
        )
    };
    let mut conversion_samples = Vec::new();
    let mut samples = Vec::new();
    let mut phase_samples = B1PhaseSamples::default();
    let (mut projection, _) = calculate(&batch);
    if measure {
        let _ = calculate(&batch);
        for _ in 0..MEASUREMENT_RUNS {
            let started = Instant::now();
            let convert_started = Instant::now();
            let batch = arrow_batch(&schema, &aligned);
            conversion_samples.push(convert_started.elapsed().as_nanos());
            let (measured, phases) = std::hint::black_box(calculate(&batch));
            projection = measured;
            samples.push(started.elapsed().as_nanos());
            phase_samples.factor_ns.push(phases.factor_ns.iter().sum());
            phase_samples
                .ranking_topk_weights_ns
                .push(phases.ranking_topk_weights_ns.iter().sum());
            phase_samples
                .return_projection_ns
                .push(phases.return_projection_ns.iter().sum());
        }
    }
    (projection, samples, phase_samples, conversion_samples)
}

fn arrow_batch(schema: &Arc<Schema>, columns: &[Vec<Option<f64>>]) -> RecordBatch {
    let columns = columns
        .iter()
        .map(|values| Arc::new(Float64Array::from(values.clone())) as arrow_array::ArrayRef)
        .collect::<Vec<_>>();
    RecordBatch::try_new(Arc::clone(schema), columns).expect("valid Arrow close batch")
}

#[cfg(test)]
mod arrow_tests {
    use super::{DatasetInput, experiment_config, prepare_dataset, run_arrow};
    use chrono::NaiveDate;
    use std::{fs, path::Path};

    fn prepared_fixture() -> super::PreparedDataset {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("poc/poc0-benchmark/fixtures/dataset-v1.json");
        let input: DatasetInput = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        prepare_dataset(input).unwrap()
    }

    #[test]
    fn arrow_checks_window_ties_missing_bars_and_return_dates() {
        let mut missing_next_bar = prepared_fixture();
        let missing_date = NaiveDate::from_ymd_opt(2026, 1, 8).unwrap();
        missing_next_bar
            .bars
            .retain(|bar| !(bar.symbol == "C" && bar.trade_date == missing_date));
        let config = experiment_config(&missing_next_bar.spec);
        let (projection, _, _, _) = run_arrow(&missing_next_bar, &config, false);
        assert_eq!(
            projection.rankings[0].date,
            NaiveDate::from_ymd_opt(2026, 1, 7).unwrap()
        );
        assert_eq!(projection.targets[0].target_symbols, ["C"]);
        assert_eq!(
            projection.portfolio_returns[0].from,
            projection.targets[0].date
        );
        assert_eq!(projection.portfolio_returns[0].to, missing_date);
        assert_eq!(projection.portfolio_returns[0].return_pct, None);
        let final_symbols = projection
            .rankings
            .last()
            .unwrap()
            .candidates
            .iter()
            .map(|row| row.symbol.as_str())
            .collect::<Vec<_>>();
        assert_eq!(final_symbols, ["C", "A"]);

        let tied = prepared_fixture();
        let tie_date = NaiveDate::from_ymd_opt(2026, 1, 7).unwrap();
        let mut config = experiment_config(&tied.spec);
        config.strategy.short_momentum_weight = 0.0;
        config.strategy.long_momentum_weight = 0.0;
        config.strategy.volatility_weight = 0.0;
        let (projection, _, _, _) = run_arrow(&tied, &config, false);
        let first_day = &projection.rankings[0];
        assert_eq!(first_day.date, tie_date);
        assert_eq!(first_day.candidates[0].symbol, "A");
        assert_eq!(first_day.candidates[1].symbol, "C");
        assert_eq!(first_day.candidates[2].symbol, "B");
        assert_eq!(first_day.candidates[0].score, first_day.candidates[1].score);
    }
}

fn run_polars(
    dataset: &PreparedDataset,
    config: &ExperimentConfig,
    measure: bool,
) -> Result<(B1SoaProjection, Vec<u128>, B1PhaseSamples, Vec<u128>)> {
    let mut symbols = dataset
        .spec
        .instruments
        .iter()
        .map(|i| i.symbol.clone())
        .collect::<Vec<_>>();
    symbols.sort();
    let days = dataset.spec.calendar.len();
    let mut closes = vec![f64::NAN; symbols.len() * days];
    for bar in &dataset.bars {
        let si = symbols.binary_search(&bar.symbol).expect("sorted symbols");
        let di = dataset
            .spec
            .calendar
            .binary_search(&bar.trade_date)
            .unwrap();
        closes[si * days + di] = bar.close;
    }
    let calculate = || -> Result<(B1SoaProjection, B1PhaseSamples, u128)> {
        let strategy = &config.strategy;
        let short = strategy
            .momentum_short_days
            .unwrap_or(strategy.lookback_days);
        let long = strategy
            .momentum_long_days
            .unwrap_or(strategy.lookback_days);
        let volatility = strategy.volatility_window.unwrap_or(short.max(2));
        let mut phases = B1PhaseSamples::default();
        let mut conversion_ns = 0;
        let mut feature_rows = Vec::new();
        for (si, symbol) in symbols.iter().enumerate() {
            let factor_started = Instant::now();
            let observed = (0..days)
                .filter(|day| closes[si * days + day].is_finite())
                .collect::<Vec<_>>();
            if observed.is_empty() {
                continue;
            }
            let observed_closes = observed
                .iter()
                .map(|day| closes[si * days + day])
                .collect::<Vec<_>>();
            let conversion_started = Instant::now();
            let frame = DataFrame::new(
                observed_closes.len(),
                vec![Series::new("close".into(), observed_closes).into()],
            )?;
            conversion_ns += conversion_started.elapsed().as_nanos();
            let mut return_sum = lit(0.0);
            let mut return_squared_sum = lit(0.0);
            for lag in 0..volatility {
                let daily_return = col("close").shift(lit(lag as i64))
                    / col("close").shift(lit((lag + 1) as i64))
                    - lit(1.0);
                return_sum = return_sum + daily_return.clone();
                return_squared_sum = return_squared_sum + daily_return.clone() * daily_return;
            }
            let volatility_expr = ((return_squared_sum
                - return_sum.clone() * return_sum / lit(volatility as f64))
                / lit((volatility - 1) as f64))
            .sqrt();
            let scored = frame
                .lazy()
                .with_columns([
                    (col("close") / col("close").shift(lit(short as i64)) - lit(1.0))
                        .alias("short"),
                    (col("close") / col("close").shift(lit(long as i64)) - lit(1.0)).alias("long"),
                    volatility_expr.alias("volatility"),
                ])
                .with_column(
                    (col("short") * lit(strategy.short_momentum_weight)
                        + col("long") * lit(strategy.long_momentum_weight)
                        - col("volatility") * lit(strategy.volatility_weight))
                    .alias("score"),
                )
                .select([col("score")])
                .collect()?;
            let scores = scored.column("score")?.f64()?;
            feature_rows.push((
                symbol.clone(),
                observed,
                (0..scores.len())
                    .map(|row| scores.get(row))
                    .collect::<Vec<_>>(),
            ));
            phases.factor_ns.push(factor_started.elapsed().as_nanos());
        }
        let mut projection = B1SoaProjection {
            rankings: Vec::new(),
            targets: Vec::new(),
            target_weights: Vec::new(),
            portfolio_returns: Vec::new(),
        };
        let mut eligible_dates = 0usize;
        for di in 0..days {
            let rank_started = Instant::now();
            let mut day_column = Vec::new();
            let mut symbol_column = Vec::new();
            let mut score_column = Vec::new();
            let mut order_column = Vec::new();
            for (symbol, observed, scores) in &feature_rows {
                let Ok(row) = observed.binary_search(&di) else {
                    continue;
                };
                let Some(score) = scores[row].filter(|score| score.is_finite()) else {
                    continue;
                };
                let order = sortable_float_key(score);
                day_column.push(di as u32);
                symbol_column.push(symbol.clone());
                score_column.push(score);
                order_column.push(order);
            }
            let conversion_started = Instant::now();
            let frame = DataFrame::new(
                day_column.len(),
                vec![
                    Series::new("day".into(), day_column).into(),
                    Series::new("symbol".into(), symbol_column).into(),
                    Series::new("score".into(), score_column).into(),
                    Series::new("score_order".into(), order_column).into(),
                ],
            )?;
            conversion_ns += conversion_started.elapsed().as_nanos();
            let evaluated = frame
                .lazy()
                .sort_by_exprs(
                    [col("score_order"), col("symbol")],
                    SortMultipleOptions::default().with_order_descending_multi([true, false]),
                )
                .collect()?;
            if evaluated.height() > 0 {
                let scores = evaluated.column("score")?.f64()?;
                let sorted_symbols = evaluated.column("symbol")?.str()?;
                let candidates = (0..evaluated.height())
                    .map(|row| RankingCandidate {
                        symbol: sorted_symbols.get(row).unwrap().to_owned(),
                        score: scores.get(row).unwrap(),
                    })
                    .collect::<Vec<_>>();
                let rebalance = strategy.rebalance_every.max(1);
                let should_rebalance = eligible_dates % rebalance == 0;
                eligible_dates += 1;
                let date = dataset.spec.calendar[di];
                if should_rebalance {
                    let mut top = evaluated
                        .clone()
                        .lazy()
                        .limit(strategy.top_n as u32)
                        .collect()?;
                    let weight = 1.0 / top.height().max(1) as f64;
                    top.with_column(
                        Series::new("weight".into(), vec![weight; top.height()]).into(),
                    )?;
                    let top_symbols = top.column("symbol")?.str()?;
                    let weights = top.column("weight")?.f64()?;
                    let chosen = (0..top.height())
                        .map(|row| top_symbols.get(row).unwrap().to_owned())
                        .collect::<Vec<_>>();
                    projection
                        .target_weights
                        .extend((0..top.height()).map(|row| WeightedTarget {
                            date,
                            symbol: chosen[row].clone(),
                            weight: weights.get(row).unwrap(),
                        }));
                    projection.targets.push(SignalProjection {
                        date,
                        target_symbols: chosen,
                    });
                }
                projection
                    .rankings
                    .push(RankingProjection { date, candidates });
            }
            phases
                .ranking_topk_weights_ns
                .push(rank_started.elapsed().as_nanos());
        }
        let return_started = Instant::now();
        for target in &projection.targets {
            let di = dataset.spec.calendar.binary_search(&target.date).unwrap();
            if di + 1 >= days {
                continue;
            }
            let mut from_values = Vec::new();
            let mut to_values = Vec::new();
            for symbol in &target.target_symbols {
                let si = symbols.binary_search(symbol).unwrap();
                let from = closes[si * days + di];
                let to = closes[si * days + di + 1];
                from_values.push(from.is_finite().then_some(from));
                to_values.push(to.is_finite().then_some(to));
            }
            let conversion_started = Instant::now();
            let frame = DataFrame::new(
                from_values.len(),
                vec![
                    Series::new("from".into(), from_values).into(),
                    Series::new("to".into(), to_values).into(),
                ],
            )?;
            conversion_ns += conversion_started.elapsed().as_nanos();
            let returns = frame
                .lazy()
                .select([
                    ((col("to") / col("from")) - lit(1.0))
                        .mean()
                        .alias("mean_return"),
                    ((col("to") / col("from")) - lit(1.0))
                        .count()
                        .alias("available_count"),
                ])
                .collect()?;
            let available = returns
                .column("available_count")?
                .u32()?
                .get(0)
                .unwrap_or(0) as usize;
            let mean = returns.column("mean_return")?.f64()?.get(0);
            let return_pct = if available == target.target_symbols.len() {
                mean
            } else {
                None
            };
            projection.portfolio_returns.push(PortfolioReturn {
                from: target.date,
                to: dataset.spec.calendar[di + 1],
                return_pct,
            });
        }
        phases
            .return_projection_ns
            .push(return_started.elapsed().as_nanos());
        Ok((projection, phases, conversion_ns))
    };
    let (mut projection, _, _) = calculate()?;
    let mut samples = Vec::new();
    let mut phases_out = B1PhaseSamples::default();
    let mut conversion_samples = Vec::new();
    if measure {
        let _ = calculate()?;
        for _ in 0..MEASUREMENT_RUNS {
            let started = Instant::now();
            let (result, phases, conversion) = std::hint::black_box(calculate()?);
            projection = result;
            samples.push(started.elapsed().as_nanos());
            phases_out.factor_ns.push(phases.factor_ns.iter().sum());
            phases_out
                .ranking_topk_weights_ns
                .push(phases.ranking_topk_weights_ns.iter().sum());
            phases_out
                .return_projection_ns
                .push(phases.return_projection_ns.iter().sum());
            conversion_samples.push(conversion);
        }
    }
    Ok((projection, samples, phases_out, conversion_samples))
}

/// Map finite IEEE-754 values to unsigned keys whose ascending order matches `total_cmp`.
fn sortable_float_key(value: f64) -> u64 {
    let bits = value.to_bits();
    if bits >> 63 == 1 {
        !bits
    } else {
        bits ^ (1 << 63)
    }
}

fn soa_factor_score(
    close: &[f64],
    observed_days: &[usize],
    signal_day: usize,
    strategy: &StrategyConfig,
) -> Option<f64> {
    let short = strategy
        .momentum_short_days
        .unwrap_or(strategy.lookback_days);
    let long = strategy
        .momentum_long_days
        .unwrap_or(strategy.lookback_days);
    let volatility = strategy.volatility_window.unwrap_or(short.max(2));
    let observation = observed_days.len().checked_sub(1)?;
    let required = short.max(long).max(volatility);
    if observed_days[observation] != signal_day || observation < required || volatility <= 1 {
        return None;
    }
    let now = close[signal_day];
    let short_return = now / close[observed_days[observation - short]] - 1.0;
    let long_return = now / close[observed_days[observation - long]] - 1.0;
    let returns = (observation - volatility + 1..=observation)
        .map(|j| close[observed_days[j]] / close[observed_days[j - 1]] - 1.0)
        .collect::<Vec<_>>();
    let mean = returns.iter().sum::<f64>() / volatility as f64;
    let variance =
        returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (volatility - 1) as f64;
    let score = strategy.short_momentum_weight * short_return
        + strategy.long_momentum_weight * long_return
        - strategy.volatility_weight * variance.sqrt();
    score.is_finite().then_some(score)
}

fn arrow_factor_score(
    close: &Float64Array,
    observed_days: &[usize],
    signal_day: usize,
    strategy: &StrategyConfig,
) -> Option<f64> {
    let short = strategy
        .momentum_short_days
        .unwrap_or(strategy.lookback_days);
    let long = strategy
        .momentum_long_days
        .unwrap_or(strategy.lookback_days);
    let volatility = strategy.volatility_window.unwrap_or(short.max(2));
    let observation = observed_days.len().checked_sub(1)?;
    let required = short.max(long).max(volatility);
    if observed_days[observation] != signal_day || observation < required || volatility <= 1 {
        return None;
    }
    let now = close.value(signal_day);
    let short_return = now / close.value(observed_days[observation - short]) - 1.0;
    let long_return = now / close.value(observed_days[observation - long]) - 1.0;
    let returns = (observation - volatility + 1..=observation)
        .map(|j| close.value(observed_days[j]) / close.value(observed_days[j - 1]) - 1.0)
        .collect::<Vec<_>>();
    let mean = returns.iter().sum::<f64>() / volatility as f64;
    let variance =
        returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (volatility - 1) as f64;
    let score = strategy.short_momentum_weight * short_return
        + strategy.long_momentum_weight * long_return
        - strategy.volatility_weight * variance.sqrt();
    score.is_finite().then_some(score)
}

fn projections_match(left: &B1SoaProjection, right: &B1SoaProjection) -> bool {
    compare_values(
        &serde_json::to_value(left).expect("serializable Arrow projection"),
        &serde_json::to_value(right).expect("serializable SoA projection"),
        "$".into(),
    )
    .is_empty()
}

fn prepare_dataset(spec: DatasetInput) -> Result<PreparedDataset> {
    ensure!(!spec.dataset_version.is_empty(), "dataset version is empty");
    ensure!(
        !spec.source_identity.is_empty(),
        "dataset source identity is empty"
    );
    ensure!(!spec.calendar.is_empty(), "dataset calendar is empty");
    ensure!(
        spec.calendar.windows(2).all(|pair| pair[0] < pair[1]),
        "dataset calendar must be strictly increasing"
    );
    ensure!(
        spec.calendar
            .iter()
            .all(|date| !matches!(date.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun)),
        "fixed synthetic market calendar contains a weekend"
    );
    ensure!(
        spec.timezone == "Asia/Shanghai",
        "unsupported fixture timezone"
    );
    ensure!(
        spec.strategy.tie_break == "symbol ascending",
        "unsupported tie-break rule {}",
        spec.strategy.tie_break
    );
    ensure!(
        spec.strategy.momentum_short_days > 0
            && spec.strategy.momentum_long_days > 0
            && spec.strategy.volatility_window > 1
            && spec.strategy.top_n > 0
            && spec.strategy.rebalance_every > 0,
        "strategy windows and sizes must be positive"
    );
    ensure!(
        spec.bar_defaults.open.is_finite()
            && spec.bar_defaults.open > 0.0
            && spec.bar_defaults.volume.is_finite()
            && spec.bar_defaults.volume >= 0.0
            && spec.bar_defaults.amount.is_finite()
            && spec.bar_defaults.amount >= 0.0,
        "synthetic bar defaults must be finite and open must be positive"
    );
    ensure!(
        spec.bar_defaults.high_rule == "max(open, close)"
            && spec.bar_defaults.low_rule == "min(open, close)",
        "unsupported synthetic OHLC rule"
    );
    let bar_open_time = timestamp(spec.calendar[0], &spec.bar_defaults.open_available_at)?;
    let bar_close_time = timestamp(spec.calendar[0], &spec.bar_defaults.close_available_at)?;
    ensure!(
        bar_open_time < bar_close_time,
        "bar close availability must follow open"
    );
    let default_status_time = timestamp(
        spec.calendar[0],
        &spec.execution_status_default.available_at,
    )?;
    ensure!(
        default_status_time < bar_open_time,
        "execution status must be available before the session open"
    );
    spec.costs.validate()?;

    let mut symbols = BTreeSet::new();
    for instrument in &spec.instruments {
        ensure!(
            symbols.insert(instrument.symbol.clone()),
            "duplicate instrument symbol"
        );
        ensure!(
            !instrument.symbol.is_empty() && !instrument.currency.is_empty(),
            "instrument identity fields must be non-empty"
        );
        ensure!(
            instrument.closes.len() == spec.calendar.len(),
            "{} close series length does not match calendar",
            instrument.symbol
        );
        ensure!(
            instrument
                .closes
                .iter()
                .all(|price| price.is_finite() && *price > 0.0),
            "{} contains a non-positive or non-finite close",
            instrument.symbol
        );
    }
    ensure!(!spec.instruments.is_empty(), "dataset has no instruments");
    ensure!(
        !spec.execution_status_default.sources.is_empty(),
        "default execution status must identify its source"
    );

    let missing = spec
        .missing_bars
        .iter()
        .map(|item| (item.symbol.as_str(), item.date))
        .collect::<BTreeSet<_>>();
    ensure!(
        missing.len() == spec.missing_bars.len(),
        "duplicate missing-bar declaration"
    );
    for item in &spec.missing_bars {
        ensure!(
            symbols.contains(&item.symbol),
            "missing-bar symbol is not in dataset"
        );
        ensure!(
            spec.calendar.contains(&item.date),
            "missing-bar date is not in calendar"
        );
        ensure!(!item.reason.is_empty(), "missing-bar reason is empty");
    }

    let mut bars = Vec::new();
    let mut timed_bars = Vec::new();
    for instrument in &spec.instruments {
        for (index, date) in spec.calendar.iter().enumerate() {
            if missing.contains(&(instrument.symbol.as_str(), *date)) {
                continue;
            }
            let close = instrument.closes[index];
            let bar = Bar {
                symbol: instrument.symbol.clone(),
                name: instrument.symbol.clone(),
                trade_date: *date,
                open: spec.bar_defaults.open,
                high: spec.bar_defaults.open.max(close),
                low: spec.bar_defaults.open.min(close),
                close,
                volume: spec.bar_defaults.volume,
                amount: Some(spec.bar_defaults.amount),
            };
            timed_bars.push(TimedBar {
                symbol: bar.symbol.clone(),
                date: bar.trade_date,
                open: bar.open,
                high: bar.high,
                low: bar.low,
                close: bar.close,
                volume: bar.volume,
                amount: bar.amount.unwrap_or_default(),
                open_available_at: timestamp(*date, &spec.bar_defaults.open_available_at)?,
                close_available_at: timestamp(*date, &spec.bar_defaults.close_available_at)?,
            });
            bars.push(bar);
        }
    }
    bars.sort_by(|left, right| {
        (left.trade_date, &left.symbol).cmp(&(right.trade_date, &right.symbol))
    });
    timed_bars.sort_by(|left, right| (left.date, &left.symbol).cmp(&(right.date, &right.symbol)));

    let mut statuses = ExecutionStatusMap::new();
    let mut status_rows = Vec::new();
    for bar in &bars {
        let available_at = timestamp(bar.trade_date, &spec.execution_status_default.available_at)?;
        statuses.insert(
            (bar.trade_date, bar.symbol.clone()),
            ExecutionStatus {
                trade_status: Some(spec.execution_status_default.trade_status.clone()),
                is_tradable: spec.execution_status_default.is_tradable,
                sources: Some(spec.execution_status_default.sources.clone()),
            },
        );
        status_rows.push(TimedStatus {
            symbol: bar.symbol.clone(),
            date: bar.trade_date,
            trade_status: spec.execution_status_default.trade_status.clone(),
            is_tradable: spec.execution_status_default.is_tradable,
            sources: spec.execution_status_default.sources.clone(),
            available_at,
        });
    }
    let mut override_keys = BTreeSet::new();
    for override_input in &spec.execution_status_overrides {
        ensure!(
            !override_input.sources.is_empty(),
            "execution-status override must identify its source"
        );
        ensure!(
            symbols.contains(&override_input.symbol),
            "status symbol is not in dataset"
        );
        ensure!(
            spec.calendar.contains(&override_input.date),
            "status date is not in calendar"
        );
        ensure!(
            !missing.contains(&(override_input.symbol.as_str(), override_input.date)),
            "status override refers to a missing bar"
        );
        ensure!(
            override_keys.insert((override_input.date, override_input.symbol.clone())),
            "duplicate execution-status override"
        );
        let available_at = timestamp(override_input.date, &override_input.available_at)?;
        ensure!(
            available_at < timestamp(override_input.date, &spec.bar_defaults.open_available_at)?,
            "execution status override must be available before the session open"
        );
        statuses.insert(
            (override_input.date, override_input.symbol.clone()),
            ExecutionStatus {
                trade_status: Some(override_input.trade_status.clone()),
                is_tradable: override_input.is_tradable,
                sources: Some(override_input.sources.clone()),
            },
        );
        let row = status_rows
            .iter_mut()
            .find(|row| row.date == override_input.date && row.symbol == override_input.symbol)
            .context("execution-status override has no matching bar")?;
        row.trade_status = override_input.trade_status.clone();
        row.is_tradable = override_input.is_tradable;
        row.sources = override_input.sources.clone();
        row.available_at = available_at;
    }
    status_rows.sort_by(|left, right| (left.date, &left.symbol).cmp(&(right.date, &right.symbol)));

    let instrument_identities = spec
        .instruments
        .iter()
        .map(|item| InstrumentIdentity {
            symbol: item.symbol.clone(),
            currency: item.currency.clone(),
            price_precision: item.price_precision,
            quantity_precision: item.quantity_precision,
        })
        .collect::<Vec<_>>();
    let canonical = DatasetContent {
        dataset_version: spec.dataset_version.clone(),
        seed: spec.seed,
        seed_semantics: spec.seed_semantics.clone(),
        source_identity: spec.source_identity.clone(),
        calendar_basis: spec.calendar_basis.clone(),
        timezone: spec.timezone.clone(),
        calendar: spec.calendar.clone(),
        bar_defaults: BarDefaults {
            open: spec.bar_defaults.open,
            volume: spec.bar_defaults.volume,
            amount: spec.bar_defaults.amount,
            high_rule: spec.bar_defaults.high_rule.clone(),
            low_rule: spec.bar_defaults.low_rule.clone(),
            open_available_at: spec.bar_defaults.open_available_at.clone(),
            close_available_at: spec.bar_defaults.close_available_at.clone(),
        },
        instruments: instrument_identities
            .iter()
            .map(|item| InstrumentIdentity {
                symbol: item.symbol.clone(),
                currency: item.currency.clone(),
                price_precision: item.price_precision,
                quantity_precision: item.quantity_precision,
            })
            .collect(),
        bars: timed_bars,
        missing_bars: spec.missing_bars.clone(),
        execution_statuses: status_rows
            .iter()
            .map(|row| TimedStatus {
                symbol: row.symbol.clone(),
                date: row.date,
                trade_status: row.trade_status.clone(),
                is_tradable: row.is_tradable,
                sources: row.sources.clone(),
                available_at: row.available_at.clone(),
            })
            .collect(),
        time_model: TimeModelInput {
            signal: spec.time_model.signal.clone(),
            execution: spec.time_model.execution.clone(),
            execution_status: spec.time_model.execution_status.clone(),
            final_session: spec.time_model.final_session.clone(),
        },
        result_model_boundary: spec.result_model_boundary.clone(),
        account: AccountInput {
            initial_cash: spec.account.initial_cash,
            lot_size: spec.account.lot_size,
        },
        strategy: StrategyInput {
            name: spec.strategy.name.clone(),
            version: spec.strategy.version.clone(),
            momentum_short_days: spec.strategy.momentum_short_days,
            momentum_long_days: spec.strategy.momentum_long_days,
            volatility_window: spec.strategy.volatility_window,
            short_momentum_weight: spec.strategy.short_momentum_weight,
            long_momentum_weight: spec.strategy.long_momentum_weight,
            volatility_weight: spec.strategy.volatility_weight,
            trend_filter: spec.strategy.trend_filter,
            top_n: spec.strategy.top_n,
            rebalance_every: spec.strategy.rebalance_every,
            tie_break: spec.strategy.tie_break.clone(),
        },
        costs: CostConfig {
            commission_rate: spec.costs.commission_rate,
            minimum_commission: spec.costs.minimum_commission,
            buy_tax_rate: spec.costs.buy_tax_rate,
            sell_tax_rate: spec.costs.sell_tax_rate,
            buy_slippage_bps: spec.costs.buy_slippage_bps,
            sell_slippage_bps: spec.costs.sell_slippage_bps,
        },
    };
    let content_sha256 = sha256(&serde_json::to_vec(&canonical)?);
    Ok(PreparedDataset {
        spec,
        bars,
        statuses,
        status_rows,
        content_sha256,
        bar_count: canonical.bars.len(),
        instrument_identities,
    })
}

fn experiment_config(spec: &DatasetInput) -> ExperimentConfig {
    let strategy = StrategyConfig {
        lookback_days: spec
            .strategy
            .momentum_short_days
            .max(spec.strategy.momentum_long_days),
        top_n: spec.strategy.top_n,
        rebalance_every: spec.strategy.rebalance_every,
        momentum_short_days: Some(spec.strategy.momentum_short_days),
        momentum_long_days: Some(spec.strategy.momentum_long_days),
        volatility_window: Some(spec.strategy.volatility_window),
        trend_window: None,
        use_trend_filter: spec.strategy.trend_filter,
        short_momentum_weight: spec.strategy.short_momentum_weight,
        long_momentum_weight: spec.strategy.long_momentum_weight,
        volatility_weight: spec.strategy.volatility_weight,
    };
    ExperimentConfig {
        name: format!("POC-0 {}", spec.strategy.name),
        initial_cash: spec.account.initial_cash,
        lot_size: spec.account.lot_size,
        strategy,
        costs: CostConfig {
            commission_rate: spec.costs.commission_rate,
            minimum_commission: spec.costs.minimum_commission,
            buy_tax_rate: spec.costs.buy_tax_rate,
            sell_tax_rate: spec.costs.sell_tax_rate,
            buy_slippage_bps: spec.costs.buy_slippage_bps,
            sell_slippage_bps: spec.costs.sell_slippage_bps,
        },
        ..ExperimentConfig::default()
    }
}

fn candidate_config(spec: &DatasetInput, config: &ExperimentConfig) -> CandidateConfig {
    CandidateConfig {
        name: spec.strategy.name.clone(),
        version: spec.strategy.version.clone(),
        initial_cash: config.initial_cash,
        lot_size: config.lot_size,
        strategy: config.strategy.clone(),
        costs: CostConfig {
            commission_rate: config.costs.commission_rate,
            minimum_commission: config.costs.minimum_commission,
            buy_tax_rate: config.costs.buy_tax_rate,
            sell_tax_rate: config.costs.sell_tax_rate,
            buy_slippage_bps: config.costs.buy_slippage_bps,
            sell_slippage_bps: config.costs.sell_slippage_bps,
        },
        future_returns_used_for_decisions: false,
    }
}

fn run_candidate(dataset: &PreparedDataset, config: &ExperimentConfig) -> BacktestReport {
    backtest::run_momentum_with_statuses(&dataset.bars, &[], config, Some(&dataset.statuses))
}

fn project_result(
    dataset: &PreparedDataset,
    config: &ExperimentConfig,
    report: &BacktestReport,
) -> ResultProjection {
    let scores = rotation_scores(&dataset.bars, &config.strategy);
    let mut bars_by_date = BTreeMap::<NaiveDate, Vec<&Bar>>::new();
    for bar in &dataset.bars {
        bars_by_date.entry(bar.trade_date).or_default().push(bar);
    }
    let mut rankings = Vec::new();
    let mut signals = Vec::new();
    for (date, bars) in bars_by_date {
        let mut candidates = bars
            .iter()
            .filter_map(|bar| {
                scores
                    .get(&(date, bar.symbol.clone()))
                    .map(|score| RankingCandidate {
                        symbol: bar.symbol.clone(),
                        score: *score,
                    })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.symbol.cmp(&right.symbol))
        });
        if !candidates.is_empty() {
            signals.push(SignalProjection {
                date,
                target_symbols: candidates
                    .iter()
                    .take(config.strategy.top_n)
                    .map(|candidate| candidate.symbol.clone())
                    .collect(),
            });
            rankings.push(RankingProjection { date, candidates });
        }
    }
    let fills = report
        .trades
        .iter()
        .map(|trade| FillProjection {
            date: trade.date,
            symbol: trade.symbol.clone(),
            side: trade.side.clone(),
            quantity: trade.quantity,
            reference_price: trade.reference_price,
            fill_price: trade.fill_price,
            gross_value: trade.gross_value,
            commission: trade.commission,
            tax: trade.tax,
            slippage_cost: trade.slippage_cost,
        })
        .collect();
    let unexecuted_orders = report
        .unexecuted_orders
        .iter()
        .map(|order| UnexecutedOrderProjection {
            decision_date: order.decision_date,
            attempt_date: order.attempt_date,
            symbol: order.symbol.clone(),
            side: order.side.clone(),
            desired_quantity: order.desired_quantity,
            trade_status: order.trade_status.clone(),
            status_sources: order.status_sources.clone(),
            reason: order.reason.clone(),
        })
        .collect();
    let ledger = report
        .position_curve
        .iter()
        .zip(&report.equity_curve)
        .map(|(position, equity)| LedgerProjection {
            date: equity.date,
            cash: position.cash,
            holdings: position
                .holdings
                .iter()
                .map(|holding| HoldingProjection {
                    symbol: holding.symbol.clone(),
                    quantity: holding.quantity,
                    mark_price: holding.mark_price,
                })
                .collect(),
            nav: equity.equity,
        })
        .collect();
    ResultProjection {
        rankings,
        signals,
        fills,
        unexecuted_orders,
        ledger,
        summary: SummaryProjection {
            initial_cash: report.metrics.initial_cash,
            final_equity: report.metrics.final_equity,
            final_positions: report
                .final_positions
                .iter()
                .filter(|(_, quantity)| **quantity > 0)
                .map(|(symbol, quantity)| (symbol.clone(), *quantity))
                .collect(),
            trade_count: report.metrics.trade_count,
            commission: report.metrics.commission,
            tax: report.metrics.tax,
            slippage_cost: report.metrics.slippage_cost,
            total_cost: report.metrics.total_cost,
        },
    }
}

fn accounting_checks(report: &BacktestReport) -> Vec<String> {
    let mut failures = Vec::new();
    for (equity, position) in report.equity_curve.iter().zip(&report.position_curve) {
        let marked_value = position
            .holdings
            .iter()
            .map(|holding| holding.quantity as f64 * holding.mark_price)
            .sum::<f64>();
        if (position.cash + marked_value - equity.equity).abs() > FLOAT_TOLERANCE {
            failures.push(format!("{} NAV != cash + marked holdings", equity.date));
        }
        if position.cash < -FLOAT_TOLERANCE {
            failures.push(format!("{} cash is negative", equity.date));
        }
        if position.holdings.iter().any(|holding| holding.quantity < 0) {
            failures.push(format!("{} contains a short position", equity.date));
        }
    }
    if report.trades.iter().any(|trade| trade.quantity <= 0) {
        failures.push("fill quantity must be positive".into());
    }
    failures
}

fn time_checks(
    report: &BacktestReport,
    projection: &ResultProjection,
    calendar: &[NaiveDate],
) -> Vec<String> {
    let mut failures = Vec::new();
    for fill in &projection.fills {
        let Some(execution_index) = calendar.iter().position(|date| *date == fill.date) else {
            failures.push(format!(
                "fill {} falls outside the dataset calendar",
                fill.date
            ));
            continue;
        };
        if execution_index == 0 {
            failures.push(format!(
                "fill {} has no prior close decision session",
                fill.date
            ));
        } else if !projection
            .signals
            .iter()
            .any(|signal| signal.date == calendar[execution_index - 1])
        {
            failures.push(format!(
                "fill {} has no signal on the preceding market session",
                fill.date
            ));
        }
    }
    if report
        .unexecuted_orders
        .iter()
        .any(|order| order.side == "TARGET" && order.reason != "no_future_execution_session")
    {
        failures.push("pending target has an unexpected terminal reason".into());
    }
    failures
}

fn measure_candidate(
    dataset: &PreparedDataset,
    config: &ExperimentConfig,
    expected: &Value,
) -> Result<(MeasurementReport, Vec<Difference>)> {
    for _ in 0..WARMUP_RUNS {
        let warmup_report = run_candidate(dataset, config);
        let warmup_projection = project_result(dataset, config, &warmup_report);
        let differences = compare_values(
            expected,
            &serde_json::to_value(warmup_projection)?,
            "$".into(),
        );
        if !differences.is_empty() {
            return Ok((
                MeasurementReport {
                    status: "correctness_failure_during_measurement",
                    warmup_runs: WARMUP_RUNS,
                    requested_runs: MEASUREMENT_RUNS,
                    raw_samples_ns: Vec::new(),
                    median_ns: None,
                    p95_ns: None,
                    peak_rss_bytes: None,
                    resource_measurement_status: "not_measured_in_ticket_01",
                    measurement_scope: "reference backtest call; excludes fixture parsing and report projection",
                },
                differences,
            ));
        }
    }
    let mut samples = Vec::with_capacity(MEASUREMENT_RUNS);
    for _ in 0..MEASUREMENT_RUNS {
        let started = Instant::now();
        let sample_report = run_candidate(dataset, config);
        let elapsed = started.elapsed().as_nanos();
        let sample_projection = project_result(dataset, config, &sample_report);
        let differences = compare_values(
            expected,
            &serde_json::to_value(sample_projection)?,
            "$".into(),
        );
        if !differences.is_empty() {
            return Ok((
                MeasurementReport {
                    status: "correctness_failure_during_measurement",
                    warmup_runs: WARMUP_RUNS,
                    requested_runs: MEASUREMENT_RUNS,
                    raw_samples_ns: Vec::new(),
                    median_ns: None,
                    p95_ns: None,
                    peak_rss_bytes: None,
                    resource_measurement_status: "not_measured_in_ticket_01",
                    measurement_scope: "reference backtest call; excludes fixture parsing and report projection",
                },
                differences,
            ));
        }
        samples.push(elapsed);
    }
    let mut sorted = samples.clone();
    sorted.sort_unstable();
    Ok((
        MeasurementReport {
            status: "measured",
            warmup_runs: WARMUP_RUNS,
            requested_runs: MEASUREMENT_RUNS,
            median_ns: Some(sorted[sorted.len() / 2]),
            p95_ns: Some(sorted[((sorted.len() * 95).div_ceil(100) - 1).min(sorted.len() - 1)]),
            raw_samples_ns: samples,
            peak_rss_bytes: None,
            resource_measurement_status: "not_measured_in_ticket_01",
            measurement_scope: "reference backtest call; excludes fixture parsing and report projection",
        },
        Vec::new(),
    ))
}

fn compare_values(expected: &Value, actual: &Value, path: String) -> Vec<Difference> {
    let mut differences = Vec::new();
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            let keys = expected
                .keys()
                .chain(actual.keys())
                .cloned()
                .collect::<BTreeSet<_>>();
            for key in keys {
                match (expected.get(&key), actual.get(&key)) {
                    (Some(expected), Some(actual)) => differences.extend(compare_values(
                        expected,
                        actual,
                        format!("{path}.{key}"),
                    )),
                    (expected, actual) => differences.push(Difference {
                        path: format!("{path}.{key}"),
                        expected: expected.cloned().unwrap_or(Value::Null),
                        actual: actual.cloned().unwrap_or(Value::Null),
                    }),
                }
            }
        }
        (Value::Array(expected), Value::Array(actual)) => {
            if expected.len() != actual.len() {
                differences.push(Difference {
                    path: format!("{path}.length"),
                    expected: Value::from(expected.len()),
                    actual: Value::from(actual.len()),
                });
            }
            for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
                differences.extend(compare_values(expected, actual, format!("{path}[{index}]")));
            }
        }
        (Value::Number(expected), Value::Number(actual)) => {
            let exact_integer = (expected.as_i64().is_some() || expected.as_u64().is_some())
                && (actual.as_i64().is_some() || actual.as_u64().is_some());
            let matches = if exact_integer {
                expected == actual
            } else {
                match (expected.as_f64(), actual.as_f64()) {
                    (Some(expected), Some(actual)) => (expected - actual).abs() <= FLOAT_TOLERANCE,
                    _ => expected == actual,
                }
            };
            if !matches {
                differences.push(Difference {
                    path,
                    expected: Value::Number(expected.clone()),
                    actual: Value::Number(actual.clone()),
                });
            }
        }
        _ if expected != actual => differences.push(Difference {
            path,
            expected: expected.clone(),
            actual: actual.clone(),
        }),
        _ => {}
    }
    differences
}

fn provenance_report() -> ProvenanceReport {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let git_revision = command_text(&repository, "git", &["rev-parse", "HEAD"]);
    let git_status = command_text(
        &repository,
        "git",
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let changed_paths = git_status
        .as_deref()
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.get(3..).map(str::to_owned))
        .collect::<Vec<_>>();
    let working_tree_dirty = git_status
        .as_deref()
        .map(|status| !status.trim().is_empty());
    let has_untracked = git_status
        .as_deref()
        .unwrap_or_default()
        .lines()
        .any(|line| line.starts_with("?? "));
    let tracked_diff = command_bytes(&repository, "git", &["diff", "--binary", "HEAD"]);
    let diff_summary = command_text(&repository, "git", &["diff", "--stat", "HEAD"]);
    let rustc = command_text(&repository, "rustc", &["--version"]);
    let cpu_model = command_text(&repository, "sysctl", &["-n", "machdep.cpu.brand_string"])
        .or_else(linux_cpu_model);
    let lockfile = repository.join("Cargo.lock");
    ProvenanceReport {
        git_revision,
        working_tree_dirty,
        working_tree_complete: git_status.is_some() && !has_untracked,
        tracked_diff_sha256: tracked_diff.as_deref().map(sha256),
        tracked_diff_summary: diff_summary,
        changed_paths,
        rustc,
        build_profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        operating_system: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        logical_cpu_count: std::thread::available_parallelism().ok().map(usize::from),
        cpu_model,
        cargo_lock_sha256: fs::read(lockfile).ok().map(|bytes| sha256(&bytes)),
        physical_memory_bytes: physical_memory_bytes(&repository),
    }
}

fn linux_cpu_model() -> Option<String> {
    fs::read_to_string("/proc/cpuinfo")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("model name\t: ").map(str::to_owned))
}

fn physical_memory_bytes(repository: &Path) -> Option<u64> {
    command_text(repository, "sysctl", &["-n", "hw.memsize"])
        .and_then(|value| value.parse().ok())
        .or_else(|| {
            fs::read_to_string("/proc/meminfo")
                .ok()?
                .lines()
                .find_map(|line| {
                    line.strip_prefix("MemTotal:").and_then(|value| {
                        value
                            .split_whitespace()
                            .next()
                            .and_then(|kilobytes| kilobytes.parse::<u64>().ok())
                            .map(|kilobytes| kilobytes * 1024)
                    })
                })
        })
}

fn command_text(directory: &Path, command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command)
        .args(args)
        .current_dir(directory)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn command_bytes(directory: &Path, command: &str, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new(command)
        .args(args)
        .current_dir(directory)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn timestamp(date: NaiveDate, time: &str) -> Result<String> {
    let value = format!("{date}T{time}");
    let parsed = DateTime::parse_from_rfc3339(&value)
        .with_context(|| format!("invalid fixture timestamp {value}"))?;
    Ok(parsed.to_rfc3339())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn write_report(report: &Poc0Report, output: &Path) -> Result<()> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create report directory {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(report)?;
    fs::write(output, bytes).with_context(|| format!("write report {}", output.display()))
}

pub fn write_and_exit_status(report: &Poc0Report, output: &Path) -> Result<bool> {
    write_report(report, output)?;
    Ok(report.correctness.status == "passed")
}

/// Enrich the canonical Rust B2 report with the fixed-version Nautilus adapter result.
/// The Python boundary owns all Nautilus objects; the Rust report remains the project contract.
pub fn run_nautilus_adapter(dataset: &Path, report: &Path, python: &Path) -> Result<String> {
    let adapter = Path::new("poc/poc0-benchmark/nautilus_adapter.py");
    let output = Command::new(python)
        .arg(adapter)
        .arg("--dataset")
        .arg(dataset)
        .arg("--reference-report")
        .arg(report)
        .arg("--output")
        .arg(report)
        .output()
        .with_context(|| format!("start Nautilus adapter with {}", python.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "Nautilus adapter failed ({}): {}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let adapter_output: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parse Nautilus adapter status output")?;
    let status = adapter_output["status"]
        .as_str()
        .context("Nautilus adapter output missing status")?;
    Ok(status.to_owned())
}

#[cfg(test)]
mod soa_tests {
    use super::soa_factor_score;
    use crate::core::StrategyConfig;

    #[test]
    fn gap_uses_prior_observed_bars_and_insufficient_window_is_missing() {
        let close = [100.0, 110.0, f64::NAN, 121.0, 133.1];
        let observed = [0, 1, 3, 4];
        let strategy = StrategyConfig {
            momentum_short_days: Some(1),
            momentum_long_days: Some(1),
            volatility_window: Some(2),
            short_momentum_weight: 1.0,
            long_momentum_weight: 1.0,
            volatility_weight: 1.0,
            ..StrategyConfig::default()
        };
        let score = soa_factor_score(&close, &observed[..3], 3, &strategy)
            .expect("third observed bar has enough history");
        assert!((score - 0.2).abs() < 1e-12);
        assert!(soa_factor_score(&close, &observed[..2], 1, &strategy).is_none());
        assert!(soa_factor_score(&close, &observed[..2], 2, &strategy).is_none());
    }
}
