//! Where an async worker waits for its next job, and what wakes it.
//!
//! A worker is a thread that occupies itself inside the FFI for the length of one command, so
//! the worker count *is* the async layer's parallelism -- the session pool underneath it can
//! grow all it likes and nothing runs in parallel that no thread is driving. The worker count
//! therefore follows the same [`Concurrency`] policy the session pool does: `min` started at
//! open, another whenever a job is queued with nobody free to take it, up to `max`, and a
//! worker that waits out `idle_timeout` with nothing to do retires itself.
//!
//! The queue is a deque under the same mutex as the worker counts rather than an
//! [`mpsc`](std::sync::mpsc) channel, because both of those decisions are made about the queue
//! and the counts *together*: whether to start a worker is "a job arrived and none is free",
//! and whether one may retire is "nothing to take, and the pool can spare me". A channel would
//! leave the two facts under separate locks, racing each other.
//!
//! The decisions themselves are [`state`](super::state)'s, and acting on them --
//! spawning, serving -- is [`workers`](super::workers)'s. What is here is the lock, the wait,
//! and the wakeups.

use super::shutdown::{Shutdown, lock};
use super::state::{Job, Spawn, State};
use crate::{Concurrency, Result};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Instant;
use tokio::sync::oneshot;

pub(super) struct Pool<C> {
    limits: Concurrency,
    state: Mutex<State<C>>,
    queued: Condvar,
}

impl<C> Pool<C> {
    pub(super) fn new(limits: Concurrency) -> Self {
        Self {
            limits,
            state: Mutex::new(State::new()),
            queued: Condvar::new(),
        }
    }

    /// Counts a worker the caller is about to start, and answers the index for its name. For
    /// the open, which starts its floor of workers with a handle it already has; growth goes
    /// through [`dispatch`](Pool::dispatch), which decides *whether* to start one.
    pub(super) fn worker_started(&self) -> u32 {
        lock(&self.state).reserve()
    }

    /// Called once by every worker as its thread starts.
    pub(super) fn worker_ready(&self) {
        lock(&self.state).arrived();
    }

    /// Gives back the count a worker that failed to start was holding, and the rendezvous of
    /// the `Stop` queued for it if a close began in the meantime -- which the caller has to
    /// answer, because the thread that would have is never coming.
    pub(super) fn spawn_failed(&self) -> Option<Arc<Shutdown>> {
        lock(&self.state).spawn_failed()
    }

    /// Records the handle workers started later will run on, once the engine is open.
    pub(super) fn opened(&self, client: Arc<C>) {
        lock(&self.state).publish(client);
    }

    /// Queues `job`, and answers the worker the caller should now start -- `None` when one is
    /// free to take it or the pool is already at its ceiling.
    pub(super) fn dispatch(&self, job: Job<C>) -> Result<Option<Spawn<C>>> {
        let spawn = lock(&self.state).queue_job(job, self.limits.max())?;
        self.queued.notify_one();
        Ok(spawn)
    }

