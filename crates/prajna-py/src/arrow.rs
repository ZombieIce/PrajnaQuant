use std::sync::Arc;

use arrow_array::{
    ArrayRef, BooleanArray, Date32Array, Float64Array, RecordBatch, RecordBatchIterator,
    StringArray, TimestampNanosecondArray, UInt32Array, ffi_stream::FFI_ArrowArrayStream,
};
use arrow_schema::{Field, Schema, SchemaRef};
use polars::prelude::{DataFrame, DataType};
use pyo3::{exceptions::PyValueError, prelude::*, types::PyCapsule};

pub(crate) fn value_error(error: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(error.to_string())
}

// Arrow owns the buffers and release callback; the capsule transfers ownership
// to the consumer without exposing raw pointers in the binding.
#[pyclass]
struct ArrowExport {
    schema: SchemaRef,
    batches: Vec<RecordBatch>,
}

#[pymethods]
impl ArrowExport {
    #[pyo3(signature = (requested_schema=None))]
    fn __arrow_c_stream__<'py>(
        &self,
        py: Python<'py>,
        requested_schema: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyCapsule>> {
        if requested_schema.is_some_and(|schema| !schema.is_none()) {
            return Err(value_error("requested Arrow schema is not supported"));
        }
        let reader = RecordBatchIterator::new(
            self.batches.clone().into_iter().map(Ok),
            self.schema.clone(),
        );
        PyCapsule::new_with_value(
            py,
            FFI_ArrowArrayStream::new(Box::new(reader)),
            c"arrow_array_stream",
        )
    }
}

pub(crate) fn table(
    py: Python<'_>,
    schema: SchemaRef,
    batches: Vec<RecordBatch>,
) -> PyResult<Py<PyAny>> {
    let export = Py::new(py, ArrowExport { schema, batches })?;
    Ok(py
        .import("pyarrow")?
        .call_method1("table", (export,))?
        .unbind())
}

pub(crate) fn dataframe_batch(frame: &DataFrame) -> PyResult<RecordBatch> {
    let mut fields = Vec::new();
    let mut arrays = Vec::<ArrayRef>::new();
    for column in frame.columns() {
        let array: ArrayRef = match column.dtype() {
            DataType::String => Arc::new(StringArray::from_iter(
                column.str().map_err(value_error)?.iter(),
            )),
            DataType::Boolean => Arc::new(BooleanArray::from_iter(
                column.bool().map_err(value_error)?.iter(),
            )),
            DataType::UInt32 => Arc::new(UInt32Array::from_iter(
                column.u32().map_err(value_error)?.iter(),
            )),
            DataType::Float64 => Arc::new(Float64Array::from_iter(
                column.f64().map_err(value_error)?.iter(),
            )),
            DataType::Date => Arc::new(Date32Array::from_iter(
                column.date().map_err(value_error)?.physical().iter(),
            )),
            DataType::Datetime(polars::prelude::TimeUnit::Nanoseconds, zone) => Arc::new(
                TimestampNanosecondArray::from_iter(
                    column.datetime().map_err(value_error)?.physical().iter(),
                )
                .with_timezone_opt(zone.as_ref().map(ToString::to_string)),
            ),
            dtype => {
                return Err(value_error(format!(
                    "unsupported Arrow column type {dtype}"
                )));
            }
        };
        fields.push(Field::new(
            column.name().as_str(),
            array.data_type().clone(),
            true,
        ));
        arrays.push(array);
    }
    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).map_err(value_error)
}
