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

use super::queue::Pool;
use super::state::Job;
use crate::{Error, OpenOptions, Result, client::Client as BlockingClient};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::oneshot;

/// The workers' queue, over the client they run commands on.
pub(super) type Workers = Pool<BlockingClient>;

/// Starts the engine opening on its first worker thread, and answers the pool the workers are
/// already serving along with the channel the open reports on.
///
/// Split from the awaiting so that the caller can hold the pool *before* it waits: an open
/// whose future is dropped has to abandon what it started, and only something already holding
/// the pool can do that on the way out. See [`Engine::open`](super::engine::Engine::open).
///
/// The open itself -- storage startup, recovery, the one-time index repair scan -- is the
/// longest block this crate ever does, which is why it happens on the worker rather than on the
/// runtime the caller is awaiting from.
pub(super) fn start(
    path: PathBuf,
    options: Option<OpenOptions>,
) -> Result<(Arc<Workers>, oneshot::Receiver<Result<()>>)> {
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
                    // Logged as well as sent: a caller that stopped awaiting -- a `timeout`
                    // around the open -- is the one case where the reason an open failed would
                    // otherwise go nowhere at all.
                    tracing::warn!(
                        target: "embedded_mongodb",
                        error = %error,
                        "the engine could not be opened"
                    );
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
            // Nothing is decided by whether this arrives. A caller that stopped awaiting has
            // already dropped its handle on the pool, which abandons it; this thread then finds
            // the queue dry and lets go like any other worker. Answering the send itself would
            // miss the case that matters -- a future dropped just *after* a successful send --
            // and leave the engine open with nobody able to reach it.
            let _ = ready.send(Ok(()));
            serve(client, first);
        })
        .map_err(Error::EngineThread)?;

    Ok((workers, opened))
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
        if let Some(shutdown) = workers.spawn_failed() {
            shutdown.retire(client);
        }
    }
}

fn serve(client: Arc<BlockingClient>, workers: Arc<Workers>) {
    workers.worker_ready();
    // Held through the pool, which takes it back under the same lock that stops counting this
    // worker: an engine handle let go a moment later would make the close rendezvous's claim
    // -- that the retiring workers hold every last reference -- briefly false.
    let mut held = Some(client);
    // `None` retires this worker: it waited out the idle timeout with the pool able to spare
    // it, or every handle on the engine is gone and the queue has run dry. Either way its
    // reference to the engine has gone with it, and if it was the last the engine closes.
    while let Some(job) = workers.next_job(&mut held) {
        let client = held
            .as_ref()
            .expect("a worker holds its handle until the job it is answered is `None`");
        match job {
            // Caught so that a panicking command fails only that command -- its reply channel
            // drops, which the awaiting caller sees as an error -- rather than unwinding this
            // worker out of the pool, which would leave `close` one `retire` short and hang it
            // forever. The client is behind the FFI's own locks, so proceeding is sound.
            Job::Run(operation) => {
                if catch_unwind(AssertUnwindSafe(|| operation(client))).is_err() {
                    // The caller sees only `Closed`, which it also sees for a real close, so
                    // without this there is nothing anywhere to tell the two apart.
                    tracing::error!(
                        target: "embedded_mongodb",
                        "a command panicked; its caller will see the engine as closed"
                    );
                }
            }
            Job::Stop(shutdown) => {
                // Taken rather than cloned: the rendezvous counts handles, and the one this
                // worker was holding is the one it deposits.
                if let Some(client) = held.take() {
                    shutdown.retire(client);
                }
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
