//! Versioned definitions from PrajnaQuant Issue #53 M6/M7; base numerical evaluation lives in `compute_base`.
//! Finite weights preserve all binary64 bits in canonical JSON, including signed zero.
//! Negative weights reverse the corresponding score contribution; positivity is not required.
//! Factor Values rows cover Universe × Session, sorted by (instrument_id, session_date).

use std::{collections::BTreeSet, error::Error, fmt, str::FromStr, sync::Arc};

use arrow_schema::{
    DataType as ArrowDataType, Field as ArrowField, Schema as ArrowSchema, SchemaRef,
    TimeUnit as ArrowTimeUnit,
};
use polars::prelude::{
    DataType as PolarsDataType, Schema as PolarsSchema, TimeUnit as PolarsTimeUnit, TimeZone,
};
use serde_json::{Value, json};

const MOMENTUM_VERSION: &str = "1";
const VOLATILITY_VERSION: &str = "1";
const TREND_FILTER_VERSION: &str = "1";
const ROTATION_SCORE_VERSION: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FactorStatus {
    Ok,
    InsufficientWindow,
    MissingInput,
    UnknownAvailability,
    Filtered,
}

impl FactorStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::InsufficientWindow => "insufficient_window",
            Self::MissingInput => "missing_input",
            Self::UnknownAvailability => "unknown_availability",
            Self::Filtered => "filtered",
        }
    }
}

impl fmt::Display for FactorStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for FactorStatus {
    type Err = FactorError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "ok" => Ok(Self::Ok),
            "insufficient_window" => Ok(Self::InsufficientWindow),
            "missing_input" => Ok(Self::MissingInput),
            "unknown_availability" => Ok(Self::UnknownAvailability),
            "filtered" => Ok(Self::Filtered),
            _ => Err(FactorError::UnknownStatus(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AvailabilityAssumption {
    None,
    TreatUnknownAsTsClose,
}

impl AvailabilityAssumption {
    pub const fn canonical_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::TreatUnknownAsTsClose => "treat_unknown_as_ts_close",
        }
    }
}

impl fmt::Display for AvailabilityAssumption {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_str())
    }
}

impl FromStr for AvailabilityAssumption {
    type Err = FactorError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "none" => Ok(Self::None),
            "treat_unknown_as_ts_close" => Ok(Self::TreatUnknownAsTsClose),
            _ => Err(FactorError::UnknownAvailabilityAssumption(value.to_owned())),
        }
    }
}

pub const VALUES_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct ValuesSchema {
    pub arrow: SchemaRef,
    pub polars: PolarsSchema,
}

