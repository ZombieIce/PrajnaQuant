//! Pre-Run routing for the closed Decision strategy family.
use polars::prelude::DataFrame;
use prajna_domain::InstrumentId;

use crate::{
    Panel, PanelSession,
    factor::AvailabilityAssumption,
    fast_event::{Rebalance, Sizing},
    strategy::{Decision, EngineError, StrategyCapability, VectorEngine, VectorStrategy},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Vector,
    FastEvent {
        sizing: Sizing,
        rebalance: Rebalance,
    },
    /// Routing rules only; no Accurate Backend is implemented.
    AccurateEvent {
        sizing: Sizing,
        rebalance: Rebalance,
    },
}

impl Engine {
    fn reject(self, strategy_kind: &str, reason: &str) -> EngineError {
        let (name, sizing, rebalance) = match self {
            Self::Vector => {
                return EngineError::UnsupportedCombination(format!(
                    "vector × {strategy_kind}: {reason}"
                ));
            }
            Self::FastEvent { sizing, rebalance } => ("fast_event", sizing, rebalance),
            Self::AccurateEvent { sizing, rebalance } => ("accurate_event", sizing, rebalance),
        };
        EngineError::UnsupportedCombination(format!(
            "{name} × {strategy_kind} × sizing={}, rebalance={}: {reason}",
            if sizing == Sizing::Lot {
                "lot"
            } else {
                "vector_parity"
            },
            if rebalance == Rebalance::FullTarget {
                "full_target"
            } else {
                "entry_only"
            },
        ))
    }

    /// Validate mode rules without loading a Dataset or expanding strategy parameters.
    pub fn validate_mode(self, strategy_kind: &str) -> Result<(), EngineError> {
        match self {
            Self::AccurateEvent {
                sizing: Sizing::VectorParity,
                ..
            } => Err(self.reject(
                strategy_kind,
                "accurate_event requires lot; vector_parity is not supported",
            )),
            Self::FastEvent {
                sizing: Sizing::VectorParity,
                rebalance,
            } if rebalance != Rebalance::FullTarget => {
                Err(self.reject(strategy_kind, "vector_parity requires full_target"))
            }
            _ => Ok(()),
        }
    }

    pub fn validate(self, strategy: &VectorStrategy) -> Result<(), EngineError> {
        if self == Self::Vector {
            return VectorEngine::validate(strategy);
        }
        self.validate_mode(strategy.kind())?;
        if let VectorStrategy::TopKRank(params) = strategy {
            if params
                .capabilities
                .contains(&StrategyCapability::OrderBookDriven)
            {
                return Err(self.reject(strategy.kind(), "OrderBookDriven is not supported"));
            }
            if !params.capabilities.iter().any(|capability| {
                matches!(
                    capability,
                    StrategyCapability::Vectorizable | StrategyCapability::EventDriven
                )
            }) {
                return Err(self.reject(
                    strategy.kind(),
                    "a Decision strategy requires Vectorizable or EventDriven",
                ));
            }
        }
        strategy
            .validate_parameters()
            .map_err(|error| self.reject(strategy.kind(), &error.to_string()))
    }

    pub fn decide(
        self,
        strategy: &VectorStrategy,
        scores: &DataFrame,
        sessions: &[PanelSession],
    ) -> Result<Vec<Decision>, EngineError> {
        self.validate(strategy)?;
        strategy.decide_validated(scores, sessions)
    }

    pub fn decide_panel(
        self,
        strategy: &VectorStrategy,
        panel: &Panel,
        members: &[InstrumentId],
        assumption: AvailabilityAssumption,
    ) -> Result<Vec<Decision>, EngineError> {
        self.validate(strategy)?;
        strategy.decide_panel_validated(panel, members, assumption)
    }
}
