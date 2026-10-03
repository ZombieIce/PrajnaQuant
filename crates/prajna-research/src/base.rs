//! Session-aligned M6/M7 base factors over the complete D7 Panel grid.
use std::collections::BTreeSet;

use polars::lazy::dsl::max_horizontal;
use polars::prelude::*;
use prajna_domain::InstrumentId;

use crate::{
    Panel,
    factor::{AvailabilityAssumption, Factor, FactorError, FactorParams, FactorStatus},
};

/// Evaluate a base factor for the sorted, unique Universe members over every Session.
/// Only required historical closes contribute to validity and availability.
/// Composite factors and persistent caching are intentionally separate contracts.
pub fn compute_base(
    panel: &Panel,
    universe_members: &[InstrumentId],
    factor: &Factor,
    assumption: AvailabilityAssumption,
) -> Result<DataFrame, FactorError> {
    let (n, start) = match factor.params() {
        FactorParams::Momentum(p) => (p.n, p.n),
        FactorParams::Volatility(p) => (p.n, p.n),
        FactorParams::TrendFilter(p) => (p.n, p.n - 1),
        FactorParams::RotationScore(_) => {
            return Err(FactorError::Evaluation(
                "compute_base requires a base factor".into(),
            ));
        }
    };
    let members: BTreeSet<_> = universe_members.iter().map(ToString::to_string).collect();
    if members.len() != universe_members.len()
        || universe_members
            .iter()
            .any(|id| !panel.instruments.contains(id))
    {
        return Err(FactorError::Evaluation(
            "Universe members must be unique and present in Panel".into(),
        ));
    }
    let ids = panel.grid.column("instrument_id")?.str()?;
    let indices = panel.grid.column("session_index")?.u32()?;
    let mut seen = BTreeSet::new();
    let mut closes = Vec::with_capacity(panel.grid.height());
    let mut selected = Vec::with_capacity(panel.grid.height());
    for row in 0..panel.grid.height() {
        let id = ids
            .get(row)
            .ok_or_else(|| FactorError::Evaluation("null Panel instrument".into()))?;
        let index = indices
            .get(row)
            .ok_or_else(|| FactorError::Evaluation("null Panel session index".into()))?;
        let session = panel
            .sessions
            .get(index as usize)
            .ok_or_else(|| FactorError::Evaluation("Panel session index out of range".into()))?;
        let keep = members.contains(id);
        if keep && !seen.insert((id, index)) {
            return Err(FactorError::Evaluation("duplicate Panel grid row".into()));
        }
        selected.push(keep);
        closes.push(session.ts_close.as_unix_nanos());
    }
    if seen.len() != members.len() * panel.sessions.len() {
        return Err(FactorError::Evaluation("incomplete Panel grid".into()));
    }
    let mut grid = panel.grid.clone();
    // Work in UTC nanoseconds to avoid timezone casts inside Polars window lists.
    grid.with_column(
        grid.column("close_available_at")?
            .datetime()?
            .physical()
            .clone()
            .into_series()
            .into(),
    )?;
    grid.with_column(Series::new("_ts_close".into(), closes).into())?;
    let mut frame = grid
        .filter(&BooleanChunked::from_slice("".into(), &selected))?
        .lazy()
        .sort(
            ["instrument_id", "session_index"],
            SortMultipleOptions::default(),
        );
    // Oversized windows have no eligible sessions; avoid allocating expressions proportional to n.
    if start as usize >= panel.sessions.len() {
        return values_with_utc(
            frame
                .select([
                    col("instrument_id"),
                    col("session_date"),
                    lit(NULL).cast(DataType::Float64).alias("value"),
                    lit(NULL)
                        .cast(DataType::Datetime(
                            TimeUnit::Nanoseconds,
                            Some(TimeZone::UTC),
                        ))
                        .alias("available_at"),
                    lit(FactorStatus::InsufficientWindow.as_str()).alias("status"),
                ])
                .collect()?,
        );
    }
    let offsets: Vec<u32> = match factor.params() {
        FactorParams::Momentum(_) => vec![0, n],
        _ => (0..=start).collect(),
    };
    let lag = |name: &str, offset: u32| {
        col(name)
            .shift(lit(i64::from(offset)))
            .over([col("instrument_id")])
    };
    let mut missing = lit(false);
    let mut unknown = lit(false);
    let mut times = Vec::new();
    for offset in offsets {
        missing = missing.or(lag("has_bar", offset)?.not().fill_null(lit(true)));
        unknown = unknown.or(lag("close_available_at", offset)?.is_null());
        let time = lag("close_available_at", offset)?;
        times.push(match assumption {
            AvailabilityAssumption::None => time,
            AvailabilityAssumption::TreatUnknownAsTsClose => {
                time.fill_null(lag("_ts_close", offset)?)
            }
        });
    }
    let value = match factor.params() {
        FactorParams::Momentum(_) => col("close") / lag("close", n)? - lit(1.0),
        FactorParams::TrendFilter(_) => {
            let sum = (0..n).try_fold(lit(0.0), |sum, offset| -> PolarsResult<Expr> {
                Ok(sum + lag("close", offset)?)
            })?;
            col("close")
                .gt_eq(sum / lit(f64::from(n)))
                .cast(DataType::Float64)
        }
        FactorParams::Volatility(_) => {
            let returns: Vec<_> = (0..n)
                .map(|offset| -> PolarsResult<Expr> {
                    Ok(lag("close", offset)? / lag("close", offset + 1)? - lit(1.0))
                })
                .collect::<PolarsResult<_>>()?;
            let mean = returns.iter().cloned().fold(lit(0.0), |sum, r| sum + r) / lit(f64::from(n));
            frame = frame.with_column(mean.alias("_return_mean"));
            let squares = returns.into_iter().fold(lit(0.0), |sum, r| {
                let deviation = r - col("_return_mean");
                sum + deviation.clone() * deviation
            });
            (squares / lit(f64::from(n - 1))).sqrt()
        }
        FactorParams::RotationScore(_) => unreachable!(),
    };
    let unknown = if assumption == AvailabilityAssumption::None {
        unknown
    } else {
        lit(false)
    };
    let status = when(col("session_index").lt(lit(start)))
        .then(lit(FactorStatus::InsufficientWindow.as_str()))
        .when(missing)
        .then(lit(FactorStatus::MissingInput.as_str()))
        .when(unknown)
        .then(lit(FactorStatus::UnknownAvailability.as_str()))
        .otherwise(lit(FactorStatus::Ok.as_str()))
        .alias("status");
    values_with_utc(
        frame
            .with_column(status)
            .select([
                col("instrument_id"),
                col("session_date"),
                when(col("status").eq(lit("ok")))
                    .then(value)
                    .otherwise(lit(NULL))
                    .cast(DataType::Float64)
                    .alias("value"),
                when(col("status").eq(lit("ok")))
                    .then(max_horizontal(times)?)
                    .otherwise(lit(NULL))
                    .cast(DataType::Datetime(
                        TimeUnit::Nanoseconds,
                        Some(TimeZone::UTC),
                    ))
                    .alias("available_at"),
                col("status"),
            ])
            .collect()?,
    )
}

// Without Polars' timezones feature, lazy casts may treat UTC and no timezone
// as equivalent and optimize away the cast. Preserve the M6 schema literally.
fn values_with_utc(mut values: DataFrame) -> Result<DataFrame, FactorError> {
    let available = values
        .column("available_at")?
        .datetime()?
        .physical()
        .clone()
        .into_datetime(TimeUnit::Nanoseconds, Some(TimeZone::UTC))
        .into_series();
    values.with_column(available.into())?;
    Ok(values)
}
