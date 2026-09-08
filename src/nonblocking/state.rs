//! What the async worker pool decides, and the counts it decides from.
//!
//! Everything here happens with the pool's mutex already held, so it is written as plain
//! `&mut self`: no locking, no waiting, no spawning. That is the point of the split --
//! [`queue`](super::queue) is where a worker blocks and where the notifies go, and every rule
//! about how many workers there should be is here, where it can be read and tested without a
//! thread.
//!
//! Generic over what a job runs against only so that these rules can be exercised without an
//! engine; the async layer instantiates them once, over the blocking client.

use super::shutdown::Shutdown;
use crate::{Error, Result};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::oneshot;

/// What travels from a task to a worker. `Run` carries the operation and its reply channel
/// inside one closure; `Stop` retires the worker that receives it, which is why
/// [`State::begin_close`] queues exactly one per live worker.
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

pub(super) struct State<C> {
    queue: VecDeque<Job<C>>,
    /// Workers started and not yet gone, counting one that has been reserved but whose thread
    /// has not been spawned yet. Exact while the pool is open, because [`State::begin_close`]
    /// queues one `Stop` per worker and a miscount would either strand a thread or leave the
    /// close waiting on one that never comes. Frozen from there: a `Stop` retires its worker
    /// without coming through here, and by then nothing reads it again.
    alive: u32,
    /// Workers counted in `alive` whose thread has not yet reached the queue. They will take a
    /// job that is waiting but are not free to take one *now*, which is a distinction both
    /// decisions below turn on.
    pending: u32,
    /// Workers waiting for a job rather than running one.
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
    /// Every handle on the engine went away without a close: run what is already queued -- a
    /// dropped cursor's `killCursors` is exactly that -- then let the workers go and the engine
    /// close silently behind the last of them, the same end dropping a blocking client comes
    /// to.
    Abandoned,
}

