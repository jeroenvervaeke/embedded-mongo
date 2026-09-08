//! The queue the async workers share, and the bookkeeping that decides how many there are.
//!
//! A worker is a thread that occupies itself inside the FFI for the length of one command, so
//! the worker count *is* the async layer's parallelism -- the session pool underneath it can
//! grow all it likes and nothing runs in parallel that no thread is driving. This is therefore
//! the same [`Concurrency`] policy the session pool follows, applied to threads: `min` started
//! at open, another started whenever a job is queued with nobody free to take it, up to `max`,
//! and a worker that waits out `idle_timeout` with nothing to do retires itself.
//!
//! The queue is a plain deque under the same mutex as that bookkeeping rather than an
//! [`mpsc`](std::sync::mpsc) channel, because both of the decisions above are made about the
//! queue and the worker counts *together*: whether to start a worker is "a job arrived and none
//! is idle", and whether one may retire is "nothing to take, and the pool can spare me". A
//! channel would leave those two facts under separate locks and racing each other.
//!
//! Acting on the decisions is [`workers`](super::workers)'s; nothing here spawns anything.
//! Generic over what a job runs against only so that the state machine can be driven without an
//! engine; the async layer instantiates it once, over the blocking client.

use super::shutdown::{Shutdown, lock};
use crate::{Concurrency, Error, Result};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Instant;
use tokio::sync::oneshot;

/// What travels from a task to a worker. `Run` carries the operation and its reply channel
/// inside one closure; `Stop` retires the worker that receives it, which is why
/// [`Pool::begin_close`] queues exactly one per live worker.
pub(super) enum Job<C> {
    Run(Box<dyn FnOnce(&C) + Send>),
    Stop(Arc<Shutdown>),
}

/// What a caller must do after queueing a job the pool had nobody free to run: start the worker
/// the pool has already counted on its behalf. `index` is only the thread's name, and never
/// reused, so two workers alive at different times are never confusable in a stack trace.
pub(super) struct Spawn<C> {
    pub(super) client: Arc<C>,
    pub(super) index: u32,
}

pub(super) struct Pool<C> {
    limits: Concurrency,
    state: Mutex<State<C>>,
    queued: Condvar,
}

struct State<C> {
    queue: VecDeque<Job<C>>,
    /// Workers started and not yet gone, counting one that has been reserved but whose thread
    /// has not been spawned yet. Exact, because [`Pool::begin_close`] queues one `Stop` per
    /// worker and a miscount would either strand a thread or leave the close waiting on one
    /// that never comes.
    alive: u32,
    /// Workers waiting for a job rather than running one. Zero is the whole of the case for
    /// starting another.
    idle: u32,
    /// Workers ever started, for thread names.
    started: u32,
    phase: Phase,
    /// A handle for workers started later to run on. Given up as the pool closes, because the
    /// close rendezvous needs the retiring workers to hold the last references to the engine.
    client: Option<Arc<C>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Open,
    /// `close` was called: no new work is taken, and one `Stop` is queued per live worker. No
    /// worker retires itself from here, so the count those stops were queued against stays
    /// true.
    Closing,
    /// Every handle on the engine went away without a close: run what is already queued, then
    /// let the workers go and the engine close silently behind the last of them -- the same end
    /// dropping a blocking client comes to.
    Abandoned,
}

impl<C> Pool<C> {
    pub(super) fn new(limits: Concurrency) -> Self {
        Self {
            limits,
            state: Mutex::new(State {
                queue: VecDeque::new(),
                alive: 0,
                idle: 0,
                started: 0,
                phase: Phase::Open,
                client: None,
            }),
            queued: Condvar::new(),
        }
    }

    /// Counts a worker the caller is about to start, and answers the index for its name. For
    /// the open, which starts its floor of workers with a handle it already has; growth goes
    /// through [`dispatch`](Pool::dispatch), which decides *whether* to start one.
    pub(super) fn worker_started(&self) -> u32 {
        lock(&self.state).reserve()
    }

    /// Gives back the count a worker that failed to start was holding, and with it the `Stop`
    /// queued for it if a close began in the meantime.
    ///
    /// That `Stop` has to go somewhere. It was queued against a count that included this
    /// worker, so the rendezvous is waiting for a retire the thread that never started can no
    /// longer make -- and the caller, which still holds the handle it was going to run on, is
    /// the one that can make it instead.
    pub(super) fn spawn_failed(&self) -> Option<Job<C>> {
        let mut state = lock(&self.state);
        state.alive -= 1;
        match state.phase {
            // The stops are appended after every job already queued and taken from the front,
            // so while a close is under way the back of the queue is one of them.
            Phase::Closing => state.queue.pop_back(),
            _ => None,
        }
    }

