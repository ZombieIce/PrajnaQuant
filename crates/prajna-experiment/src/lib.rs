//! Experiment definitions, content-addressed identities, and in-process Run execution.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    error::Error,
    fmt, fs,
    path::Path,
    process::Command,
    time::Instant,
};

use polars::prelude::DataFrame;
use prajna_data::{ManifestError, read_manifest, restricted_jcs};
use prajna_domain::{InstrumentId, VenueId};
use prajna_research::{
    Panel, StaticUniverse,
    cache::{key::FactorKey, store::FactorCache},
    executable,
    factor::{
        AvailabilityAssumption, Factor, FactorGraph, FactorKind, RotationScoreParams,
        canonical_float_bits,
    },
    load_execution_status, load_panel,
    strategy::{
        RankDirection, StrategyCapability, VectorEngine, VectorStrategy, Weighting, execute,
    },
    vector::{VectorCosts, VectorResult, run_vector},
};
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

mod storage;
mod summary;
pub use storage::{ExecutionDiff, ResultLevel, StoredExecution, diff_executions};
pub use summary::{RunSummary, summarize};

const PARAMETERS: [&str; 9] = [
    "short",
    "long",
    "vol",
    "w_s",
    "w_l",
    "w_v",
    "trend",
    "top_k",
    "rebalance_every",
];

#[derive(Debug)]
pub enum ExperimentError {
    Json(serde_json::Error),
    Manifest(ManifestError),
    Universe(prajna_research::UniverseError),
    InvalidDefinition(String),
    Canonical(String),
    Io(std::io::Error),
    Git(String),
    Execution(String),
}

impl fmt::Display for ExperimentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid experiment JSON: {error}"),
            Self::Manifest(error) => write!(formatter, "invalid dataset manifest: {error}"),
            Self::Universe(error) => write!(formatter, "invalid experiment universe: {error}"),
            Self::InvalidDefinition(message) => formatter.write_str(message),
            Self::Canonical(message) => {
                write!(formatter, "cannot canonicalize identity: {message}")
            }
            Self::Io(error) => write!(formatter, "experiment provenance I/O error: {error}"),
            Self::Git(message) => write!(formatter, "cannot capture git provenance: {message}"),
            Self::Execution(message) => formatter.write_str(message),
        }
    }
}

impl Error for ExperimentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Manifest(error) => Some(error),
            Self::Universe(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::InvalidDefinition(_) | Self::Canonical(_) | Self::Git(_) | Self::Execution(_) => {
                None
            }
        }
    }
}

impl From<serde_json::Error> for ExperimentError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
impl From<ManifestError> for ExperimentError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}
impl From<prajna_research::UniverseError> for ExperimentError {
    fn from(error: prajna_research::UniverseError) -> Self {
        Self::Universe(error)
    }
}
impl From<std::io::Error> for ExperimentError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StrategyParameters {
    pub short: u64,
    pub long: u64,
    pub vol: u64,
    pub w_s: f64,
    pub w_l: f64,
    pub w_v: f64,
    pub trend: Option<u64>,
    pub top_k: u64,
    pub rebalance_every: u64,
}

impl StrategyParameters {
    fn parse(value: &Value) -> Result<Self, ExperimentError> {
        let object = value
            .as_object()
            .ok_or_else(|| invalid("each parameter combination must be an object"))?;
        exact_keys(object, &PARAMETERS, "strategy parameter combination")?;
        Ok(Self {
            short: positive_integer(&object["short"], "short")?,
            long: positive_integer(&object["long"], "long")?,
            vol: positive_integer(&object["vol"], "vol")?,
            w_s: finite_number(&object["w_s"], "w_s")?,
            w_l: finite_number(&object["w_l"], "w_l")?,
            w_v: finite_number(&object["w_v"], "w_v")?,
            trend: optional_positive_integer(&object["trend"], "trend")?,
            top_k: nonnegative_integer(&object["top_k"], "top_k")?,
            rebalance_every: nonnegative_integer(&object["rebalance_every"], "rebalance_every")?,
        })
    }

    fn normalized(&self) -> Value {
        serde_json::json!({
            "short": self.short,
            "long": self.long,
            "vol": self.vol,
            "w_s": canonical_float_bits(self.w_s),
            "w_l": canonical_float_bits(self.w_l),
            "w_v": canonical_float_bits(self.w_v),
            "trend": self.trend,
            "top_k": self.top_k,
            "rebalance_every": self.rebalance_every,
        })
    }

    fn factor(&self) -> Result<Factor, ExperimentError> {
        let short = u32_parameter(self.short, "short")?;
        let long = u32_parameter(self.long, "long")?;
        let vol = u32_parameter(self.vol, "vol")?;
        let trend = self
            .trend
            .map(|value| u32_parameter(value, "trend"))
            .transpose()?;
        Factor::new(
            FactorKind::RotationScore,
            RotationScoreParams {
                short,
                long,
                vol,
                w_s: self.w_s,
                w_l: self.w_l,
                w_v: self.w_v,
                trend,
            },
        )
        .map_err(|error| invalid(format!("invalid rotation score factor: {error}")))
    }

