use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type SymbolId = u32;
pub type DateId = u32;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ForwardReturnMethod {
    /// Close(T) to close at the instrument's H-th subsequent observed bar.
    CloseToClose,
    /// Open(T+1) to open H market-bar intervals later.
    #[default]
    NextOpenToForwardOpen,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bar {
    pub symbol: String,
    pub name: String,
    pub trade_date: NaiveDate,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub amount: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionStatus {
    pub trade_status: Option<String>,
    pub is_tradable: bool,
    pub sources: Option<String>,
}

pub type ExecutionStatusMap = HashMap<(NaiveDate, String), ExecutionStatus>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CostConfig {
    /// Commission as a fraction of gross consideration, e.g. 0.0003 is 3 bps.
    pub commission_rate: f64,
    pub minimum_commission: f64,
    pub buy_tax_rate: f64,
    pub sell_tax_rate: f64,
    pub buy_slippage_bps: f64,
    pub sell_slippage_bps: f64,
}

impl Default for CostConfig {
    fn default() -> Self {
        Self {
            commission_rate: 0.0003,
            minimum_commission: 5.0,
            buy_tax_rate: 0.0,
            sell_tax_rate: 0.0,
            buy_slippage_bps: 2.0,
            sell_slippage_bps: 2.0,
        }
    }
}

impl CostConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        let values = [
            self.commission_rate,
            self.minimum_commission,
            self.buy_tax_rate,
            self.sell_tax_rate,
            self.buy_slippage_bps,
            self.sell_slippage_bps,
        ];
        anyhow::ensure!(
            values.iter().all(|v| v.is_finite() && *v >= 0.0),
            "cost values must be finite and non-negative"
        );
        anyhow::ensure!(
            self.commission_rate <= 0.1 && self.buy_tax_rate <= 0.1 && self.sell_tax_rate <= 0.1,
            "cost rates must be fractions no greater than 0.1"
        );
        anyhow::ensure!(
            self.buy_slippage_bps <= 10_000.0 && self.sell_slippage_bps <= 10_000.0,
            "slippage must be no greater than 10000 bps"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StrategyConfig {
    pub lookback_days: usize,
    pub top_n: usize,
    pub rebalance_every: usize,
    #[serde(default)]
    pub momentum_short_days: Option<usize>,
    #[serde(default)]
    pub momentum_long_days: Option<usize>,
    #[serde(default)]
    pub volatility_window: Option<usize>,
    #[serde(default)]
    pub trend_window: Option<usize>,
    #[serde(default)]
    pub use_trend_filter: bool,
    #[serde(default = "default_score_weight")]
    pub short_momentum_weight: f64,
    #[serde(default = "default_score_weight")]
    pub long_momentum_weight: f64,
    #[serde(default = "default_score_weight")]
    pub volatility_weight: f64,
}

fn default_score_weight() -> f64 {
    1.0
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            lookback_days: 20,
            top_n: 5,
            rebalance_every: 5,
            momentum_short_days: None,
            momentum_long_days: None,
            volatility_window: None,
            trend_window: None,
            use_trend_filter: false,
            short_momentum_weight: 1.0,
            long_momentum_weight: 1.0,
            volatility_weight: 1.0,
        }
    }
}

impl StrategyConfig {
    pub fn uses_rotation_score(&self) -> bool {
        self.momentum_short_days.is_some() || self.momentum_long_days.is_some()
    }

    pub fn score_lookback(&self) -> usize {
        self.momentum_long_days.unwrap_or(self.lookback_days)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ResearchConfig {
    pub forward_days: usize,
    pub quantiles: usize,
    pub label_method: ForwardReturnMethod,
}

impl Default for ResearchConfig {
    fn default() -> Self {
        Self {
            forward_days: 5,
            quantiles: 5,
            label_method: ForwardReturnMethod::NextOpenToForwardOpen,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExperimentConfig {
    pub name: String,
    /// Immutable universe selection for new runs. Missing values are retained
    /// only for backwards-compatible loading of legacy experiment configs.
    #[serde(default)]
    pub universe: Option<crate::universe::UniverseSelection>,
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    pub initial_cash: f64,
    pub lot_size: i64,
    pub strategy: StrategyConfig,
    pub research: ResearchConfig,
    pub costs: CostConfig,
}

impl Default for ExperimentConfig {
    fn default() -> Self {
        Self {
            name: "ETF 动量研究".into(),
            universe: None,
            start: None,
            end: None,
            initial_cash: 1_000_000.0,
            lot_size: 100,
            strategy: StrategyConfig::default(),
            research: ResearchConfig::default(),
            costs: CostConfig::default(),
        }
    }
}

impl ExperimentConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.initial_cash.is_finite() && self.initial_cash > 0.0,
            "initial_cash must be positive"
        );
        anyhow::ensure!(self.lot_size > 0, "lot_size must be positive");
        if let Some(universe) = &self.universe {
            anyhow::ensure!(
                crate::universe::strategy_capability(universe.asset_scope).ready,
                "capability_not_ready: stock ETF Rotation execution has not been validated"
            );
        }
        anyhow::ensure!(
            self.strategy.lookback_days > 0
                && self.strategy.top_n > 0
                && self.strategy.rebalance_every > 0,
            "strategy values must be positive"
        );
        if self.strategy.uses_rotation_score() {
            anyhow::ensure!(
                self.strategy.momentum_short_days.is_some()
                    && self.strategy.momentum_long_days.is_some()
                    && self.strategy.volatility_window.is_some(),
                "ETF rotation score requires momentum_short_days, momentum_long_days and volatility_window"
            );
            anyhow::ensure!(
                self.strategy.momentum_short_days.unwrap_or_default() > 0
                    && self.strategy.momentum_long_days.unwrap_or_default() > 0
                    && self.strategy.volatility_window.unwrap_or_default() > 1
                    && self.strategy.trend_window.is_none_or(|v| v > 0),
                "ETF rotation score windows must be positive and volatility_window must exceed one"
            );
            anyhow::ensure!(
                [
                    self.strategy.short_momentum_weight,
                    self.strategy.long_momentum_weight,
                    self.strategy.volatility_weight,
                ]
                .iter()
                .all(|v| v.is_finite() && *v >= 0.0),
                "ETF rotation score weights must be finite and non-negative"
            );
        }
        anyhow::ensure!(
            self.research.forward_days > 0 && (2..=20).contains(&self.research.quantiles),
            "research horizon must be positive and quantiles must be 2..=20"
        );
        if let (Some(start), Some(end)) = (self.start, self.end) {
            anyhow::ensure!(start <= end, "start cannot be after end");
        }
        self.costs.validate()
    }
}
