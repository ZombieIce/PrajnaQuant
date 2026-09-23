use crate::{
    backtest::BacktestReport, core::ExperimentConfig, data::SnapshotManifest, factor::FactorReport,
};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentResult {
    pub experiment_id: Uuid,
    pub created_at: String,
    pub engine_version: String,
    pub name: String,
    pub config: ExperimentConfig,
    pub snapshot: SnapshotManifest,
    pub factor: FactorReport,
    pub backtest: BacktestReport,
    pub assumptions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentSummary {
    pub experiment_id: Uuid,
    pub created_at: String,
    pub name: String,
    pub total_return: f64,
    pub max_drawdown: f64,
    pub mean_rank_ic: f64,
    pub annualized_return: f64,
    pub sharpe_ratio: f64,
    pub calmar_ratio: f64,
    pub turnover: f64,
    pub lookback_days: usize,
    pub top_n: usize,
    pub rebalance_every: usize,
}

impl ExperimentResult {
    pub fn new(
        config: ExperimentConfig,
        snapshot: SnapshotManifest,
        factor: FactorReport,
        backtest: BacktestReport,
    ) -> Self {
        Self{experiment_id:Uuid::new_v4(),created_at:Utc::now().to_rfc3339(),engine_version:env!("CARGO_PKG_VERSION").into(),name:config.name.clone(),config,snapshot,factor,backtest,assumptions:vec![
            "signals are formed after the close and executed at the next available daily open".into(),
            "daily-bar execution applies configured slippage; it does not model intraday queue priority".into(),
            "missing execution bars leave the affected position unchanged".into(),
            "cash earns no interest and ETF distributions are only reflected when present in source prices/data".into(),
        ]}
    }
    pub fn summary(&self) -> ExperimentSummary {
        ExperimentSummary {
            experiment_id: self.experiment_id,
            created_at: self.created_at.clone(),
            name: self.name.clone(),
            total_return: self.backtest.metrics.total_return,
            max_drawdown: self.backtest.metrics.max_drawdown,
            mean_rank_ic: self.factor.mean_rank_ic,
            annualized_return: self.backtest.metrics.annualized_return,
            sharpe_ratio: self.backtest.metrics.sharpe_ratio,
            calmar_ratio: self.backtest.metrics.calmar_ratio,
            turnover: self.backtest.metrics.turnover,
            lookback_days: self.config.strategy.lookback_days,
            top_n: self.config.strategy.top_n,
            rebalance_every: self.config.strategy.rebalance_every,
        }
    }
    pub fn save(&self, root: &Path) -> Result<PathBuf> {
        let dir = root
            .join("experiments")
            .join(self.experiment_id.to_string());
        fs::create_dir_all(&dir)?;
        let path = dir.join("experiment.json");
        fs::write(&path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("write {}", path.display()))?;
        Ok(path)
    }
}

pub fn list_experiments(root: &Path) -> Result<Vec<ExperimentSummary>> {
    let dir = root.join("experiments");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut values = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path().join("experiment.json");
        if path.is_file() {
            if let Ok(value) = serde_json::from_slice::<ExperimentResult>(&fs::read(path)?) {
                values.push(value.summary());
            }
        }
    }
    values.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(values)
}

pub fn load_experiment(root: &Path, id: &str) -> Result<ExperimentResult> {
    anyhow::ensure!(
        !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
        "invalid experiment id"
    );
    let path = root.join("experiments").join(id).join("experiment.json");
    Ok(serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )?)
}
