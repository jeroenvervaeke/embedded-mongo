mod wire;

use std::sync::Mutex;

use embedded_mongodb::Client;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyModule};

#[pyclass]
struct NativeClient {
    inner: Mutex<Option<Client>>,
}

#[pymethods]
impl NativeClient {
    /// Opens the directory, which also runs the one-time index repair pass over a directory an
    /// older build damaged.
    ///
    /// Detached from the interpreter for the same reason `round_trip` and `close` are: the
    /// engine's own startup already blocks, and a directory that has not been checked before
    /// adds a full scan of every collection in it to that. Holding the GIL across it would
    /// stop every other Python thread for the length of the scan.
    #[new]
    fn new(py: Python<'_>, path: &str) -> PyResult<Self> {
        let client = py
            .detach(|| Client::new(path))
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        Ok(Self {
            inner: Mutex::new(Some(client)),
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
                    .run_command_bytes(&request.database, &request.command)
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
