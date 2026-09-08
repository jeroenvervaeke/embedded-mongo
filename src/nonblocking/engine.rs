//! The handle every async call reaches the engine through.
//!
//! A command occupies the thread that enters the FFI until it is done -- there is no completion
//! callback in the engine to wrap a future around. So the async layer keeps a pool of dedicated
//! threads to do the occupying, one per session the engine will run a command on: the sessions
//! bound how many commands run in parallel, so a thread per session is exactly enough to
//! saturate the engine and never enough to queue inside the FFI. Callers park on a oneshot
//! instead, which is what makes an `await` here cost a task and not a runtime thread.
//!
//! How many threads that is follows the same [`Concurrency`](crate::Concurrency) the session
//! pool follows, elastic pool included. [`queue`](super::queue) decides it and
//! [`workers`](super::workers) acts on it; what is here is the handle, and turning one call
//! into one job.

use super::queue::{Job, Spawn};
use super::workers::{self, Workers, start_worker};
use crate::{Error, OpenOptions, Result, client::Client as BlockingClient, pool::CommandSlot};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::oneshot;

/// A handle on the pool: queueing a job is all it takes to run a command, so everything that
/// must outlive a borrow of [`Client`](super::Client) -- a cursor with batches left to fetch --
/// carries a clone of this rather than a lifetime.
#[derive(Clone)]
pub(super) struct Engine {
    handle: Arc<Handle>,
}

/// The last of these to drop is what tells the workers nobody is left to send them anything.
/// It is a type of its own rather than a `Drop` on [`Engine`] because `Engine` is cloned: the
/// engine is abandoned when the last *handle* goes, not the first.
struct Handle {
    workers: Arc<Workers>,
}

impl Engine {
    /// Opens the engine, and answers a handle on the workers now serving it. Everything the
    /// open does -- the thread that carries it, the floor of workers started behind it -- is in
    /// [`workers::open`](super::workers::open); this is only what wraps the result.
    pub(super) async fn open(path: PathBuf, options: Option<OpenOptions>) -> Result<Self> {
        let workers = workers::open(path, options).await?;
        Ok(Self {
            handle: Arc::new(Handle { workers }),
        })
    }

    /// Runs `operation` on a worker thread and hands its answer back across an await.
    ///
    /// The caller's span travels with the closure, so a command dispatched here reports under
    /// the same trace as one run on the blocking client directly.
    pub(super) async fn run<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&BlockingClient) -> Result<T> + Send + 'static,
    {
        let (reply, response) = oneshot::channel();
        let span = tracing::Span::current();
        self.dispatch(Job::Run(Box::new(move |client| {
            let result = span.in_scope(|| operation(client));
            // A caller that stopped awaiting is the only way this fails, and it is the caller's
            // answer to refuse.
            let _ = reply.send(result);
        })))?;
        response.await.map_err(|_| Error::Closed)?
    }

    /// [`Engine::run`] for a command the caller may stop waiting for.
    ///
    /// Dropping the returned future interrupts the command rather than leaving it to run to
    /// completion holding a session -- which is what makes a `tokio::time::timeout` or a losing
    /// `select!` branch actually give the session back. A command still queued when the future
    /// is dropped is never run at all.
    ///
    /// The interrupt cannot land on the wrong command: [`CommandSlot`] arms and disarms while
    /// the session is still checked out, so a cancel arriving after the command finished finds
    /// nothing to stop.
    pub(super) async fn run_cancellable<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&BlockingClient, &CommandSlot) -> Option<Result<T>> + Send + 'static,
    {
        let (reply, response) = oneshot::channel();
        let span = tracing::Span::current();
        let slot = Arc::new(CommandSlot::new());
        let dispatched = Arc::clone(&slot);
        self.dispatch(Job::Run(Box::new(move |client| {
            // `None` is a command that was cancelled before it started, so there is no answer
            // to send and nobody waiting for one.
            if let Some(result) = span.in_scope(|| operation(client, &dispatched)) {
                let _ = reply.send(result);
            }
        })))?;

        // Cancels on every exit, including a normal one -- by then the slot has been disarmed
        // by the worker, so the cancel finds nothing running and does nothing. Cheaper than
        // tracking which exit this was.
        let _interrupt = CancelOnDrop(&slot);
        response.await.map_err(|_| Error::Closed)?
    }

    /// [`Engine::run`] for work whose outcome nobody is left to hear -- a dropped cursor's
    /// `killCursors`. Nothing to await, so nothing for a `Drop` implementation to block on.
    pub(super) fn run_detached(&self, operation: impl FnOnce(&BlockingClient) + Send + 'static) {
        let _ = self.dispatch(Job::Run(Box::new(operation)));
    }

    /// Retires every worker and closes the engine behind them, reporting what the close said.
    ///
    /// One `Stop` per worker: each retires the worker that receives it, and the last worker out
    /// is the one with no peer left holding the engine, so it is the one that can close it --
    /// see [`Shutdown`](super::shutdown::Shutdown). Jobs queued after those stops are refused
    /// with [`Error::Closed`], exactly as commands after a blocking
    /// [`close`](crate::blocking::Client::close) are.
    pub(super) async fn close(&self) -> Result<()> {
        let (reply, closed) = oneshot::channel();
        self.handle.workers.begin_close(reply)?;
        closed.await.map_err(|_| Error::Closed)?
    }

    /// Queues one job, and starts the worker the pool asked for if it was short one.
    ///
    /// Starting the thread on the caller's task rather than on a worker is deliberate: the
    /// reason the pool is short a worker is that every one of them is inside the FFI, so there
    /// is nobody else to do it and waiting for one would give up exactly the parallelism the
    /// new worker is being started for.
    fn dispatch(&self, job: Job<BlockingClient>) -> Result<()> {
        if let Some(Spawn { client, index }) = self.handle.workers.dispatch(job)? {
            start_worker(&self.handle.workers, client, index);
        }
        Ok(())
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.workers.abandon();
    }
}

/// Interrupts a dispatched command when the future waiting for it goes away.
struct CancelOnDrop<'slot>(&'slot CommandSlot);

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