pub fn values_schema() -> ValuesSchema {
    let arrow = Arc::new(ArrowSchema::new(vec![
        ArrowField::new("instrument_id", ArrowDataType::Utf8, false),
        ArrowField::new("session_date", ArrowDataType::Date32, false),
        ArrowField::new("value", ArrowDataType::Float64, true),
        ArrowField::new(
            "available_at",
            ArrowDataType::Timestamp(ArrowTimeUnit::Nanosecond, Some("UTC".into())),
            true,
        ),
        ArrowField::new("status", ArrowDataType::Utf8, false),
    ]));
    let polars = PolarsSchema::from_iter([
        ("instrument_id".into(), PolarsDataType::String),
        ("session_date".into(), PolarsDataType::Date),
        ("value".into(), PolarsDataType::Float64),
        (
            "available_at".into(),
            PolarsDataType::Datetime(PolarsTimeUnit::Nanoseconds, Some(TimeZone::UTC)),
        ),
        ("status".into(), PolarsDataType::String),
    ]);
    ValuesSchema { arrow, polars }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Closed MVP-1 M6 factor definitions, versioned independently by kind.
///
/// The v1 inputs are D7 normalized `bars.close` values for Venue Sessions.
/// Each value is observable after the current session close; its availability
/// is the maximum `available_at` of its required inputs. An unknown input
/// availability yields `unknown_availability`, unless the Run explicitly
/// chooses the M7 `TreatUnknownAsTsClose` assumption. A missing required bar
/// yields `missing_input`, while insufficient session history yields
/// `insufficient_window`; non-`ok` values are null. This crate defines those
/// semantics; `compute_base` evaluates the three base kinds.
///
/// | Kind | v1 definition and direction |
/// | --- | --- |
/// | [`Momentum`](Self::Momentum) | `close_t / close_(t-n) - 1`; larger values indicate stronger momentum. |
/// | [`Volatility`](Self::Volatility) | Sample standard deviation of `n` consecutive close-to-close returns; the composite subtracts `w_v * volatility`. |
/// | [`TrendFilter`](Self::TrendFilter) | `close_t >= mean(close_(t-n+1..t))`, yielding 1 or 0; zero filters a composite score. |
/// | [`RotationScore`](Self::RotationScore) | `w_s * momentum(short) + w_l * momentum(long) - w_v * volatility(vol)`; larger scores rank higher. |
///
/// `RotationScore` propagates dependency statuses in the order
/// `insufficient_window > missing_input > unknown_availability`. These
/// definitions follow the MVP-1 M6/M7 contract over D7 bars; each kind's v1
/// identity is included in [`Factor::canonical_json`].
pub enum FactorKind {
    /// Uses the close at `t` and `t-n`; `n` must be at least 1.
    Momentum,
    /// Uses `n` consecutive returns and therefore `n+1` closes; `n` must be at least 2.
    Volatility,
    /// Uses `n` closes from `t-n+1` through `t`; `n` must be at least 1.
    TrendFilter,
    /// Composes the configured child factors; each window follows its child kind's rules.
    RotationScore,
}

impl FactorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Momentum => "momentum",
            Self::Volatility => "volatility",
            Self::TrendFilter => "trend_filter",
            Self::RotationScore => "rotation_score",
        }
    }

    pub const fn version(self) -> &'static str {
        match self {
            Self::Momentum => MOMENTUM_VERSION,
            Self::Volatility => VOLATILITY_VERSION,
            Self::TrendFilter => TREND_FILTER_VERSION,
            Self::RotationScore => ROTATION_SCORE_VERSION,
        }
    }
}

