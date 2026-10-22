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

pub const ACCURATE_SEMANTIC_VERSION: &str = "accurate_event@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendIdentity {
    pub name: String,
    pub version: String,
}

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
#[serde(deny_unknown_fields)]
pub(super) struct VectorPitSettingsInput {
    initial_capital: String,
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
    pub(super) backend: Option<BackendIdentity>,
    capitals: BTreeMap<VirtualPortfolioId, Notional>,
    rebalance: Rebalance,
    policy: UnfilledEntry,
    normalized: Option<Value>,
    vector_pit_capital: Option<Notional>,
}
impl EngineSettings {
    pub(super) fn parse(input: &DefinitionInput, lake: &Path) -> Result<Self, ExperimentError> {
        if !matches!(
            input.strategy.as_str(),
            "s2_rotation" | "s2_pit_rotation" | "buy_and_hold" | "ma_crossover"
        ) && !is_python_strategy(&input.strategy)
        {
            return Err(invalid(
                "strategy must be s2_rotation, buy_and_hold, ma_crossover or strategy_id@version",
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
            backend: input.backend.clone(),
            capitals: BTreeMap::new(),
            rebalance: Rebalance::FullTarget,
            policy,
            normalized: None,
            vector_pit_capital: None,
        };
        if input.vector_pit.is_some() && (input.engine != "vector" || input.pit.is_none()) {
            return Err(invalid("vector_pit is only valid for Vector PIT S2"));
        }
        if input.engine != "accurate_event" && input.backend.is_some() {
            return Err(invalid("Backend identity is only valid for accurate_event"));
        }
        if input.engine == "vector" {
            if input.fast_event.is_some() {
                return Err(invalid("vector cannot accept fast_event settings"));
            }
            if input.pit.is_some() {
                let settings = input
                    .vector_pit
                    .as_ref()
                    .ok_or_else(|| invalid("Vector PIT requires vector_pit.initial_capital"))?;
                let capital: Notional = settings
                    .initial_capital
                    .parse()
                    .map_err(|e| invalid(format!("invalid PIT capital: {e}")))?;
                if capital <= Notional::ZERO {
                    return Err(invalid("Vector PIT initial_capital must be positive"));
                }
                result.vector_pit_capital = Some(capital);
            }
            return Ok(result);
        }
        if !matches!(input.engine.as_str(), "fast_event" | "accurate_event") {
            return Err(invalid(
                "engine must be vector, fast_event or accurate_event",
            ));
        }
        if input.engine == "accurate_event"
            && input.backend.as_ref().is_none_or(|backend| {
                backend.name.trim().is_empty()
                    || backend.version.trim().is_empty()
                    || backend.name.trim() != backend.name
                    || backend.version.trim() != backend.version
            })
        {
            return Err(invalid(
                "accurate_event requires Backend identity with nonempty, trimmed name and version",
            ));
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
        result
            .selection_for(sizing)
            .validate_mode(&input.strategy)
            .map_err(|error| invalid(error.to_string()))?;
        if result.rebalance != Rebalance::FullTarget && policy != UnfilledEntry::Retry {
            return Err(invalid(format!(
                "{} × {} × sizing={}, rebalance=entry_only, unfilled_entry={}: entry_only requires unfilled_entry=retry",
                input.engine,
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
        let universe = if input.pit.is_some() {
            None
        } else {
            Some(StaticUniverse::load(lake, &input.universe)?)
        };
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
                    if universe
                        .as_ref()
                        .is_none_or(|u| u.members().contains(instrument.id()))
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
    pub(super) fn configure_pit(
        &mut self,
        catalog: &prajna_research::pit_universe::HistoricalCatalog,
    ) -> Result<(), ExperimentError> {
        if self.rebalance != Rebalance::FullTarget || self.policy != UnfilledEntry::Skip {
            return Err(invalid(
                "PIT S2 requires full_target and unfilled_entry=skip",
            ));
        }
        if let Some(config) = &mut self.fast {
            let mut lots = BTreeMap::new();
            for life in &catalog.input().lifecycles {
                let native: InstrumentId = format!("{}.SZSE", life.native_symbol)
                    .parse()
                    .map_err(|e| invalid(format!("{e}")))?;
                if let Some(lot) = config.lot_sizes.get(&native) {
                    lots.insert(
                        life.instrument_id().map_err(|e| invalid(e.to_string()))?,
                        *lot,
                    );
                } else if config.sizing == Sizing::Lot {
                    return Err(invalid(
                        "PIT lot sizing requires a resolved native lot for each lifecycle",
                    ));
                }
            }
            config.lot_sizes = lots;
            if let Some(normalized) = &mut self.normalized {
                normalized["lot_size_source"]["resolved"] = serde_json::json!(
                    config
                        .lot_sizes
                        .iter()
                        .map(|(id, q)| (id.to_string(), q.to_string()))
                        .collect::<BTreeMap<_, _>>()
                );
            }
        }
        Ok(())
    }
    pub(super) fn packet(
        &self,
        lake: &Path,
        dsv: &str,
    ) -> Result<prajna_adapter::RunPacket, ExperimentError> {
        let config = self
            .fast
            .as_ref()
            .ok_or_else(|| invalid("missing lot configuration"))?;
        prajna_adapter::RunPacket::from_dataset_with_lots(lake, dsv, &config.lot_sizes)
            .map_err(|error| execution_error("load effective Run packet", error))
    }
    pub(super) fn selection(&self) -> Engine {
        match &self.fast {
            Some(config) => self.selection_for(config.sizing),
            None => Engine::Vector,
        }
    }
    fn selection_for(&self, sizing: Sizing) -> Engine {
        if self.backend.is_some() {
            Engine::AccurateEvent {
                sizing,
                rebalance: self.rebalance,
            }
        } else {
            Engine::FastEvent {
                sizing,
                rebalance: self.rebalance,
            }
        }
    }
    pub(super) fn normalize_definition(&self, value: &mut Value, strategy: &str) {
        if let Some(capital) = self.vector_pit_capital {
            value["vector_pit"] = serde_json::json!({"initial_capital":capital.to_string()});
        }
        if self.fast.is_some() || self.policy != UnfilledEntry::Skip || strategy != "s2_rotation" {
            value["unfilled_entry"] = self.policy_name().into();
        }
        if let Some(settings) = &self.normalized {
            value["fast_event"] = settings.clone();
        }
        if let Some(backend) = &self.backend {
            value["backend"] = serde_json::json!(backend);
        }
    }
    pub(super) fn normalize_spec(&self, value: &mut Value) {
        if let Some(capital) = self.vector_pit_capital {
            value["engine"]["initial_capital"] = serde_json::json!(capital.to_string());
        }
        if self.fast.is_some()
            || self.policy != UnfilledEntry::Skip
            || value["strategy"]["kind"] != "s2_rotation"
        {
            value["strategy"]["unfilled_entry"] = self.policy_name().into();
        }
        if let Some(settings) = &self.normalized {
            value["engine"] = if self.backend.is_some() {
                serde_json::json!({"kind":"accurate_event", "semantic_version":ACCURATE_SEMANTIC_VERSION})
            } else {
                let semantic_version = match self.fast.as_ref().map(|config| config.sizing) {
                    Some(Sizing::VectorParity) => fast_event::PARITY_CASH_SEMANTIC_VERSION,
                    _ => fast_event::SEMANTIC_VERSION,
                };
                serde_json::json!({"kind":"fast_event", "semantic_version": semantic_version})
            };
            value["fast_event"] = settings.clone();
        }
        if let Some(backend) = &self.backend {
            value["backend"] = serde_json::json!(backend);
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
    let python = is_python_strategy(kind);
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
        let expected = if kind == "s2_pit_rotation" {
            vec!["lookback", "top_k", "rebalance_every"]
        } else if kind == "buy_and_hold" {
            vec!["params_version"]
        } else {
            vec!["params_version", "short_window", "long_window"]
        };
        if !python {
            exact_keys(axes, &expected, "strategy grid")?;
        }
        combinations.push(serde_json::json!({}));
        for (name, values) in axes {
            let values = values
                .as_array()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| invalid("grid axis must be nonempty array"))?;
            combinations
                .len()
                .checked_mul(values.len())
                .ok_or_else(|| invalid("parameter grid size exceeds addressable memory"))?;
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
        if python {
            if !parameters.is_object() {
                return Err(invalid("Python strategy parameters must be JSON objects"));
            }
            normalize_python_parameters(parameters)?;
        } else if kind == "s2_pit_rotation" {
            let params: prajna_research::pit_strategy::PitS2Params =
                serde_json::from_value(parameters.clone())?;
            params.validate().map_err(|e| invalid(e.to_string()))?;
        } else {
            native_strategy(kind, parameters)?;
        }
    }
    Ok(ParameterSpace::Native {
        combinations,
        normalized: if python {
            normalize_python_parameters(space)?
        } else {
            space.clone()
        },
    })
}

pub(super) fn is_python_strategy(kind: &str) -> bool {
    kind.split_once('@').is_some_and(|(id, version)| {
        !id.is_empty()
            && !version.is_empty()
            && !version.contains('@')
            && !kind.chars().any(char::is_whitespace)
    })
}

fn normalize_python_parameters(value: &Value) -> Result<Value, ExperimentError> {
    // Tag floats and strings so arbitrary dictionaries cannot alias float bit strings.
    match value {
        Value::Number(number) if number.is_f64() => Ok(serde_json::json!({
            "f64": canonical_float_bits(finite_number(value, "Python parameter")?)
        })),
        Value::Number(_) => {
            // Reuse the identity contract's safe-integer validation.
            restricted_jcs(value).map_err(|error| invalid(error.to_string()))?;
            Ok(value.clone())
        }
        Value::Array(values) => values
            .iter()
            .map(normalize_python_parameters)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), normalize_python_parameters(value)?)))
            .collect::<Result<Map<_, _>, ExperimentError>>()
            .map(Value::Object),
        Value::String(text) => Ok(serde_json::json!({"string": text})),
        _ => Ok(value.clone()),
    }
}

pub(super) fn python_run_specs(
    experiment: &Experiment,
    combinations: &[Value],
) -> Result<Vec<RunDefinition>, ExperimentError> {
    let mut runs = BTreeMap::new();
    for parameters in combinations {
        let mut spec = serde_json::json!({
            "dsv": experiment.dsv, "universe": experiment.universe, "venue": experiment.venue,
            "strategy": {"kind": experiment.strategy_kind, "parameters": normalize_python_parameters(parameters)?},
            "costs": experiment.costs.normalized(),
            "availability_assumption": experiment.availability_assumption,
            "engine": {"kind": "vector", "semantic_version": "vector@1"}, "seed": null,
        });
        experiment.engine.normalize_spec(&mut spec);
        let run_id = identity(
            "run",
            &restricted_jcs(&spec).map_err(|error| invalid(error.to_string()))?,
        );
        runs.insert(
            run_id.clone(),
            RunDefinition {
                run_id,
                run_spec: spec,
            },
        );
    }
    Ok(runs.into_values().collect())
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
                strategy: RunStrategy::Native(strategy),
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
    let strategy = match &run.strategy {
        RunStrategy::Prepared(decisions) => {
            let decisions = decisions.as_ref().map_err(Clone::clone)?;
            validate_prepared(decisions, panel, members)?;
            engine
                .validate_mode(&run.run_spec["strategy"]["kind"].to_string())
                .map_err(|error| error.to_string())?;
            return Ok(decisions.clone());
        }
        RunStrategy::Native(strategy) => strategy,
    };
    if let Some(factor) = &run.factor {
        let score = values
            .get(&factor.canonical_json().to_string())
            .ok_or("stage 1 did not produce this Run's score Factor Values")?;
        engine
            .decide(strategy, score, &panel.sessions)
            .map_err(|e| e.to_string())
    } else {
        engine
            .decide_panel(strategy, panel, members, assumption)
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
    packet: Option<&prajna_adapter::RunPacket>,
) -> RunExecution {
    let mut execution = RunExecution {
        run_spec: run.run_spec.clone(),
        run_id: run.run_id.clone(),
        status: RunStatus::Completed,
        result: None,
        fast_event_result: None,
        backend_diagnostics: None,
        adapter_events: vec![],
        backend_events: vec![],
        summary: None,
        error: None,
    };
    let result = (|| -> Result<(), String> {
        if let Some(fixed_value) = run
            .run_spec
            .get("pit")
            .and_then(|pit| pit.get("fixed_universe"))
            .filter(|value| !value.is_null())
        {
            let fixed: prajna_research::pit_universe::FixedUniverseSpec =
                serde_json::from_value(fixed_value.clone())
                    .map_err(|error| format!("invalid fixed Universe Run Spec: {error}"))?;
            fixed
                .validate_escalation_threshold()
                .map_err(|error| error.to_string())?;
            if let Some(minimum) = fixed.minimum_code_count
                && run
                    .run_spec
                    .get("session_inputs")
                    .and_then(Value::as_array)
                    .is_some_and(|sessions| {
                        sessions.iter().any(|input| {
                            input
                                .get("eligible")
                                .and_then(|eligible| eligible.get("universe"))
                                .and_then(|universe| universe.get("run_spec"))
                                .and_then(|spec| spec.get("fixed_list"))
                                .and_then(Value::as_array)
                                .is_some_and(|codes| codes.len() < minimum)
                        })
                    })
            {
                return Err(format!(
                    "P27-19 escalation: effective fixed Universe has fewer than {minimum} codes"
                ));
            }
        }
        let engine = experiment.engine.selection();
        if let RunStrategy::Native(strategy) = &run.strategy {
            engine
                .validate(strategy)
                .map_err(|error| error.to_string())?;
        }
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
            let result = if let Some(identity) = &experiment.engine.backend {
                if experiment
                    .pit
                    .as_ref()
                    .is_some_and(|pit| pit.has_corporate_action_table)
                {
                    return Err(
                        "Accurate Event does not implement the corporate-action ledger".into(),
                    );
                }
                let factory = experiment
                    .backend_factory
                    .as_ref()
                    .ok_or("missing worker Backend")?;
                let mut backend = factory
                    .create(identity, config, &experiment.dsv)
                    .map_err(|e| e.to_string())?;
                let result = prajna_adapter::run_adapter(
                    packet.ok_or("missing Run packet")?,
                    panel,
                    statuses,
                    config,
                    &portfolios,
                    backend.as_mut(),
                )
                .map_err(|e| e.to_string())?;
                let mut diagnostics = backend.finish().map_err(|e| e.to_string())?;
                diagnostics
                    .compare_accounts(&result.execution, &result.backend_events, config)
                    .map_err(|e| e.to_string())?;
                if level == ResultLevel::Full {
                    execution.adapter_events = result.adapter_events;
                    execution.backend_events = result.backend_events;
                }
                execution.backend_diagnostics = Some(diagnostics);
                result.execution
            } else {
                if let Some(pit) = &experiment.pit {
                    let actions = pit
                        .corporate_action_schedule(panel)
                        .map_err(|e| e.to_string())?;
                    if pit.terminal_settlement_semantic_version.is_some() {
                        fast_event::run_fast_event_pit_with_terminal_settlement(
                            panel,
                            &pit.market,
                            &pit.risk,
                            config,
                            &portfolios,
                            actions.as_ref(),
                        )
                    } else {
                        match actions {
                            Some(actions) => fast_event::run_fast_event_pit_with_actions(
                                panel,
                                &pit.market,
                                &pit.risk,
                                config,
                                &portfolios,
                                &actions,
                            ),
                            None => fast_event::run_fast_event_pit(
                                panel,
                                &pit.market,
                                &pit.risk,
                                config,
                                &portfolios,
                            ),
                        }
                    }
                } else {
                    fast_event::run_fast_event(panel, statuses, config, &portfolios)
                }
                .map_err(|e| e.to_string())?
            };
            execution.summary = Some(crate::summary::summarize_fast(
                &result,
                experiment.sessions_per_year,
                &experiment.availability_assumption,
            )?);
            if let Some(diagnostics) = &execution.backend_diagnostics {
                execution
                    .summary
                    .as_mut()
                    .ok_or("missing summary")?
                    .assumptions
                    .push(format!(
                        "accurate_event@1 Backend evidence: {}; not #133 A15 acceptance",
                        diagnostics.evidence
                    ));
                let unclassified = diagnostics
                    .comparisons
                    .iter()
                    .filter(|comparison| comparison["difference_class"] == "unclassified")
                    .count();
                if unclassified > 0 {
                    execution.summary.as_mut().ok_or("missing summary")?.assumptions.push(
                        format!("{unclassified} unclassified Backend account differences; #133 A15 would not pass")
                    );
                }
            }
            if level != ResultLevel::Full {
                execution.backend_diagnostics = None;
            }
            if level != ResultLevel::Summary {
                execution.fast_event_result = Some(result);
            }
        } else {
            // Native strategy membership is the declared Universe, never all dataset instruments.
            let decisions = decisions(engine, run, factors, panel, members, assumption)?;
            execution.result = Some(
                if let Some(pit) = &experiment.pit {
                    let capital = experiment
                        .engine
                        .vector_pit_capital
                        .ok_or("missing Vector PIT capital")?;
                    let actions = pit
                        .corporate_action_schedule(panel)
                        .map_err(|e| e.to_string())?;
                    if pit.terminal_settlement_semantic_version.is_some() {
                        prajna_research::vector::run_vector_pit_with_terminal_settlement(
                            panel,
                            &decisions,
                            &pit.market,
                            &pit.risk,
                            costs,
                            capital,
                            actions.as_ref(),
                        )
                    } else {
                        match actions {
                            Some(actions) => prajna_research::vector::run_vector_pit_with_actions(
                                panel,
                                &decisions,
                                &pit.market,
                                &pit.risk,
                                costs,
                                capital,
                                &actions,
                            ),
                            None => prajna_research::vector::run_vector_pit(
                                panel,
                                &decisions,
                                &pit.market,
                                &pit.risk,
                                costs,
                                capital,
                            ),
                        }
                    }
                } else {
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
                }
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
        execution.backend_diagnostics = None;
        execution.adapter_events.clear();
        execution.backend_events.clear();
        execution.summary = None;
    }
    execution
}

// Defense at the Rust execution seam, including callers other than the Python binding.
fn validate_prepared(
    decisions: &[Decision],
    panel: &Panel,
    members: &[InstrumentId],
) -> Result<(), String> {
    let mut previous = None;
    for decision in decisions {
        let session = &decision.decision_session;
        if previous.is_some_and(|date| date >= session.session_date)
            || !panel.sessions.contains(session)
        {
            return Err(
                "prepared Decisions require strictly increasing, exact Venue Sessions".into(),
            );
        }
        previous = Some(session.session_date);
        let mut sum = 0.0;
        for (id, weight) in &decision.targets {
            if !members.contains(id) || !weight.is_finite() || *weight < 0.0 {
                return Err("prepared Decision has invalid target or weight".into());
            }
            sum += weight;
        }
        if sum > 1.0 {
            return Err("prepared Decision weight sum exceeds 1".into());
        }
        let mut ids = BTreeSet::new();
        let mut ranks = BTreeSet::new();
        for (id, score, rank) in &decision.ranked {
            if !members.contains(id)
                || !score.is_finite()
                || *rank == 0
                || !ids.insert(id)
                || !ranks.insert(rank)
            {
                return Err("prepared Decision has invalid ranking".into());
            }
        }
    }
    Ok(())
}
