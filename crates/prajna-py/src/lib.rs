use prajna_experiment::{Experiment, ExperimentError, ResultLevel, WorkerCommand};
use pyo3::exceptions::{PyOSError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use std::time::Duration;
use std::{path::PathBuf, sync::Arc};

fn python_error(error: ExperimentError) -> PyErr {
    match error {
        ExperimentError::Json(_)
        | ExperimentError::Manifest(_)
        | ExperimentError::Universe(_)
        | ExperimentError::InvalidDefinition(_)
        | ExperimentError::Canonical(_) => PyValueError::new_err(error.to_string()),
        ExperimentError::Io(_) => PyOSError::new_err(error.to_string()),
        _ => PyRuntimeError::new_err(error.to_string()),
    }
}

#[pyfunction]
fn _describe_experiment(py: Python<'_>, definition: String, lake: PathBuf) -> PyResult<String> {
    py.detach(|| {
        let experiment = Experiment::parse(&definition, &lake).map_err(python_error)?;
        let canonical = String::from_utf8(experiment.canonical_json().map_err(python_error)?)
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        let runs = experiment.run_definitions(&lake).map_err(python_error)?;
        Ok(serde_json::json!({
            "experiment_id": experiment.id().map_err(python_error)?,
            "canonical_json": canonical,
            "runs": runs,
        })
        .to_string())
    })
}

#[pyfunction]
#[pyo3(signature = (definition, lake, repository, level, threads=None, backend_python=None, backend_worker=None))]
#[allow(clippy::too_many_arguments)]
fn _run_experiment(
    py: Python<'_>,
    definition: String,
    lake: PathBuf,
    repository: PathBuf,
    level: &str,
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
    let worker = match (backend_python, backend_worker) {
        (Some(python), Some(worker)) => Some(WorkerCommand::new(python, worker)),
        (None, None) => None,
        _ => {
            return Err(PyValueError::new_err(
                "backend_python and backend_worker must be supplied together",
            ));
        }
    };
    py.detach(|| {
        let mut experiment = Experiment::parse(&definition, &lake).map_err(python_error)?;
        if let Some(worker) = worker {
            experiment = experiment
                .with_backend_factory(Arc::new(worker))
                .map_err(python_error)?;
        }
        experiment.expand(&lake).map_err(python_error)?;
        let stored = experiment
            .execute_and_store(&lake, &repository, level, threads)
            .map_err(python_error)?;
        Ok(serde_json::json!({
            "manifest": stored.manifest,
            "path": stored.path,
            "statistics": stored.statistics,
            "action": stored.action,
        })
        .to_string())
    })
}

mod arrow;
mod execution;
mod factor;
mod read;
mod strategy;

#[pyfunction]
#[pyo3(signature = (duration_ms = 0))]
fn smoke_test(py: Python<'_>, duration_ms: u64) -> &'static str {
    py.detach(|| {
        std::thread::sleep(Duration::from_millis(duration_ms));
        "prajna-rust-ok"
    })
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add_function(wrap_pyfunction!(smoke_test, module)?)?;
    module.add_function(wrap_pyfunction!(_describe_experiment, module)?)?;
    module.add_function(wrap_pyfunction!(_run_experiment, module)?)?;
    strategy::register(module)?;
    execution::register(module)?;
    module.add_function(wrap_pyfunction!(read::read_dataset, module)?)?;
    module.add_function(wrap_pyfunction!(read::read_factor, module)?)?;
    module.add_class::<factor::FactorDeclaration>()?;
    module.add_function(wrap_pyfunction!(read::read_run, module)?)?;
    module.add(
        "ResultNotSavedError",
        module.py().get_type::<read::ResultNotSavedError>(),
    )?;
    module.add(
        "EngineNotProducedError",
        module.py().get_type::<read::EngineNotProducedError>(),
    )?;
    Ok(())
}
