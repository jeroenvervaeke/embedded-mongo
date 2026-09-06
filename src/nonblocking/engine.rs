//! The worker pool that makes a synchronous engine safe to await.
//!
//! A command occupies the thread that enters the FFI until it is done -- there is no
//! completion callback in the engine to wrap a future around. So the async layer keeps a pool
//! of dedicated threads to do the occupying, sized one-to-one with the engine's command
//! strands: the strands bound how many commands the engine runs in parallel, so a thread per
//! strand is exactly enough to saturate the engine and never enough to queue inside the FFI.
//! Callers park on a oneshot instead, which is what makes an `await` here cost a task and not
//! a runtime thread.

use crate::{Error, OpenOptions, Result, client::Client as BlockingClient};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use tokio::sync::oneshot;

/// What travels from a task to a worker. `Run` carries the operation and its reply channel
/// inside one closure; `Stop` retires the worker that receives it, which is why
/// [`Engine::close`] sends exactly one per worker.
enum Job {
    Run(Box<dyn FnOnce(&BlockingClient) + Send>),
    Stop(Arc<Shutdown>),
}

/// A handle on the pool: sending on `jobs` is all it takes to run a command, so everything
/// that must outlive a borrow of [`Client`](super::Client) -- a cursor with batches left to
/// fetch -- carries a clone of this rather than a lifetime.
#[derive(Clone)]
pub(super) struct Engine {
    jobs: mpsc::Sender<Job>,
    workers: u32,
}

impl Engine {
    /// Opens the engine on the first worker thread and starts the rest once it is up.
    ///
    /// The open itself -- storage startup, recovery, the one-time index repair scan -- is the
    /// longest block this crate ever does, which is why it happens on the worker rather than
    /// on the runtime the caller is awaiting from.
    pub(super) async fn open(path: PathBuf, options: Option<OpenOptions>) -> Result<Self> {
        // One worker per session in the blocking client's pool: each worker holds exactly one
        // session for the length of a command, so this many run in parallel and none waits --
        // the pool's checkout is therefore uncontended when driven from here, though it still
        // does its job for the blocking API, whose caller threads are arbitrary and many.
        let workers = OpenOptions::strand_count(options.as_ref()).count();
        let (jobs, queue) = mpsc::channel();
        let queue = Arc::new(Mutex::new(queue));
        let (ready, opened) = oneshot::channel();
        // The open's span would otherwise end at the thread boundary and everything the
        // engine logs while starting up would dangle outside it.
        let span = tracing::Span::current();

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
                // This thread is the first worker; the rest are spawned here. Count the ones
                // that actually start, because `close` retires exactly this many -- a worker
                // that failed to spawn must not be one the shutdown waits for.
                let mut started = 1u32;
                for index in 1..workers {
                    let client = Arc::clone(&client);
                    let queue = Arc::clone(&queue);
                    match std::thread::Builder::new()
                        .name(format!("embedded-mongodb-{index}"))
                        .spawn(move || serve(client, queue))
                    {
                        Ok(_) => started += 1,
                        // A pool short a worker still runs every command, just with less
                        // parallelism -- not worth failing an engine that is already open.
                        Err(error) => tracing::warn!(
                            target: "embedded_mongodb",
                            error = %error,
                            "an engine worker thread could not be started"
                        ),
                    }
                }
                // Sent after the spawns so the count is the real one. A caller that dropped the
                // opening future has no way left to reach the engine: returning drops this
                // thread's handle, the helper threads find the job queue's sender gone and exit,
                // and the last handle out closes what was just opened.
                if ready.send(Ok(started)).is_err() {
                    return;
                }
                serve(client, queue);
            })
            .map_err(Error::EngineThread)?;

        let workers = opened.await.map_err(|_| Error::Closed)??;
        Ok(Self { jobs, workers })
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
        self.jobs
            .send(Job::Run(Box::new(move |client| {
                let result = span.in_scope(|| operation(client));
                // A caller that stopped awaiting is the only way this fails, and it is the
                // caller's answer to refuse.
                let _ = reply.send(result);
            })))
            .map_err(|_| Error::Closed)?;
        response.await.map_err(|_| Error::Closed)?
    }

    /// [`Engine::run`] for work whose outcome nobody is left to hear -- a dropped cursor's
    /// `killCursors`. Nothing to await, so nothing for a `Drop` implementation to block on.
    pub(super) fn run_detached(&self, operation: impl FnOnce(&BlockingClient) + Send + 'static) {
        let _ = self.jobs.send(Job::Run(Box::new(operation)));
    }

    /// Retires every worker and closes the engine behind them, reporting what the close said.
    ///
    /// One `Stop` per worker: each retires the worker that receives it, and the last worker
    /// out is the one with no peer left holding the engine, so it is the one that can close
    /// it -- see [`Shutdown`]. Jobs sent after these stops are never run; their senders find
    /// the queue gone and answer [`Error::Closed`], exactly as commands after a blocking
    /// [`close`](crate::blocking::Client::close) do.
    pub(super) async fn close(&self) -> Result<()> {
        let (reply, closed) = oneshot::channel();
        let shutdown = Arc::new(Shutdown::new(self.workers, reply));
        for _ in 0..self.workers {
            self.jobs
                .send(Job::Stop(Arc::clone(&shutdown)))
                .map_err(|_| Error::Closed)?;
        }
        closed.await.map_err(|_| Error::Closed)?
    }
}

