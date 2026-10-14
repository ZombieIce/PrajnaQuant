use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};

use arrow_array::{
    Array, Date32Array, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray,
    UInt32Array,
};
use arrow_schema::{DataType, Field, Schema};
use arrow_select::{concat::concat_batches, take::take};
use chrono::NaiveDate;
use prajna_data::{Manifest, read_manifest, read_parquet};
use prajna_domain::{InstrumentId, VenueId};
use prajna_research::{
    PanelSession, compute,
    factor::{AvailabilityAssumption, Factor, values_schema},
    load_panel,
};
use pyo3::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use crate::arrow::value_error as invalid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    strategy_id: String,
    version: String,
    bar_columns: Vec<String>,
    factors: Vec<FactorDeclaration>,
    lookback_sessions: usize,
    rebalance_every: usize,
    capability: String,
}

impl Declaration {
    fn validate(&self) -> PyResult<()> {
        for (name, value) in [
            ("strategy_id", &self.strategy_id),
            ("version", &self.version),
        ] {
            if value.is_empty()
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            {
                return Err(invalid(format!(
                    "{name} must be a nonempty identity component"
                )));
            }
        }
        if self.capability != "Vectorizable" {
            return Err(invalid("Python strategies require capability Vectorizable"));
        }
        if self.lookback_sessions == 0 || self.rebalance_every == 0 {
            return Err(invalid(
                "lookback_sessions and rebalance_every must be at least 1",
            ));
        }
        let mut columns = BTreeSet::new();
        for name in &self.bar_columns {
            if !["open", "high", "low", "close", "volume", "amount"].contains(&name.as_str())
                || !columns.insert(name)
            {
                return Err(invalid(format!("invalid or duplicate Bar column {name}")));
            }
        }
        let mut factors = BTreeSet::new();
        for factor in &self.factors {
            if factor.name.is_empty() || !factors.insert(&factor.name) {
                return Err(invalid("Factor names must be nonempty and unique"));
            }
            factor.definition()?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FactorDeclaration {
    name: String,
    kind: String,
    params: Value,
}

impl FactorDeclaration {
    fn definition(&self) -> PyResult<Factor> {
        crate::factor::definition(
            &self.kind,
            &serde_json::to_string(&self.params).map_err(invalid)?,
        )
    }
}

#[pyclass(name = "Decision", module = "prajna", frozen, skip_from_py_object)]
#[derive(Clone)]
pub(crate) struct PyDecision {
    targets: BTreeMap<String, f64>,
    ranked: Vec<(String, f64, u32)>,
    session_date: Option<String>,
    decision_session: Option<PanelSession>,
}

impl PyDecision {
    pub(crate) fn into_rust(self) -> PyResult<prajna_research::strategy::Decision> {
        Ok(prajna_research::strategy::Decision {
            decision_session: self
                .decision_session
                .ok_or_else(|| invalid("missing Decision Session"))?,
            targets: self
                .targets
                .into_iter()
                .map(|(id, weight)| Ok((id.parse().map_err(invalid)?, weight)))
                .collect::<PyResult<_>>()?,
            ranked: self
                .ranked
                .into_iter()
                .map(|(id, score, rank)| Ok((id.parse().map_err(invalid)?, score, rank)))
                .collect::<PyResult<_>>()?,
        })
    }
    fn validate(&self, universe: &BTreeSet<String>, session: &PanelSession) -> PyResult<()> {
        let fail = |message: String| {
            invalid(format!(
                "invalid Decision on {}: {message}",
                session.session_date
            ))
        };
        let mut sum = 0.0;
        for (id, weight) in &self.targets {
            if !universe.contains(id) {
                return Err(fail(format!("target {id} is outside the Session Universe")));
            }
            if !weight.is_finite() {
                return Err(fail(format!("weight for {id} must be finite")));
            }
            if *weight < 0.0 {
                return Err(fail(format!("weight for {id} must be nonnegative")));
            }
            sum += weight;
        }
        if sum > 1.0 {
            return Err(fail(format!("weight sum {sum} exceeds 1")));
        }
        let mut ids = BTreeSet::new();
        let mut ranks = BTreeSet::new();
        for (id, score, rank) in &self.ranked {
            if !universe.contains(id) {
                return Err(fail(format!("ranked {id} is outside the Session Universe")));
            }
            if !score.is_finite() || *rank == 0 || !ids.insert(id) || !ranks.insert(rank) {
                return Err(fail(
                    "ranking requires finite scores, unique members and unique positive ranks"
                        .into(),
                ));
            }
        }
        Ok(())
    }
}

#[pymethods]
impl PyDecision {
    #[new]
    #[pyo3(signature = (targets, ranked = None))]
    fn new(targets: BTreeMap<String, f64>, ranked: Option<Vec<(String, f64, u32)>>) -> Self {
        Self {
            targets,
            ranked: ranked.unwrap_or_default(),
            session_date: None,
            decision_session: None,
        }
    }

    #[getter]
    fn targets(&self) -> BTreeMap<String, f64> {
        self.targets.clone()
    }

    #[getter]
    fn ranked(&self) -> Vec<(String, f64, u32)> {
        self.ranked.clone()
    }

    #[getter]
    fn session_date(&self) -> Option<String> {
        self.session_date.clone()
    }
}

#[pyclass(name = "AsOfView", module = "prajna", frozen)]
struct AsOfView {
    #[pyo3(get)]
    session_date: String,
    #[pyo3(get)]
    ts_close: i64,
    #[pyo3(get)]
    universe: Vec<String>,
    #[pyo3(get)]
    sessions: Py<PyAny>,
    #[pyo3(get)]
    bars: Py<PyAny>,
    #[pyo3(get)]
    execution_status: Py<PyAny>,
    factors: BTreeMap<String, Py<PyAny>>,
    #[pyo3(get)]
    availability_assumption: String,
}

#[pymethods]
impl AsOfView {
    #[getter]
    fn factors(&self, py: Python<'_>) -> BTreeMap<String, Py<PyAny>> {
        self.factors
            .iter()
            .map(|(name, table)| (name.clone(), table.clone_ref(py)))
            .collect()
    }
}

fn factor_batch(values: &polars::prelude::DataFrame) -> PyResult<RecordBatch> {
    let batch = crate::arrow::dataframe_batch(values)?;
    RecordBatch::try_new(values_schema().arrow, batch.columns().to_vec()).map_err(invalid)
}

fn usable_factor(batch: &RecordBatch, session: &PanelSession) -> PyResult<bool> {
    let date = date32(session.session_date)?;
    let dates = column::<Date32Array>(batch, "session_date")?;
    let values = column::<Float64Array>(batch, "value")?;
    let times = column::<TimestampNanosecondArray>(batch, "available_at")?;
    let statuses = column::<StringArray>(batch, "status")?;
    Ok((0..batch.num_rows()).any(|row| {
        dates.value(row) == date
            && !values.is_null(row)
            && statuses.value(row) == "ok"
            && !times.is_null(row)
            && times.value(row) <= session.ts_close.as_unix_nanos()
    }))
}

fn export(py: Python<'_>, batch: RecordBatch) -> PyResult<Py<PyAny>> {
    crate::arrow::table(py, batch.schema(), vec![batch])
}

fn date32(date: NaiveDate) -> PyResult<i32> {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid epoch");
    i32::try_from((date - epoch).num_days()).map_err(invalid)
}

fn column<'a, T: Array + 'static>(batch: &'a RecordBatch, name: &str) -> PyResult<&'a T> {
    batch
        .column_by_name(name)
        .and_then(|array| array.as_any().downcast_ref::<T>())
        .ok_or_else(|| invalid(format!("missing or invalid column {name}")))
}

fn table(root: &Path, manifest: &Manifest, name: &str) -> PyResult<RecordBatch> {
    let descriptor = manifest
        .core
        .tables
        .iter()
        .find(|table| table.table == name)
        .ok_or_else(|| invalid(format!("DSV is missing {name} table")))?;
    if descriptor.schema_version != 1 {
        return Err(invalid(format!("unsupported {name} schema version")));
    }
    let hex = manifest
        .dsv
        .strip_prefix("dsv:sha256:")
        .ok_or_else(|| invalid("invalid DSV identity"))?;
    let batches = read_parquet(
        root.join("normalized")
            .join(name)
            .join(hex)
            .join("part-00000.parquet"),
        name,
        "1",
    )
    .map_err(invalid)?;
    let schema = batches
        .first()
        .ok_or_else(|| invalid(format!("no record batches in {name}")))?
        .schema();
    if schema.metadata().get("prajna.dsv") != Some(&manifest.dsv) {
        return Err(invalid(format!("{name} DSV metadata mismatch")));
    }
    concat_batches(&schema, &batches).map_err(invalid)
}

fn bounded(
    batch: &RecordBatch,
    members: &BTreeSet<String>,
    window: &[PanelSession],
    names: &[String],
    assumption: AvailabilityAssumption,
) -> PyResult<RecordBatch> {
    let start = date32(window[0].session_date)?;
    let last = window.last().expect("nonempty window");
    let end = date32(last.session_date)?;
    let cutoff = last.ts_close.as_unix_nanos();
    let ids = column::<StringArray>(batch, "instrument_id")?;
    let dates = column::<Date32Array>(batch, "session_date")?;
    let times = column::<TimestampNanosecondArray>(batch, "available_at")?;
    let indices = (0..batch.num_rows())
        .filter(|&row| {
            !ids.is_null(row)
                && !dates.is_null(row)
                && members.contains(ids.value(row))
                && dates.value(row) >= start
                && dates.value(row) <= end
                && if times.is_null(row) {
                    assumption == AvailabilityAssumption::TreatUnknownAsTsClose
                } else {
                    times.value(row) <= cutoff
                }
        })
        .map(|row| u32::try_from(row).map_err(invalid))
        .collect::<PyResult<Vec<_>>>()?;
    let indices = UInt32Array::from(indices);
    let fields = names
        .iter()
        .map(|name| {
            batch
                .schema()
                .field_with_name(name)
                .cloned()
                .map_err(invalid)
        })
        .collect::<PyResult<Vec<_>>>()?;
    // take compacts buffers before export: a slice would expose excluded rows via buffers().
    let arrays = names
        .iter()
        .map(|name| {
            take(
                batch
                    .column_by_name(name)
                    .ok_or_else(|| invalid(format!("missing {name}")))?,
                &indices,
                None,
            )
            .map_err(invalid)
        })
        .collect::<PyResult<Vec<_>>>()?;
    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).map_err(invalid)
}

fn session_batch(window: &[PanelSession]) -> PyResult<RecordBatch> {
    RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("session_date", DataType::Date32, false),
            Field::new(
                "ts_close",
                DataType::Timestamp(arrow_schema::TimeUnit::Nanosecond, Some("UTC".into())),
                false,
            ),
        ])),
        vec![
            Arc::new(Date32Array::from(
                window
                    .iter()
                    .map(|session| date32(session.session_date))
                    .collect::<PyResult<Vec<_>>>()?,
            )),
            Arc::new(
                TimestampNanosecondArray::from(
                    window
                        .iter()
                        .map(|session| session.ts_close.as_unix_nanos())
                        .collect::<Vec<_>>(),
                )
                .with_timezone("UTC"),
            ),
        ],
    )
    .map_err(invalid)
}