    fn strategy(&self, score: Factor) -> Result<VectorStrategy, ExperimentError> {
        let top_k = u32_parameter(self.top_k, "top_k")?;
        let rebalance_every = u32_parameter(self.rebalance_every, "rebalance_every")?;
        let strategy = VectorStrategy {
            score,
            direction: RankDirection::Descending,
            top_k,
            rebalance_every,
            weighting: Weighting::EqualWeight,
            capabilities: BTreeSet::from([StrategyCapability::Vectorizable]),
        };
        VectorEngine::validate(&strategy)
            .map_err(|error| invalid(format!("invalid vector strategy: {error}")))?;
        Ok(strategy)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParameterSpace {
    Grid(BTreeMap<String, Vec<Value>>),
    List(Vec<StrategyParameters>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Costs {
    pub commission_rate: f64,
    pub buy_slippage_bps: f64,
    pub sell_slippage_bps: f64,
    pub buy_tax_rate: f64,
    pub sell_tax_rate: f64,
}

impl Costs {
    fn parse(value: &Value) -> Result<Self, ExperimentError> {
        let object = value
            .as_object()
            .ok_or_else(|| invalid("costs must be an object"))?;
        const KEYS: [&str; 5] = [
            "commission_rate",
            "buy_slippage_bps",
            "sell_slippage_bps",
            "buy_tax_rate",
            "sell_tax_rate",
        ];
        exact_keys(object, &KEYS, "costs")?;
        let result = Self {
            commission_rate: finite_number(&object["commission_rate"], "costs.commission_rate")?,
            buy_slippage_bps: finite_number(&object["buy_slippage_bps"], "costs.buy_slippage_bps")?,
            sell_slippage_bps: finite_number(
                &object["sell_slippage_bps"],
                "costs.sell_slippage_bps",
            )?,
            buy_tax_rate: finite_number(&object["buy_tax_rate"], "costs.buy_tax_rate")?,
            sell_tax_rate: finite_number(&object["sell_tax_rate"], "costs.sell_tax_rate")?,
        };
        if [
            result.commission_rate,
            result.buy_slippage_bps,
            result.sell_slippage_bps,
            result.buy_tax_rate,
            result.sell_tax_rate,
        ]
        .iter()
        .any(|value| *value < 0.0)
        {
            return Err(invalid("cost rates and slippage must be non-negative"));
        }
        Ok(result)
    }

    fn normalized(self) -> Value {
        serde_json::json!({
            "commission_rate": canonical_float_bits(self.commission_rate),
            "buy_slippage_bps": canonical_float_bits(self.buy_slippage_bps),
            "sell_slippage_bps": canonical_float_bits(self.sell_slippage_bps),
            "buy_tax_rate": canonical_float_bits(self.buy_tax_rate),
            "sell_tax_rate": canonical_float_bits(self.sell_tax_rate),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Experiment {
    dsv: String,
    universe: String,
    venue: String,
    availability_assumption: String,
    costs: Costs,
    sessions_per_year: u64,
    parameter_space: ParameterSpace,
    normalized: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DefinitionInput {
    experiment_version: u32,
    dsv: String,
    universe: String,
    venue: String,
    strategy: String,
    engine: String,
    availability_assumption: String,
    costs: Value,
    sessions_per_year: u64,
    #[serde(default)]
    seed: Option<Value>,
    parameter_space: Value,
}

impl Experiment {
    /// Parses and validates a definition against its local DSV and Static Universe.
    pub fn parse(json: &str, lake_root: impl AsRef<Path>) -> Result<Self, ExperimentError> {
        let input: DefinitionInput = serde_json::from_str(json)?;
        if input.experiment_version != 1 {
            return Err(invalid("experiment_version must be 1"));
        }
        if input.sessions_per_year == 0 || input.sessions_per_year > MAX_SAFE_INTEGER {
            return Err(invalid(
                "sessions_per_year must be a positive integer no greater than 2^53 - 1",
            ));
        }
        if input.strategy != "s2_rotation" {
            return Err(invalid("strategy must be s2_rotation"));
        }
        if input.engine != "vector" {
            return Err(invalid("engine must be vector"));
        }
        if input.seed.as_ref().is_some_and(|seed| !seed.is_null()) {
            return Err(invalid("Vector engine seed must be omitted or null"));
        }
        input
            .venue
            .parse::<VenueId>()
            .map_err(|_| invalid("venue is not a valid VenueId"))?;
        match input.availability_assumption.as_str() {
            "none" | "treat_unknown_as_ts_close" => {}
            _ => {
                return Err(invalid(
                    "availability_assumption must be none or treat_unknown_as_ts_close",
                ));
            }
        }
        read_manifest(lake_root.as_ref(), &input.dsv)?;
        let universe = StaticUniverse::load(lake_root.as_ref(), &input.universe)?;
        universe.validate_against(lake_root.as_ref(), &input.dsv)?;
        validate_universe_venue(&universe, &input.venue)?;
        let costs = Costs::parse(&input.costs)?;
        let parameter_space = parse_parameter_space(&input.parameter_space)?;
        let normalized = normalize_definition(&input, costs, &parameter_space);
        Ok(Self {
            dsv: input.dsv,
            universe: input.universe,
            venue: input.venue,
            availability_assumption: input.availability_assumption,
            costs,
            sessions_per_year: input.sessions_per_year,
            parameter_space,
            normalized,
        })
    }

    pub fn dsv(&self) -> &str {
        &self.dsv
    }
    pub fn universe(&self) -> &str {
        &self.universe
    }
    pub fn venue(&self) -> &str {
        &self.venue
    }
    pub fn costs(&self) -> Costs {
        self.costs
    }
    pub fn sessions_per_year(&self) -> u64 {
        self.sessions_per_year
    }
    pub fn parameter_space(&self) -> &ParameterSpace {
        &self.parameter_space
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, ExperimentError> {
        restricted_jcs(&self.normalized)
            .map_err(|error| ExperimentError::Canonical(error.to_string()))
    }

    pub fn id(&self) -> Result<String, ExperimentError> {
        Ok(identity("exp", &self.canonical_json()?))
    }

    /// Returns the content address of one parameterized Run Spec.
    pub fn run_id(&self, parameters: &StrategyParameters) -> Result<String, ExperimentError> {
        let bytes = restricted_jcs(&self.run_spec(parameters))
            .map_err(|error| ExperimentError::Canonical(error.to_string()))?;
        Ok(identity("run", &bytes))
    }

    /// Canonical Run Spec JSON, including its identity-bearing semantics.
    pub fn run_spec(&self, parameters: &StrategyParameters) -> Value {
        serde_json::json!({
            "dsv": self.dsv,
            "universe": self.universe,
            "venue": self.venue,
            "strategy": { "kind": "s2_rotation", "parameters": parameters.normalized() },
            "costs": self.costs.normalized(),
            "availability_assumption": self.availability_assumption,
            "engine": { "kind": "vector", "semantic_version": "vector@1" },
            "seed": Value::Null,
        })
    }

    /// Expands and validates every parameter combination before returning any Runs.
    pub fn expand(&self, lake_root: impl AsRef<Path>) -> Result<Vec<ExpandedRun>, ExperimentError> {
        let universe = StaticUniverse::load(lake_root, &self.universe)?;
        self.expand_for_universe(&universe)
    }

    fn expand_for_universe(
        &self,
        universe: &StaticUniverse,
    ) -> Result<Vec<ExpandedRun>, ExperimentError> {
        let candidates = match &self.parameter_space {
            ParameterSpace::Grid(axes) => expand_grid(axes)?,
            ParameterSpace::List(values) => values.clone(),
        };
        let mut runs = BTreeMap::new();
        for (index, parameters) in candidates.into_iter().enumerate() {
            let fail = |message: String| {
                invalid(format!(
                    "parameter combination {} is invalid: {message}",
                    index + 1
                ))
            };
            if parameters.short >= parameters.long {
                return Err(fail("short must be less than long".into()));
            }
            if parameters.top_k == 0 {
                return Err(fail("top_k must be greater than zero".into()));
            }
            if parameters.rebalance_every == 0 {
                return Err(fail("rebalance_every must be greater than zero".into()));
            }
            if parameters.top_k as u128 > universe.members().len() as u128 {
                return Err(fail(format!(
                    "top_k {} exceeds Universe member count {}",
                    parameters.top_k,
                    universe.members().len()
                )));
            }
            let factor = parameters
                .factor()
                .map_err(|error| fail(error.to_string()))?;
            parameters
                .strategy(factor.clone())
                .map_err(|error| fail(error.to_string()))?;
            let run_id = self.run_id(&parameters)?;
            runs.entry(run_id.clone()).or_insert_with(|| ExpandedRun {
                run_spec: self.run_spec(&parameters),
                run_id,
                parameters,
                factor,
            });
        }
        Ok(runs.into_values().collect())
    }

    /// Executes a grid using one local Rayon pool and a two-stage factor/run schedule.
    pub fn execute(
        &self,
        lake_root: impl AsRef<Path>,
        threads: usize,
    ) -> Result<ExecutionReport, ExperimentError> {
        if threads == 0 {
            return Err(invalid("threads must be greater than zero"));
        }
        let lake_root = lake_root.as_ref();
        let universe = StaticUniverse::load(lake_root, &self.universe)?;
        let runs = self.expand_for_universe(&universe)?;
        validate_universe_venue(&universe, &self.venue)
            .map_err(|error| execution_error("validate Universe venue", error))?;
        let venue = self
            .venue
            .parse::<VenueId>()
            .map_err(|_| invalid("venue is not a valid VenueId"))?;
        let panel = load_panel(lake_root, &self.dsv, &venue)
            .map_err(|error| execution_error("load panel", error))?;
        let statuses = load_execution_status(lake_root, &self.dsv)
            .map_err(|error| execution_error("load execution status", error))?;
        universe
            .validate_against(lake_root, &self.dsv)
            .map_err(|error| execution_error("validate Universe", error))?;

        // Resolve execution availability once; the callback in each Run then reads this shared map.
        let mut executable_by_key = HashMap::new();
        for instrument in universe.members() {
            for session in &panel.sessions {
                let allowed = executable(&panel, &statuses, instrument, session)
                    .map_err(|error| execution_error("resolve execution status", error))?;
                executable_by_key.insert((instrument.clone(), session.session_date), allowed);
            }
        }

        let mut factor_nodes = BTreeMap::new();
        for run in &runs {
            for factor in FactorGraph::resolve(run.factor.clone())
                .map_err(|error| execution_error("resolve factor graph", error))?
            {
                factor_nodes
                    .entry(factor.canonical_json().to_string())
                    .or_insert(factor);
            }
        }
        let mut base_nodes = Vec::new();
        let mut score_nodes = Vec::new();
        for factor in factor_nodes.into_values() {
            let key = FactorKey::for_tree(
                &factor,
                &self.dsv,
                &self.universe,
                self.availability_assumption()?,
            );
            if factor.kind() == FactorKind::RotationScore {
                score_nodes.push((factor, key));
            } else {
                base_nodes.push((factor, key));
            }
        }
        base_nodes.sort_by(|left, right| left.1.as_str().cmp(right.1.as_str()));
        score_nodes.sort_by(|left, right| left.1.as_str().cmp(right.1.as_str()));
        let cache = FactorCache::open(lake_root)
            .map_err(|error| execution_error("open Factor Cache", error))?;
        let pool = ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .map_err(|error| invalid(format!("cannot create Rayon pool: {error}")))?;

        let assumption = self.availability_assumption()?;
        let factor_started = Instant::now();
        let base_values = pool
            .install(|| {
                base_nodes
                    .par_iter()
                    .map(|(factor, _key)| {
                        cache
                            .get_or_compute(
                                &panel,
                                universe.members(),
                                &self.universe,
                                &self.dsv,
                                factor,
                                assumption,
                            )
                            .map(|values| (factor.canonical_json().to_string(), values))
                            .map_err(|error| error.to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|error| {
                ExperimentError::Execution(format!("base factor stage failed: {error}"))
            })?;
        let mut factor_values = base_values.into_iter().collect::<HashMap<_, _>>();

        // All dependency keys have been published by the completed base layer before scores start.
        let score_values = pool
            .install(|| {
                score_nodes
                    .par_iter()
                    .map(|(factor, _key)| {
                        cache
                            .get_or_compute(
                                &panel,
                                universe.members(),
                                &self.universe,
                                &self.dsv,
                                factor,
                                assumption,
                            )
                            .map(|values| (factor.canonical_json().to_string(), values))
                            .map_err(|error| error.to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|error| {
                ExperimentError::Execution(format!("score factor stage failed: {error}"))
            })?;
        factor_values.extend(score_values);
        let factor_time_ms = factor_started.elapsed().as_millis();

        let costs = self.vector_costs();
        let run_started = Instant::now();
        let results = pool.install(|| {
            execute_runs(&runs, |run| {
                execute_run(
                    run,
                    &factor_values,
                    &panel,
                    &executable_by_key,
                    &costs,
                    assumption,
                )
            })
        });
        let run_time_ms = run_started.elapsed().as_millis();
        let compute_count = cache.compute_count();
        let cache_hit_count = cache.hit_count();
        Ok(ExecutionReport {
            runs: results,
            factor_time_ms,
            run_time_ms,
            compute_count,
            cache_hit_count,
        })
    }

    /// Executes and stores a Vector experiment at the requested result level.
    /// If this Execution identity already exists, all saved logical hashes are verified.
    pub fn execute_and_store(
        &self,
        lake_root: impl AsRef<Path>,
        repository: impl AsRef<Path>,
        result_level: ResultLevel,
        threads: usize,
    ) -> Result<StoredExecution, ExperimentError> {
        let lake_root = lake_root.as_ref();
        let started_at = storage::now_rfc3339();
        let identity = ExecutionIdentity::capture(&self.id()?, repository)?;
        let report = self.execute(lake_root, threads)?;
        storage::store_execution(
            self,
            &identity,
            &report,
            lake_root,
            result_level,
            threads,
            started_at,
        )
    }

    /// Adds result tables for selected Runs without changing their Run identities.
    pub fn promote(
        &self,
        lake_root: impl AsRef<Path>,
        repository: impl AsRef<Path>,
        execution_id: &str,
        run_ids: &[String],
        result_level: ResultLevel,
        threads: usize,
    ) -> Result<StoredExecution, ExperimentError> {
        let lake_root = lake_root.as_ref();
        let identity = ExecutionIdentity::capture(&self.id()?, repository)?;
        storage::promote(
            self,
            &identity,
            lake_root,
            execution_id,
            run_ids,
            result_level,
            threads,
        )
    }

    fn availability_assumption(&self) -> Result<AvailabilityAssumption, ExperimentError> {
        self.availability_assumption
            .parse()
            .map_err(|error| invalid(format!("invalid availability assumption: {error}")))
    }

    fn vector_costs(&self) -> VectorCosts {
        VectorCosts {
            commission_rate: self.costs.commission_rate,
            buy_slippage_bps: self.costs.buy_slippage_bps,
            sell_slippage_bps: self.costs.sell_slippage_bps,
            buy_tax_rate: self.costs.buy_tax_rate,
            sell_tax_rate: self.costs.sell_tax_rate,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExpandedRun {
    pub run_spec: Value,
    pub run_id: String,
    pub parameters: StrategyParameters,
    factor: Factor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunExecution {
    pub run_spec: Value,
    pub run_id: String,
    pub status: RunStatus,
    pub result: Option<VectorResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionReport {
    pub runs: Vec<RunExecution>,
    pub factor_time_ms: u128,
    pub run_time_ms: u128,
    pub compute_count: u64,
    pub cache_hit_count: u64,
}

fn execute_run(
    run: &ExpandedRun,
    factor_values: &HashMap<String, DataFrame>,
    panel: &Panel,
    executable_by_key: &HashMap<(InstrumentId, chrono::NaiveDate), bool>,
    costs: &VectorCosts,
    assumption: AvailabilityAssumption,
) -> Result<VectorResult, String> {
    (|| {
        let score = factor_values
            .get(&run.factor.canonical_json().to_string())
            .ok_or_else(|| "stage 1 did not produce this Run's score Factor Values".to_owned())?;
        let strategy = run
            .parameters
            .strategy(run.factor.clone())
            .map_err(|error| error.to_string())?;
        let decisions = strategy
            .decide(score, &panel.sessions)
            .map_err(|error| error.to_string())?;
        let events = execute(&decisions, &panel.sessions, |instrument, session| {
            executable_by_key
                .get(&(instrument.clone(), session.session_date))
                .copied()
                .unwrap_or(false)
        });
        run_vector(panel, &decisions, &events, costs, assumption).map_err(|error| error.to_string())
    })()
}

fn execute_runs(
    runs: &[ExpandedRun],
    execute_one: impl Fn(&ExpandedRun) -> Result<VectorResult, String> + Sync,
) -> Vec<RunExecution> {
    runs.par_iter()
        .map(|run| match execute_one(run) {
            Ok(result) => RunExecution {
                run_spec: run.run_spec.clone(),
                run_id: run.run_id.clone(),
                status: RunStatus::Completed,
                result: Some(result),
                error: None,
            },
            Err(error) => RunExecution {
                run_spec: run.run_spec.clone(),
                run_id: run.run_id.clone(),
                status: RunStatus::Failed,
                result: None,
                error: Some(error),
            },
        })
        .collect()
}

fn expand_grid(
    axes: &BTreeMap<String, Vec<Value>>,
) -> Result<Vec<StrategyParameters>, ExperimentError> {
    let mut count = 1usize;
    for name in PARAMETERS {
        count = count
            .checked_mul(axes[name].len())
            .ok_or_else(|| invalid("parameter grid size exceeds addressable memory"))?;
    }
    let mut combinations = Vec::with_capacity(count);
    let mut current = Map::new();
    fn append(
        axis_index: usize,
        axes: &BTreeMap<String, Vec<Value>>,
        current: &mut Map<String, Value>,
        combinations: &mut Vec<StrategyParameters>,
    ) -> Result<(), ExperimentError> {
        if axis_index == PARAMETERS.len() {
            combinations.push(StrategyParameters::parse(&Value::Object(current.clone()))?);
            return Ok(());
        }
        let name = PARAMETERS[axis_index];
        for value in &axes[name] {
            current.insert(name.to_owned(), value.clone());
            append(axis_index + 1, axes, current, combinations)?;
        }
        current.remove(name);
        Ok(())
    }
    append(0, axes, &mut current, &mut combinations)?;
    Ok(combinations)
}

fn u32_parameter(value: u64, name: &str) -> Result<u32, ExperimentError> {
    u32::try_from(value)
        .map_err(|_| invalid(format!("{name} must be no greater than {}", u32::MAX)))
}

fn execution_error(context: &str, error: impl fmt::Display) -> ExperimentError {
    ExperimentError::Execution(format!("{context} failed: {error}"))
}

fn parse_parameter_space(value: &Value) -> Result<ParameterSpace, ExperimentError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("parameter_space must be an object"))?;
    if object.len() != 1 {
        return Err(invalid(
            "parameter_space must contain exactly one of grid or list",
        ));
    }
    if let Some(grid_value) = object.get("grid") {
        let grid = grid_value
            .as_object()
            .ok_or_else(|| invalid("parameter_space.grid must be an object"))?;
        exact_keys(grid, &PARAMETERS, "parameter_space.grid")?;
        let mut axes = BTreeMap::new();
        for name in PARAMETERS {
            let values = grid[name]
                .as_array()
                .ok_or_else(|| invalid(format!("grid axis {name} must be an array")))?;
            if values.is_empty() {
                return Err(invalid(format!("grid axis {name} must not be empty")));
            }
            for value in values {
                validate_parameter_value(name, value)?;
            }
            axes.insert(name.to_owned(), values.clone());
        }
        Ok(ParameterSpace::Grid(axes))
    } else if let Some(list_value) = object.get("list") {
        let list = list_value
            .as_array()
            .ok_or_else(|| invalid("parameter_space.list must be an array"))?;
        if list.is_empty() {
            return Err(invalid("parameter_space.list must not be empty"));
        }
        Ok(ParameterSpace::List(
            list.iter()
                .map(StrategyParameters::parse)
                .collect::<Result<_, _>>()?,
        ))
    } else {
        Err(invalid("parameter_space must contain grid or list"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParameterValueKind {
    PositiveInteger,
    NonnegativeInteger,
    FloatBits,
    OptionalPositiveInteger,
}

fn parameter_value_kind(name: &str) -> Option<ParameterValueKind> {
    match name {
        "short" | "long" | "vol" => Some(ParameterValueKind::PositiveInteger),
        "top_k" | "rebalance_every" => Some(ParameterValueKind::NonnegativeInteger),
        "w_s" | "w_l" | "w_v" => Some(ParameterValueKind::FloatBits),
        "trend" => Some(ParameterValueKind::OptionalPositiveInteger),
        _ => None,
    }
}

fn validate_parameter_value(name: &str, value: &Value) -> Result<(), ExperimentError> {
    match parameter_value_kind(name) {
        Some(ParameterValueKind::PositiveInteger) => {
            positive_integer(value, name)?;
        }
        Some(ParameterValueKind::NonnegativeInteger) => {
            nonnegative_integer(value, name)?;
        }
        Some(ParameterValueKind::FloatBits) => {
            finite_number(value, name)?;
        }
        Some(ParameterValueKind::OptionalPositiveInteger) => {
            optional_positive_integer(value, name)?;
        }
        None => return Err(invalid(format!("unknown strategy parameter {name}"))),
    }
    Ok(())
}

fn normalize_definition(input: &DefinitionInput, costs: Costs, space: &ParameterSpace) -> Value {
    let parameter_space = match space {
        ParameterSpace::Grid(axes) => {
            let normalized = axes
                .iter()
                .map(|(name, values)| {
                    let values = values
                        .iter()
                        .map(|value| normalize_parameter_axis(name, value))
                        .collect::<Vec<_>>();
                    (name.clone(), Value::Array(values))
                })
                .collect::<Map<_, _>>();
            serde_json::json!({ "grid": normalized })
        }
        ParameterSpace::List(values) => {
            serde_json::json!({ "list": values.iter().map(StrategyParameters::normalized).collect::<Vec<_>>() })
        }
    };
    serde_json::json!({
        "experiment_version": input.experiment_version,
        "dsv": input.dsv,
        "universe": input.universe,
        "venue": input.venue,
        "strategy": "s2_rotation",
        "engine": "vector",
        "availability_assumption": input.availability_assumption,
        "costs": costs.normalized(),
        "sessions_per_year": input.sessions_per_year,
        "seed": Value::Null,
        "parameter_space": parameter_space,
    })
}

fn normalize_parameter_axis(name: &str, value: &Value) -> Value {
    match parameter_value_kind(name) {
        Some(ParameterValueKind::FloatBits) => Value::String(canonical_float_bits(
            value.as_f64().expect("validated finite weight"),
        )),
        _ => value.clone(),
    }
}

/// Target triple this crate was compiled for, captured by `build.rs`.
const BUILD_TARGET: &str = env!("PRAJNA_BUILD_TARGET");

#[cfg(unix)]
fn path_from_git_bytes(bytes: &[u8]) -> Result<std::path::PathBuf, ExperimentError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

#[cfg(not(unix))]
fn path_from_git_bytes(bytes: &[u8]) -> Result<std::path::PathBuf, ExperimentError> {
    String::from_utf8(bytes.to_vec())
        .map(std::path::PathBuf::from)
        .map_err(|error| ExperimentError::Git(format!("git path is not UTF-8: {error}")))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentity {
    pub id: String,
    pub reproducible: bool,
    pub git_revision: String,
    pub working_tree_diff_sha256: String,
    pub rustc_version: String,
    pub target_triple: String,
}

impl ExecutionIdentity {
    /// Captures git/compiler provenance. Git or rustc failures are fatal.
    pub fn capture(
        experiment_id: &str,
        repository: impl AsRef<Path>,
    ) -> Result<Self, ExperimentError> {
        let repository = repository.as_ref();
        let git_revision = git_text(repository, &["rev-parse", "HEAD"])?;
        let status = git_bytes(
            repository,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?;
        let reproducible = status.is_empty();
        let mut diff = git_bytes(repository, &["diff", "--binary", "HEAD"])?;
        diff.extend_from_slice(b"\0untracked\0");
        let untracked = git_bytes(
            repository,
            &["ls-files", "-z", "--others", "--exclude-standard"],
        )?;
        let mut paths = untracked
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let file = repository.join(path_from_git_bytes(path)?);
            let content_hash = digest(&fs::read(file)?);
            diff.extend_from_slice(path);
            diff.push(0);
            diff.extend_from_slice(content_hash.as_bytes());
            diff.push(b'\n');
        }
        let working_tree_diff_sha256 = digest(&diff);
        let rustc_version = String::from_utf8(command_output(repository, "rustc", &["--version"])?)
            .map_err(|error| ExperimentError::Git(format!("rustc output is not UTF-8: {error}")))?
            .trim()
            .to_owned();
        if rustc_version.is_empty() {
            return Err(ExperimentError::Git(
                "rustc --version returned an empty version".into(),
            ));
        }
        let target_triple = BUILD_TARGET.to_owned();
        let value = serde_json::json!({
            "experiment": experiment_id,
            "git_revision": git_revision,
            "working_tree_diff_sha256": working_tree_diff_sha256,
            "rustc_version": rustc_version,
            "target_triple": target_triple,
        });
        let canonical = restricted_jcs(&value)
            .map_err(|error| ExperimentError::Canonical(error.to_string()))?;
        Ok(Self {
            id: identity("exe", &canonical),
            reproducible,
            git_revision,
            working_tree_diff_sha256,
            rustc_version,
            target_triple,
        })
    }
}

fn command_output(cwd: &Path, command: &str, args: &[&str]) -> Result<Vec<u8>, ExperimentError> {
    let output = Command::new(command)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| ExperimentError::Git(format!("failed to run {command}: {error}")))?;
    if !output.status.success() {
        return Err(ExperimentError::Git(format!(
            "{command} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}
fn git_bytes(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, ExperimentError> {
    command_output(cwd, "git", args)
}
fn git_text(cwd: &Path, args: &[&str]) -> Result<String, ExperimentError> {
    String::from_utf8(git_bytes(cwd, args)?)
        .map_err(|error| ExperimentError::Git(format!("git output is not UTF-8: {error}")))
}

fn positive_integer(value: &Value, name: &str) -> Result<u64, ExperimentError> {
    value
        .as_u64()
        .filter(|value| *value > 0 && *value <= MAX_SAFE_INTEGER)
        .ok_or_else(|| {
            invalid(format!(
                "{name} must be a positive integer no greater than 2^53 - 1"
            ))
        })
}
fn nonnegative_integer(value: &Value, name: &str) -> Result<u64, ExperimentError> {
    value
        .as_u64()
        .filter(|value| *value <= MAX_SAFE_INTEGER)
        .ok_or_else(|| {
            invalid(format!(
                "{name} must be a non-negative integer no greater than 2^53 - 1"
            ))
        })
}
fn optional_positive_integer(value: &Value, name: &str) -> Result<Option<u64>, ExperimentError> {
    if value.is_null() {
        Ok(None)
    } else {
        positive_integer(value, name).map(Some)
    }
}
fn finite_number(value: &Value, name: &str) -> Result<f64, ExperimentError> {
    let number = value
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| invalid(format!("{name} must be a finite number")))?;
    Ok(number)
}
fn exact_keys(
    object: &Map<String, Value>,
    keys: &[&str],
    context: &str,
) -> Result<(), ExperimentError> {
    let expected = keys.iter().copied().collect::<BTreeSet<_>>();
    if let Some(key) = object.keys().find(|key| !expected.contains(key.as_str())) {
        return Err(invalid(format!("unknown field {context}.{key}")));
    }
    if let Some(key) = expected.iter().find(|key| !object.contains_key(**key)) {
        return Err(invalid(format!("missing field {context}.{key}")));
    }
    Ok(())
}
const MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;

fn invalid(message: impl Into<String>) -> ExperimentError {
    ExperimentError::InvalidDefinition(message.into())
}

fn validate_universe_venue(universe: &StaticUniverse, venue: &str) -> Result<(), ExperimentError> {
    let venue = venue
        .parse::<VenueId>()
        .map_err(|_| invalid("venue is not a valid VenueId"))?;
    if let Some(instrument) = universe
        .members()
        .iter()
        .find(|instrument| instrument.venue() != &venue)
    {
        return Err(invalid(format!(
            "Universe member {instrument} does not belong to experiment venue {venue}"
        )));
    }
    Ok(())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn identity(prefix: &str, canonical: &[u8]) -> String {
    format!("{prefix}:sha256:{}", digest(canonical))
}

#[cfg(test)]
mod tests {
    use super::*;
    use prajna_data::{
        NormalizerRegistry, RawStore, SourceKind, SourceRecordInput, publish_dataset,
    };
    use prajna_domain::InstrumentId;
    use prajna_research::StaticUniverse;
    use serde_json::json;
    use std::{fs, path::Path, process::Command};
    use tempfile::TempDir;

    fn fixture(root: &Path) -> (String, String, String) {
        const FIXTURE: &[u8] =
            include_bytes!("../../../poc/poc0-benchmark/fixtures/dataset-v1.json");
        let lake = root.join("lake");
        let store = RawStore::open(&lake).unwrap();
        let raw_hash = store
            .put(
                FIXTURE,
                SourceRecordInput {
                    content_type: "application/json".into(),
                    source_kind: SourceKind::Fixture,
                    source_id: "poc0-dataset-v1".into(),
                    request: json!({}),
                    observed_at: "synthetic".into(),
                    ingested_by: "prajna-experiment-tests".into(),
                },
            )
            .unwrap();
        let manifest = publish_dataset(
            &lake,
            &store,
            &NormalizerRegistry::with_builtins(),
            "synthetic-etf-daily",
            "1",
            &[raw_hash],
            "2026-10-03T00:00:00Z",
        )
        .unwrap();
        let universe = StaticUniverse::new(
            "fixture",
            ["A.SYNTH", "B.SYNTH"]
                .map(|id| id.parse::<InstrumentId>().unwrap())
                .to_vec(),
        )
        .unwrap();
        universe.store(&lake).unwrap();
        (
            lake.to_string_lossy().into_owned(),
            manifest.dsv,
            universe.id().unwrap(),
        )
    }

    fn scale_fixture(root: &Path) -> (String, String, String) {
        const FIXTURE: &[u8] =
            include_bytes!("../../../poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-v2.json");
        let lake = root.join("lake");
        let store = RawStore::open(&lake).unwrap();
        let raw_hash = store
            .put(
                FIXTURE,
                SourceRecordInput {
                    content_type: "application/json".into(),
                    source_kind: SourceKind::Fixture,
                    source_id: "b2-s2-scale-64x252-v2".into(),
                    request: json!({}),
                    observed_at: "synthetic".into(),
                    ingested_by: "prajna-experiment-tests".into(),
                },
            )
            .unwrap();
        let manifest = publish_dataset(
            &lake,
            &store,
            &NormalizerRegistry::with_builtins(),
            "synthetic-etf-daily",
            "3",
            &[raw_hash],
            "2026-10-03T00:00:00Z",
        )
        .unwrap();
        let venue = VenueId::new("SYNTH").unwrap();
        let panel = load_panel(&lake, &manifest.dsv, &venue).unwrap();
        let universe = StaticUniverse::new("all", panel.instruments).unwrap();
        universe.store(&lake).unwrap();
        (
            lake.to_string_lossy().into_owned(),
            manifest.dsv,
            universe.id().unwrap(),
        )
    }

    #[test]
    fn universe_members_must_match_experiment_venue() {
        let universe = StaticUniverse::new(
            "mixed",
            vec![
                InstrumentId::new("AAA", VenueId::new("SYNTH").unwrap()).unwrap(),
                InstrumentId::new("BBB", VenueId::new("OTHER").unwrap()).unwrap(),
            ],
        )
        .unwrap();

        assert!(
            validate_universe_venue(&universe, "SYNTH")
                .unwrap_err()
                .to_string()
                .contains("does not belong to experiment venue")
        );
    }

    fn scale_definition(dsv: &str, universe: &str, space: Value) -> Value {
        json!({
            "experiment_version": 1,
            "dsv": dsv,
            "universe": universe,
            "venue": "SYNTH",
            "strategy": "s2_rotation",
            "engine": "vector",
            "availability_assumption": "none",
            "costs": {
                "commission_rate": 0.001,
                "buy_slippage_bps": 10.0,
                "sell_slippage_bps": 10.0,
                "buy_tax_rate": 0.0,
                "sell_tax_rate": 0.0
            },
            "sessions_per_year": 252,
            "parameter_space": space
        })
    }

    fn definition(dsv: &str, universe: &str) -> Value {
        json!({
            "experiment_version": 1,
            "dsv": dsv,
            "universe": universe,
            "venue": "SYNTH",
            "strategy": "s2_rotation",
            "engine": "vector",
            "availability_assumption": "none",
            "costs": {
                "commission_rate": 0.001,
                "buy_slippage_bps": 10.0,
                "sell_slippage_bps": 10.0,
                "buy_tax_rate": 0.0,
                "sell_tax_rate": 0.0
            },
            "sessions_per_year": 252,
            "parameter_space": {"list": [{
                "short": 2, "long": 5, "vol": 3,
                "w_s": 0.1, "w_l": 1.0, "w_v": 0.2,
                "trend": 20, "top_k": 2, "rebalance_every": 1
            }]}
        })
    }

    fn run_parameters(experiment: &Experiment) -> &StrategyParameters {
        match experiment.parameter_space() {
            ParameterSpace::List(parameters) => &parameters[0],
            ParameterSpace::Grid(_) => panic!("expected list parameter space"),
        }
    }

    #[test]
    fn experiment_ids_normalize_key_order_and_equivalent_float_spellings() {
        let temp = TempDir::new().unwrap();
        let (lake, dsv, universe) = fixture(temp.path());
        let mut first = definition(&dsv, &universe);
        first["costs"]["commission_rate"] = json!(0.1);
        first["parameter_space"]["list"][0]["w_s"] = json!(0.1);
        let second_json = first
            .to_string()
            .replacen('{', "{\"seed\":null,", 1)
            .replace("\"commission_rate\":0.1", "\"commission_rate\":1e-1")
            .replace("\"w_s\":0.1", "\"w_s\":1e-1");
        let second = Experiment::parse(&second_json, &lake).unwrap();
        assert_eq!(
            Experiment::parse(&first.to_string(), &lake)
                .unwrap()
                .id()
                .unwrap(),
            second.id().unwrap()
        );
        let experiment = Experiment::parse(&first.to_string(), &lake).unwrap();
        assert_eq!(canonical_float_bits(0.1), "3fb999999999999a");
        assert_eq!(
            experiment.run_id(run_parameters(&experiment)).unwrap(),
            second.run_id(run_parameters(&second)).unwrap()
        );
    }

    #[test]
    fn sessions_per_year_changes_experiment_but_not_run_identity() {
        let temp = TempDir::new().unwrap();
        let (lake, dsv, universe) = fixture(temp.path());
        let first_value = definition(&dsv, &universe);
        let mut second_value = first_value.clone();
        second_value["sessions_per_year"] = json!(365);
        let first = Experiment::parse(&first_value.to_string(), &lake).unwrap();
        let second = Experiment::parse(&second_value.to_string(), &lake).unwrap();
        assert_ne!(first.id().unwrap(), second.id().unwrap());
        assert_eq!(
            first.run_id(run_parameters(&first)).unwrap(),
            second.run_id(run_parameters(&second)).unwrap()
        );
    }

    #[test]
    fn validates_grid_axes_and_accepts_null_trend_and_seed() {
        let temp = TempDir::new().unwrap();
        let (lake, dsv, universe) = fixture(temp.path());
        let mut value = definition(&dsv, &universe);
        value["seed"] = Value::Null;
        value["parameter_space"] = json!({"grid": {
            "short": [2], "long": [5], "vol": [3],
            "w_s": [0.1], "w_l": [1.0], "w_v": [0.2],
            "trend": [null, 20], "top_k": [2], "rebalance_every": [1]
        }});
        let experiment = Experiment::parse(&value.to_string(), &lake).unwrap();
        assert!(matches!(
            experiment.parameter_space(),
            ParameterSpace::Grid(_)
        ));
        assert_eq!(
            Experiment::parse(&value.to_string(), &lake)
                .unwrap()
                .id()
                .unwrap(),
            experiment.id().unwrap()
        );
        assert_eq!(experiment.expand(&lake).unwrap().len(), 2);
    }

    #[test]
    fn expands_fixed_order_grid_and_deduplicates_runs_by_identity() {
        let temp = TempDir::new().unwrap();
        let (lake, dsv, universe) = fixture(temp.path());
        let mut value = definition(&dsv, &universe);
        value["parameter_space"] = json!({"grid": {
            "short": [1, 2], "long": [3, 5], "vol": [2],
            "w_s": [0.1], "w_l": [1.0], "w_v": [0.2],
            "trend": [null, 20], "top_k": [1], "rebalance_every": [1, 2]
        }});
        let experiment = Experiment::parse(&value.to_string(), &lake).unwrap();
        let expanded = experiment.expand(&lake).unwrap();
        assert_eq!(expanded.len(), 16);
        assert!(
            expanded
                .windows(2)
                .all(|pair| pair[0].run_id < pair[1].run_id)
        );

        value["parameter_space"] = json!({"list": [
            value["parameter_space"]["grid"]
                .as_object().unwrap().iter().map(|(key, values)| (key.clone(), values[0].clone())).collect::<Map<_, _>>(),
            value["parameter_space"]["grid"]
                .as_object().unwrap().iter().map(|(key, values)| (key.clone(), values[0].clone())).collect::<Map<_, _>>()
        ]});
        let duplicates = Experiment::parse(&value.to_string(), &lake)
            .unwrap()
            .expand(&lake)
            .unwrap();
        assert_eq!(duplicates.len(), 1);
    }

    #[test]
    fn expansion_rejects_the_first_invalid_combination() {
        let temp = TempDir::new().unwrap();
        let (lake, dsv, universe) = fixture(temp.path());
        let valid = definition(&dsv, &universe)["parameter_space"]["list"][0].clone();
        for (field, invalid_value, expected) in [
            ("long", json!(2), "short must be less than long"),
            ("vol", json!(1), "invalid rotation score factor"),
            ("top_k", json!(0), "top_k must be greater than zero"),
            ("top_k", json!(3), "exceeds Universe member count"),
            (
                "rebalance_every",
                json!(0),
                "rebalance_every must be greater than zero",
            ),
        ] {
            let mut combination = valid.clone();
            combination[field] = invalid_value;
            let experiment = Experiment::parse(
                &{
                    let mut input = definition(&dsv, &universe);
                    input["parameter_space"] = json!({"list": [combination]});
                    input.to_string()
                },
                &lake,
            )
            .unwrap();
            let error = experiment.expand(&lake).unwrap_err().to_string();
            assert!(error.contains("combination 1"), "{error}");
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn rayon_grid_is_deterministic_reuses_factor_cache_and_matches_s2_golden() {
        let temp = TempDir::new().unwrap();
        let (lake, dsv, universe) = scale_fixture(temp.path());
        let space = json!({"grid": {
            "short": [5, 10, 20], "long": [40], "vol": [10],
            "w_s": [1.0], "w_l": [1.0], "w_v": [1.0],
            "trend": [null], "top_k": [1, 3], "rebalance_every": [1, 5, 10, 20]
        }});
        let experiment =
            Experiment::parse(&scale_definition(&dsv, &universe, space).to_string(), &lake)
                .unwrap();
        let expanded = experiment.expand(&lake).unwrap();
        let expected_keys = expanded
            .iter()
            .flat_map(|run| FactorGraph::resolve(run.factor.clone()).unwrap())
            .map(|factor| {
                FactorKey::for_tree(&factor, &dsv, &universe, AvailabilityAssumption::None)
                    .as_str()
                    .to_owned()
            })
            .collect::<BTreeSet<_>>();
        let one_thread = experiment.execute(&lake, 1).unwrap();
        let four_threads = experiment.execute(&lake, 4).unwrap();
        assert_eq!(one_thread.runs.len(), 24);
        assert_eq!(one_thread.compute_count, expected_keys.len() as u64);
        assert!(one_thread.cache_hit_count > 0);
        assert_eq!(four_threads.compute_count, 0);
        assert!(four_threads.cache_hit_count > 0);
        assert_eq!(
            serde_json::to_vec(&one_thread.runs).unwrap(),
            serde_json::to_vec(&four_threads.runs).unwrap()
        );
        let injected = execute_runs(&expanded[..2], |run| {
            if run.run_id == expanded[0].run_id {
                Err("injected stage 2 failure".to_owned())
            } else {
                Ok(one_thread.runs[1].result.clone().unwrap())
            }
        });
        assert_eq!(injected[0].status, RunStatus::Failed);
        assert_eq!(
            injected[0].error.as_deref(),
            Some("injected stage 2 failure")
        );
        assert_eq!(injected[1].status, RunStatus::Completed);
        assert_eq!(
            serde_json::to_vec(injected[1].result.as_ref().unwrap()).unwrap(),
            serde_json::to_vec(one_thread.runs[1].result.as_ref().unwrap()).unwrap()
        );

        let golden_experiment = Experiment::parse(
            &scale_definition(
                &dsv,
                &universe,
                json!({"list": [{
                    "short": 20, "long": 60, "vol": 20,
                    "w_s": 1.0, "w_l": 1.0, "w_v": 1.0,
                    "trend": 20, "top_k": 5, "rebalance_every": 5
                }]}),
            )
            .to_string(),
            &lake,
        )
        .unwrap();
        let golden = golden_experiment.execute(&lake, 1).unwrap();
        let expected: Value = serde_json::from_slice(include_bytes!(
            "../../../poc/mvp1-golden/expected/b2-s2-scale-64x252-v2.trend20.json"
        ))
        .unwrap();
        let expected_vector = &expected["vector"];
        let actual_vector = serde_json::to_value(golden.runs[0].result.as_ref().unwrap()).unwrap();
        assert_vector_matches(
            &actual_vector,
            expected_vector,
            "vector",
            &expected["tolerance"],
        );
    }

    #[test]
    fn rejects_unknown_or_incomplete_and_unsupported_definitions() {
        let temp = TempDir::new().unwrap();
        let (lake, dsv, universe) = fixture(temp.path());
        let base = definition(&dsv, &universe);
        for (name, mutate) in [
            ("unknown field", 0),
            ("missing parameter", 1),
            ("non-null seed", 2),
            ("both space forms", 3),
            ("missing sessions", 4),
            ("zero sessions", 5),
        ] {
            let mut value = base.clone();
            match mutate {
                0 => value["unexpected"] = json!(true),
                1 => {
                    value["parameter_space"]["list"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("short");
                }
                2 => value["seed"] = json!(1),
                3 => value["parameter_space"]["grid"] = json!({}),
                4 => {
                    value.as_object_mut().unwrap().remove("sessions_per_year");
                }
                5 => value["sessions_per_year"] = json!(0),
                _ => unreachable!(),
            }
            assert!(
                Experiment::parse(&value.to_string(), &lake).is_err(),
                "accepted {name}"
            );
        }
        let mut with_result_level = base;
        with_result_level["result_level"] = json!("full");
        assert!(Experiment::parse(&with_result_level.to_string(), &lake).is_err());
    }

    #[test]
    fn run_identity_changes_with_strategy_parameters_and_costs() {
        let temp = TempDir::new().unwrap();
        let (lake, dsv, universe) = fixture(temp.path());
        let original_value = definition(&dsv, &universe);
        let original = Experiment::parse(&original_value.to_string(), &lake).unwrap();
        let original_run = original.run_id(run_parameters(&original)).unwrap();
        let original_experiment = original.id().unwrap();
        for (path, replacement) in [
            ("short", json!(3)),
            ("long", json!(6)),
            ("vol", json!(4)),
            ("w_s", json!(0.9)),
            ("top_k", json!(3)),
            ("w_l", json!(0.9)),
            ("w_v", json!(0.3)),
            ("trend", Value::Null),
            ("rebalance_every", json!(2)),
        ] {
            let mut changed = original_value.clone();
            changed["parameter_space"]["list"][0][path] = replacement;
            let changed = Experiment::parse(&changed.to_string(), &lake).unwrap();
            assert_ne!(
                original_run,
                changed.run_id(run_parameters(&changed)).unwrap()
            );
            assert_ne!(original_experiment, changed.id().unwrap());
        }
        for field in [
            "commission_rate",
            "buy_slippage_bps",
            "sell_slippage_bps",
            "buy_tax_rate",
            "sell_tax_rate",
        ] {
            let mut changed = original_value.clone();
            changed["costs"][field] = json!(0.002);
            let changed = Experiment::parse(&changed.to_string(), &lake).unwrap();
            assert_ne!(
                original_run,
                changed.run_id(run_parameters(&changed)).unwrap()
            );
            assert_ne!(original_experiment, changed.id().unwrap());
        }

        {
            let mut changed = original_value.clone();
            changed["availability_assumption"] = json!("treat_unknown_as_ts_close");
            let changed = Experiment::parse(&changed.to_string(), &lake).unwrap();
            assert_ne!(
                original_run,
                changed.run_id(run_parameters(&changed)).unwrap()
            );
            assert_ne!(original_experiment, changed.id().unwrap());
        }

        let alternate_universe = StaticUniverse::new(
            "different-label",
            ["A.SYNTH", "B.SYNTH"]
                .map(|id| id.parse::<InstrumentId>().unwrap())
                .to_vec(),
        )
        .unwrap();
        alternate_universe.store(&lake).unwrap();
        let mut changed = original_value;
        changed["universe"] = json!(alternate_universe.id().unwrap());
        let changed = Experiment::parse(&changed.to_string(), &lake).unwrap();
        assert_ne!(
            original_run,
            changed.run_id(run_parameters(&changed)).unwrap()
        );
        assert_ne!(original_experiment, changed.id().unwrap());

        const FIXTURE: &[u8] =
            include_bytes!("../../../poc/poc0-benchmark/fixtures/dataset-v1.json");
        let variant = std::str::from_utf8(FIXTURE).unwrap().replace(
            "poc0-hand-authored-golden-v1",
            "poc0-hand-authored-golden-v1-variant",
        );
        let store = RawStore::open(&lake).unwrap();
        let raw_hash = store
            .put(
                variant.as_bytes(),
                SourceRecordInput {
                    content_type: "application/json".into(),
                    source_kind: SourceKind::Fixture,
                    source_id: "poc0-dataset-v1-variant".into(),
                    request: json!({}),
                    observed_at: "synthetic".into(),
                    ingested_by: "prajna-experiment-tests".into(),
                },
            )
            .unwrap();
        let variant_manifest = publish_dataset(
            &lake,
            &store,
            &NormalizerRegistry::with_builtins(),
            "synthetic-etf-daily",
            "1",
            &[raw_hash],
            "2026-10-03T00:00:00Z",
        )
        .unwrap();
        let changed = definition(&variant_manifest.dsv, &universe);
        let changed = Experiment::parse(&changed.to_string(), &lake).unwrap();
        assert_ne!(
            original_run,
            changed.run_id(run_parameters(&changed)).unwrap()
        );
        assert_ne!(original_experiment, changed.id().unwrap());
    }

    #[test]
    fn execution_identity_tracks_dirty_and_untracked_worktree_content() {
        let repository = TempDir::new().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(repository.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "experiment-test@example.invalid"]);
        git(&["config", "user.name", "Experiment Test"]);
        fs::write(repository.path().join("tracked.txt"), "initial").unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "-qm", "initial"]);
        let clean = ExecutionIdentity::capture("exp:sha256:test", repository.path()).unwrap();
        assert!(clean.reproducible);
        fs::write(repository.path().join("tracked.txt"), "changed").unwrap();
        let tracked_dirty =
            ExecutionIdentity::capture("exp:sha256:test", repository.path()).unwrap();
        assert!(!tracked_dirty.reproducible);
        assert_ne!(
            clean.working_tree_diff_sha256,
            tracked_dirty.working_tree_diff_sha256
        );
        fs::write(repository.path().join("untracked.txt"), "untracked").unwrap();
        let untracked_dirty =
            ExecutionIdentity::capture("exp:sha256:test", repository.path()).unwrap();
        assert!(!untracked_dirty.reproducible);
        assert_ne!(
            tracked_dirty.working_tree_diff_sha256,
            untracked_dirty.working_tree_diff_sha256
        );
        assert_ne!(clean.id, tracked_dirty.id);
        assert_ne!(tracked_dirty.id, untracked_dirty.id);
        assert_ne!(
            untracked_dirty.id,
            ExecutionIdentity::capture("exp:sha256:other", repository.path())
                .unwrap()
                .id
        );
        assert_eq!(clean.target_triple, env!("PRAJNA_BUILD_TARGET"));

        fs::write(repository.path().join("研究 \"x\".txt"), "unicode").unwrap();
        let special = ExecutionIdentity::capture("exp:sha256:test", repository.path()).unwrap();
        assert!(!special.reproducible);
        assert_ne!(untracked_dirty.id, special.id);
        git(&["add", "tracked.txt"]);
        git(&["commit", "-qm", "second"]);
        let next = ExecutionIdentity::capture("exp:sha256:test", repository.path()).unwrap();
        assert_ne!(clean.git_revision, next.git_revision);
        assert_ne!(special.id, next.id);
    }

    #[test]
    fn float_bit_encoding_is_shared_with_factor_canonicalization() {
        assert_eq!(canonical_float_bits(0.1), "3fb999999999999a");
    }

    fn test_execution_identity(_experiment_id: &str, marker: char) -> ExecutionIdentity {
        ExecutionIdentity {
            id: format!("exe:sha256:{}", marker.to_string().repeat(64)),
            reproducible: true,
            git_revision: marker.to_string().repeat(40),
            working_tree_diff_sha256: "0".repeat(64),
            rustc_version: "rustc test".into(),
            target_triple: "test-target".into(),
        }
    }

    #[test]
    fn summary_storage_has_no_runs_and_replay_preserves_files() {
        let temp = TempDir::new().unwrap();
        let (lake, dsv, universe) = scale_fixture(temp.path());
        let experiment =
            Experiment::parse(&definition(&dsv, &universe).to_string(), &lake).unwrap();
        let report = experiment.execute(&lake, 1).unwrap();
        let identity = test_execution_identity(&experiment.id().unwrap(), 'a');
        let stored = storage::store_execution(
            &experiment,
            &identity,
            &report,
            Path::new(&lake),
            ResultLevel::Summary,
            1,
            "2026-10-09T00:00:00Z".into(),
        )
        .unwrap();
        assert!(!stored.path.join("runs").exists());
        assert_eq!(stored.manifest.runs.len(), 1);
        assert!(stored.manifest.runs[0].table_hashes.is_empty());
        assert_eq!(stored.manifest.runs[0].not_saved.len(), 6);
        assert_eq!(
            stored.manifest.runs[0].not_produced_by_engine,
            ["orders", "fills", "cash_ledger"]
        );
        let summary = stored.path.join("summary.parquet");
        let summary_reader =
            parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
                fs::File::open(&summary).unwrap(),
            )
            .unwrap();
        let summary_columns = summary_reader
            .schema()
            .fields()
            .iter()
            .map(|field| field.name().as_str())
            .collect::<Vec<_>>();
        for expected in [
            "run_id",
            "short",
            "w_s",
            "status",
            "error",
            "total_return",
            "insufficient_sessions",
            "zero_volatility",
        ] {
            assert!(summary_columns.contains(&expected), "missing {expected}");
        }
        let original = fs::read(&summary).unwrap();
        storage::store_execution(
            &experiment,
            &identity,
            &report,
            Path::new(&lake),
            ResultLevel::Summary,
            1,
            "2026-10-09T00:01:00Z".into(),
        )
        .unwrap();
        assert_eq!(fs::read(summary).unwrap(), original);
        assert!(
            !fs::read_dir(stored.path.parent().unwrap())
                .unwrap()
                .any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .contains("staging")
                })
        );
    }

    #[test]
    fn replay_detects_modified_parquet_without_overwriting_it() {
        let temp = TempDir::new().unwrap();
        let (lake, dsv, universe) = scale_fixture(temp.path());
        let experiment =
            Experiment::parse(&definition(&dsv, &universe).to_string(), &lake).unwrap();
        let report = experiment.execute(&lake, 1).unwrap();
        let identity = test_execution_identity(&experiment.id().unwrap(), 'b');
        let stored = storage::store_execution(
            &experiment,
            &identity,
            &report,
            Path::new(&lake),
            ResultLevel::Standard,
            1,
            "2026-10-09T00:00:00Z".into(),
        )
        .unwrap();
        assert_eq!(stored.manifest.runs[0].not_saved.len(), 4);
        let table = stored
            .path
            .join("runs")
            .join(storage::run_hex(&report.runs[0].run_id).unwrap())
            .join("sessions.parquet");
        let sessions_reader =
            parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
                fs::File::open(&table).unwrap(),
            )
            .unwrap();
        let session_columns = sessions_reader
            .schema()
            .fields()
            .iter()
            .map(|field| field.name().as_str())
            .collect::<Vec<_>>();
        for expected in [
            "session_date",
            "nav",
            "gross_return",
            "net_return",
            "turnover",
            "cost",
            "valuation_carried",
        ] {
            assert!(session_columns.contains(&expected), "missing {expected}");
        }
        fs::write(&table, b"corrupted parquet").unwrap();
        let corrupted = fs::read(&table).unwrap();
        let error = storage::store_execution(
            &experiment,
            &identity,
            &report,
            Path::new(&lake),
            ResultLevel::Standard,
            1,
            "2026-10-09T00:01:00Z".into(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("sessions.parquet"));
        assert_eq!(fs::read(table).unwrap(), corrupted);
    }

    #[test]
    fn promotion_writes_full_tables_for_one_run_and_rejects_a_different_execution() {
        let temp = TempDir::new().unwrap();
        let (lake, dsv, universe) = scale_fixture(temp.path());
        let mut definition = definition(&dsv, &universe);
        definition["parameter_space"]["list"] = json!([
            {"short": 2, "long": 5, "vol": 3, "w_s": 0.1, "w_l": 1.0, "w_v": 0.2, "trend": 20, "top_k": 1, "rebalance_every": 1},
            {"short": 2, "long": 5, "vol": 3, "w_s": 0.2, "w_l": 1.0, "w_v": 0.2, "trend": 20, "top_k": 1, "rebalance_every": 1}
        ]);
        let experiment = Experiment::parse(&definition.to_string(), &lake).unwrap();
        let report = experiment.execute(&lake, 1).unwrap();
        assert_eq!(report.runs.len(), 2);
        let identity = test_execution_identity(&experiment.id().unwrap(), 'c');
        let stored = storage::store_execution(
            &experiment,
            &identity,
            &report,
            Path::new(&lake),
            ResultLevel::Summary,
            1,
            "2026-10-09T00:00:00Z".into(),
        )
        .unwrap();
        let target = &report.runs[0].run_id;
        let promoted = storage::promote(
            &experiment,
            &identity,
            Path::new(&lake),
            &identity.id,
            std::slice::from_ref(target),
            ResultLevel::Full,
            1,
        )
        .unwrap();
        let target_record = promoted
            .manifest
            .runs
            .iter()
            .find(|run| run.run_id == *target)
            .unwrap();
        let other_record = promoted
            .manifest
            .runs
            .iter()
            .find(|run| run.run_id != *target)
            .unwrap();
        assert_eq!(target_record.result_level, ResultLevel::Full);
        assert_eq!(target_record.table_hashes.len(), 6);
        assert!(target_record.not_saved.is_empty());
        assert_eq!(other_record.result_level, ResultLevel::Summary);
        assert!(other_record.table_hashes.is_empty());
        let other_run_dir = stored
            .path
            .join("runs")
            .join(storage::run_hex(&other_record.run_id).unwrap());
        assert!(!other_run_dir.exists());

        let wrong_identity = test_execution_identity(&experiment.id().unwrap(), 'd');
        assert!(
            storage::promote(
                &experiment,
                &wrong_identity,
                Path::new(&lake),
                &identity.id,
                std::slice::from_ref(target),
                ResultLevel::Full,
                1,
            )
            .is_err()
        );
    }

    #[test]
    fn execution_diff_rejects_other_experiments_and_locates_metric_and_table_changes() {
        let temp = TempDir::new().unwrap();
        let (lake, dsv, universe) = scale_fixture(temp.path());
        let definition = definition(&dsv, &universe);
        let experiment = Experiment::parse(&definition.to_string(), &lake).unwrap();
        let first_report = experiment.execute(&lake, 1).unwrap();
        let left_identity = test_execution_identity(&experiment.id().unwrap(), 'e');
        let left = storage::store_execution(
            &experiment,
            &left_identity,
            &first_report,
            Path::new(&lake),
            ResultLevel::Full,
            1,
            "2026-10-09T00:00:00Z".into(),
        )
        .unwrap();

        let mut changed_report = experiment.execute(&lake, 1).unwrap();
        changed_report.runs[0]
            .result
            .as_mut()
            .unwrap()
            .sessions
            .last_mut()
            .unwrap()
            .nav += 0.25;
        let right_identity = test_execution_identity(&experiment.id().unwrap(), 'f');
        let right = storage::store_execution(
            &experiment,
            &right_identity,
            &changed_report,
            Path::new(&lake),
            ResultLevel::Full,
            1,
            "2026-10-09T00:02:00Z".into(),
        )
        .unwrap();
        let difference = diff_executions(&lake, &left_identity.id, &right_identity.id).unwrap();
        assert_eq!(difference.runs.len(), 1);
        let run = &difference.runs[0];
        assert_ne!(run.metric_deltas["total_return"], Some(0.0));
        let sessions = run
            .table_differences
            .iter()
            .find(|table| table.table == "sessions")
            .unwrap();
        assert_eq!(sessions.first_different_field.as_deref(), Some("nav"));
        assert!(sessions.first_different_key.is_some());

        let mut changed_definition = definition;
        changed_definition["sessions_per_year"] = json!(365);
        let other_experiment = Experiment::parse(&changed_definition.to_string(), &lake).unwrap();
        let other_report = other_experiment.execute(&lake, 1).unwrap();
        let other_identity = test_execution_identity(&other_experiment.id().unwrap(), '0');
        storage::store_execution(
            &other_experiment,
            &other_identity,
            &other_report,
            Path::new(&lake),
            ResultLevel::Summary,
            1,
            "2026-10-09T00:03:00Z".into(),
        )
        .unwrap();
        assert!(
            diff_executions(&lake, &left_identity.id, &other_identity.id)
                .unwrap_err()
                .to_string()
                .contains("different Experiments")
        );
        assert!(left.path.exists() && right.path.exists());
    }

    fn assert_vector_matches(actual: &Value, expected: &Value, path: &str, tolerance: &Value) {
        match (actual, expected) {
            (Value::Object(actual), Value::Object(expected)) => {
                assert_eq!(
                    actual.keys().collect::<Vec<_>>(),
                    expected.keys().collect::<Vec<_>>(),
                    "{path}"
                );
                for (key, expected_value) in expected {
                    assert_vector_matches(
                        &actual[key],
                        expected_value,
                        &format!("{path}.{key}"),
                        tolerance,
                    );
                }
            }
            (Value::Array(actual), Value::Array(expected)) => {
                assert_eq!(actual.len(), expected.len(), "{path}");
                for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                    assert_vector_matches(actual, expected, &format!("{path}[{index}]"), tolerance);
                }
            }
            (Value::Number(actual), Value::Number(expected))
                if actual.is_f64() || expected.is_f64() =>
            {
                let actual = actual.as_f64().unwrap();
                let expected = expected.as_f64().unwrap();
                let tolerance_key = if path.contains(".ranked[") && path.ends_with(".score") {
                    "factor_abs"
                } else {
                    "nav_abs"
                };
                let tolerance = tolerance[tolerance_key].as_f64().unwrap();
                assert!(
                    (actual - expected).abs() <= tolerance,
                    "{path}: {actual} != {expected} (tolerance {tolerance})"
                );
            }
            _ => assert_eq!(actual, expected, "{path}"),
        }
    }
}
