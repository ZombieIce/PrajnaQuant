//! Fixed S2 parameter sweep for comparing reusable factor-cache conditions.
use super::*;
use rayon::prelude::*;
use std::collections::HashMap;

const FORMAT: &str = "poc0-b1-sweep.v1";
const MINIMUM_THROUGHPUT_GAIN_PCT: f64 = 20.0;
const PARALLEL_THREADS: usize = 2;
const SWEEP_REPETITIONS: usize = 6;

pub struct SweepOptions {
    pub instruments: usize,
    pub sessions: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Layout {
    Soa,
    Arrow,
    Polars,
}

impl Layout {
    const ALL: [Self; 3] = [Self::Soa, Self::Arrow, Self::Polars];

    fn name(self) -> &'static str {
        match self {
            Self::Soa => "soa",
            Self::Arrow => "arrow",
            Self::Polars => "polars",
        }
    }
}

#[derive(Clone)]
struct SweepParameters {
    top_n: usize,
    rebalance_every: usize,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FactorCacheKey {
    dataset_sha256: String,
    layout: Layout,
    factor_version: &'static str,
    momentum_short_days: usize,
    momentum_long_days: usize,
    volatility_window: usize,
    short_weight_bits: u64,
    long_weight_bits: u64,
    volatility_weight_bits: u64,
    trend_filter: bool,
}

fn cache_key(
    dataset: &PreparedDataset,
    config: &ExperimentConfig,
    layout: Layout,
) -> FactorCacheKey {
    FactorCacheKey {
        dataset_sha256: dataset.content_sha256.clone(),
        layout,
        factor_version: "s2-momentum-volatility.v1",
        momentum_short_days: config
            .strategy
            .momentum_short_days
            .unwrap_or(config.strategy.lookback_days),
        momentum_long_days: config
            .strategy
            .momentum_long_days
            .unwrap_or(config.strategy.lookback_days),
        volatility_window: config
            .strategy
            .volatility_window
            .unwrap_or(config.strategy.lookback_days.max(2)),
        short_weight_bits: config.strategy.short_momentum_weight.to_bits(),
        long_weight_bits: config.strategy.long_momentum_weight.to_bits(),
        volatility_weight_bits: config.strategy.volatility_weight.to_bits(),
        trend_filter: config.strategy.use_trend_filter,
    }
}

fn config_for(dataset: &PreparedDataset, parameters: &SweepParameters) -> ExperimentConfig {
    let mut config = experiment_config(&dataset.spec);
    config.strategy.top_n = parameters.top_n;
    config.strategy.rebalance_every = parameters.rebalance_every;
    config
}

fn run_layout(
    layout: Layout,
    dataset: &PreparedDataset,
    config: &ExperimentConfig,
) -> Result<B1SoaProjection> {
    match layout {
        Layout::Soa => Ok(run_soa(dataset, config, false).0),
        Layout::Arrow => Ok(run_arrow(dataset, config, false).0),
        Layout::Polars => Ok(run_polars(dataset, config, false)?.0),
    }
}

fn synthetic_dataset(
    mut spec: DatasetInput,
    instrument_count: usize,
    session_count: usize,
) -> Result<DatasetInput> {
    ensure!(instrument_count > 0, "instrument count must be positive");
    ensure!(session_count > 0, "session count must be positive");
    let first_date = *spec.calendar.first().context("fixture calendar is empty")?;
    let mut calendar = Vec::with_capacity(session_count);
    let mut date = first_date;
    while calendar.len() < session_count {
        if date.weekday().number_from_monday() <= 5 {
            calendar.push(date);
        }
        date = date.succ_opt().context("synthetic calendar overflow")?;
    }
    let mut instruments = Vec::with_capacity(instrument_count);
    let mut missing_bars = Vec::new();
    for instrument_index in 0..instrument_count {
        let symbol = format!("S{instrument_index:04}");
        let mut closes = Vec::with_capacity(session_count);
        for (day_index, trade_date) in calendar.iter().copied().enumerate() {
            let cycle = (instrument_index * 37 + day_index * 13) % 997;
            let close = 80.0
                + instrument_index as f64 * 0.2
                + day_index as f64 * 0.005
                + cycle as f64 / 997.0;
            closes.push(close);
            if (instrument_index * 17 + day_index * 29) % 1009 == 0 {
                missing_bars.push(MissingBarInput {
                    symbol: symbol.clone(),
                    date: trade_date,
                    reason: "deterministic_synthetic_missing_bar".into(),
                });
            }
        }
        instruments.push(InstrumentInput {
            symbol,
            currency: "CNY".into(),
            price_precision: 4,
            quantity_precision: 0,
            closes,
        });
    }
    spec.dataset_version = "poc0.synthetic.b1-sweep.v1".into();
    spec.seed_semantics =
        "deterministic weekday calendar and close formula; missing bars from modular index rule"
            .into();
    spec.source_identity = "poc0-b1-sweep-synthetic-v1".into();
    spec.calendar_basis = "synthetic_weekday_sessions_no_holidays".into();
    spec.calendar = calendar;
    spec.instruments = instruments;
    spec.missing_bars = missing_bars;
    spec.execution_status_overrides.clear();
    spec.strategy.momentum_short_days = (session_count / 10).clamp(2, 20);
    spec.strategy.momentum_long_days = (session_count / 4).clamp(4, 60);
    spec.strategy.volatility_window = spec.strategy.momentum_short_days.max(2);
    spec.strategy.short_momentum_weight = 1.0;
    spec.strategy.long_momentum_weight = 1.0;
    spec.strategy.volatility_weight = 0.5;
    spec.strategy.top_n = 5.min(instrument_count);
    spec.strategy.rebalance_every = 1;
    Ok(spec)
}

fn close_index(dataset: &PreparedDataset) -> BTreeMap<(NaiveDate, String), f64> {
    dataset
        .bars
        .iter()
        .map(|bar| ((bar.trade_date, bar.symbol.clone()), bar.close))
        .collect()
}

fn project_from_rankings(
    dataset: &PreparedDataset,
    close_by_date_symbol: &BTreeMap<(NaiveDate, String), f64>,
    rankings: &[RankingProjection],
    config: &ExperimentConfig,
) -> B1SoaProjection {
    let mut targets = Vec::new();
    let mut target_weights = Vec::new();
    let mut eligible_dates = 0usize;
    let rebalance_every = config.strategy.rebalance_every.max(1);
    for ranking in rankings {
        if ranking.candidates.is_empty() {
            continue;
        }
        if eligible_dates % rebalance_every == 0 {
            let chosen = ranking
                .candidates
                .iter()
                .take(config.strategy.top_n)
                .map(|candidate| candidate.symbol.clone())
                .collect::<Vec<_>>();
            target_weights.extend(chosen.iter().map(|symbol| WeightedTarget {
                date: ranking.date,
                symbol: symbol.clone(),
                weight: 1.0 / chosen.len() as f64,
            }));
            targets.push(SignalProjection {
                date: ranking.date,
                target_symbols: chosen,
            });
        }
        eligible_dates += 1;
    }

    let mut portfolio_returns = Vec::new();
    for signal in &targets {
        let Ok(day_index) = dataset.spec.calendar.binary_search(&signal.date) else {
            continue;
        };
        if day_index + 1 >= dataset.spec.calendar.len() {
            continue;
        }
        let next_date = dataset.spec.calendar[day_index + 1];
        let returns = signal
            .target_symbols
            .iter()
            .map(|symbol| {
                let today = close_by_date_symbol.get(&(signal.date, symbol.clone()))?;
                let next = close_by_date_symbol.get(&(next_date, symbol.clone()))?;
                Some(next / today - 1.0)
            })
            .collect::<Option<Vec<_>>>();
        let return_pct = returns
            .filter(|values| !values.is_empty())
            .map(|values| values.iter().sum::<f64>() / values.len() as f64);
        portfolio_returns.push(PortfolioReturn {
            from: signal.date,
            to: next_date,
            return_pct,
        });
    }

    B1SoaProjection {
        rankings: rankings.to_vec(),
        targets,
        target_weights,
        portfolio_returns,
    }
}

fn cached_projection(
    dataset: &PreparedDataset,
    close_by_date_symbol: &BTreeMap<(NaiveDate, String), f64>,
    config: &ExperimentConfig,
    layout: Layout,
    cache: &HashMap<FactorCacheKey, Vec<RankingProjection>>,
) -> Result<B1SoaProjection> {
    let rankings = cache
        .get(&cache_key(dataset, config, layout))
        .context("factor cache was not populated")?;
    Ok(project_from_rankings(
        dataset,
        close_by_date_symbol,
        rankings,
        config,
    ))
}

fn cache_miss_projection(
    dataset: &PreparedDataset,
    config: &ExperimentConfig,
    layout: Layout,
    cache: &mut HashMap<FactorCacheKey, Vec<RankingProjection>>,
) -> Result<B1SoaProjection> {
    let key = cache_key(dataset, config, layout);
    cache.remove(&key);
    let projection = run_layout(layout, dataset, config)?;
    cache.insert(key.clone(), projection.rankings.clone());
    Ok(projection)
}

fn checksum(projection: &B1SoaProjection) -> Result<String> {
    Ok(sha256(&serde_json::to_vec(projection)?))
}

fn median(samples: &[u128]) -> Option<u128> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    Some(if sorted.len() % 2 == 0 {
        sorted[middle - 1] + (sorted[middle] - sorted[middle - 1]) / 2
    } else {
        sorted[middle]
    })
}

