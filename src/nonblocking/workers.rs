//! The threads that occupy themselves inside the FFI, and how they come and go.
//!
//! A command occupies the thread that enters the FFI until it is done -- there is no completion
//! callback in the engine to wrap a future around -- so every command here runs on one of these
//! rather than on the caller's runtime. That includes the open itself: storage startup,
//! recovery and the one-time index repair scan are the longest block this crate ever does, and
//! the first worker is what carries them.
//!
//! Which jobs these threads take, and whether there should be more or fewer of them, is
//! [`queue`](super::queue)'s to decide; this is what acts on it.

use super::queue::{Job, Pool};
use crate::{Error, OpenOptions, Result, client::Client as BlockingClient};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::oneshot;

/// The workers' queue, over the client they run commands on.
pub(super) type Workers = Pool<BlockingClient>;

/// Opens the engine on the first worker thread and starts the rest once it is up.
///
/// The open itself -- storage startup, recovery, the one-time index repair scan -- is the
/// longest block this crate ever does, which is why it happens on the worker rather than on
/// the runtime the caller is awaiting from.
pub(super) async fn open(path: PathBuf, options: Option<OpenOptions>) -> Result<Arc<Workers>> {
    // One worker per session the pool underneath may open: each worker holds exactly one
    // session for the length of a command, so this many run in parallel and none waits --
    // the pool's checkout is therefore uncontended when driven from here, though it still
    // does its job for the blocking API, whose caller threads are arbitrary and many.
    let limits = OpenOptions::resolve_concurrency(options.as_ref());
    let workers = Arc::new(Workers::new(limits));
    let (ready, opened) = oneshot::channel();
    // The open's span would otherwise end at the thread boundary and everything the engine
    // logs while starting up would dangle outside it.
    let span = tracing::Span::current();

    let first = Arc::clone(&workers);
    std::thread::Builder::new()
        .name("embedded-mongodb".to_owned())
        .spawn(move || {
            let client = match span.in_scope(|| open_blocking(&path, options)) {
                Ok(client) => Arc::new(client),
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
            // This thread is the first worker; the floor's remaining workers are started
            // here, and any the pool grows to later are started by whoever queued the job
            // that called for them.
            first.worker_started();
            for _ in 1..limits.min() {
                start_worker(&first, Arc::clone(&client), first.worker_started());
            }
            first.opened(Arc::clone(&client));
            // A caller that dropped the opening future has no way left to reach the engine,
            // so the pool is abandoned on its behalf: the workers finish what is queued and
            // let go, and the last handle out closes what was just opened.
            if ready.send(Ok(())).is_err() {
                first.abandon();
                return;
            }
            serve(client, first);
        })
        .map_err(Error::EngineThread)?;

    opened.await.map_err(|_| Error::Closed)??;
    Ok(workers)
}

/// Starts one worker on a count the pool has already made. A pool short a worker still runs
/// every command, just with less parallelism -- not worth failing an engine that is already
/// open, nor a command that has already been queued.
///
/// The handle is cloned for the thread rather than moved, so that a failure leaves this holding
/// one: a close that began between the count and this failure has a `Stop` queued for a worker
/// that will now never take it, and answering it here is what keeps that close from waiting
/// forever. It is the one place a retire happens off a worker thread, and only when a thread
/// could not be started at all.
pub(super) fn start_worker(workers: &Arc<Workers>, client: Arc<BlockingClient>, index: u32) {
    let served = Arc::clone(workers);
    let serving = Arc::clone(&client);
    if let Err(error) = std::thread::Builder::new()
        .name(format!("embedded-mongodb-{index}"))
        .spawn(move || serve(serving, served))
    {
        tracing::warn!(
            target: "embedded_mongodb",
            error = %error,
            "an engine worker thread could not be started"
        );
        if let Some(Job::Stop(shutdown)) = workers.spawn_failed() {
            shutdown.retire(client);
        }
    }
}

fn serve(client: Arc<BlockingClient>, workers: Arc<Workers>) {
    // `None` retires this worker: it waited out the idle timeout with the pool able to spare
    // it, or every handle on the engine is gone and the queue has run dry. Either way its own
    // reference to the engine goes with it, and if it was the last one the engine closes.
    while let Some(job) = workers.next_job() {
        match job {
            // Caught so that a panicking command fails only that command -- its reply channel
            // drops, which the awaiting caller sees as an error -- rather than unwinding this
            // worker out of the pool, which would leave `close` one `retire` short and hang it
            // forever. The client is behind the FFI's own locks, so proceeding is sound.
            Job::Run(operation) => {
                let _ = catch_unwind(AssertUnwindSafe(|| operation(&client)));
            }
            Job::Stop(shutdown) => {
                shutdown.retire(client);
                return;
            }
        }
    }
}

fn open_blocking(path: &Path, options: Option<OpenOptions>) -> Result<BlockingClient> {
    match options {
        Some(options) => BlockingClient::with_options(path, options),
        None => BlockingClient::new(path),
    }
}
