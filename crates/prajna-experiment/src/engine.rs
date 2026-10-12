//! Engine configuration, strategy expansion and the per-Run failure boundary.
use super::*;
use prajna_account::VirtualPortfolioId;
use prajna_domain::{Currency, Notional, Quantity};
use prajna_research::{
    ExecutionStatusMap,
    engine::Engine,
    fast_event::{
        self, EntryBudget, FastEventConfig, FastEventCosts, PortfolioRun, Rebalance, Sizing,
    },
    strategy::{BuyAndHoldParams, Decision, MaCrossoverParams, UnfilledEntry},
};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FastSettingsInput {
    currency: String,
    initial_cash: String,
    sizing: String,
    rebalance: String,
    #[serde(default)]
    entry_budget: Option<String>,
    minimum_commission: String,
    portfolios: Vec<CapitalInput>,
    lot_size_source: LotSource,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapitalInput {
    id: u64,
    capital: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum LotSource {
    Instrument,
    Explicit {
        values: BTreeMap<InstrumentId, String>,
    },
}

#[derive(Debug, Clone)]
pub(super) struct EngineSettings {
    fast: Option<FastEventConfig>,
    capitals: BTreeMap<VirtualPortfolioId, Notional>,
    rebalance: Rebalance,
    policy: UnfilledEntry,
    normalized: Option<Value>,
}
impl EngineSettings {
    pub(super) fn parse(input: &DefinitionInput, lake: &Path) -> Result<Self, ExperimentError> {
        if !matches!(
            input.strategy.as_str(),
            "s2_rotation" | "buy_and_hold" | "ma_crossover"
        ) {
            return Err(invalid(
                "strategy must be s2_rotation, buy_and_hold or ma_crossover",
            ));
        }
        let policy = match input.unfilled_entry.as_deref() {
            None if input.strategy == "buy_and_hold" => UnfilledEntry::Retry,
            None | Some("skip") => UnfilledEntry::Skip,
            Some("retry") => UnfilledEntry::Retry,
            _ => return Err(invalid("unfilled_entry must be skip or retry")),
        };
        if input.strategy == "buy_and_hold" && policy != UnfilledEntry::Retry {
            return Err(invalid("BuyAndHold requires unfilled_entry=retry"));
        }
        let mut result = Self {
            fast: None,
            capitals: BTreeMap::new(),
            rebalance: Rebalance::FullTarget,
            policy,
            normalized: None,
        };
        if input.engine == "vector" {
            if input.fast_event.is_some() {
                return Err(invalid("vector cannot accept fast_event settings"));
            }
            return Ok(result);
        }
        if input.engine == "accurate_event" {
            return Err(invalid(format!(
                "accurate_event × {}: Accurate Event execution is not implemented; routing rules are registered in prajna-research",
                input.strategy
            )));
        }
        if input.engine != "fast_event" {
            return Err(invalid("engine must be vector or fast_event"));
        }
        let settings = input
            .fast_event
            .as_ref()
            .ok_or_else(|| invalid("fast_event settings are required"))?;
        let decimal = |value: &str| -> Result<Notional, ExperimentError> {
            value
                .parse()
                .map_err(|e| invalid(format!("invalid decimal {value}: {e}")))
        };
        let initial_cash = decimal(&settings.initial_cash)?;
        if initial_cash <= Notional::ZERO {
            return Err(invalid("initial_cash must be positive"));
        }
        let sizing = match settings.sizing.as_str() {
            "vector_parity" => Sizing::VectorParity,
            "lot" => Sizing::Lot,
            _ => return Err(invalid("sizing must be vector_parity or lot")),
        };
        result.rebalance = match (
            settings.rebalance.as_str(),
            settings.entry_budget.as_deref(),
        ) {
            ("full_target", None) => Rebalance::FullTarget,
            ("entry_only", Some("fixed_initial")) => {
                Rebalance::EntryOnly(EntryBudget::FixedInitial)
            }
            ("entry_only", Some("remaining_cash")) => {
                Rebalance::EntryOnly(EntryBudget::RemainingCash)
            }
            _ => {
                return Err(invalid(
                    "full_target omits entry_budget; entry_only requires fixed_initial or remaining_cash",
                ));
            }
        };
        Engine::FastEvent {
            sizing,
            rebalance: result.rebalance,
        }
        .validate_mode(&input.strategy)
        .map_err(|error| invalid(error.to_string()))?;
        if result.rebalance != Rebalance::FullTarget && policy != UnfilledEntry::Retry {
            return Err(invalid(format!(
                "fast_event × {} × sizing={}, rebalance=entry_only, unfilled_entry={}: entry_only requires unfilled_entry=retry",
                input.strategy,
                settings.sizing,
                result.policy_name()
            )));
        }
        let mut total = 0i128;
        for capital in &settings.portfolios {
            if capital.id > MAX_SAFE_INTEGER {
                return Err(invalid("portfolio ID exceeds 2^53-1"));
            }
            let amount = decimal(&capital.capital)?;
            if amount <= Notional::ZERO {
                return Err(invalid("portfolio capital must be positive"));
            }
            total = total
                .checked_add(amount.mantissa())
                .ok_or_else(|| invalid("capital overflow"))?;
            if result
                .capitals
                .insert(VirtualPortfolioId(capital.id), amount)
                .is_some()
            {
                return Err(invalid("duplicate portfolio ID"));
            }
        }
        if result.capitals.is_empty() || total > initial_cash.mantissa() {
            return Err(invalid(
                "VP capital must be nonempty and not exceed initial_cash",
            ));
        }
        let universe = StaticUniverse::load(lake, &input.universe)?;
        let manifest = read_manifest(lake, &input.dsv)?;
        let table = manifest
            .core
            .tables
            .iter()
            .find(|t| t.table == "instruments")
            .ok_or_else(|| invalid("missing instruments table"))?;
        let batches = prajna_data::read_parquet(
            lake.join("normalized/instruments")
                .join(input.dsv.trim_start_matches("dsv:sha256:"))
                .join("part-00000.parquet"),
            "instruments",
            &table.schema_version.to_string(),
        )
        .map_err(|e| invalid(e.to_string()))?;
        let mut instruments = BTreeMap::new();
        for batch in batches {
            if batch.schema().metadata().get("prajna.dsv") != Some(&input.dsv) {
                return Err(invalid("Instrument DSV mismatch"));
            }
            for instrument in prajna_data::instruments_from_record_batch(&batch)
                .map_err(|e| invalid(e.to_string()))?
            {
                if instrument.id().venue().as_str() == input.venue {
                    if universe.members().contains(instrument.id())
                        && instrument.quote_currency().map(|c| c.as_str())
                            != Some(settings.currency.as_str())
                    {
                        return Err(invalid("Instrument currency differs from account currency"));
                    }
                    instruments.insert(instrument.id().clone(), instrument);
                }
            }
        }
        let mut lots = BTreeMap::new();
        let source = match &settings.lot_size_source {
            LotSource::Explicit { values } => {
                for (id, size) in values {
                    let size: Quantity = size
                        .parse()
                        .map_err(|e| invalid(format!("invalid lot size: {e}")))?;
                    if size <= Quantity::ZERO || !instruments.contains_key(id) {
                        return Err(invalid(
                            "lot size must be positive and belong to dataset Venue",
                        ));
                    }
                    lots.insert(id.clone(), size);
                }
                "explicit"
            }
            LotSource::Instrument => {
                lots.extend(instruments.iter().filter_map(|(id, instrument)| {
                    instrument.lot_size().map(|size| (id.clone(), size))
                }));
                "instrument"
            }
        };
        // The L1 seam validates the whole Venue panel, including non-Universe marks.
        if sizing == Sizing::Lot && instruments.keys().any(|id| !lots.contains_key(id)) {
            return Err(invalid(
                "lot sizing requires every dataset Venue Instrument lot_size",
            ));
        }
        let costs = Costs::parse(&input.costs)?;
        let rate = |v: f64| decimal(&v.to_string());
        let bps = |v: f64| {
            rate(v)?
                .checked_div_decimal("10000".parse().unwrap())
                .map_err(|e| invalid(e.to_string()))
        };
        let minimum_commission = decimal(&settings.minimum_commission)?;
        if minimum_commission < Notional::ZERO {
            return Err(invalid("minimum_commission must be nonnegative"));
        }
        let config = FastEventConfig {
            currency: settings
                .currency
                .parse::<Currency>()
                .map_err(|e| invalid(e.to_string()))?,
            venue: input
                .venue
                .parse()
                .map_err(|e| invalid(format!("invalid Venue: {e}")))?,
            initial_cash,
            sizing,
            costs: FastEventCosts {
                commission_rate: rate(costs.commission_rate)?,
                buy_slippage_rate: bps(costs.buy_slippage_bps)?,
                sell_slippage_rate: bps(costs.sell_slippage_bps)?,
                buy_tax_rate: rate(costs.buy_tax_rate)?,
                sell_tax_rate: rate(costs.sell_tax_rate)?,
                minimum_commission,
            },
            lot_sizes: lots,
        };
        let one: Notional = "1".parse().unwrap();
        if [
            config.costs.commission_rate,
            config.costs.buy_slippage_rate,
            config.costs.sell_slippage_rate,
            config.costs.buy_tax_rate,
            config.costs.sell_tax_rate,
        ]
        .iter()
        .any(|v| *v >= one)
        {
            return Err(invalid("Fast Event rates must be less than one"));
        }
        result.normalized = Some(serde_json::json!({
            "currency": settings.currency, "initial_cash": initial_cash.to_string(), "sizing": settings.sizing,
            "rebalance": settings.rebalance, "entry_budget": settings.entry_budget, "minimum_commission": minimum_commission.to_string(),
            "portfolios": result.capitals.iter().map(|(id,capital)| serde_json::json!({"id":id.0,"capital":capital.to_string()})).collect::<Vec<_>>(),
            "lot_size_source": {"kind":source, "resolved":config.lot_sizes.iter().map(|(id,size)| (id.to_string(),Value::String(size.to_string()))).collect::<Map<_,_>>()},
        }));
        result.fast = Some(config);
        Ok(result)
    }
    fn policy_name(&self) -> &'static str {
        if self.policy == UnfilledEntry::Skip {
            "skip"
        } else {
            "retry"
        }
    }
    pub(super) fn selection(&self) -> Engine {
        match &self.fast {
            Some(config) => Engine::FastEvent {
                sizing: config.sizing,
                rebalance: self.rebalance,
            },
            None => Engine::Vector,
        }
    }
    pub(super) fn normalize_definition(&self, value: &mut Value, strategy: &str) {
        if self.fast.is_some() || self.policy != UnfilledEntry::Skip || strategy != "s2_rotation" {
            value["unfilled_entry"] = self.policy_name().into();
        }
        if let Some(settings) = &self.normalized {
            value["fast_event"] = settings.clone();
        }
    }
    pub(super) fn normalize_spec(&self, value: &mut Value) {
        if self.fast.is_some()
            || self.policy != UnfilledEntry::Skip
            || value["strategy"]["kind"] != "s2_rotation"
        {
            value["strategy"]["unfilled_entry"] = self.policy_name().into();
        }
        if let Some(settings) = &self.normalized {
            value["engine"] = serde_json::json!({"kind":"fast_event", "semantic_version":fast_event::SEMANTIC_VERSION});
            value["fast_event"] = settings.clone();
        }
    }
    pub(super) fn set_policy(&self, strategy: &mut VectorStrategy) {
        if let VectorStrategy::TopKRank(p) = strategy {
            p.unfilled_entry = self.policy;
        }
    }
}

pub(super) fn parse_space(kind: &str, space: &Value) -> Result<ParameterSpace, ExperimentError> {
    if kind == "s2_rotation" {
        return parse_parameter_space(space);
    }
    let object = space
        .as_object()
        .ok_or_else(|| invalid("parameter_space must be object"))?;
    if object.len() != 1 {
        return Err(invalid(
            "parameter_space must contain exactly one of grid or list",
        ));
    }
    let mut combinations = vec![];
    if let Some(list) = object.get("list") {
        combinations = list
            .as_array()
            .ok_or_else(|| invalid("list must be array"))?
            .clone();
    } else if let Some(grid) = object.get("grid") {
        let axes = grid
            .as_object()
            .ok_or_else(|| invalid("grid must be object"))?;
        let expected = if kind == "buy_and_hold" {
            vec!["params_version"]
        } else {
            vec!["params_version", "short_window", "long_window"]
        };
        exact_keys(axes, &expected, "strategy grid")?;
        combinations.push(serde_json::json!({}));
        for (name, values) in axes {
            let values = values
                .as_array()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| invalid("grid axis must be nonempty array"))?;
            combinations = combinations
                .into_iter()
                .flat_map(|combo| {
                    values.iter().map(move |v| {
                        let mut c = combo.clone();
                        c[name] = v.clone();
                        c
                    })
                })
                .collect();
        }
    } else {
        return Err(invalid("parameter_space must contain grid or list"));
    }
    if combinations.is_empty() {
        return Err(invalid("parameter list must not be empty"));
    }
    for parameters in &combinations {
        native_strategy(kind, parameters)?;
    }
    Ok(ParameterSpace::Native {
        combinations,
        normalized: space.clone(),
    })
}
fn native_strategy(kind: &str, value: &Value) -> Result<VectorStrategy, ExperimentError> {
    let strategy = match kind {
        "buy_and_hold" => {
            VectorStrategy::BuyAndHold(serde_json::from_value::<BuyAndHoldParams>(value.clone())?)
        }
        "ma_crossover" => {
            VectorStrategy::MaCrossover(serde_json::from_value::<MaCrossoverParams>(value.clone())?)
        }
        _ => return Err(invalid("unknown native strategy")),
    };
    VectorEngine::validate(&strategy).map_err(|e| invalid(e.to_string()))?;
    Ok(strategy)
}
pub(super) fn expand_native(
    experiment: &Experiment,
    parameters: &[Value],
) -> Result<Vec<ExpandedRun>, ExperimentError> {
    let mut runs = BTreeMap::new();
    for params in parameters {
        let strategy = native_strategy(&experiment.strategy_kind, params)?;
        experiment
            .engine
            .selection()
            .validate(&strategy)
            .map_err(|error| invalid(error.to_string()))?;
        let mut spec = serde_json::json!({"dsv":experiment.dsv,"universe":experiment.universe,"venue":experiment.venue,
            "strategy":{"kind":experiment.strategy_kind,"parameters":params}, "costs":experiment.costs.normalized(),
            "availability_assumption":experiment.availability_assumption,"engine":{"kind":"vector","semantic_version":"vector@1"},"seed":null});
        experiment.engine.normalize_spec(&mut spec);
        let run_id = identity(
            "run",
            &restricted_jcs(&spec).map_err(|e| invalid(e.to_string()))?,
        );
        runs.insert(
            run_id.clone(),
            ExpandedRun {
                run_spec: spec,
                run_id,
                parameters: None,
                factor: None,
                strategy,
            },
        );
    }
    Ok(runs.into_values().collect())
}
pub(super) fn decisions(
    engine: Engine,
    run: &ExpandedRun,
    values: &HashMap<String, DataFrame>,
    panel: &Panel,
    members: &[InstrumentId],
    assumption: AvailabilityAssumption,
) -> Result<Vec<Decision>, String> {
    if let Some(factor) = &run.factor {
        let score = values
            .get(&factor.canonical_json().to_string())
            .ok_or("stage 1 did not produce this Run's score Factor Values")?;
        engine
            .decide(&run.strategy, score, &panel.sessions)
            .map_err(|e| e.to_string())
    } else {
        engine
            .decide_panel(&run.strategy, panel, members, assumption)
            .map_err(|e| e.to_string())
    }
}
#[allow(clippy::too_many_arguments)]
pub(super) fn execute(
    experiment: &Experiment,
    run: &ExpandedRun,
    factors: &HashMap<String, DataFrame>,
    panel: &Panel,
    statuses: &ExecutionStatusMap,
    executable: &HashMap<(InstrumentId, chrono::NaiveDate), bool>,
    costs: &VectorCosts,
    assumption: AvailabilityAssumption,
    members: &[InstrumentId],
    level: ResultLevel,
) -> RunExecution {
    let mut execution = RunExecution {
        run_spec: run.run_spec.clone(),
        run_id: run.run_id.clone(),
        status: RunStatus::Completed,
        result: None,
        fast_event_result: None,
        summary: None,
        error: None,
    };
    let result = (|| -> Result<(), String> {
        let engine = experiment.engine.selection();
        engine
            .validate(&run.strategy)
            .map_err(|error| error.to_string())?;
        if let Some(config) = &experiment.engine.fast {
            let decisions = decisions(engine, run, factors, panel, members, assumption)?;
            let portfolios = experiment
                .engine
                .capitals
                .iter()
                .map(|(id, capital)| PortfolioRun {
                    id: *id,
                    capital: *capital,
                    decisions: decisions.clone(),
                    unfilled_entry: experiment.engine.policy,
                    rebalance: experiment.engine.rebalance,
                })
                .collect::<Vec<_>>();
            let result = fast_event::run_fast_event(panel, statuses, config, &portfolios)
                .map_err(|e| e.to_string())?;
            execution.summary = Some(crate::summary::summarize_fast(
                &result,
                experiment.sessions_per_year,
                &experiment.availability_assumption,
            )?);
            if level != ResultLevel::Summary {
                execution.fast_event_result = Some(result);
            }
        } else {
            // Native strategy membership is the declared Universe, never all dataset instruments.
            let decisions = decisions(engine, run, factors, panel, members, assumption)?;
            execution.result = Some(
                run_vector_with_policy(
                    panel,
                    &decisions,
                    experiment.engine.policy,
                    costs,
                    assumption,
                    |id, s| {
                        executable
                            .get(&(id.clone(), s.session_date))
                            .copied()
                            .unwrap_or(false)
                    },
                )
                .map_err(|e| e.to_string())?,
            );
        }
        Ok(())
    })();
    if let Err(error) = result {
        execution.status = RunStatus::Failed;
        execution.error = Some(error);
        execution.result = None;
        execution.fast_event_result = None;
        execution.summary = None;
    }
    execution
}
