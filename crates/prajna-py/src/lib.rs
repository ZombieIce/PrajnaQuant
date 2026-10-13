use pyo3::prelude::*;
use std::time::Duration;

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
    Ok(())
}
