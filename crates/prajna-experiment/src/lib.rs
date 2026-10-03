//! Experiment definition validation and content-addressed identities.
//!
//! This crate parses MVP-2 `experiment.json` definitions and creates Experiment,
//! Run Spec, and Experiment Execution identities. It does not execute Runs.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, fs,
    path::Path,
    process::Command,
};

use prajna_data::{ManifestError, read_manifest, restricted_jcs};
use prajna_domain::VenueId;
use prajna_research::{StaticUniverse, factor::canonical_float_bits};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

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
            Self::InvalidDefinition(_) | Self::Canonical(_) | Self::Git(_) => None,
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
            top_k: positive_integer(&object["top_k"], "top_k")?,
            rebalance_every: positive_integer(&object["rebalance_every"], "rebalance_every")?,
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
        let run_spec = serde_json::json!({
            "dsv": self.dsv,
            "universe": self.universe,
            "venue": self.venue,
            "strategy": { "kind": "s2_rotation", "parameters": parameters.normalized() },
            "costs": self.costs.normalized(),
            "availability_assumption": self.availability_assumption,
            "engine": { "kind": "vector", "semantic_version": "vector@1" },
            "seed": Value::Null,
        });
        let bytes = restricted_jcs(&run_spec)
            .map_err(|error| ExperimentError::Canonical(error.to_string()))?;
        Ok(identity("run", &bytes))
    }
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
    FloatBits,
    OptionalPositiveInteger,
}

fn parameter_value_kind(name: &str) -> Option<ParameterValueKind> {
    match name {
        "short" | "long" | "vol" | "top_k" | "rebalance_every" => {
            Some(ParameterValueKind::PositiveInteger)
        }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

        for (field, value) in [
            ("venue", json!("OTHER")),
            (
                "availability_assumption",
                json!("treat_unknown_as_ts_close"),
            ),
        ] {
            let mut changed = original_value.clone();
            changed[field] = value;
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
}