/// The rendezvous that turns N retiring workers into one engine close.
///
/// Closing needs what no single worker has: certainty that every other worker has let go of
/// the engine. Each worker deposits its handle here before counting itself out, so the worker
/// whose decrement hits zero knows every handle is in the vault, drains it down to one, and
/// closes the engine through that -- on a worker thread, which is where blocking work lives.
struct Shutdown {
    remaining: AtomicUsize,
    handles: Mutex<Vec<Arc<BlockingClient>>>,
    /// Reached only by the single worker whose decrement hits zero, so it never contends. The
    /// `Mutex` is here only to get interior mutability for the `take` through the shared `Arc`,
    /// not to guard against a race.
    reply: Mutex<Option<oneshot::Sender<Result<()>>>>,
}

impl Shutdown {
    fn new(workers: u32, reply: oneshot::Sender<Result<()>>) -> Self {
        Self {
            remaining: AtomicUsize::new(workers as usize),
            handles: Mutex::new(Vec::with_capacity(workers as usize)),
            reply: Mutex::new(Some(reply)),
        }
    }

    /// Called once by every worker as it retires. The deposit has to precede the decrement:
    /// the last worker's claim to the engine is that the vault is complete, and a worker that
    /// counted out before depositing would leave that claim briefly false.
    fn retire(&self, client: Arc<BlockingClient>) {
        lock(&self.handles).push(client);
        if self.remaining.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }

        let mut handles = std::mem::take(&mut *lock(&self.handles));
        let last = handles.pop();
        // Every handle but the survivor goes first, so the `into_inner` below meets a count of
        // one.
        drop(handles);
        let result = match last.and_then(Arc::into_inner) {
            Some(client) => client.close(),
            // Unreachable while workers are the only holders of engine handles, which they
            // are; answered rather than unwrapped so a future holder is a wrong error, not
            // an abort.
            None => Err(Error::Closed),
        };
        if let Some(reply) = lock(&self.reply).take() {
            let _ = reply.send(result);
        }
    }
}

fn serve(client: Arc<BlockingClient>, queue: Arc<Mutex<mpsc::Receiver<Job>>>) {
    loop {
        // The lock is held only while waiting: a worker takes one job and releases the
        // receiver before running it, so the queue is shared and the commands are not.
        let job = lock(&queue).recv();
        match job {
            // Caught so that a panicking command fails only that command -- its reply channel
            // drops, which the awaiting caller sees as an error -- rather than unwinding this
            // worker out of the pool, which would leave `close` one `retire` short and hang it
            // forever. The client is behind the FFI's own locks, so proceeding is sound.
            Ok(Job::Run(operation)) => {
                let _ = catch_unwind(AssertUnwindSafe(|| operation(&client)));
            }
            Ok(Job::Stop(shutdown)) => {
                shutdown.retire(client);
                return;
            }
            // Every handle dropped without a close: the last worker's Arc goes with it, and
            // the native destructor closes the engine silently, as dropping the blocking
            // client does.
            Err(_) => return,
        }
    }
}

fn open_blocking(path: &Path, options: Option<OpenOptions>) -> Result<BlockingClient> {
    match options {
        Some(options) => BlockingClient::with_options(path, options),
        None => BlockingClient::new(path),
    }
}

/// A poisoned lock here means a worker panicked with the guard held; the data under every one
/// of these locks is still sound -- a queue, a vault of handles, a reply slot -- and refusing
/// it would strand every other worker, so the poison is shrugged off the way
/// `limits::process` does.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