    /// Waits for the calling worker's next job, and takes its handle on the engine back when
    /// there will be no next job. `None` retires it: it has waited out the idle timeout and the
    /// pool can spare it, or the engine was abandoned and the queue has run dry.
    pub(super) fn next_job(&self, client: &mut Option<Arc<C>>) -> Option<Job<C>> {
        let mut state = lock(&self.state);
        state.waiting();
        // Set only once this worker is one the pool could do without: a worker at the floor has
        // nothing to count down to, and a fixed pool never counts at all.
        let mut sparable_since: Option<Instant> = None;
        loop {
            if let Some(job) = state.take_job() {
                state.resumed();
                return Some(job);
            }
            if state.abandoned() {
                state.retired(client);
                return None;
            }
            let sparable = state.sparable(self.limits.min());
            let Some(remaining) = self
                .limits
                .idle_timeout()
                .filter(|_| sparable)
                .map(|timeout| {
                    timeout
                        .saturating_sub(sparable_since.get_or_insert_with(Instant::now).elapsed())
                })
            else {
                // Nothing to wait *out*: either the pool keeps every worker it has, or this one
                // is at the floor. Woken by whatever queues a job, and every reservation queues
                // one too, so a worker that becomes sparable hears about it.
                sparable_since = None;
                state = self
                    .queued
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
                continue;
            };
            if remaining.is_zero() {
                state.retired(client);
                return None;
            }
            state = self
                .queued
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Queues one `Stop` per live worker and hands them the rendezvous that closes the engine
    /// once the last of them has let go of it.
    pub(super) fn begin_close(&self, reply: oneshot::Sender<Result<()>>) -> Result<()> {
        lock(&self.state).begin_close(reply, |workers, reply| {
            Arc::new(Shutdown::new(workers, reply))
        })?;
        self.queued.notify_all();
        Ok(())
    }

    /// Every handle on the engine is gone and nobody called `close`. The workers run what is
    /// already queued and then let go, which closes the engine silently.
    pub(super) fn abandon(&self) {
        lock(&self.state).abandon();
        self.queued.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::{Job, Pool};
    use crate::Concurrency;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::Duration;

    type Client = ();

    fn job() -> Job<Client> {
        Job::Run(Box::new(|_| {}))
    }

    fn elastic(min: u32, max: u32) -> Concurrency {
        Concurrency::dynamic(min, max, Duration::from_millis(20)).expect("a policy in range")
    }

    /// A pool with `alive` workers counted, arrived, and its handle published.
    fn opened(limits: Concurrency, alive: u32) -> Pool<Client> {
        let pool = Pool::new(limits);
        for _ in 0..alive {
            pool.worker_started();
            pool.worker_ready();
        }
        pool.opened(Arc::new(()));
        pool
    }

    /// What a worker holds while it serves.
    fn held() -> Option<Arc<Client>> {
        Some(Arc::new(()))
    }

    #[test]
    fn a_worker_the_pool_can_spare_retires_when_it_waits_out_the_timeout() {
        let pool = Arc::new(opened(elastic(1, 4), 2));

        let worker = {
            let pool = Arc::clone(&pool);
            std::thread::spawn(move || pool.next_job(&mut held()).is_none())
        };

        assert!(
            worker.join().expect("the worker returns"),
            "a spare worker with nothing to do should retire itself"
        );
    }

    #[test]
    fn the_last_worker_the_floor_needs_never_retires() {
        let pool = Arc::new(opened(elastic(1, 4), 1));
        let took_a_job = Arc::new(AtomicBool::new(false));

        let worker = {
            let pool = Arc::clone(&pool);
            let took_a_job = Arc::clone(&took_a_job);
            std::thread::spawn(move || {
                took_a_job.store(pool.next_job(&mut held()).is_some(), Ordering::Relaxed);
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
        assert!(
            !waiting.is_finished(),
            "a fixed pool retires nobody, however long they wait"
        );

        pool.dispatch(job()).expect("an open pool takes jobs");
        waiting.join().expect("the waiting worker takes the job");
    }

    #[test]
    fn a_queued_job_wakes_a_waiting_worker() {
        let pool = Arc::new(opened(elastic(1, 4), 1));
        let waiting = idle_worker(&pool);

        pool.dispatch(job()).expect("an open pool takes jobs");

        waiting.join().expect("the waiting worker takes the job");
    }

    #[test]
    fn abandoning_wakes_every_worker_so_that_each_can_let_go() {
        let pool = Arc::new(opened(elastic(1, 4), 2));
        let waiting: Vec<_> = (0..2).map(|_| idle_worker_holding(&pool)).collect();

        pool.abandon();

        for worker in waiting {
            assert!(
                worker.join().expect("the worker returns").is_none(),
                "an abandoned pool answers every waiting worker, not just the first"
            );
        }
    }

    /// A worker parked in `next_job` until something is queued for it.
    fn idle_worker(pool: &Arc<Pool<Client>>) -> JoinHandle<()> {
        let worker = idle_worker_holding(pool);
        std::thread::spawn(move || {
            worker.join().expect("the worker returns");
        })
    }

    fn idle_worker_holding(pool: &Arc<Pool<Client>>) -> JoinHandle<Option<Job<Client>>> {
        let waiting = Arc::clone(pool);
        let worker = std::thread::spawn(move || waiting.next_job(&mut held()));
        // Parked, so a test that queues something next is testing the wakeup and not a race.
        std::thread::sleep(Duration::from_millis(20));
        worker
    }
}