impl fmt::Display for FactorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for FactorKind {
    type Err = FactorError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "momentum" => Ok(Self::Momentum),
            "volatility" => Ok(Self::Volatility),
            "trend_filter" => Ok(Self::TrendFilter),
            "rotation_score" => Ok(Self::RotationScore),
            _ => Err(FactorError::UnknownKind(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MomentumParams {
    pub n: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VolatilityParams {
    pub n: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrendFilterParams {
    pub n: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RotationScoreParams {
    pub short: u32,
    pub long: u32,
    pub vol: u32,
    pub w_s: f64,
    pub w_l: f64,
    pub w_v: f64,
    pub trend: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FactorParams {
    Momentum(MomentumParams),
    Volatility(VolatilityParams),
    TrendFilter(TrendFilterParams),
    RotationScore(RotationScoreParams),
}

impl From<MomentumParams> for FactorParams {
    fn from(params: MomentumParams) -> Self {
        Self::Momentum(params)
    }
}

impl From<VolatilityParams> for FactorParams {
    fn from(params: VolatilityParams) -> Self {
        Self::Volatility(params)
    }
}

impl From<TrendFilterParams> for FactorParams {
    fn from(params: TrendFilterParams) -> Self {
        Self::TrendFilter(params)
    }
}

impl From<RotationScoreParams> for FactorParams {
    fn from(params: RotationScoreParams) -> Self {
        Self::RotationScore(params)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactorError {
    Evaluation(String),
    UnknownKind(String),
    UnknownStatus(String),
    UnknownAvailabilityAssumption(String),
    ParamsKindMismatch {
        kind: FactorKind,
        params_kind: FactorKind,
    },
    InvalidWindow {
        kind: FactorKind,
        parameter: &'static str,
        minimum: u32,
        value: u32,
    },
    InvalidWeight {
        kind: FactorKind,
        parameter: &'static str,
    },
    DependencyCycle {
        factor: String,
    },
}

impl fmt::Display for FactorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Evaluation(message) => formatter.write_str(message),
            Self::UnknownKind(kind) => write!(formatter, "unknown factor kind {kind:?}"),
            Self::UnknownStatus(status) => write!(formatter, "unknown factor status {status:?}"),
            Self::UnknownAvailabilityAssumption(assumption) => {
                write!(formatter, "unknown availability assumption {assumption:?}")
            }
            Self::ParamsKindMismatch { kind, params_kind } => {
                write!(
                    formatter,
                    "factor kind {kind} does not match params for {params_kind}"
                )
            }
            Self::InvalidWindow {
                kind,
                parameter,
                minimum,
                value,
            } => write!(
                formatter,
                "{kind} parameter {parameter} must be at least {minimum}, got {value}"
            ),
            Self::InvalidWeight { kind, parameter } => {
                write!(formatter, "{kind} weight {parameter} must be finite")
            }
            Self::DependencyCycle { factor } => {
                write!(formatter, "factor dependency cycle includes {factor}")
            }
        }
    }
}

impl Error for FactorError {}

impl From<polars::error::PolarsError> for FactorError {
    fn from(error: polars::error::PolarsError) -> Self {
        Self::Evaluation(error.to_string())
    }
}

impl FactorParams {
    fn kind(&self) -> FactorKind {
        match self {
            Self::Momentum(_) => FactorKind::Momentum,
            Self::Volatility(_) => FactorKind::Volatility,
            Self::TrendFilter(_) => FactorKind::TrendFilter,
            Self::RotationScore(_) => FactorKind::RotationScore,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Factor {
    kind: FactorKind,
    params: FactorParams,
}

impl Factor {
    pub fn new<P>(kind: FactorKind, params: P) -> Result<Self, FactorError>
    where
        P: Into<FactorParams>,
    {
        let params = params.into();
        if params.kind() != kind {
            return Err(FactorError::ParamsKindMismatch {
                kind,
                params_kind: params.kind(),
            });
        }

        validate_params(kind, &params)?;
        Ok(Self::from_validated(kind, params))
    }

    fn from_validated(kind: FactorKind, params: FactorParams) -> Self {
        Self { kind, params }
    }

    pub const fn kind(&self) -> FactorKind {
        self.kind
    }

    pub fn params(&self) -> &FactorParams {
        &self.params
    }

    pub fn dependencies(&self) -> Vec<(&'static str, Factor)> {
        let FactorParams::RotationScore(params) = &self.params else {
            return Vec::new();
        };

        let mut dependencies = vec![
            (
                "momentum_short",
                Self::from_validated(
                    FactorKind::Momentum,
                    MomentumParams { n: params.short }.into(),
                ),
            ),
            (
                "momentum_long",
                Self::from_validated(
                    FactorKind::Momentum,
                    MomentumParams { n: params.long }.into(),
                ),
            ),
            (
                "volatility",
                Self::from_validated(
                    FactorKind::Volatility,
                    VolatilityParams { n: params.vol }.into(),
                ),
            ),
        ];
        if let Some(trend) = params.trend {
            dependencies.push((
                "trend",
                Self::from_validated(
                    FactorKind::TrendFilter,
                    TrendFilterParams { n: trend }.into(),
                ),
            ));
        }
        dependencies
    }

    pub fn canonical_json(&self) -> Value {
        let params = match &self.params {
            FactorParams::Momentum(params) => json!({"n": params.n}),
            FactorParams::Volatility(params) => json!({"n": params.n}),
            FactorParams::TrendFilter(params) => json!({"n": params.n}),
            FactorParams::RotationScore(params) => json!({
                "short": params.short,
                "long": params.long,
                "vol": params.vol,
                "w_s": weight_bits(params.w_s),
                "w_l": weight_bits(params.w_l),
                "w_v": weight_bits(params.w_v),
                "trend": params.trend
            }),
        };
        json!({
            "kind": self.kind.as_str(),
            "version": self.kind.version(),
            "params": params
        })
    }
}

impl PartialEq for Factor {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_json() == other.canonical_json()
    }
}

impl Eq for Factor {}

#[derive(Debug, Clone, Copy, Default)]
pub struct FactorGraph;

impl FactorGraph {
    pub fn resolve(root: Factor) -> Result<Vec<Factor>, FactorError> {
        resolve_with_dependencies(root, &Factor::dependencies)
    }
}

fn resolve_with_dependencies<F>(root: Factor, dependencies: &F) -> Result<Vec<Factor>, FactorError>
where
    F: Fn(&Factor) -> Vec<(&'static str, Factor)>,
{
    fn visit<F>(
        factor: Factor,
        dependencies: &F,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        ordered: &mut Vec<Factor>,
    ) -> Result<(), FactorError>
    where
        F: Fn(&Factor) -> Vec<(&'static str, Factor)>,
    {
        let identity = factor.canonical_json().to_string();
        if visited.contains(&identity) {
            return Ok(());
        }
        if !visiting.insert(identity.clone()) {
            return Err(FactorError::DependencyCycle { factor: identity });
        }

        for (_, dependency) in dependencies(&factor) {
            visit(dependency, dependencies, visiting, visited, ordered)?;
        }

        visiting.remove(&identity);
        visited.insert(identity);
        ordered.push(factor);
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut ordered = Vec::new();
    visit(
        root,
        dependencies,
        &mut visiting,
        &mut visited,
        &mut ordered,
    )?;
    Ok(ordered)
}

fn weight_bits(value: f64) -> String {
    format!("{:016x}", value.to_bits())
}

fn validate_params(kind: FactorKind, params: &FactorParams) -> Result<(), FactorError> {
    let window = |parameter, value, minimum| {
        if value < minimum {
            Err(FactorError::InvalidWindow {
                kind,
                parameter,
                minimum,
                value,
            })
        } else {
            Ok(())
        }
    };

    let finite_weight = |parameter, value: f64| {
        if value.is_finite() {
            Ok(())
        } else {
            Err(FactorError::InvalidWeight { kind, parameter })
        }
    };

    match params {
        FactorParams::Momentum(params) => window("n", params.n, 1),
        FactorParams::Volatility(params) => window("n", params.n, 2),
        FactorParams::TrendFilter(params) => window("n", params.n, 1),
        FactorParams::RotationScore(params) => {
            window("short", params.short, 1)?;
            window("long", params.long, 1)?;
            window("vol", params.vol, 2)?;
            if let Some(trend) = params.trend {
                window("trend", trend, 1)?;
            }
            finite_weight("w_s", params.w_s)?;
            finite_weight("w_l", params.w_l)?;
            finite_weight("w_v", params.w_v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_self_dependency_is_rejected() {
        let root = Factor::new(FactorKind::Momentum, MomentumParams { n: 1 }).unwrap();
        let cycle = resolve_with_dependencies(root.clone(), &|_| vec![("self", root.clone())]);
        assert!(matches!(cycle, Err(FactorError::DependencyCycle { .. })));
    }

    #[test]
    fn injected_dependency_cycle_is_rejected() {
        let momentum = Factor::new(FactorKind::Momentum, MomentumParams { n: 1 }).unwrap();
        let volatility = Factor::new(FactorKind::Volatility, VolatilityParams { n: 2 }).unwrap();
        let cycle = resolve_with_dependencies(momentum.clone(), &|factor| match factor.kind() {
            FactorKind::Momentum => vec![("cycle", volatility.clone())],
            FactorKind::Volatility => vec![("cycle", momentum.clone())],
            _ => Vec::new(),
        });

        assert!(matches!(cycle, Err(FactorError::DependencyCycle { .. })));
    }
}