fn p95(samples: &[u128]) -> Option<u128> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let index = (sorted.len() * 95).div_ceil(100).saturating_sub(1);
    sorted.get(index).copied()
}

fn p95_f64(samples: &[f64]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = (sorted.len() * 95).div_ceil(100).saturating_sub(1);
    sorted.get(index).copied()
}

fn runs_per_second(run_count: usize, duration_ns: Option<u128>) -> Option<f64> {
    duration_ns
        .filter(|duration| *duration > 0)
        .map(|duration| run_count as f64 * 1_000_000_000.0 / duration as f64)
}

fn rotated_layouts(sample: usize) -> [Layout; 3] {
    std::array::from_fn(|index| Layout::ALL[(index + sample) % Layout::ALL.len()])
}

pub fn run_sweep(
    dataset_path: &Path,
    expected_path: &Path,
    options: SweepOptions,
) -> Result<Value> {
    let golden = run(dataset_path, expected_path, "soa")?;
    let dataset_input: DatasetInput = serde_json::from_slice(&fs::read(dataset_path)?)?;
    let generation_started = Instant::now();
    let dataset_input = synthetic_dataset(dataset_input, options.instruments, options.sessions)?;
    let generation_ns = generation_started.elapsed().as_nanos();
    let dataset = prepare_dataset(dataset_input)?;
    let close_by_date_symbol = close_index(&dataset);
    let instrument_count = dataset.spec.instruments.len();
    ensure!(
        instrument_count > 0,
        "sweep requires at least one instrument"
    );

    let mut top_values = BTreeSet::new();
    for requested in [1, 5, 10] {
        top_values.insert(requested.min(instrument_count));
    }
    let parameters = top_values
        .iter()
        .copied()
        .flat_map(|top_n| {
            [1, 5]
                .into_iter()
                .map(move |rebalance_every| SweepParameters {
                    top_n,
                    rebalance_every,
                })
        })
        .collect::<Vec<_>>();

    let mut factor_caches = HashMap::new();
    let mut parameter_runs = Vec::new();
    let mut correctness_passed = golden.correctness_status() == "passed";
    let mut layout_checksums: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut layout_projections: BTreeMap<&'static str, Vec<B1SoaProjection>> = BTreeMap::new();
    for layout in Layout::ALL {
        let first_config = config_for(&dataset, &parameters[0]);
        let first_projection = run_layout(layout, &dataset, &first_config)?;
        let key = cache_key(&dataset, &first_config, layout);
        factor_caches.insert(key, first_projection.rankings.clone());
        let mut checksums = Vec::new();
        let mut projections = Vec::with_capacity(parameters.len());
        for run_parameters in &parameters {
            let config = config_for(&dataset, run_parameters);
            let miss_projection = run_layout(layout, &dataset, &config)?;
            let hit_projection = cached_projection(
                &dataset,
                &close_by_date_symbol,
                &config,
                layout,
                &factor_caches,
            )?;
            let miss_checksum = checksum(&miss_projection)?;
            let hit_checksum = checksum(&hit_projection)?;
            if miss_checksum != hit_checksum
                || !projections_match(&miss_projection, &hit_projection)
            {
                correctness_passed = false;
            }
            checksums.push((run_parameters.clone(), miss_checksum, hit_checksum));
            projections.push(miss_projection);
        }
        layout_checksums.insert(
            layout.name().to_owned(),
            checksums.iter().map(|(_, miss, _)| miss.clone()).collect(),
        );
        layout_projections.insert(layout.name(), projections);
        for (run_parameters, miss_checksum, hit_checksum) in checksums {
            if let Some(existing) = parameter_runs.iter_mut().find(|row: &&mut Value| {
                row["parameters"]["top_n"] == run_parameters.top_n
                    && row["parameters"]["rebalance_every"] == run_parameters.rebalance_every
            }) {
                let checksums = existing["checksums"]
                    .as_object_mut()
                    .context("checksum report object missing")?;
                let layout_checksums = checksums
                    .entry(layout.name().to_owned())
                    .or_insert_with(|| serde_json::json!({}));
                layout_checksums["cache_miss"] = Value::String(miss_checksum);
                layout_checksums["cache_hit"] = Value::String(hit_checksum);
            } else {
                parameter_runs.push(serde_json::json!({
                    "parameters": {"top_n": run_parameters.top_n,
                        "rebalance_every": run_parameters.rebalance_every},
                    "checksums": {layout.name(): {"cache_miss": miss_checksum,
                        "cache_hit": hit_checksum}}
                }));
            }
        }
    }

    for ((soa, arrow), polars) in layout_projections["soa"]
        .iter()
        .zip(&layout_projections["arrow"])
        .zip(&layout_projections["polars"])
    {
        if !projections_match(soa, arrow) || !projections_match(soa, polars) {
            correctness_passed = false;
        }
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(PARALLEL_THREADS)
        .build()?;
    let warmup_order = rotated_layouts(SWEEP_REPETITIONS - 1);
    if correctness_passed {
        for layout in warmup_order {
            let config = config_for(&dataset, &parameters[0]);
            let mut warm_cache = HashMap::new();
            let miss = cache_miss_projection(&dataset, &config, layout, &mut warm_cache)?;
            let hit = cached_projection(
                &dataset,
                &close_by_date_symbol,
                &config,
                layout,
                &factor_caches,
            )?;
            ensure!(
                checksum(&miss)? == checksum(&hit)?,
                "warmup cache checksum mismatch for {}",
                layout.name()
            );
        }
    }
    let mut measurements = serde_json::Map::new();
    let mut order_schedule = Vec::new();
    if correctness_passed {
        for sample in 0..SWEEP_REPETITIONS {
            let ordered = rotated_layouts(sample);
            order_schedule.push(ordered.map(Layout::name));
            for layout in ordered {
                let mut condition_order = ["cache_miss", "cache_hit"];
                if sample % 2 == 1 {
                    condition_order.reverse();
                }
                for condition in condition_order {
                    let started = Instant::now();
                    let mut raw_samples_ns = Vec::with_capacity(parameters.len());
                    let mut raw_samples_by_parameter = serde_json::Map::new();
                    let mut miss_cache = HashMap::new();
                    for run_parameters in &parameters {
                        let config = config_for(&dataset, run_parameters);
                        let run_started = Instant::now();
                        let projection = if condition == "cache_miss" {
                            cache_miss_projection(&dataset, &config, layout, &mut miss_cache)?
                        } else {
                            cached_projection(
                                &dataset,
                                &close_by_date_symbol,
                                &config,
                                layout,
                                &factor_caches,
                            )?
                        };
                        std::hint::black_box(&projection);
                        let run_ns = run_started.elapsed().as_nanos();
                        raw_samples_ns.push(run_ns);
                        raw_samples_by_parameter
                            .entry(format!(
                                "top_n={};rebalance_every={}",
                                run_parameters.top_n, run_parameters.rebalance_every
                            ))
                            .or_insert_with(|| Value::Array(Vec::new()))
                            .as_array_mut()
                            .context("per-parameter raw sample must be an array")?
                            .push(serde_json::json!(run_ns));
                    }
                    let scan_duration_ns = started.elapsed().as_nanos();

                    let parallel_started = Instant::now();
                    let parallel_results = pool.install(|| {
                        parameters
                            .par_iter()
                            .map(|run_parameters| {
                                let config = config_for(&dataset, run_parameters);
                                if condition == "cache_miss" {
                                    let mut miss_cache = HashMap::new();
                                    cache_miss_projection(
                                        &dataset,
                                        &config,
                                        layout,
                                        &mut miss_cache,
                                    )
                                } else {
                                    cached_projection(
                                        &dataset,
                                        &close_by_date_symbol,
                                        &config,
                                        layout,
                                        &factor_caches,
                                    )
                                }
                            })
                            .collect::<Result<Vec<_>>>()
                    })?;
                    let parallel_scan_duration_ns = parallel_started.elapsed().as_nanos();
                    for (result, expected_checksums) in parallel_results.iter().zip(
                        parameter_runs
                            .iter()
                            .filter(|row| row["checksums"][layout.name()][condition].is_string()),
                    ) {
                        if checksum(result)?
                            != expected_checksums["checksums"][layout.name()][condition]
                                .as_str()
                                .unwrap_or_default()
                        {
                            correctness_passed = false;
                        }
                    }

                    let entry = measurements
                        .entry(layout.name().to_owned())
                        .or_insert_with(|| serde_json::json!({}));
                    let condition_entry = entry
                        .as_object_mut()
                        .context("layout measurement must be an object")?
                        .entry(condition.to_owned())
                        .or_insert_with(|| {
                            serde_json::json!({
                                "raw_samples_ns": [],
                                "raw_samples_ns_by_parameter": {},
                                "scan_samples_ns": [],
                                "parallel_scan_samples_ns": [],
                                "parallel_threads": PARALLEL_THREADS,
                                "cache_warmup_runs": WARMUP_RUNS,
                                "peak_rss_bytes": null,
                                "resource_measurement_status": "unknown"
                            })
                        });
                    condition_entry["raw_samples_ns"]
                        .as_array_mut()
                        .context("raw sample array missing")?
                        .extend(
                            raw_samples_ns
                                .into_iter()
                                .map(serde_json::to_value)
                                .collect::<std::result::Result<Vec<_>, _>>()?,
                        );
                    for (parameter, samples) in raw_samples_by_parameter {
                        condition_entry["raw_samples_ns_by_parameter"]
                            .as_object_mut()
                            .context("per-parameter samples must be an object")?
                            .entry(parameter)
                            .or_insert_with(|| Value::Array(Vec::new()))
                            .as_array_mut()
                            .context("per-parameter sample bucket must be an array")?
                            .extend(samples.as_array().cloned().unwrap_or_default());
                    }
                    condition_entry["scan_samples_ns"]
                        .as_array_mut()
                        .context("scan sample array missing")?
                        .push(serde_json::json!(scan_duration_ns));
                    condition_entry["parallel_scan_samples_ns"]
                        .as_array_mut()
                        .context("parallel sample array missing")?
                        .push(serde_json::json!(parallel_scan_duration_ns));
                }
            }
        }
    }

    for layout in Layout::ALL {
        for condition in ["cache_miss", "cache_hit"] {
            let Some(measurement) = measurements
                .get_mut(layout.name())
                .and_then(|entry| entry.get_mut(condition))
            else {
                continue;
            };
            let raw_samples = measurement["raw_samples_ns"]
                .as_array()
                .context("raw samples missing")?
                .iter()
                .filter_map(Value::as_u64)
                .map(u128::from)
                .collect::<Vec<_>>();
            let scan_samples = measurement["scan_samples_ns"]
                .as_array()
                .context("scan samples missing")?
                .iter()
                .filter_map(Value::as_u64)
                .map(u128::from)
                .collect::<Vec<_>>();
            let parallel_samples = measurement["parallel_scan_samples_ns"]
                .as_array()
                .context("parallel samples missing")?
                .iter()
                .filter_map(Value::as_u64)
                .map(u128::from)
                .collect::<Vec<_>>();
            measurement["median_ns"] = serde_json::json!(median(&raw_samples));
            measurement["p95_ns"] = serde_json::json!(p95(&raw_samples));
            measurement["single_run_runs_per_second"] =
                serde_json::json!(runs_per_second(1, median(&raw_samples)));
            measurement["scan_median_ns"] = serde_json::json!(median(&scan_samples));
            measurement["scan_p95_ns"] = serde_json::json!(p95(&scan_samples));
            measurement["runs_per_second"] =
                serde_json::json!(runs_per_second(parameters.len(), median(&scan_samples)));
            measurement["parallel_runs_per_second"] =
                serde_json::json!(runs_per_second(parameters.len(), median(&parallel_samples)));
            let parallel_throughput_samples = parallel_samples
                .iter()
                .filter_map(|duration| runs_per_second(parameters.len(), Some(*duration)))
                .collect::<Vec<_>>();
            measurement["parallel_runs_per_second_samples"] =
                serde_json::json!(parallel_throughput_samples);
            measurement["parallel_runs_per_second_p95"] =
                serde_json::json!(p95_f64(&parallel_throughput_samples));
            let mut per_parameter_statistics = serde_json::Map::new();
            for (parameter, values) in measurement["raw_samples_ns_by_parameter"]
                .as_object()
                .context("per-parameter raw samples missing")?
            {
                let samples = values
                    .as_array()
                    .context("per-parameter raw samples must be an array")?
                    .iter()
                    .filter_map(Value::as_u64)
                    .map(u128::from)
                    .collect::<Vec<_>>();
                per_parameter_statistics.insert(
                    parameter.clone(),
                    serde_json::json!({
                        "raw_sample_count": samples.len(),
                        "median_ns": median(&samples),
                        "p95_ns": p95(&samples),
                        "single_run_runs_per_second": runs_per_second(1, median(&samples))
                    }),
                );
            }
            measurement["per_parameter_statistics"] = Value::Object(per_parameter_statistics);
        }
    }

    let cache_key_schema = "structured key: dataset_content_sha256, layout, factor_version, momentum windows, volatility window, score-weight bit patterns, and trend_filter; excludes top_n and rebalance_every";
    let layout_checksums_json = layout_checksums
        .into_iter()
        .map(|(layout, checksums)| (layout, serde_json::json!(checksums)))
        .collect::<serde_json::Map<_, _>>();
    let decision_status = "unresolved";
    Ok(serde_json::json!({
        "format": FORMAT,
        "status": if correctness_passed { "passed" } else { "failed" },
        "dataset_content_sha256": dataset.content_sha256,
        "source_identity": dataset.spec.source_identity,
        "target_load": {
            "kind": "deterministic synthetic weekday panel; no holidays or real-market inference",
            "instrument_count": instrument_count,
            "session_count": dataset.spec.calendar.len(),
            "bar_count": dataset.bar_count,
            "missing_bar_count": dataset.spec.missing_bars.len(),
            "momentum_short_days": dataset.spec.strategy.momentum_short_days,
            "momentum_long_days": dataset.spec.strategy.momentum_long_days,
            "volatility_window": dataset.spec.strategy.volatility_window
        },
        "dataset_generation_ns": generation_ns,
        "factor_cache": {
            "key_schema": cache_key_schema,
            "value": "per-session ranked factor candidates before top_n and rebalance filtering",
            "reuse_rule": "reuse only when dataset content, layout, factor version, factor windows, score weights, and trend filter match"
        },
        "strategy_parameters": {
            "momentum_short_days": dataset.spec.strategy.momentum_short_days,
            "momentum_long_days": dataset.spec.strategy.momentum_long_days,
            "volatility_window": dataset.spec.strategy.volatility_window,
            "short_momentum_weight": dataset.spec.strategy.short_momentum_weight,
            "long_momentum_weight": dataset.spec.strategy.long_momentum_weight,
            "volatility_weight": dataset.spec.strategy.volatility_weight,
            "ranking_tie_break": "score descending, then symbol ascending",
            "missing_bar_rule": "skip missing closes when forming observed-close windows; selected target return is null if the next calendar-session close is unavailable",
            "return_scope": "signal-session close to next calendar-session close; evaluation label only"
        },
        "parameter_space": {
            "top_n": top_values,
            "rebalance_every": [1, 5],
            "run_count": parameters.len()
        },
        "parameter_runs": parameter_runs,
        "layout_checksums": layout_checksums_json,
        "warmup_runs_per_layout_condition": WARMUP_RUNS,
        "warmup_candidate_order": warmup_order.map(Layout::name),
        "repetitions": SWEEP_REPETITIONS,
        "candidate_order_schedule": order_schedule,
        "measurement_order_policy": "candidate order rotates by sample; cache-condition order alternates",
        "measurements": measurements,
        "implementation_costs": {
            "conversion": "layout preparation and Polars/Arrow conversion are included in per-Run and sweep timings; not reported as a separately measured component by this sweep",
            "custom_soa_maintenance": "qualitative only; a third independent S2 implementation adds code and correctness maintenance, not monetized here"
        },
        "process_peak_rss_bytes": null,
        "process_peak_rss_status": "unknown; measure with the documented RSS wrapper",
        "decision_threshold": {
            "minimum_throughput_gain_pct": MINIMUM_THROUGHPUT_GAIN_PCT,
            "registered_before_measurement": true,
            "adoption_rule": "Custom SoA must exceed the best alternative by the registered threshold on a representative target load and pass correctness/resource gates"
        },
        "conclusion": {
            "status": decision_status,
            "custom_soa": "unresolved",
            "reason": if !correctness_passed {
                "One or more independent golden, layout, or cache checksum gates failed; performance results are not decision evidence."
            } else if instrument_count < 64 || dataset.spec.calendar.len() < 252 {
                "The requested workload is below the registered 64-instrument by 252-session target; no layout is adopted from this override."
            } else {
                "The target-size sweep passed correctness, but peak RSS is not measured per layout/cache condition and build-cost evidence is recorded separately; keep the architecture choice unresolved."
            }
        },
        "build_cost_evidence": {
            "status": "unknown",
            "note": "Record candidate incremental and release build wall time and shared-target byte delta separately; do not mix build cost into runs/s."
        },
        "provenance": serde_json::to_value(golden.provenance)?,
        "correctness": {
            "golden_status": golden.correctness.status,
            "layout_and_cache_checksums_match": correctness_passed
        }
    }))
}
