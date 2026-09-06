mod wire;

use std::sync::{PoisonError, RwLock};

use embedded_mongodb::blocking::Client;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyModule};

/// The engine behind every connection PyMongo's pool hands out.
///
/// One client, shared by all of them, because the parallelism lives a layer down: an
/// `embedded_mongodb::blocking::Client` is `Send + Sync` and runs commands on a pool of
/// sessions, so N callers reaching it at once is what it is built for. The lock here therefore
/// guards only whether the client is still open, and a read guard is enough to run a command --
/// a `Mutex` would make this binding the one place that serialised what everything below it
/// runs in parallel.
///
/// Poison is recovered from rather than reported, as `pool` and the Android registry do with
/// theirs. A panic cannot leave this inconsistent: readers never touch the `Option`, and the
/// one writer replaces it with `None`, which is exactly the closed state. Reporting instead
/// would brick the binding for the life of the process -- a poisoned lock refuses reads too --
/// over a failure that already reached Python once as an exception.
#[pyclass]
struct NativeClient {
    inner: RwLock<Option<Client>>,
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
            inner: RwLock::new(Some(client)),
        })
    }

    /// Runs one command and answers the reply.
    ///
    /// Takes the read guard, so commands from other Python threads run alongside this one; only
    /// [`close`](NativeClient::close) excludes them.
    fn round_trip<'py>(
        &self,
        py: Python<'py>,
        message: &Bound<'_, PyBytes>,
    ) -> PyResult<(i32, bool, Bound<'py, PyBytes>)> {
        let request = wire::parse(message.as_bytes()).map_err(PyValueError::new_err)?;
        let response = py
            .detach(|| {
                let guard = self.inner.read().unwrap_or_else(PoisonError::into_inner);
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

    /// Closes the engine, and is the one caller that excludes commands: the write guard waits
    /// for every command still running to finish before the client is taken out from under them.
    ///
    /// The wait is inside the detach, not around it. Taking the guard is itself blocking now,
    /// for as long as the slowest command another thread has in flight, and holding the GIL
    /// across that would stop the whole interpreter for the length of somebody else's query.
    /// Neither way round deadlocks -- a thread inside `round_trip` releases its read guard
    /// before it asks for the GIL back -- so what detaching first buys is only that every
    /// unrelated Python thread goes on running meanwhile. Which is the point.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            let closing = self
                .inner
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            let Some(client) = closing else { return Ok(()) };
            client.close().map_err(|error| error.to_string())
        })
        .map_err(PyRuntimeError::new_err)
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<NativeClient>()
}
