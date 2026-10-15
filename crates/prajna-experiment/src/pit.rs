//! Offline MVP-6 inputs shared by CLI and Python views; no corporate-action ledger.
use super::*;
use prajna_data::RawStore;
use prajna_research::{
    etf_eligible::EligibilityParams,
    etf_market::{DerivedMarket, MarketEvidence},
    etf_momentum::{EmptyActionCoverage, TotalReturnIndex},
    pit_strategy::{self, PitS2Params, RiskNotice, RiskSignals},
    pit_universe::{CatalogInput, HistoricalCatalog},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PitInput {
    catalog: CatalogInput,
    market: MarketEvidence,
    empty_actions: EmptyActionCoverage,
    #[serde(default)]
    eligibility: EligibilityParams,
    #[serde(default)]
    risk_notices: Vec<RiskNotice>,
}

#[derive(Debug)]
pub struct PitContext {
    pub catalog: HistoricalCatalog,
    pub market: DerivedMarket,
    pub tri: TotalReturnIndex,
    pub risk: RiskSignals,
    pub eligibility: EligibilityParams,
    pub identity: Value,
}

impl PitContext {
    pub fn build(input: &PitInput, root: &Path, dsv: &str) -> Result<Self, ExperimentError> {
        let raw = RawStore::open(root).map_err(|e| invalid(e.to_string()))?;
        let catalog = HistoricalCatalog::build(input.catalog.clone(), &raw)
            .map_err(|e| invalid(e.to_string()))?;
        let market = DerivedMarket::from_dataset(root, dsv, &catalog, input.market.clone())
            .map_err(|e| invalid(e.to_string()))?;
        let tri = TotalReturnIndex::build(&market, None, Some(&input.empty_actions), &raw)
            .map_err(|e| invalid(e.to_string()))?;
        let risk = RiskSignals::build(&catalog, input.risk_notices.clone(), &raw)
            .map_err(|e| invalid(e.to_string()))?;
        let identity = serde_json::json!({
            "semantic_version": pit_strategy::SEMANTIC_VERSION,
            "catalog_version": catalog.version().map_err(|e| invalid(e.to_string()))?,
            "derived_market_version": market.version(),
            "execution_rule_version": market.evidence().rule_version,
            "return_index": tri.identity(), "return_index_version": tri.version(),
            "eligibility": input.eligibility, "risk_signals": risk,
            "risk_exit_rule": "persistent-next-explicitly-sellable-open@1",
            "evidence_mode": "offline-opening-evidence",
            "return_mode": "price_engineering_6a_empty_actions",
        });
        Ok(Self {
            catalog,
            market,
            tri,
            risk,
            eligibility: input.eligibility.clone(),
            identity,
        })
    }

    pub fn expand(
        &self,
        experiment: &Experiment,
        combinations: &[Value],
        panel: &Panel,
    ) -> Result<Vec<ExpandedRun>, ExperimentError> {
        let mut runs = BTreeMap::new();
        for value in combinations {
            let params: PitS2Params = serde_json::from_value(value.clone())?;
            let plan = pit_strategy::plan(
                &self.catalog,
                &self.market,
                &self.tri,
                &self.risk,
                self.eligibility.clone(),
                &params,
                &panel.sessions,
            )
            .map_err(|e| invalid(e.to_string()))?;
            let mut spec = serde_json::json!({
                "dsv": experiment.dsv, "universe": experiment.universe, "venue": experiment.venue,
                "strategy": {"kind": experiment.strategy_kind, "parameters": params},
                "pit": self.identity, "pit_plan_version": plan.version,
                "session_inputs": plan.inputs,
                "execution_exclusions": self.market.cells().values().filter(|cell| {
                    !cell.permits(true) || !cell.permits(false)
                }).collect::<Vec<_>>(),
                "costs": experiment.costs.normalized(), "availability_assumption": "none",
                "engine": {"kind":"vector", "semantic_version":"vector@1"}, "seed": null,
            });
            experiment.engine.normalize_spec(&mut spec);
            spec["engine"]["pit_semantic_version"] =
                serde_json::json!(pit_strategy::SEMANTIC_VERSION);
            let id = identity(
                "run",
                &restricted_jcs(&spec).map_err(|e| invalid(e.to_string()))?,
            );
            runs.insert(
                id.clone(),
                ExpandedRun {
                    run_id: id,
                    run_spec: spec,
                    parameters: None,
                    factor: None,
                    strategy: RunStrategy::Prepared(
                        if engine::is_python_strategy(&experiment.strategy_kind) {
                            Err("strategy requires Python host".into())
                        } else {
                            Ok(plan.decisions)
                        },
                    ),
                },
            );
        }
        Ok(runs.into_values().collect())
    }
}
