//! Offline MVP-6 inputs shared by CLI and Python views, including the PIT action schedule.
use super::*;
use prajna_data::RawStore;
use prajna_domain::Notional;
use prajna_research::{
    corporate_actions::CorporateActionSchedule,
    etf_eligible::EligibilityParams,
    etf_market::{DerivedMarket, MarketEvidence},
    etf_momentum::{CorporateActionRules, EmptyActionCoverage, TotalReturnIndex},
    pit_strategy::{self, PitS2Params, RiskNotice, RiskSignals},
    pit_universe::{CatalogInput, FixedUniverseSpec, HistoricalCatalog},
};

fn default_dividend_tax_rate() -> Notional {
    Notional::ZERO
}

fn default_fixed_universe() -> Option<FixedUniverseSpec> {
    Some(FixedUniverseSpec::mvp6())
}

fn bind_corporate_action_engine_version(spec: &mut Value) -> Result<(), ExperimentError> {
    if spec.get("corporate_action_schedule").is_none() {
        return Ok(());
    }
    let engine_kind = spec["engine"]
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("corporate-action Run Spec has no Engine kind"))?;
    let semantic_version = match engine_kind {
        "fast_event" => prajna_research::fast_event::CORPORATE_ACTION_SEMANTIC_VERSION,
        "vector" => prajna_research::vector::CORPORATE_ACTION_SEMANTIC_VERSION,
        "accurate_event" => {
            return Err(invalid(
                "Accurate Event does not implement the corporate-action ledger",
            ));
        }
        _ => return Err(invalid(format!("unsupported PIT Engine {engine_kind}"))),
    };
    spec["engine"]["semantic_version"] = serde_json::json!(semantic_version);
    spec["corporate_action_schedule"]["semantic_version"] =
        serde_json::json!(prajna_research::corporate_actions::SEMANTIC_VERSION);
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PitInput {
    catalog: CatalogInput,
    market: MarketEvidence,
    #[serde(default)]
    empty_actions: Option<EmptyActionCoverage>,
    #[serde(default)]
    corporate_action_rules: Option<CorporateActionRules>,
    #[serde(default = "default_dividend_tax_rate")]
    dividend_tax_rate: Notional,
    #[serde(default)]
    eligibility: EligibilityParams,
    #[serde(
        default = "default_fixed_universe",
        skip_serializing_if = "Option::is_none"
    )]
    fixed_universe: Option<FixedUniverseSpec>,
    #[serde(default)]
    risk_notices: Vec<RiskNotice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cninfo_index: Option<prajna_data::cninfo::IndexDataset>,
}

#[derive(Debug)]
pub struct PitContext {
    pub catalog: HistoricalCatalog,
    pub market: DerivedMarket,
    pub tri: TotalReturnIndex,
    pub risk: RiskSignals,
    pub eligibility: EligibilityParams,
    pub fixed_universe: Option<FixedUniverseSpec>,
    pub identity: Value,
    pub has_corporate_action_table: bool,
    pub dividend_tax_rate: Notional,
}