    /// Records the handle workers started later will run on, once the engine is open. Ignored
    /// if the pool is already closing: a pool nobody holds any more must not be given a handle
    /// to keep, or the engine it names would never close.
    pub(super) fn opened(&self, client: Arc<C>) {
        let mut state = lock(&self.state);
        if state.phase == Phase::Open {
            state.client = Some(client);
        }
    }

    /// Queues `job`, and answers the worker the caller should now start -- `None` when one is
    /// free to take it or the pool is already at its ceiling.
    pub(super) fn dispatch(&self, job: Job<C>) -> Result<Option<Spawn<C>>> {
        let mut state = lock(&self.state);
        if state.phase != Phase::Open {
            return Err(Error::Closed);
        }
        state.queue.push_back(job);
        let spawn = match state.idle == 0 && state.alive < self.limits.max() {
            true => state.client.clone().map(|client| Spawn {
                client,
                index: state.reserve(),
            }),
            false => None,
        };
        drop(state);
        self.queued.notify_one();
        Ok(spawn)
    }

    /// Waits for the calling worker's next job. `None` retires it: it has waited out the idle
    /// timeout and the pool can spare it, or the engine was abandoned and the queue has run
    /// dry.
    pub(super) fn next_job(&self) -> Option<Job<C>> {
        let mut state = lock(&self.state);
        state.idle += 1;
        let mut waiting_since = Instant::now();
        let job = loop {
            if let Some(job) = state.queue.pop_front() {
                break Some(job);
            }
            if state.phase == Phase::Abandoned {
                break None;
            }
            let Some(timeout) = self.limits.idle_timeout() else {
                state = self
                    .queued
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
                continue;
            };
            let waited = waiting_since.elapsed();
            if state.phase == Phase::Open && state.alive > self.limits.min() && waited >= timeout {
                break None;
            }
            let remaining = match timeout.checked_sub(waited) {
                Some(remaining) if !remaining.is_zero() => remaining,
                // A worker the pool cannot spare has nothing to count down to, so its clock
                // starts again rather than its waiting on zero, which would spin.
                _ => {
                    waiting_since = Instant::now();
                    timeout
                }
            };
            state = self
                .queued
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        };
        state.idle -= 1;
        if job.is_none() {
            state.alive -= 1;
        }
        job
    }

    /// Queues one `Stop` per live worker and hands them the rendezvous that closes the engine
    /// once the last of them has let go of it.
    ///
    /// Everything here happens under the one lock, which is what makes the count true: from the
    /// moment the phase changes no worker retires and no new one starts, so the stops queued
    /// are exactly the workers left to take one.
    pub(super) fn begin_close(&self, reply: oneshot::Sender<Result<()>>) -> Result<()> {
        let mut state = lock(&self.state);
        if state.phase != Phase::Open || state.alive == 0 {
            return Err(Error::Closed);
        }
        state.phase = Phase::Closing;
        // The pool's own handle goes before the workers deposit theirs: the last worker to
        // retire has to be the engine's sole owner for the close to run eagerly and report.
        state.client = None;
        let shutdown = Arc::new(Shutdown::new(state.alive, reply));
        for _ in 0..state.alive {
            state.queue.push_back(Job::Stop(Arc::clone(&shutdown)));
        }
        drop(state);
        self.queued.notify_all();
        Ok(())
    }

    /// Every handle on the engine is gone and nobody called `close`. See [`Phase::Abandoned`];
    /// the work already queued is a dropped cursor's `killCursors`, which still has to run.
    pub(super) fn abandon(&self) {
        let mut state = lock(&self.state);
        if state.phase != Phase::Open {
            return;
        }
        state.phase = Phase::Abandoned;
        state.client = None;
        drop(state);
        self.queued.notify_all();
    }
}

impl<C> State<C> {
    /// Counts one more worker and answers the index for its name.
    fn reserve(&mut self) -> u32 {
        self.alive += 1;
        let index = self.started;
        self.started += 1;
        index
    }
}

#[cfg(test)]
mod tests {
    use super::{Job, Pool, Spawn};
    use crate::{Concurrency, Error};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::Duration;

