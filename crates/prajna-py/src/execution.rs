use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Instant};

use prajna_experiment::{Experiment, PythonProvenance, ResultLevel, WorkerCommand};
use prajna_research::StaticUniverse;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use crate::{python_error, strategy::generate_decisions};

#[pyfunction]
#[pyo3(signature = (definition, lake, repository, level, strategy_json, decide, provenance_json, threads=None, backend_python=None, backend_worker=None))]
#[allow(clippy::too_many_arguments)]
fn _run_python_experiment(
    py: Python<'_>,
    definition: String,
    lake: PathBuf,
    repository: PathBuf,
    level: &str,
    strategy_json: &str,
    decide: Py<PyAny>,
    provenance_json: &str,
    threads: Option<usize>,
    backend_python: Option<PathBuf>,
    backend_worker: Option<PathBuf>,
) -> PyResult<String> {
    let level = match level {
        "summary" => ResultLevel::Summary,
        "standard" => ResultLevel::Standard,
        "full" => ResultLevel::Full,
        _ => {
            return Err(PyValueError::new_err(
                "level must be summary, standard or full",
            ));
        }
    };
    let threads = threads.unwrap_or_else(rayon::current_num_threads);
    if threads == 0 {
        return Err(PyValueError::new_err("threads must be greater than zero"));
    }
    crate::strategy::validate_declaration(strategy_json)?;
    let mut experiment = Experiment::parse(&definition, &lake).map_err(python_error)?;
    match (backend_python, backend_worker) {
        (Some(python), Some(worker)) => {
            experiment = experiment
                .with_backend_factory(Arc::new(WorkerCommand::new(python, worker)))
                .map_err(python_error)?;
        }
        (None, None) => {}
        _ => {
            return Err(PyValueError::new_err(
                "backend_python and backend_worker must be supplied together",
            ));
        }
    }
    let provenance: PythonProvenance = serde_json::from_str(provenance_json)
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    let members = if experiment.pit_context().is_some() {
        Vec::new()
    } else {
        StaticUniverse::load(&lake, experiment.universe())
            .map_err(|error| PyValueError::new_err(error.to_string()))?
            .members()
            .iter()
            .map(ToString::to_string)
            .collect()
    };
    let definition: serde_json::Value = serde_json::from_str(&definition)
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    // Python strategies are single-Venue; Experiment::parse rejects a multi-Venue set.
    let venue = experiment
        .single_venue()
        .ok_or_else(|| PyValueError::new_err("Python strategies require exactly one Venue"))?
        .as_str()
        .to_owned();
    let mut prepared = BTreeMap::new();
    let mut asof_view_ms = Vec::new();
    // Stage 1: no Rayon callbacks, each canonical/deduplicated Run serially under GIL.
    let stage1_started = Instant::now();
    for (run_id, params) in experiment.python_parameters(&lake).map_err(python_error)? {
        if params.get("seed").is_some() {
            return Err(PyValueError::new_err(
                "Python strategies do not accept seed",
            ));
        }
        let decisions = generate_decisions(
            py,
            lake.to_str()
                .ok_or_else(|| PyValueError::new_err("lake path must be UTF-8"))?,
            experiment.dsv(),
            &venue,
            members.clone(),
            strategy_json,
            decide.clone_ref(py),
            &params.to_string(),
            definition["availability_assumption"]
                .as_str()
                .unwrap_or("none"),
            &mut asof_view_ms,
            experiment.pit_context(),
        )
        .and_then(|decisions| {
            decisions
                .into_iter()
                .map(|decision| decision.into_rust())
                .collect::<PyResult<Vec<_>>>()
        })
        .map_err(|error| error.to_string());
        prepared.insert(run_id, decisions);
    }
    let stage1_ms = stage1_started.elapsed().as_secs_f64() * 1000.0;
    // Stage 2: only owned Rust data crosses detach. Existing Rayon / Accurate process pool.
    let stage2_started = Instant::now();
    let stored = py.detach(|| {
        let stored = experiment
            .execute_python_and_store(&lake, &repository, level, threads, &prepared, provenance)
            .map_err(python_error)?;
        Ok::<_, PyErr>(stored)
    })?;
    let stage2_ms = stage2_started.elapsed().as_secs_f64() * 1000.0;
    let mut statistics = serde_json::to_value(stored.statistics)
        .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
    statistics["python"] = serde_json::json!({
        "stage1_ms": stage1_ms,
        "stage2_ms": stage2_ms,
        "asof_view_ms": asof_view_ms,
    });
    Ok(serde_json::json!({"manifest": stored.manifest, "path": stored.path, "statistics": statistics, "action": stored.action}).to_string())
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(_run_python_experiment, module)?)
}
