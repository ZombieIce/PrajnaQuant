use crate::{
    backtest,
    core::{Bar, ExperimentConfig},
    data::SnapshotManifest,
    experiment::ExperimentResult,
    factor,
};
use anyhow::Result;
use chrono::NaiveDate;
use std::collections::HashMap;

pub fn run_experiment(
    config: ExperimentConfig,
    snapshot: SnapshotManifest,
    bars: &[Bar],
    benchmark_bars: &[Bar],
) -> Result<ExperimentResult> {
    config.validate()?;
    let factor = if config.strategy.uses_rotation_score() {
        factor::evaluate_observations(
            "etf_rotation_score",
            config.strategy.score_lookback(),
            config.research.forward_days,
            config.research.quantiles,
            crate::strategy::rotation_factor_observations(
                bars,
                &config.strategy,
                config.research.forward_days,
            ),
        )
    } else {
        factor::evaluate_momentum(
            bars,
            config.strategy.score_lookback(),
            config.research.forward_days,
            config.research.quantiles,
        )
    };
    let scores = if config.strategy.uses_rotation_score() {
        crate::strategy::rotation_scores(bars, &config.strategy)
    } else {
        factor::observation_map(&factor::momentum_observations(
            bars,
            config.strategy.lookback_days,
            0,
        ))
    };
    run_experiment_with_inputs(config, snapshot, factor, &scores, bars, benchmark_bars)
}

pub fn run_experiment_with_inputs(
    config: ExperimentConfig,
    snapshot: SnapshotManifest,
    factor: factor::FactorReport,
    scores: &HashMap<(NaiveDate, String), f64>,
    bars: &[Bar],
    benchmark_bars: &[Bar],
) -> Result<ExperimentResult> {
    config.validate()?;
    let backtest = backtest::run_with_scores(bars, benchmark_bars, &config, scores);
    Ok(ExperimentResult::new(config, snapshot, factor, backtest))
}