    /// What a job runs against here. These tests move jobs through the queue and never run one,
    /// so the pool needs a client type but no engine behind it.
    type Client = ();

    fn job() -> Job<Client> {
        Job::Run(Box::new(|_| {}))
    }

    fn elastic(min: u32, max: u32) -> Concurrency {
        Concurrency::dynamic(min, max, Duration::from_millis(20)).expect("a policy in range")
    }

    /// A pool with `alive` workers counted and its handle published, as an open leaves it.
    fn opened(limits: Concurrency, alive: u32) -> Pool<Client> {
        let pool = Pool::new(limits);
        for _ in 0..alive {
            pool.worker_started();
        }
        pool.opened(Arc::new(()));
        pool
    }

    #[test]
    fn a_job_queued_with_nobody_free_asks_for_another_worker() {
        let pool = opened(elastic(1, 4), 1);

        let spawn = pool.dispatch(job()).expect("an open pool takes jobs");

        assert!(
            matches!(spawn, Some(Spawn { index: 1, .. })),
            "the second worker should be started, under the second name"
        );
    }

    #[test]
    fn a_job_queued_with_a_worker_free_asks_for_nothing() {
        let pool = Arc::new(opened(elastic(1, 4), 1));
        let waiting = idle_worker(&pool);

        let spawn = pool.dispatch(job()).expect("an open pool takes jobs");

        assert!(
            spawn.is_none(),
            "a worker already waiting is the one that should run it"
        );
        waiting.join().expect("the waiting worker takes the job");
    }

    #[test]
    fn the_ceiling_is_the_end_of_growing() {
        let pool = opened(elastic(1, 2), 2);

        let spawn = pool.dispatch(job()).expect("an open pool takes jobs");

        assert!(spawn.is_none(), "the pool is already at its ceiling");
    }

    #[test]
    fn a_fixed_pool_never_grows() {
        let pool = opened(
            Concurrency::from_count(2).expect("2 sessions is in range"),
            2,
        );

        let spawn = pool.dispatch(job()).expect("an open pool takes jobs");

        assert!(spawn.is_none());
    }

    #[test]
    fn a_worker_the_pool_can_spare_retires_when_it_waits_out_the_timeout() {
        let pool = Arc::new(opened(elastic(1, 4), 2));

        let worker = {
            let pool = Arc::clone(&pool);
            std::thread::spawn(move || pool.next_job().is_none())
        };

        assert!(
            worker.join().expect("the worker returns"),
            "a spare worker with nothing to do should retire itself"
        );
        assert_eq!(super::lock(&pool.state).alive, 1, "and stop being counted");
    }

    #[test]
    fn the_last_worker_the_floor_needs_never_retires() {
        let pool = Arc::new(opened(elastic(1, 4), 1));
        let took_a_job = Arc::new(AtomicBool::new(false));

        let worker = {
            let pool = Arc::clone(&pool);
            let took_a_job = Arc::clone(&took_a_job);
            std::thread::spawn(move || {
                took_a_job.store(pool.next_job().is_some(), Ordering::Relaxed);
            })
        };

        // Well past the timeout, so a worker that was going to retire has done it by now.
        std::thread::sleep(Duration::from_millis(100));
        pool.dispatch(job()).expect("an open pool takes jobs");
        worker.join().expect("the worker returns");

        assert!(
            took_a_job.load(Ordering::Relaxed),
            "the pool's floor of one worker has to still be there to take the job"
        );
    }

    #[test]
    fn a_worker_without_an_idle_timeout_waits_however_long_it_takes() {
        let pool = Arc::new(opened(
            Concurrency::from_count(2).expect("2 sessions is in range"),
            2,
        ));
        let waiting = idle_worker(&pool);

        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            super::lock(&pool.state).alive,
            2,
            "a fixed pool retires nobody, however long they wait"
        );

