//! Runs synchronous engine work on dedicated threads, keeping async runtimes unblocked.
//! The session and worker pools follow the same fixed or dynamic concurrency policy.

use super::shutdown::{Shutdown, lock};
use crate::{
    Concurrency, Error, OpenOptions, Result, client::Client as BlockingClient, pool::CommandSlot,
};
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::Instant;
use tokio::sync::oneshot;

enum Job {
    Run(Box<dyn FnOnce(&BlockingClient) + Send>),
    /// An idle timeout, distinct from the explicit-close rendezvous.
    Retire,
    Stop(Arc<Shutdown>),
}

#[derive(Clone)]
pub(super) struct Engine {
    handle: Arc<Handle>,
}

/// Workers own the pool, but only callers own this handle. Dropping the last caller wakes
/// idle workers to drain accepted work and leave, without keeping the engine alive in a cycle.
struct Handle {
    pool: Arc<WorkerPool>,
}

struct WorkerPool {
    state: Mutex<WorkerState>,
    available: Condvar,
    concurrency: Concurrency,
    client: Weak<BlockingClient>,
}

struct WorkerState {
    jobs: VecDeque<Job>,
    workers: usize,
    /// Queued plus running commands, so dispatch can grow before a busy worker receives again.
    pending: usize,
    closing: bool,
}

impl Engine {
    /// Storage startup and the repair pass stay on the first worker, off the async runtime.
    pub(super) async fn open(path: PathBuf, options: Option<OpenOptions>) -> Result<Self> {
        let concurrency = OpenOptions::concurrency_policy(options.as_ref())?;
        let (ready, opened) = oneshot::channel();
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
                let pool = Arc::new(WorkerPool {
                    state: Mutex::new(WorkerState {
                        jobs: VecDeque::new(),
                        workers: 1,
                        pending: 0,
                        closing: false,
                    }),
                    available: Condvar::new(),
                    concurrency,
                    client: Arc::downgrade(&client),
                });
                {
                    let mut state = lock(&pool.state);
                    for _ in 1..concurrency.min() {
                        pool.spawn_worker(&mut state, &client);
                    }
                }
                let engine = Self {
                    handle: Arc::new(Handle {
                        pool: Arc::clone(&pool),
                    }),
                };
                // If opening was abandoned, dropping the unsent handle wakes the helpers.
                if ready.send(Ok(engine)).is_err() {
                    return;
                }
                serve(client, pool);
            })
            .map_err(Error::EngineThread)?;
        opened.await.map_err(|_| Error::Closed)?
    }

    fn dispatch(&self, job: Job) -> Result<()> {
        let pool = &self.handle.pool;
        let mut state = lock(&pool.state);
        if state.closing {
            return Err(Error::Closed);
        }
        state.jobs.push_back(job);
        state.pending += 1;
        if state.pending > state.workers
            && state.workers < pool.concurrency.max() as usize
            && let Some(client) = pool.client.upgrade()
        {
            pool.spawn_worker(&mut state, &client);
        }
        pool.available.notify_one();
        Ok(())
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
            // A caller that stopped awaiting is the only way this fails, and it is the
            // caller's answer to refuse.
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
            // `None` is a command that was cancelled before it started, so there is no
            // answer to send and nobody waiting for one.
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

    /// Freeze growth/retirement and enqueue one stop per live worker under the same lock.
    /// Already accepted work drains before shutdown; later dispatches answer Closed.
    pub(super) async fn close(&self) -> Result<()> {
        let (reply, closed) = oneshot::channel();
        {
            let pool = &self.handle.pool;
            let mut state = lock(&pool.state);
            if state.closing {
                return Err(Error::Closed);
            }
            state.closing = true;
            let shutdown = Arc::new(Shutdown::new(state.workers, reply));
            for _ in 0..state.workers {
                state.jobs.push_back(Job::Stop(Arc::clone(&shutdown)));
            }
            pool.available.notify_all();
        }
        closed.await.map_err(|_| Error::Closed)?
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        lock(&self.pool.state).closing = true;
        self.pool.available.notify_all();
    }
}

impl WorkerPool {
    fn spawn_worker(self: &Arc<Self>, state: &mut WorkerState, client: &Arc<BlockingClient>) {
        let client = Arc::clone(client);
        let pool = Arc::clone(self);
        match std::thread::Builder::new()
            .name(format!("embedded-mongodb-{}", state.workers))
            .spawn(move || serve(client, pool))
        {
            Ok(_) => state.workers += 1,
            // Existing workers can still drain the queue. Count only successful spawns so
            // shutdown never waits for a thread that does not exist.
            Err(error) => tracing::warn!(
                target: "embedded_mongodb",
                error = %error,
                "an engine worker thread could not be started"
            ),
        }
    }

