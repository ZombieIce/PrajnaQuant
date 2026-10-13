use pyo3::prelude::*;
use std::time::Duration;

mod arrow;
mod factor;
mod read;

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
