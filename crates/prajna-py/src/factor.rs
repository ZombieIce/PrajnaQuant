use prajna_research::factor::{
    Factor, FactorKind, MomentumParams, RotationScoreParams, TrendFilterParams, VolatilityParams,
};
use pyo3::{prelude::*, types::PyDict};
use serde::Deserialize;

use crate::arrow::value_error;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Window {
    n: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rotation {
    short: u32,
    long: u32,
    vol: u32,
    w_s: f64,
    w_l: f64,
    w_v: f64,
    trend: Option<u32>,
}

#[pyclass(name = "Factor", frozen)]
pub(crate) struct FactorDeclaration {
    pub(crate) factor: Factor,
}

#[pymethods]
impl FactorDeclaration {
    #[new]
    fn new(py: Python<'_>, kind: &str, params: &Bound<'_, PyDict>) -> PyResult<Self> {
        let kwargs = PyDict::new(py);
        kwargs.set_item("allow_nan", false)?;
        let json: String = py
            .import("json")?
            .call_method("dumps", (params,), Some(&kwargs))?
            .extract()?;
        Ok(Self {
            factor: definition(kind, &json)?,
        })
    }
}

pub(crate) fn definition(kind: &str, json: &str) -> PyResult<Factor> {
    let kind: FactorKind = kind.parse().map_err(value_error)?;
    match kind {
        FactorKind::RotationScore => {
            let params: Rotation = serde_json::from_str(json).map_err(value_error)?;
            Factor::new(
                kind,
                RotationScoreParams {
                    short: params.short,
                    long: params.long,
                    vol: params.vol,
                    w_s: params.w_s,
                    w_l: params.w_l,
                    w_v: params.w_v,
                    trend: params.trend,
                },
            )
        }
        _ => {
            let Window { n } = serde_json::from_str(json).map_err(value_error)?;
            match kind {
                FactorKind::Momentum => Factor::new(kind, MomentumParams { n }),
                FactorKind::Volatility => Factor::new(kind, VolatilityParams { n }),
                FactorKind::TrendFilter => Factor::new(kind, TrendFilterParams { n }),
                FactorKind::RotationScore => unreachable!("handled above"),
            }
        }
    }
    .map_err(value_error)
}
