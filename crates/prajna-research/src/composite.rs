//! M6 composite evaluation over dependency Factor Values, aligned by primary key.
use std::collections::BTreeMap;

use polars::prelude::*;

use crate::factor::{Factor, FactorError, FactorParams, FactorStatus, values_schema};

#[derive(Clone, Copy)]
struct ValueRow {
    value: Option<f64>,
    available_at: Option<i64>,
    status: FactorStatus,
}

fn dependency_rows(frame: &DataFrame) -> Result<BTreeMap<(&str, i32), ValueRow>, FactorError> {
    if frame.schema().as_ref() != &values_schema().polars {
        return Err(FactorError::Evaluation(
            "dependency must use the Factor Values v1 schema".into(),
        ));
    }
    let ids = frame.column("instrument_id")?.str()?;
    let dates = frame.column("session_date")?.date()?.physical();
    let values = frame.column("value")?.f64()?;
    let times = frame.column("available_at")?.datetime()?.physical();
    let statuses = frame.column("status")?.str()?;
    let mut rows = BTreeMap::new();
    for row in 0..frame.height() {
        let id = ids
            .get(row)
            .ok_or_else(|| FactorError::Evaluation("null dependency instrument_id".into()))?;
        let date = dates
            .get(row)
            .ok_or_else(|| FactorError::Evaluation("null dependency session_date".into()))?;
        let status: FactorStatus = statuses
            .get(row)
            .ok_or_else(|| FactorError::Evaluation("null dependency status".into()))?
            .parse()?;
        let value = values.get(row);
        let available_at = times.get(row);
        if status == FactorStatus::Filtered {
            return Err(FactorError::Evaluation(
                "rotation_score dependencies must be base factors, not filtered scores".into(),
            ));
        }
        if (status == FactorStatus::Ok
            && (value.is_none_or(|v| !v.is_finite()) || available_at.is_none()))
            || (status != FactorStatus::Ok && value.is_some())
        {
            return Err(FactorError::Evaluation(
                "dependency value/availability contradicts its status".into(),
            ));
        }
        if rows
            .insert(
                (id, date),
                ValueRow {
                    value,
                    available_at,
                    status,
                },
            )
            .is_some()
        {
            return Err(FactorError::Evaluation("duplicate dependency key".into()));
        }
    }
    Ok(rows)
}

fn priority(status: FactorStatus) -> u8 {
    match status {
        FactorStatus::InsufficientWindow => 3,
        FactorStatus::MissingInput => 2,
        FactorStatus::UnknownAvailability => 1,
        FactorStatus::Ok | FactorStatus::Filtered => 0,
    }
}

/// Evaluate rotation_score using exactly its declared dependency roles.
///
/// Dependencies must have the same unique key grid and the v1 Values schema;
/// input row order and role order are irrelevant. Non-ok values are null.
/// Availability is the maximum known dependency availability, including on
/// filtered/non-ok rows; no unknown timestamp is inferred here.
pub fn compute_composite(
    factor: &Factor,
    deps: &[(&str, DataFrame)],
) -> Result<DataFrame, FactorError> {
    let FactorParams::RotationScore(params) = factor.params() else {
        return Err(FactorError::Evaluation(
            "compute_composite requires rotation_score".into(),
        ));
    };
    let expected = factor.dependencies();
    if deps.len() != expected.len() {
        return Err(FactorError::Evaluation(
            "dependency roles must exactly match the factor definition".into(),
        ));
    }
    let mut inputs = BTreeMap::new();
    for (role, _) in &expected {
        let matching: Vec<_> = deps.iter().filter(|(name, _)| name == role).collect();
        if matching.len() != 1 {
            return Err(FactorError::Evaluation(format!(
                "expected exactly one dependency for role {role}"
            )));
        }
        inputs.insert(*role, dependency_rows(&matching[0].1)?);
    }
    let first = &inputs["momentum_short"];
    if inputs.values().any(|rows| !rows.keys().eq(first.keys())) {
        return Err(FactorError::Evaluation(
            "dependency key grids must match".into(),
        ));
    }
    let mut ids = Vec::with_capacity(first.len());
    let mut dates = Vec::with_capacity(first.len());
    let mut values = Vec::with_capacity(first.len());
    let mut times = Vec::with_capacity(first.len());
    let mut statuses = Vec::with_capacity(first.len());
    for key in first.keys() {
        let rows: Vec<_> = inputs.values().map(|input| input[key]).collect();
        let mut status = rows
            .iter()
            .map(|row| row.status)
            .max_by_key(|s| priority(*s))
            .ok_or_else(|| FactorError::Evaluation("missing dependencies".into()))?;
        let mut value = None;
        if status == FactorStatus::Ok {
            let number = |role: &str| {
                inputs
                    .get(role)
                    .and_then(|input| input[key].value)
                    .ok_or_else(|| {
                        FactorError::Evaluation(format!("ok dependency {role} has no value"))
                    })
            };
            let filtered = if params.trend.is_some() {
                match number("trend")? {
                    0.0 => true,
                    1.0 => false,
                    _ => {
                        return Err(FactorError::Evaluation(
                            "trend dependency must be 0 or 1".into(),
                        ));
                    }
                }
            } else {
                false
            };
            if filtered {
                status = FactorStatus::Filtered;
            } else {
                let score = params.w_s * number("momentum_short")?
                    + params.w_l * number("momentum_long")?
                    - params.w_v * number("volatility")?;
                if !score.is_finite() {
                    return Err(FactorError::Evaluation("non-finite rotation_score".into()));
                }
                value = Some(score);
            }
        }
        ids.push(key.0);
        dates.push(key.1);
        values.push(value);
        times.push(rows.iter().filter_map(|row| row.available_at).max());
        statuses.push(status.as_str());
    }
    Ok(DataFrame::new(
        first.len(),
        vec![
            Series::new("instrument_id".into(), ids).into(),
            Series::new("session_date".into(), dates)
                .cast(&DataType::Date)?
                .into(),
            Series::new("value".into(), values).into(),
            Series::new("available_at".into(), times)
                .i64()?
                .clone()
                .into_datetime(TimeUnit::Nanoseconds, Some(TimeZone::UTC))
                .into_series()
                .into(),
            Series::new("status".into(), statuses).into(),
        ],
    )?)
}
