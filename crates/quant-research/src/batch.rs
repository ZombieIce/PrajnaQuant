use crate::{
    core::ExperimentConfig,
    data::SnapshotManifest,
    experiment::ExperimentSummary,
    factor,
    runner::run_experiment_with_inputs,
    strategy::{feature_key, rotation_scores},
};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use rayon::{ThreadPoolBuilder, prelude::*};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BatchConfig {
    pub name: String,
    pub max_parallelism: usize,
    pub base: ExperimentConfig,
    pub grid: EtfParameterGrid,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            name: "ETF 参数实验".into(),
            max_parallelism: 2,
            base: ExperimentConfig::default(),
            grid: EtfParameterGrid::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EtfParameterGrid {
    pub lookback_days: Vec<usize>,
    pub top_n: Vec<usize>,
    pub rebalance_every: Vec<usize>,
    pub momentum_short_days: Vec<usize>,
    pub momentum_long_days: Vec<usize>,
    pub volatility_window: Vec<usize>,
    pub trend_window: Vec<usize>,
    pub use_trend_filter: Vec<bool>,
}

impl BatchConfig {
    pub fn expand(&self) -> Result<Vec<ExperimentConfig>> {
        let mut configs = vec![self.base.clone()];
        expand_usize(&mut configs, &self.grid.lookback_days, |config, value| {
            config.strategy.lookback_days = value
        });
        expand_usize(&mut configs, &self.grid.top_n, |config, value| {
            config.strategy.top_n = value
        });
        expand_usize(&mut configs, &self.grid.rebalance_every, |config, value| {
            config.strategy.rebalance_every = value
        });
        expand_usize(
            &mut configs,
            &self.grid.momentum_short_days,
            |config, value| config.strategy.momentum_short_days = Some(value),
        );
        expand_usize(
            &mut configs,
            &self.grid.momentum_long_days,
            |config, value| config.strategy.momentum_long_days = Some(value),
        );
        expand_usize(
            &mut configs,
            &self.grid.volatility_window,
            |config, value| config.strategy.volatility_window = Some(value),
        );
        expand_usize(&mut configs, &self.grid.trend_window, |config, value| {
            config.strategy.trend_window = Some(value)
        });
        if !self.grid.use_trend_filter.is_empty() {
            configs = configs
                .into_iter()
                .flat_map(|config| {
                    self.grid.use_trend_filter.iter().map(move |value| {
                        let mut next = config.clone();
                        next.strategy.use_trend_filter = *value;
                        next
                    })
                })
                .collect();
        }
        ensure!(
            !configs.is_empty(),
            "parameter grid expands to no configurations"
        );
        for (index, config) in configs.iter_mut().enumerate() {
            config.name = format!(
                "{} · #{:04} · lb{} n{} rb{}",
                self.name,
                index + 1,
                config.strategy.score_lookback(),
                config.strategy.top_n,
                config.strategy.rebalance_every
            );
            config.validate()?;
        }
        Ok(configs)
    }
}

fn expand_usize(
    configs: &mut Vec<ExperimentConfig>,
    values: &[usize],
    mut assign: impl FnMut(&mut ExperimentConfig, usize),
) {
    if values.is_empty() {
        return;
    }
    let original = std::mem::take(configs);
    let mut expanded = Vec::with_capacity(original.len() * values.len());
    for config in original {
        for value in values {
            let mut next = config.clone();
            assign(&mut next, *value);
            expanded.push(next);
        }
    }
    *configs = expanded;
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchResult {
    pub batch_id: Uuid,
    pub created_at: String,
    pub name: String,
    pub snapshot_id: Uuid,
    pub experiment_count: usize,
    pub experiments: Vec<ExperimentSummary>,
}

pub fn run_batch(
    root: &Path,
    config: &BatchConfig,
    snapshot: &SnapshotManifest,
    bars: &[crate::core::Bar],
    benchmark_bars: &[crate::core::Bar],
) -> Result<(BatchResult, PathBuf)> {
    let configs = config.expand()?;
    ensure!(
        config.max_parallelism > 0,
        "max_parallelism must be positive"
    );
    let pool = ThreadPoolBuilder::new()
        .num_threads(config.max_parallelism)
        .build()
        .context("create batch worker pool")?;
    let mut groups: BTreeMap<String, Vec<ExperimentConfig>> = BTreeMap::new();
    for item in configs {
        groups
            .entry(feature_key(&item.strategy))
            .or_default()
            .push(item);
    }
    let mut results = Vec::new();
    for (_, group) in groups {
        let template = group.first().context("empty feature group")?;
        let factor = if template.strategy.uses_rotation_score() {
            factor::evaluate_observations(
                "etf_rotation_score",
                template.strategy.score_lookback(),
                template.research.forward_days,
                template.research.quantiles,
                crate::strategy::rotation_factor_observations(
                    bars,
                    &template.strategy,
                    template.research.forward_days,
                ),
            )
        } else {
            factor::evaluate_momentum(
                bars,
                template.strategy.score_lookback(),
                template.research.forward_days,
                template.research.quantiles,
            )
        };
        let scores = if template.strategy.uses_rotation_score() {
            rotation_scores(bars, &template.strategy)
        } else {
            factor::observation_map(&factor::momentum_observations(
                bars,
                template.strategy.lookback_days,
                0,
            ))
        };
        let group_results = pool.install(|| {
            group
                .into_par_iter()
                .map(|item| {
                    run_experiment_with_inputs(
                        item,
                        snapshot.clone(),
                        factor.clone(),
                        &scores,
                        bars,
                        benchmark_bars,
                    )
                })
                .collect::<Result<Vec<_>>>()
        })?;
        results.extend(group_results);
    }
    let mut summaries = Vec::with_capacity(results.len());
    for result in results {
        summaries.push(result.summary());
        result.save(root)?;
    }
    summaries.sort_by(|left, right| right.total_return.total_cmp(&left.total_return));
    let batch = BatchResult {
        batch_id: Uuid::new_v4(),
        created_at: Utc::now().to_rfc3339(),
        name: config.name.clone(),
        snapshot_id: snapshot.snapshot_id,
        experiment_count: summaries.len(),
        experiments: summaries,
    };
    let dir = root.join("batches").join(batch.batch_id.to_string());
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join("batch.json");
    fs::write(&path, serde_json::to_vec_pretty(&batch)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok((batch, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_parameter_grid_cartesian_product() {
        let config = BatchConfig {
            grid: EtfParameterGrid {
                lookback_days: vec![20, 60],
                top_n: vec![2, 3],
                rebalance_every: vec![5],
                ..EtfParameterGrid::default()
            },
            ..BatchConfig::default()
        };
        let expanded = config.expand().unwrap();
        assert_eq!(expanded.len(), 4);
        assert!(expanded.iter().all(|item| item.name.contains("#")));
    }
}