    fn next_job(&self) -> Option<Job> {
        let idle_since = Instant::now();
        let mut state = lock(&self.state);
        loop {
            if let Some(job) = state.jobs.pop_front() {
                return Some(job);
            }
            if state.closing {
                return None;
            }
            state = match self.concurrency.idle_timeout() {
                Some(timeout) => {
                    let remaining = timeout.saturating_sub(idle_since.elapsed());
                    if remaining.is_zero() {
                        return Some(Job::Retire);
                    }
                    self.available
                        .wait_timeout(state, remaining)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self
                    .available
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
}

/// Interrupts a dispatched command when the future waiting for it goes away.
struct CancelOnDrop<'slot>(&'slot CommandSlot);

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn serve(client: Arc<BlockingClient>, pool: Arc<WorkerPool>) {
    while let Some(job) = pool.next_job() {
        match job {
            Job::Run(operation) => {
                // A panic fails its reply, but must not silently lose a worker and hang close.
                let _ = catch_unwind(AssertUnwindSafe(|| operation(&client)));
                lock(&pool.state).pending -= 1;
            }
            Job::Retire => {
                client.reap_idle_sessions();
                let mut state = lock(&pool.state);
                if !state.closing
                    && state.jobs.is_empty()
                    && state.workers > pool.concurrency.min() as usize
                {
                    // Release the engine BEFORE counting out, under the shutdown lock. Close
                    // can then rely on its worker count to cover every remaining strong handle.
                    drop(client);
                    state.workers -= 1;
                    return;
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    const PATIENCE: Duration = Duration::from_secs(5);

    /// Block twice the ceiling's worth of jobs, proving that the queue grows the worker pool
    /// to max and that later jobs still wait there. Channels make this independent of query speed.
    fn saturate(engine: &Engine) -> (Vec<mpsc::Sender<()>>, Vec<oneshot::Receiver<()>>) {
        let max = engine.handle.pool.concurrency.max();
        let (started, running) = mpsc::channel();
        let mut releases = Vec::new();
        let mut replies = Vec::new();
        for _ in 0..max * 2 {
            let started = started.clone();
            let (release, wait) = mpsc::channel();
            let (reply, done) = oneshot::channel();
            engine.run_detached(move |client| {
                let _ = started.send(());
                wait.recv_timeout(PATIENCE).unwrap();
                client
                    .database("admin")
                    .run_command(&bson::doc! { "ping": 1 })
                    .unwrap();
                reply.send(()).unwrap();
            });
            releases.push(release);
            replies.push(done);
        }
        for _ in 0..max {
            running.recv_timeout(PATIENCE).unwrap();
        }
        assert!(
            matches!(
                running.recv_timeout(Duration::from_millis(20)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "workers exceeded max"
        );
        assert_eq!(lock(&engine.handle.pool.state).workers, max as usize);
        (releases, replies)
    }

    async fn settle(engine: &Engine, workers: u32) {
        tokio::time::timeout(PATIENCE, async {
            loop {
                {
                    let state = lock(&engine.handle.pool.state);
                    if state.workers == workers as usize && state.pending == 0 {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("workers did not reach the expected idle count");
    }

    #[test]
    fn workers_scale_retire_regrow_and_close_with_queued_work() {
        let _engine = crate::TEST_ENGINE.lock().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let directory = tempfile::tempdir().unwrap();
            for policy in [
                Concurrency::Fixed(2),
                Concurrency::Dynamic {
                    min: 1,
                    max: 3,
                    idle_timeout: Duration::from_millis(40),
                },
                Concurrency::Dynamic {
                    min: 2,
                    max: 2,
                    idle_timeout: Duration::from_millis(40),
                },
            ] {
                let engine = Engine::open(
                    directory.path().to_owned(),
                    Some(OpenOptions::new().concurrency(policy)),
                )
                .await
                .unwrap();
                assert_eq!(
                    lock(&engine.handle.pool.state).workers,
                    policy.min() as usize
                );
                for _ in 0..2 {
                    let (releases, replies) = saturate(&engine);
                    for release in releases {
                        release.send(()).unwrap();
                    }
                    for reply in replies {
                        tokio::time::timeout(PATIENCE, reply)
                            .await
                            .unwrap()
                            .unwrap();
                    }
                    settle(&engine, policy.min()).await;
                }
                // A panicking operation must neither leak pending capacity nor lose a worker.
                assert!(matches!(
                    engine.run::<(), _>(|_| panic!("test command panic")).await,
                    Err(Error::Closed)
                ));
                engine
                    .run(|client| {
                        client
                            .database("admin")
                            .run_command(&bson::doc! { "ping": 1 })
                    })
                    .await
                    .unwrap();

                let (releases, replies) = saturate(&engine);
                let close = engine.close();
                tokio::pin!(close);
                assert!(
                    tokio::time::timeout(Duration::from_millis(20), &mut close)
                        .await
                        .is_err()
                );
                assert!(matches!(engine.run(|_| Ok(())).await, Err(Error::Closed)));
                for release in releases {
                    release.send(()).unwrap();
                }
                tokio::time::timeout(PATIENCE, close)
                    .await
                    .unwrap()
                    .unwrap();
                for reply in replies {
                    reply.await.unwrap();
                }
                // Close succeeds even though this Engine handle survives, as a cursor's can.
                assert!(matches!(engine.run(|_| Ok(())).await, Err(Error::Closed)));
            }

            // The last caller disappearing must drain detached work and release the engine,
            // even though the workers themselves still own the queue.
            let engine = Engine::open(directory.path().to_owned(), None)
                .await
                .unwrap();
            let survivor = engine.clone();
            drop(engine);
            assert!(!lock(&survivor.handle.pool.state).closing);
            let (releases, replies) = saturate(&survivor);
            drop(survivor);
            for release in releases {
                release.send(()).unwrap();
            }
            for reply in replies {
                tokio::time::timeout(PATIENCE, reply)
                    .await
                    .unwrap()
                    .unwrap();
            }
            // A reply can precede the final worker's destructor. Reopening is the observable
            // proof that shutdown finished, so allow that destructor time to close storage.
            tokio::time::timeout(PATIENCE, async {
                loop {
                    if let Ok(client) = BlockingClient::new(directory.path()) {
                        client.close().unwrap();
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("dropping the last async handle leaked the engine");
        });
    }
}