impl PitContext {
    pub fn build(input: &PitInput, root: &Path, dsv: &str) -> Result<Self, ExperimentError> {
        let raw = RawStore::open(root).map_err(|e| invalid(e.to_string()))?;
        let catalog = HistoricalCatalog::build(input.catalog.clone(), &raw)
            .map_err(|e| invalid(e.to_string()))?;
        let market = DerivedMarket::from_dataset(root, dsv, &catalog, input.market.clone())
            .map_err(|e| invalid(e.to_string()))?;
        let manifest = prajna_data::read_manifest(root, dsv).map_err(|e| invalid(e.to_string()))?;
        let has_actions = manifest
            .core
            .tables
            .iter()
            .any(|table| table.table == "corporate_actions");
        let tri = if has_actions {
            let rules = input.corporate_action_rules.as_ref().ok_or_else(|| {
                invalid("corporate_action_rules are required by this Dataset Version")
            })?;
            TotalReturnIndex::build_from_dataset(&market, root, rules, &raw)
        } else {
            TotalReturnIndex::build(
                &market,
                None,
                Some(input.empty_actions.as_ref().ok_or_else(|| {
                    invalid("empty_actions coverage is required for a 6a Dataset Version")
                })?),
                &raw,
            )
        }
        .map_err(|e| invalid(e.to_string()))?;
        let risk = match &input.cninfo_index {
            Some(index) => {
                RiskSignals::build_from_cninfo(&catalog, input.risk_notices.clone(), index, &raw)
            }
            None => RiskSignals::build(&catalog, input.risk_notices.clone(), &raw),
        }
        .map_err(|e| invalid(e.to_string()))?;
        let mut identity = serde_json::json!({
            "semantic_version": pit_strategy::SEMANTIC_VERSION,
            "catalog_version": catalog.version().map_err(|e| invalid(e.to_string()))?,
            "derived_market_version": market.version(),
            "execution_rule_version": market.evidence().rule_version,
            "return_index": tri.identity(), "return_index_version": tri.version(),
            "eligibility": input.eligibility, "risk_signals": risk,
            "risk_exit_rule": "persistent-next-explicitly-sellable-open@1",
            "evidence_mode": "offline-opening-evidence",
            "return_mode": if has_actions { "corporate_action_total_return_6b" } else { "price_engineering_6a_empty_actions" },
        });
        if has_actions {
            identity["dividend_tax_rate"] = serde_json::json!(input.dividend_tax_rate);
        }
        if let Some(fixed) = &input.fixed_universe {
            identity["fixed_universe"] =
                serde_json::to_value(fixed).map_err(|e| invalid(e.to_string()))?;
        }
        Ok(Self {
            catalog,
            market,
            tri,
            risk,
            eligibility: input.eligibility.clone(),
            fixed_universe: input.fixed_universe.clone(),
            identity,
            has_corporate_action_table: has_actions,
            dividend_tax_rate: input.dividend_tax_rate,
        })
    }

    pub fn corporate_action_schedule(
        &self,
        panel: &Panel,
    ) -> Result<Option<CorporateActionSchedule>, ExperimentError> {
        if !self.has_corporate_action_table {
            return Ok(None);
        }
        let version = format!(
            "{}:{}",
            prajna_research::corporate_actions::SEMANTIC_VERSION,
            self.tri.version()
        );
        CorporateActionSchedule::from_total_return_index(
            version,
            panel,
            &self.tri,
            self.dividend_tax_rate,
        )
        .map(Some)
        .map_err(|e| invalid(e.to_string()))
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
            let plan = pit_strategy::plan_with_fixed_universe(
                &self.catalog,
                &self.market,
                &self.tri,
                &self.risk,
                &params,
                pit_strategy::PitUniverseSelection {
                    eligibility: self.eligibility.clone(),
                    fixed_universe: self.fixed_universe.as_ref(),
                },
                &panel.sessions,
            )
            .map_err(|e| invalid(e.to_string()))?;
            let corporate_actions = self.corporate_action_schedule(panel)?;
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
            if let Some(schedule) = &corporate_actions {
                spec["corporate_action_schedule"] = serde_json::json!({
                    "semantic_version": prajna_research::corporate_actions::SEMANTIC_VERSION,
                    "version": schedule.version(),
                    "dividend_tax_rate": schedule.dividend_tax_rate(),
                    "coverage": schedule.coverage(),
                    "return_basis": "total_return",
                });
            }
            experiment.engine.normalize_spec(&mut spec);
            bind_corporate_action_engine_version(&mut spec)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_schedule_versions_are_bound_to_pit_engine_identity() {
        for (kind, expected) in [
            (
                "fast_event",
                prajna_research::fast_event::CORPORATE_ACTION_SEMANTIC_VERSION,
            ),
            (
                "vector",
                prajna_research::vector::CORPORATE_ACTION_SEMANTIC_VERSION,
            ),
        ] {
            let mut spec = serde_json::json!({
                "engine": {"kind": kind, "semantic_version": "legacy"},
                "corporate_action_schedule": {"version": "fixture"},
            });
            bind_corporate_action_engine_version(&mut spec).unwrap();
            assert_eq!(spec["engine"]["semantic_version"], expected);
            assert_eq!(
                spec["corporate_action_schedule"]["semantic_version"],
                prajna_research::corporate_actions::SEMANTIC_VERSION
            );
        }
    }

    #[test]
    fn action_schedule_is_rejected_for_accurate_event() {
        let mut spec = serde_json::json!({
            "engine": {"kind": "accurate_event", "semantic_version": "accurate_event@1"},
            "corporate_action_schedule": {"version": "fixture"},
        });
        assert!(
            bind_corporate_action_engine_version(&mut spec)
                .unwrap_err()
                .to_string()
                .contains("does not implement the corporate-action ledger")
        );
    }
}
