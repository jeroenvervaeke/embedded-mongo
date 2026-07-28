mod wire;

use std::sync::Mutex;

use embedded_mongodb_sys::Client;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyModule};

#[pyclass]
struct NativeClient {
    inner: Mutex<Option<Client>>,
}

#[pymethods]
impl NativeClient {
    #[new]
    fn new(path: &str) -> PyResult<Self> {
        Ok(Self {
            inner: Mutex::new(Some(
                Client::open(path).map_err(|error| PyRuntimeError::new_err(error.to_string()))?,
            )),
        })
    }

    fn round_trip<'py>(
        &self,
        py: Python<'py>,
        message: &Bound<'_, PyBytes>,
    ) -> PyResult<(i32, bool, Bound<'py, PyBytes>)> {
        let request = wire::parse(message.as_bytes()).map_err(PyValueError::new_err)?;
        let response = py
            .detach(|| {
                let guard = self
                    .inner
                    .lock()
                    .map_err(|_| "embedded MongoDB client lock is poisoned".to_owned())?;
                guard
                    .as_ref()
                    .ok_or_else(|| "embedded MongoDB client is closed".to_owned())?
                    .run_command(&request.database, &request.command)
                    .map_err(|error| error.to_string())
            })
            .map_err(PyRuntimeError::new_err)?;
        Ok((
            request.request_id,
            request.more_to_come,
            PyBytes::new(py, &response),
        ))
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let client = self
            .inner
            .lock()
            .map_err(|_| PyRuntimeError::new_err("embedded MongoDB client lock is poisoned"))?
            .take();
        if let Some(client) = client {
            py.detach(move || client.close())
                .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        }
        Ok(())
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<NativeClient>()
}