impl<C> State<C> {
    pub(super) fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            alive: 0,
            pending: 0,
            idle: 0,
            started: 0,
            phase: Phase::Open,
            client: None,
        }
    }

    /// Counts a worker somebody is about to start, and answers the index for its name.
    pub(super) fn reserve(&mut self) -> u32 {
        self.alive += 1;
        self.pending += 1;
        let index = self.started;
        self.started += 1;
        index
    }

    /// Called by every worker as its thread starts, which is what stops it counting as one the
    /// pool is still waiting on.
    pub(super) fn arrived(&mut self) {
        self.pending -= 1;
    }

    /// Gives back the count a worker that failed to start was holding, and with it the
    /// rendezvous of the `Stop` queued for it if a close began in the meantime.
    ///
    /// That `Stop` has to go somewhere. It was queued against a count that included this
    /// worker, so the rendezvous is waiting for a retire the thread that never started can no
    /// longer make -- and the caller, which still holds the handle it was going to run on, is
    /// the one that can make it instead.
    pub(super) fn spawn_failed(&mut self) -> Option<Arc<Shutdown>> {
        self.alive -= 1;
        self.pending -= 1;
        match self.phase {
            // The stops are appended after every job already queued and are taken from the
            // front, so while a close is under way the back of the queue is one of them.
            Phase::Closing => match self.queue.pop_back() {
                Some(Job::Stop(shutdown)) => Some(shutdown),
                // Unreachable while stops are only ever queued by `begin_close`. Put back
                // rather than dropped, so a future that broke that would cost a command its
                // answer rather than lose it silently.
                Some(job) => {
                    self.queue.push_back(job);
                    None
                }
                None => None,
            },
            Phase::Open | Phase::Abandoned => None,
        }
    }

    /// Records the handle workers started later will run on. Ignored once the pool is closing:
    /// a pool nobody holds any more must not be given a handle to keep, or the engine it names
    /// would never close.
    pub(super) fn publish(&mut self, client: Arc<C>) {
        if self.phase == Phase::Open {
            self.client = Some(client);
        }
    }

    /// Queues `job`, and answers the worker the caller should now start -- `None` when one is
    /// free to take it or the pool is already at its ceiling.
    pub(super) fn queue_job(&mut self, job: Job<C>, max: u32) -> Result<Option<Spawn<C>>> {
        if self.phase != Phase::Open {
            return Err(Error::Closed);
        }
        self.queue.push_back(job);
        match self.wants_worker(max) {
            true => Ok(self.client.clone().map(|client| Spawn {
                client,
                index: self.reserve(),
            })),
            false => Ok(None),
        }
    }

    /// Counts the calling worker as one waiting for a job rather than running one.
    pub(super) fn waiting(&mut self) {
        self.idle += 1;
    }

    /// The calling worker has a job again.
    pub(super) fn resumed(&mut self) {
        self.idle -= 1;
    }

    /// The calling worker is leaving, and gives up its handle on the engine here.
    ///
    /// Under the same lock that stops counting it, on purpose. A handle let go a moment later
    /// would leave [`Shutdown::retire`]'s claim -- that the retiring workers hold every last
    /// reference to the engine -- briefly false, and a close racing that window answers an
    /// error for an engine that closed perfectly well.
    pub(super) fn retired(&mut self, client: &mut Option<Arc<C>>) {
        self.idle -= 1;
        self.alive -= 1;
        *client = None;
    }

    pub(super) fn take_job(&mut self) -> Option<Job<C>> {
        self.queue.pop_front()
    }

    pub(super) fn abandoned(&self) -> bool {
        self.phase == Phase::Abandoned
    }

    /// Whether the pool would still have its floor of workers without the caller.
    pub(super) fn sparable(&self, min: u32) -> bool {
        self.phase == Phase::Open && self.serving() > min
    }

    /// Queues one `Stop` per live worker against `shutdown`, built here from the count so that
    /// the two cannot disagree.
    ///
    /// Everything happens under the one lock, which is what makes the count true: from the
    /// moment the phase changes no worker retires and no new one starts, so the stops queued
    /// are exactly the workers left to take one.
    pub(super) fn begin_close(
        &mut self,
        reply: oneshot::Sender<Result<()>>,
        rendezvous: impl FnOnce(u32, oneshot::Sender<Result<()>>) -> Arc<Shutdown>,
    ) -> Result<()> {
        if self.phase != Phase::Open || self.alive == 0 {
            return Err(Error::Closed);
        }
        self.phase = Phase::Closing;
        // The pool's own handle goes before the workers deposit theirs: the last worker to
        // retire has to be the engine's sole owner for the close to run eagerly and report.
        self.client = None;
        let shutdown = rendezvous(self.alive, reply);
        for _ in 0..self.alive {
            self.queue.push_back(Job::Stop(Arc::clone(&shutdown)));
        }
        Ok(())
    }

    /// Every handle on the engine is gone and nobody called `close`; see [`Phase::Abandoned`].
    pub(super) fn abandon(&mut self) {
        if self.phase != Phase::Open {
            return;
        }
        self.phase = Phase::Abandoned;
        self.client = None;
    }

    /// Workers whose threads are running: waiting for a job or inside one. Never more than
    /// `alive`, which counts these plus the ones still on their way.
    fn serving(&self) -> u32 {
        self.alive - self.pending
    }

    /// Whether the queue holds more work than the workers already free, or on their way to
    /// being free, can pick up.
    ///
    /// Both halves matter. Asking only whether one is idle under-counts demand -- eight jobs
    /// queued while a single worker has yet to be rescheduled all see it free, and the pool
    /// stays at its floor and drains them one at a time. Ignoring the ones on their way
    /// over-counts it -- every job of a burst starts a thread, because none of the threads
    /// already starting has reached the queue yet.
    fn wants_worker(&self, max: u32) -> bool {
        self.alive < max && self.queue.len() > (self.idle + self.pending) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::{Job, Spawn, State};
    use crate::Error;
    use std::sync::Arc;

    /// What a job runs against here. These tests move jobs through the queue and never run one,
    /// so the state needs a client type but no engine behind it.
    type Client = ();

    fn job() -> Job<Client> {
        Job::Run(Box::new(|_| {}))
    }

    /// A state with `alive` workers counted, arrived, and its handle published -- an open that
    /// has finished starting its floor.
    fn opened(alive: u32) -> State<Client> {
        let mut state = State::new();
        for _ in 0..alive {
            state.reserve();
            state.arrived();
        }
        state.publish(Arc::new(()));
        state
    }

    /// Closes, discarding the rendezvous' answer: what these check is the queue and the
    /// counts the stops were built from, not the engine close on the other end of it.
    fn close(state: &mut State<Client>) -> crate::Result<()> {
        let (reply, _answer) = tokio::sync::oneshot::channel();
        state.begin_close(reply, |workers, reply| {
            Arc::new(super::Shutdown::new(workers, reply))
        })
    }

    #[test]
    fn a_job_queued_with_nobody_free_asks_for_another_worker() {
        let mut state = opened(1);

        let spawn = state.queue_job(job(), 4).expect("an open pool takes jobs");

        assert!(
            matches!(spawn, Some(Spawn { index: 1, .. })),
            "the second worker should be started, under the second name"
        );
    }

    #[test]
    fn a_job_queued_with_a_worker_free_asks_for_nothing() {
        let mut state = opened(1);
        state.waiting();

        let spawn = state.queue_job(job(), 4).expect("an open pool takes jobs");

        assert!(
            spawn.is_none(),
            "a worker already waiting is the one that should run it"
        );
    }

    /// The under-spawn an `idle == 0` test would have: a worker counts as free until it has
    /// actually taken something, so a burst arriving before it is rescheduled would all see it
    /// free, and the pool would stay at its floor and drain the burst one job at a time.
    #[test]
    fn a_burst_past_the_one_free_worker_asks_for_one_worker_per_job_left_waiting() {
        let mut state = opened(1);
        state.waiting();

        let spawns: Vec<_> = (0..3)
            .map(|_| state.queue_job(job(), 4).expect("an open pool takes jobs"))
            .collect();

        assert!(spawns[0].is_none(), "the waiting worker takes the first");
        assert!(spawns[1].is_some(), "the second job has nobody left");
        assert!(spawns[2].is_some(), "and neither has the third");
        // The other side of the same count: each worker is counted from the moment it is asked
        // for, so the job after it sees one on its way rather than an empty pool. Three jobs
        // ask for two workers, not for one apiece.
        assert_eq!(state.pending, 2);
        assert_eq!(state.alive, 3);
    }

    #[test]
    fn the_ceiling_is_the_end_of_growing() {
        let mut state = opened(2);

        let spawn = state.queue_job(job(), 2).expect("an open pool takes jobs");

        assert!(spawn.is_none(), "the pool is already at its ceiling");
    }

    #[test]
    fn a_worker_the_pool_can_spare_is_one_past_its_floor_that_has_arrived() {
        let state = opened(2);

        assert!(state.sparable(1));
        assert!(!state.sparable(2), "the floor keeps every worker it counts");
    }

    /// The starvation the `pending` count rules out: a floor worker that counted a
    /// still-starting peer as one of its own could retire behind it, and if that peer then
    /// failed to start the pool would be left open with no worker at all and jobs queued for
    /// nobody.
    #[test]
    fn a_worker_that_has_yet_to_start_does_not_make_the_floor_sparable() {
        let mut state = opened(1);
        state.queue_job(job(), 4).expect("an open pool takes jobs");

        assert!(
            !state.sparable(1),
            "the only arrived worker must not retire behind one that has yet to start"
        );
    }

    #[test]
    fn a_retiring_worker_gives_up_its_handle_as_it_stops_being_counted() {
        let mut state = opened(2);
        state.waiting();
        let client = Arc::new(());
        let mut held = Some(Arc::clone(&client));

        state.retired(&mut held);

        assert!(held.is_none());
        assert_eq!(
            Arc::strong_count(&client),
            1,
            "the handle has to be gone by the time the count is"
        );
        assert_eq!(state.alive, 1);
    }

    #[test]
    fn a_closing_pool_takes_no_more_work() {
        let mut state = opened(1);
        close(&mut state).expect("an open pool closes");

        assert!(matches!(state.queue_job(job(), 4), Err(Error::Closed)));
    }

    #[test]
    fn closing_queues_one_stop_for_every_worker() {
        let mut state = opened(3);

        close(&mut state).expect("an open pool closes");

        assert_eq!(state.queue.len(), 3);
        assert!(state.queue.iter().all(|job| matches!(job, Job::Stop(_))));
    }

    #[test]
    fn closing_gives_up_the_handle_the_pool_was_holding() {
        let mut state = State::new();
        state.reserve();
        state.arrived();
        let client = Arc::new(());
        state.publish(Arc::clone(&client));

        close(&mut state).expect("an open pool closes");

        assert_eq!(
            Arc::strong_count(&client),
            1,
            "a handle the pool kept would leave the last worker unable to close the engine"
        );
    }

    #[test]
    fn closing_a_pool_with_no_workers_left_is_refused_rather_than_waited_on() {
        let mut state = opened(0);

        assert!(
            matches!(close(&mut state), Err(Error::Closed)),
            "nobody would ever answer the rendezvous, so it must not be started"
        );
    }

    #[test]
    fn closing_twice_is_refused() {
        let mut state = opened(1);
        close(&mut state).expect("an open pool closes");

        assert!(matches!(close(&mut state), Err(Error::Closed)));
    }

    #[test]
    fn an_abandoned_pool_still_holds_the_work_already_queued() {
        let mut state = opened(1);
        state.queue_job(job(), 4).expect("an open pool takes jobs");

        state.abandon();

        assert!(state.abandoned());
        assert!(
            state.take_job().is_some(),
            "the work already queued still runs"
        );
        assert!(state.take_job().is_none());
    }

    #[test]
    fn abandoning_gives_up_the_handle_the_pool_was_holding() {
        let mut state = State::new();
        state.reserve();
        state.arrived();
        let client = Arc::new(());
        state.publish(Arc::clone(&client));

        state.abandon();

        assert_eq!(
            Arc::strong_count(&client),
            1,
            "a handle the pool kept would stop the engine ever closing"
        );
    }

    #[test]
    fn a_pool_abandoned_while_it_was_still_opening_refuses_the_handle() {
        let mut state = State::new();
        let client = Arc::new(());

        state.abandon();
        state.publish(Arc::clone(&client));

        assert_eq!(Arc::strong_count(&client), 1);
    }

    #[test]
    fn a_worker_that_failed_to_start_stops_being_counted() {
        let mut state = opened(1);
        state.reserve();

        let unclaimed = state.spawn_failed();

        assert!(unclaimed.is_none(), "no close is under way to hand back to");
        assert_eq!(
            state.alive, 1,
            "a close must not wait for a thread that never started"
        );
        assert_eq!(state.pending, 0, "nor may it be waited for as one arriving");
    }

    /// The race the reserve-then-spawn split opens: a close can queue a `Stop` for a worker
    /// between its being counted and its thread failing to start. Left there, the rendezvous
    /// would wait for a retire nobody can make.
    #[test]
    fn a_worker_that_failed_to_start_while_the_pool_was_closing_hands_back_its_stop() {
        let mut state = opened(1);
        state.reserve();
        close(&mut state).expect("an open pool closes");

        let unclaimed = state.spawn_failed();

        assert!(
            unclaimed.is_some(),
            "the stop queued for the worker that never started has to come back"
        );
        assert_eq!(state.alive, 1);
        assert_eq!(
            state.queue.len(),
            1,
            "one stop left, for the one worker still there to take it"
        );
    }
}
