//! The async engine behind `pymongo_embedded.AsyncMongoClient`.
//!
//! The synchronous [`NativeClient`](crate::NativeClient) releases the interpreter and blocks a
//! thread for the length of a command, which is right for a thread-per-request caller and wrong
//! for an event loop: the loop thread is the one it would block. This one hands each command to
//! the crate's async API, whose workers do the blocking, so awaiting costs the loop a parked
//! task and nothing else.
//!
//! It also buys something the synchronous binding cannot have at all. Commands here cancel on
//! drop -- dropping the future interrupts the command in the engine and returns its session
//! rather than abandoning the answer -- and pyo3's coroutines drop the Rust future when the
//! Python task is cancelled. So `asyncio.wait_for` around a command stops the engine working,
//! not merely the waiting for it.

use crate::wait::block_on;
use crate::wire;
use embedded_mongodb::Client;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use tokio::sync::RwLock;

/// The engine behind every connection an async PyMongo pool hands out.
///
/// `tokio`'s lock rather than the standard library's, and not only because a lock that parks
/// the thread would park the event loop with it: a command holds the guard across an await,
/// `std::sync::RwLockReadGuard` is `!Send`, and pyo3 requires a coroutine's future to be `Send`
/// -- so the standard library's lock would not compile here at all. The shape is otherwise what
/// the synchronous binding settled on: a read guard per command so they run in parallel, the
/// write guard only to close.
///
/// Poison does not arise: a `tokio::sync::RwLock` has no poisoning, so unlike its synchronous
/// counterpart there is nothing here to recover from.
#[pyclass]
pub(crate) struct AsyncNativeClient {
    inner: RwLock<Option<Client>>,
}

#[pymethods]
impl AsyncNativeClient {
    /// Opens the directory, synchronously, because `AsyncMongoClient(...)` is a constructor in
    /// PyMongo's API rather than something to await.
    ///
    /// Blocking here is the honest thing rather than a shortcut: the open does storage
    /// recovery and the one-time index repair scan, which is the longest block this library
    /// ever does, and there is no version of it that an event loop should be running. The
    /// interpreter is released for the whole of it, so other threads carry on.
    #[new]
    fn new(py: Python<'_>, path: &str) -> PyResult<Self> {
        let client = py
            .detach(|| block_on(Client::new(path)))
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        Ok(Self {
            inner: RwLock::new(Some(client)),
        })
    }

    /// Runs one command and answers the reply.
    ///
    /// Cancellable, and that is the point of the class: dropping the coroutine -- which is what
    /// cancelling the awaiting task does -- drops the command's future, and the engine stops.
    /// Both guards go with it, so a cancelled command holds nothing.
    async fn round_trip(&self, message: Vec<u8>) -> PyResult<(i32, bool, Vec<u8>)> {
        let request = wire::parse(&message).map_err(PyValueError::new_err)?;
        let guard = self.inner.read().await;
        let client = guard
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("embedded MongoDB client is closed"))?;
        let response = client
            .run_command_bytes(&request.database, request.command)
            .await
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        Ok((request.request_id, request.more_to_come, response))
    }

    /// Closes the engine, waiting for the commands other tasks are still running.
    async fn close(&self) -> PyResult<()> {
        self.shut_down().await.map_err(PyRuntimeError::new_err)
    }

    /// The same close, for the one caller that has no loop to await on: `AsyncMongoClient`
    /// undoing its own open when the rest of its constructor fails. Leaving the engine open
    /// there would cost the whole process its one runtime, and every later client would be
    /// refused -- so the constructor has to finish the job synchronously, as it started it.
    fn close_blocking(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| block_on(self.shut_down()))
            .map_err(PyRuntimeError::new_err)
    }
}

impl AsyncNativeClient {
    /// The write guard is a temporary of the first statement, so the engine's own shutdown runs
    /// with the lock already released -- and a command arriving in between finds `None` and is
    /// told the client is closed, which by then it is.
    async fn shut_down(&self) -> Result<(), String> {
        let closing = self.inner.write().await.take();
        let Some(client) = closing else { return Ok(()) };
        client.close().await.map_err(|error| error.to_string())
    }
}
