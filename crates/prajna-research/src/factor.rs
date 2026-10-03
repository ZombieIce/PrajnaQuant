//! Versioned factor definitions only; evaluation belongs to a separate layer.
//! Definition source: PrajnaQuant Issue #53 M6/M7; each kind is version 1.
//!
//! Windows count Venue Sessions. Inputs are raw close, never future-return labels.
//! Momentum uses close[t]/close[t-n]-1; volatility uses n returns and sample
//! standard deviation; trend uses close[t] >= mean of the last n closes.
//! Rotation combines short/long momentum minus volatility, optionally filtering
//! on trend. Higher momentum means a larger trailing return; higher volatility
//! means greater dispersion (a penalty for positive w_v). Trend is a Boolean
//! eligibility gate, with 1 passing and 0 filtering. Rotation is ranked higher
//! first: w_s*m_short + w_l*m_long - w_v*volatility. Negative weights reverse
//! the corresponding contribution; no positivity restriction is imposed.
//! Availability is the maximum input availability (unknown propagates
//! unless the Run explicitly declares an assumption). Missing inputs are never
//! filled; status precedence is insufficient_window > missing_input >
//! unknown_availability, then filtered when an otherwise valid trend is zero.

use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    error::Error,
    fmt,
    str::FromStr,
    sync::Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorKind {
    Momentum,
    Volatility,
    TrendFilter,
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
        "1"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowParams {
    pub n: u32,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RotationScoreParams {
    pub short: u32,
    pub long: u32,
    pub vol: u32,
    pub w_s: f64,
    pub w_l: f64,
    pub w_v: f64,
    pub trend: Option<u32>,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FactorParams {
    Momentum(WindowParams),
    Volatility(WindowParams),
    TrendFilter(WindowParams),
    RotationScore(RotationScoreParams),
}
/// A validated definition. Private fields prevent mismatched kind/params and
/// bypassing validation; callers may copy parameters but cannot mutate a Factor.
#[derive(Debug, Clone)]
pub struct Factor {
    params: FactorParams,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactorError {
    InvalidWindow {
        role: &'static str,
        minimum: u32,
        actual: u32,
    },
    NonFiniteWeight(&'static str),
    Cycle,
    UnknownString {
        category: &'static str,
        value: String,
    },
}
impl fmt::Display for FactorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidWindow {
                role,
                minimum,
                actual,
            } => write!(f, "{role} window {actual} must be >= {minimum}"),
            Self::NonFiniteWeight(role) => write!(f, "{role} weight must be finite"),
            Self::Cycle => f.write_str("factor dependency cycle"),
            Self::UnknownString { category, value } => write!(f, "unknown {category}: {value}"),
        }
    }
}
impl Error for FactorError {}
fn window(role: &'static str, n: u32, minimum: u32) -> Result<(), FactorError> {
    if n < minimum {
        Err(FactorError::InvalidWindow {
            role,
            minimum,
            actual: n,
        })
    } else {
        Ok(())
    }
}
impl Factor {
    pub fn new(params: FactorParams) -> Result<Self, FactorError> {
        match params {
            FactorParams::Momentum(p) => window("momentum", p.n, 1)?,
            FactorParams::Volatility(p) => window("volatility", p.n, 2)?,
            FactorParams::TrendFilter(p) => window("trend", p.n, 1)?,
            FactorParams::RotationScore(p) => {
                window("short", p.short, 1)?;
                window("long", p.long, 1)?;
                window("volatility", p.vol, 2)?;
                if let Some(n) = p.trend {
                    window("trend", n, 1)?;
                }
                for (role, weight) in [("w_s", p.w_s), ("w_l", p.w_l), ("w_v", p.w_v)] {
                    if !weight.is_finite() {
                        return Err(FactorError::NonFiniteWeight(role));
                    }
                }
            }
        }
        Ok(Self { params })
    }
    pub fn kind(&self) -> FactorKind {
        match self.params {
            FactorParams::Momentum(_) => FactorKind::Momentum,
            FactorParams::Volatility(_) => FactorKind::Volatility,
            FactorParams::TrendFilter(_) => FactorKind::TrendFilter,
            FactorParams::RotationScore(_) => FactorKind::RotationScore,
        }
    }
    pub fn params(&self) -> &FactorParams {
        &self.params
    }
    /// Weights are 16 lowercase hex digits encoding IEEE-754 binary64 bits.
    /// All finite bits are preserved, including distinct positive/negative zero.
    pub fn canonical_json(&self) -> Value {
        let params = match self.params {
            FactorParams::Momentum(p)
            | FactorParams::Volatility(p)
            | FactorParams::TrendFilter(p) => json!({"n": p.n}),
            FactorParams::RotationScore(p) => {
                json!({"short": p.short, "long": p.long, "vol": p.vol, "w_s": format!("{:016x}", p.w_s.to_bits()), "w_l": format!("{:016x}", p.w_l.to_bits()), "w_v": format!("{:016x}", p.w_v.to_bits()), "trend": p.trend})
            }
        };
        json!({"kind": self.kind().as_str(), "version": self.kind().version(), "params": params})
    }
    pub fn dependencies(&self) -> Vec<(&'static str, Factor)> {
        let FactorParams::RotationScore(p) = self.params else {
            return vec![];
        };
        // Parent validation guarantees every dependency is valid.
        let mut deps = vec![
            (
                "momentum_short",
                Self {
                    params: FactorParams::Momentum(WindowParams { n: p.short }),
                },
            ),
            (
                "momentum_long",
                Self {
                    params: FactorParams::Momentum(WindowParams { n: p.long }),
                },
            ),
            (
                "volatility",
                Self {
                    params: FactorParams::Volatility(WindowParams { n: p.vol }),
                },
            ),
        ];
        if let Some(n) = p.trend {
            deps.push((
                "trend",
                Self {
                    params: FactorParams::TrendFilter(WindowParams { n }),
                },
            ));
        }
        deps
    }
}

pub struct FactorGraph;
impl FactorGraph {
    /// Deterministic dependency-first order, deduplicated by full identity.
    pub fn resolve(root: &Factor) -> Result<Vec<Factor>, FactorError> {
        Self::resolve_with(root, &Factor::dependencies)
    }
    // Private injection seam lets tests exercise cycles without opening the
    // production registry to unsupported kinds or arbitrary dependencies.
    fn resolve_with(
        root: &Factor,
        dependencies: &impl Fn(&Factor) -> Vec<(&'static str, Factor)>,
    ) -> Result<Vec<Factor>, FactorError> {
        fn visit(
            factor: &Factor,
            dependencies: &impl Fn(&Factor) -> Vec<(&'static str, Factor)>,
            active: &mut BTreeSet<String>,
            done: &mut BTreeSet<String>,
            output: &mut Vec<Factor>,
        ) -> Result<(), FactorError> {
            let key = factor.canonical_json().to_string();
            if done.contains(&key) {
                return Ok(());
            }
            if !active.insert(key.clone()) {
                return Err(FactorError::Cycle);
            }
            for (_, dependency) in dependencies(factor) {
                visit(&dependency, dependencies, active, done, output)?;
            }
            active.remove(&key);
            done.insert(key);
            output.push(factor.clone());
            Ok(())
        }
        let mut output = vec![];
        visit(
            root,
            dependencies,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            &mut output,
        )?;
        Ok(output)
    }
}

macro_rules! string_enum {
    ($name:ident, $category:literal, {$($variant:ident => $text:literal),+ $(,)?}) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name { $($variant),+ }
        impl $name { pub const fn as_str(self) -> &'static str { match self { $(Self::$variant => $text),+ } } }
        impl fmt::Display for $name { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) } }
        impl FromStr for $name { type Err = FactorError; fn from_str(value: &str) -> Result<Self, Self::Err> { match value { $($text => Ok(Self::$variant)),+, _ => Err(FactorError::UnknownString {category: $category, value: value.into()}) } } }
    };
}
string_enum!(FactorStatus, "factor status", { Ok => "ok", InsufficientWindow => "insufficient_window", MissingInput => "missing_input", UnknownAvailability => "unknown_availability", Filtered => "filtered" });
string_enum!(AvailabilityAssumption, "availability assumption", { None => "none", TreatUnknownAsTsClose => "treat_unknown_as_ts_close" });

pub const VALUES_SCHEMA_VERSION: u32 = 1;
/// Arrow schema for the Universe × Session long table, sorted by
/// (instrument_id, session_date). Non-ok status requires a null value.
/// Arrow Utf8/Date32/Float64/Timestamp(ns, UTC) map to Polars
/// String/Date/Float64/Datetime(Nanoseconds, UTC), respectively.
pub fn values_schema() -> SchemaRef {
    Arc::new(Schema::new_with_metadata(
        vec![
            Field::new("instrument_id", DataType::Utf8, false),
            Field::new("session_date", DataType::Date32, false),
            Field::new("value", DataType::Float64, true),
            Field::new(
                "available_at",
                DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
                true,
            )
            .with_metadata(HashMap::from([(
                "prajna.time_role".into(),
                "available_at".into(),
            )])),
            Field::new("status", DataType::Utf8, false),
        ],
        HashMap::from([
            ("prajna.table".into(), "factor_values".into()),
            (
                "prajna.schema_version".into(),
                VALUES_SCHEMA_VERSION.to_string(),
            ),
        ]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_direct_and_indirect_cycles() {
        let a = Factor::new(FactorParams::Momentum(WindowParams { n: 1 })).unwrap();
        let b = Factor::new(FactorParams::Momentum(WindowParams { n: 2 })).unwrap();
        assert!(matches!(
            FactorGraph::resolve_with(&a, &|_| vec![("self", a.clone())]),
            Err(FactorError::Cycle)
        ));
        assert!(matches!(
            FactorGraph::resolve_with(&a, &|f| vec![(
                "next",
                if f.canonical_json() == a.canonical_json() {
                    b.clone()
                } else {
                    a.clone()
                }
            )]),
            Err(FactorError::Cycle)
        ));
    }
}
