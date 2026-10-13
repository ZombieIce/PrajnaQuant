use chrono::NaiveDate;
use polars::prelude::{BooleanChunked, DataFrame, NewChunkedArray};
use prajna_domain::VenueId;
use prajna_experiment::{ResultLevel, ResultReadError, read_run_table};
use prajna_research::{
    Panel, StaticUniverse,
    cache::{key::FactorKey, store::FactorCache},
    factor::{AvailabilityAssumption, values_schema},
    load_panel,
};
use pyo3::prelude::*;

use crate::arrow::{dataframe_batch, table, value_error};
use crate::factor::FactorDeclaration;

pyo3::create_exception!(_native, ResultNotSavedError, pyo3::exceptions::PyValueError);
pyo3::create_exception!(
    _native,
    EngineNotProducedError,
    pyo3::exceptions::PyValueError
);

fn load_inputs(
    lake: &str,
    dsv: &str,
    universe: &str,
    venue: &str,
) -> PyResult<(Panel, StaticUniverse)> {
    let universe = StaticUniverse::load(lake, universe).map_err(value_error)?;
    universe.validate_against(lake, dsv).map_err(value_error)?;
    let panel =
        load_panel(lake, dsv, &VenueId::new(venue).map_err(value_error)?).map_err(value_error)?;
    if universe
        .members()
        .iter()
        .any(|member| !panel.instruments.contains(member))
    {
        return Err(value_error(
            "Universe contains members outside the selected venue",
        ));
    }
    Ok((panel, universe))
}

fn select(
    frame: DataFrame,
    columns: Option<Vec<String>>,
    start: Option<String>,
    end: Option<String>,
) -> PyResult<DataFrame> {
    let parse = |date: String| NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(value_error);
    let start = start.map(parse).transpose()?;
    let end = end.map(parse).transpose()?;
    if matches!((start, end), (Some(start), Some(end)) if start > end) {
        return Err(value_error("start must not be after end"));
    }
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid epoch");
    let dates = frame
        .column("session_date")
        .map_err(value_error)?
        .date()
        .map_err(value_error)?;
    let mask = BooleanChunked::from_iter_values(
        "selection".into(),
        dates.physical().iter().map(|days| {
            days.is_some_and(|days| {
                start.is_none_or(|start| i64::from(days) >= (start - epoch).num_days())
                    && end.is_none_or(|end| i64::from(days) <= (end - epoch).num_days())
            })
        }),
    );
    let frame = frame.filter(&mask).map_err(value_error)?;
    match columns {
        Some(columns) if columns.is_empty() => Err(value_error("columns must not be empty")),
        Some(columns) => frame.select(columns).map_err(value_error),
        None => Ok(frame),
    }
}

#[pyfunction]
#[pyo3(signature = (lake, dsv, universe, venue, *, columns=None, start=None, end=None))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn read_dataset(
    py: Python<'_>,
    lake: String,
    dsv: String,
    universe: String,
    venue: String,
    columns: Option<Vec<String>>,
    start: Option<String>,
    end: Option<String>,
) -> PyResult<Py<PyAny>> {
    let batch = py.detach(|| {
        let (panel, universe) = load_inputs(&lake, &dsv, &universe, &venue)?;
        let ids = panel
            .grid
            .column("instrument_id")
            .map_err(value_error)?
            .str()
            .map_err(value_error)?;
        let members = universe
            .members()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let mask = BooleanChunked::from_iter_values(
            "universe".into(),
            ids.iter()
                .map(|id| id.is_some_and(|id| members.iter().any(|member| member == id))),
        );
        let frame = panel.grid.filter(&mask).map_err(value_error)?;
        dataframe_batch(&select(frame, columns, start, end)?)
    })?;
    table(py, batch.schema(), vec![batch])
}

#[pyfunction]
#[pyo3(signature = (lake, dsv, universe, venue, factor, *, availability_assumption="none"))]
pub(crate) fn read_factor(
    py: Python<'_>,
    lake: String,
    dsv: String,
    universe: String,
    venue: String,
    factor: PyRef<'_, FactorDeclaration>,
    availability_assumption: &str,
) -> PyResult<Py<PyAny>> {
    let factor = factor.factor.clone();
    let assumption: AvailabilityAssumption =
        availability_assumption.parse().map_err(value_error)?;
    let batch = py.detach(|| {
        let (panel, members) = load_inputs(&lake, &dsv, &universe, &venue)?;
        let cache = FactorCache::open(&lake).map_err(value_error)?;
        let values = cache
            .get_or_compute(
                &panel,
                members.members(),
                &universe,
                &dsv,
                &factor,
                assumption,
            )
            .map_err(value_error)?;
        let converted = dataframe_batch(&values)?;
        let batch =
            arrow_array::RecordBatch::try_new(values_schema().arrow, converted.columns().to_vec())
                .map_err(value_error)?;
        let key = FactorKey::for_tree(&factor, &dsv, &universe, assumption);
        let mut metadata = batch.schema().metadata().clone();
        metadata.insert("prajna.fv".into(), key.as_str().into());
        metadata.insert(
            "prajna.cache_hit".into(),
            (cache.compute_count() == 0).to_string(),
        );
        let schema = std::sync::Arc::new(batch.schema().as_ref().clone().with_metadata(metadata));
        batch.with_schema(schema).map_err(value_error)
    })?;
    table(py, batch.schema(), vec![batch])
}

#[pyfunction]
#[pyo3(signature = (lake, experiment, execution, run, *, table, result_level="summary"))]
pub(crate) fn read_run(
    py: Python<'_>,
    lake: String,
    experiment: String,
    execution: String,
    run: String,
    table: String,
    result_level: &str,
) -> PyResult<Py<PyAny>> {
    let level = match result_level {
        "summary" => ResultLevel::Summary,
        "standard" => ResultLevel::Standard,
        "full" => ResultLevel::Full,
        _ => {
            return Err(value_error(
                "result_level must be summary, standard or full",
            ));
        }
    };
    let (schema, batches) = py.detach(|| {
        read_run_table(lake, &experiment, &execution, &run, level, &table).map_err(|error| {
            match error {
                ResultReadError::NotSaved(message) => ResultNotSavedError::new_err(message),
                ResultReadError::NotProducedByEngine(message) => {
                    EngineNotProducedError::new_err(message)
                }
                ResultReadError::Storage(error) => value_error(error),
            }
        })
    })?;
    crate::arrow::table(py, schema, batches)
}