        pool.dispatch(job()).expect("an open pool takes jobs");
        waiting.join().expect("the waiting worker takes the job");
    }

    #[test]
    fn a_closing_pool_takes_no_more_work() {
        let pool = opened(elastic(1, 4), 1);
        let (reply, _closed) = tokio::sync::oneshot::channel();
        pool.begin_close(reply).expect("an open pool closes");

        let refused = pool.dispatch(job());

        assert!(matches!(refused, Err(Error::Closed)));
    }

    #[test]
    fn closing_queues_one_stop_for_every_worker() {
        let pool = opened(elastic(1, 4), 3);
        let (reply, _closed) = tokio::sync::oneshot::channel();

        pool.begin_close(reply).expect("an open pool closes");

        let state = super::lock(&pool.state);
        assert_eq!(state.queue.len(), 3);
        assert!(state.queue.iter().all(|job| matches!(job, Job::Stop(_))));
    }

    #[test]
    fn closing_gives_up_the_handle_the_pool_was_holding() {
        let pool = Pool::new(elastic(1, 4));
        pool.worker_started();
        let client = Arc::new(());
        pool.opened(Arc::clone(&client));
        let (reply, _closed) = tokio::sync::oneshot::channel();

        pool.begin_close(reply).expect("an open pool closes");

        assert_eq!(
            Arc::strong_count(&client),
            1,
            "a handle the pool kept would leave the last worker unable to close the engine"
        );
    }

    #[test]
    fn closing_a_pool_with_no_workers_left_is_refused_rather_than_waited_on() {
        let pool = opened(elastic(1, 4), 0);
        let (reply, _closed) = tokio::sync::oneshot::channel();

        let refused = pool.begin_close(reply);

        assert!(
            matches!(refused, Err(Error::Closed)),
            "nobody would ever answer the rendezvous, so it must not be started"
        );
    }

    #[test]
    fn closing_twice_is_refused() {
        let pool = opened(elastic(1, 4), 1);
        let (first, _closed) = tokio::sync::oneshot::channel();
        pool.begin_close(first).expect("an open pool closes");
        let (second, _also_closed) = tokio::sync::oneshot::channel();

        assert!(matches!(pool.begin_close(second), Err(Error::Closed)));
    }

    #[test]
    fn an_abandoned_pool_runs_what_is_queued_before_its_workers_let_go() {
        let pool = opened(elastic(1, 4), 1);
        pool.dispatch(job()).expect("an open pool takes jobs");

        pool.abandon();

        assert!(
            pool.next_job().is_some(),
            "the work already queued still runs"
        );
        assert!(
            pool.next_job().is_none(),
            "and then the worker lets go of the engine"
        );
    }

    #[test]
    fn abandoning_gives_up_the_handle_the_pool_was_holding() {
        let pool = Pool::new(elastic(1, 4));
        pool.worker_started();
        let client = Arc::new(());
        pool.opened(Arc::clone(&client));

        pool.abandon();

        assert_eq!(
            Arc::strong_count(&client),
            1,
            "a handle the pool kept would stop the engine ever closing"
        );
    }

    #[test]
    fn a_pool_abandoned_while_it_was_still_opening_refuses_the_handle() {
        let pool = Pool::new(elastic(1, 4));
        pool.worker_started();
        let client = Arc::new(());

        pool.abandon();
        pool.opened(Arc::clone(&client));

        assert_eq!(Arc::strong_count(&client), 1);
    }

    #[test]
    fn a_worker_that_failed_to_start_stops_being_counted() {
        let pool = opened(elastic(1, 4), 1);
        pool.worker_started();

        let unclaimed = pool.spawn_failed();

        assert!(unclaimed.is_none(), "no close is under way to hand back to");
        assert_eq!(
            super::lock(&pool.state).alive,
            1,
            "a close must not wait for a thread that never started"
        );
    }

    /// The race the reserve-then-spawn split opens: a close can queue a `Stop` for a worker
    /// between its being counted and its thread failing to start. Left there, the rendezvous
    /// would wait for a retire nobody can make.
    #[test]
    fn a_worker_that_failed_to_start_while_the_pool_was_closing_hands_back_its_stop() {
        let pool = opened(elastic(1, 4), 1);
        pool.worker_started();
        let (reply, _closed) = tokio::sync::oneshot::channel();
        pool.begin_close(reply).expect("an open pool closes");

        let unclaimed = pool.spawn_failed();

        assert!(
            matches!(unclaimed, Some(Job::Stop(_))),
            "the stop queued for the worker that never started has to come back"
        );
        let state = super::lock(&pool.state);
        assert_eq!(state.alive, 1);
        assert_eq!(
            state.queue.len(),
            1,
            "one stop left, for the one worker still there to take it"
        );
    }

    /// A worker parked in `next_job` until something is queued for it.
    fn idle_worker(pool: &Arc<Pool<Client>>) -> JoinHandle<()> {
        let waiting = Arc::clone(pool);
        let worker = std::thread::spawn(move || {
            waiting.next_job();
        });
        while super::lock(&pool.state).idle == 0 {
            std::hint::spin_loop();
        }
        worker
    }
}