pub(crate) fn validate_declaration(strategy_json: &str) -> PyResult<()> {
    let declaration: Declaration = serde_json::from_str(strategy_json).map_err(invalid)?;
    declaration.validate()
}

#[pyfunction]
#[allow(clippy::too_many_arguments)]
pub(crate) fn _generate_decisions(
    py: Python<'_>,
    lake_root: &str,
    dsv: &str,
    venue: &str,
    members: Vec<String>,
    strategy_json: &str,
    decide: Py<PyAny>,
    params_json: &str,
    availability_assumption: &str,
) -> PyResult<Vec<PyDecision>> {
    let declaration: Declaration = serde_json::from_str(strategy_json).map_err(invalid)?;
    declaration.validate()?;
    let params: Value = serde_json::from_str(params_json).map_err(invalid)?;
    if !params.is_object() {
        return Err(invalid("strategy params must be a JSON object"));
    }
    let params_json = serde_json::to_string(&params).map_err(invalid)?;
    if !decide.bind(py).is_callable() {
        return Err(invalid("decide must be callable"));
    }
    let assumption = availability_assumption.parse().map_err(invalid)?;
    let venue = VenueId::new(venue).map_err(invalid)?;
    let panel = load_panel(lake_root, dsv, &venue).map_err(invalid)?;
    let universe = members.iter().cloned().collect::<BTreeSet<_>>();
    if universe.is_empty() || universe.len() != members.len() {
        return Err(invalid("Universe must contain unique members"));
    }
    for member in &universe {
        let id: InstrumentId = member.parse().map_err(invalid)?;
        if !panel.instruments.contains(&id) {
            return Err(invalid(format!(
                "Universe member {member} is absent from the Venue DSV"
            )));
        }
    }
    let root = Path::new(lake_root);
    let manifest = read_manifest(root, dsv).map_err(invalid)?;
    let bars = table(root, &manifest, "bars")?;
    let statuses = table(root, &manifest, "execution_status")?;
    let ids = universe
        .iter()
        .map(|member| member.parse::<InstrumentId>().map_err(invalid))
        .collect::<PyResult<Vec<_>>>()?;
    let factors = declaration
        .factors
        .iter()
        .map(|dependency| {
            let values =
                compute(&panel, &ids, &dependency.definition()?, assumption).map_err(invalid)?;
            Ok((dependency.name.clone(), factor_batch(&values)?))
        })
        .collect::<PyResult<BTreeMap<_, _>>>()?;
    let mut bar_names = vec![
        "instrument_id".into(),
        "session_date".into(),
        "available_at".into(),
    ];
    bar_names.extend(declaration.bar_columns);
    let status_names = [
        "instrument_id",
        "session_date",
        "trade_status",
        "is_tradable",
        "available_at",
        "source",
    ]
    .map(String::from);
    let factor_names = [
        "instrument_id",
        "session_date",
        "value",
        "available_at",
        "status",
    ]
    .map(String::from);
    let mut decisions: Vec<PyDecision> = Vec::new();
    let mut first_decision = None;
    for index in declaration.lookback_sessions - 1..panel.sessions.len() {
        let window = &panel.sessions[index + 1 - declaration.lookback_sessions..=index];
        let session = &panel.sessions[index];
        if first_decision.is_none() {
            let readiness = factors
                .values()
                .map(|values| usable_factor(values, session))
                .collect::<PyResult<Vec<_>>>()?;
            if readiness.iter().any(|ready| !ready) {
                continue;
            }
            first_decision = Some(index);
        }
        if (index - first_decision.expect("set above")) % declaration.rebalance_every != 0 {
            continue;
        }
        let factor_views = factors
            .iter()
            .map(|(name, batch)| {
                Ok((
                    name.clone(),
                    export(
                        py,
                        bounded(batch, &universe, window, &factor_names, assumption)?,
                    )?,
                ))
            })
            .collect::<PyResult<BTreeMap<_, _>>>()?;
        let view = AsOfView {
            session_date: session.session_date.to_string(),
            ts_close: session.ts_close.as_unix_nanos(),
            universe: universe.iter().cloned().collect(),
            sessions: export(py, session_batch(window)?)?,
            bars: export(
                py,
                bounded(&bars, &universe, window, &bar_names, assumption)?,
            )?,
            execution_status: export(
                py,
                bounded(&statuses, &universe, window, &status_names, assumption)?,
            )?,
            factors: factor_views,
            availability_assumption: assumption.to_string(),
        };
        let previous = decisions.last().cloned();
        let params = py.import("json")?.call_method1("loads", (&params_json,))?;
        let result = decide.bind(py).call1((view, params, previous))?;
        let mut decision = result.extract::<PyRef<'_, PyDecision>>()?.clone();
        decision.validate(&universe, session)?;
        decision.session_date = Some(session.session_date.to_string());
        decision.decision_session = Some(session.clone());
        decisions.push(decision);
    }
    Ok(decisions)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyDecision>()?;
    module.add_class::<AsOfView>()?;
    module.add_function(wrap_pyfunction!(_generate_decisions, module)?)?;
    Ok(())
}
