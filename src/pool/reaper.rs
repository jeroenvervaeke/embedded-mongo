//! Which sessions are free, since when, and the thread that closes the ones nobody wants.
//!
//! Kept apart from the pool because it answers a different question: the pool is about handing
//! one session to one caller, this is about how many sessions there should be at all. The
//! bookkeeping lives here rather than beside the checkout because every decision made about
//! it -- what to close, and when to look again -- is made here.
//!
//! Generic over the session so that the part with the clock in it can be exercised without an
//! engine to open sessions on.

use super::lock;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The free sessions and the two waits over them.
pub(super) struct Idles<S> {
    pub(super) state: Mutex<State<S>>,
    /// Woken when a session goes back into the pool. Waited on only by checkouts, so that a
    /// returning session never spends its wakeup on the reaper instead of a caller waiting for
    /// one -- which is why the reaper has a condvar of its own rather than sharing this.
    pub(super) returned: Condvar,
    /// Woken once, when the pool is closing, so the reaper stops waiting out its timeout and
    /// can be joined.
    closing: Condvar,
}

/// The pool's bookkeeping.
pub(super) struct State<S> {
    /// Sessions free to be taken, least recently used first: [`take`](State::take) pops the
    /// back, so a session nobody needs sinks to the front and is the first one the reaper
    /// closes.
    idle: Vec<Idle<S>>,
    /// Sessions open or about to be: idle, checked out, or reserved by a caller that is opening
    /// one right now. The reservation is what keeps two callers from both opening the last
    /// session the ceiling allows.
    pub(super) open: u32,
    pub(super) closing: bool,
}

/// A session waiting to be taken, and since when.
struct Idle<S> {
    session: S,
    since: Instant,
}

impl<S> Idles<S> {
    pub(super) fn new(sessions: Vec<S>) -> Self {
        let open = sessions.len() as u32;
        Self {
            state: Mutex::new(State {
                idle: sessions.into_iter().map(Idle::taken).collect(),
                open,
                closing: false,
            }),
            returned: Condvar::new(),
            closing: Condvar::new(),
        }
    }

    /// Hands a session back and wakes whoever is waiting for one.
    pub(super) fn give_back(&self, session: S) {
        lock(&self.state).idle.push(Idle::taken(session));
        self.returned.notify_one();
    }

    /// Tells the reaper to stop, so that the pool's drop can join it.
    pub(super) fn stop_reaping(&self) {
        lock(&self.state).closing = true;
        self.closing.notify_all();
    }
}

impl<S> State<S> {
    /// The most recently returned free session, if there is one. Most recently rather than
    /// least, so that the sessions the pool could do without are the ones that go stale.
    pub(super) fn take(&mut self) -> Option<S> {
        self.idle.pop().map(|entry| entry.session)
    }

    /// Takes every session that has been idle at least `timeout`, down to a floor of `min`
    /// sessions open. Answers them -- for the caller to close with the pool unlocked -- and how
    /// long to wait before there is anything to do here again.
    ///
    /// The answered wait is never zero: a session that has already outstayed the timeout is
    /// either taken now or is one the pool cannot spare, and a pool that can spare none waits
    /// out a whole timeout before asking again. That is what keeps the reaper from spinning.
    fn reap(&mut self, min: u32, timeout: Duration, now: Instant) -> (Vec<S>, Duration) {
        let mut spare = self.open.saturating_sub(min);
        if spare == 0 || self.idle.is_empty() {
            return (Vec::new(), timeout);
        }

        let idle = std::mem::take(&mut self.idle);
        let mut kept = Vec::with_capacity(idle.len());
        let mut expired = Vec::new();
        let mut wait = timeout;
        for entry in idle {
            let waited = now.saturating_duration_since(entry.since);
            if spare > 0 && waited >= timeout {
                spare -= 1;
                self.open -= 1;
                expired.push(entry.session);
                continue;
            }
            if spare > 0 {
                // The next thing to do here is this session coming due, unless an older one
                // beat it to it.
                wait = wait.min(timeout - waited);
            }
            kept.push(entry);
        }
        self.idle = kept;
        (expired, wait)
    }
}

impl<S> Idle<S> {
    fn taken(session: S) -> Self {
        Self {
            session,
            since: Instant::now(),
        }
    }
}

/// Starts the thread that closes idle sessions. A pool that cannot start one still runs every
/// command, it just never shrinks, which is not worth failing an open over -- the same call the
/// async engine makes about a worker thread it could not start.
pub(super) fn start_reaper<S: Send + 'static>(
    idles: &Arc<Idles<S>>,
    min: u32,
    timeout: Duration,
) -> Option<JoinHandle<()>> {
    let idles = Arc::clone(idles);
    match std::thread::Builder::new()
        .name("embedded-mongodb-reaper".to_owned())
        .spawn(move || reap(&idles, min, timeout))
    {
        Ok(reaper) => Some(reaper),
        Err(error) => {
            tracing::warn!(
                target: "embedded_mongodb",
                error = %error,
                "the session pool's reaper thread could not be started; idle sessions will be \
                 kept rather than closed"
            );
            None
        }
    }
}

/// The reaper's whole life: wait, close what has gone stale, wait again, and stop when the pool
/// says it is closing.
fn reap<S>(idles: &Idles<S>, min: u32, timeout: Duration) {
    let mut state = lock(&idles.state);
    loop {
        if state.closing {
            return;
        }
        let (expired, wait) = state.reap(min, timeout, Instant::now());
        if !expired.is_empty() {
            // Closed with the pool unlocked: a caller waiting for a session must not also wait
            // for sessions being destroyed.
            drop(state);
            drop(expired);
            state = lock(&idles.state);
            continue;
        }
        state = idles
            .closing
            .wait_timeout(state, wait)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

#[cfg(test)]
mod tests {
    use super::{Idle, Idles, State, reap};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    /// A stand-in for a session: it cannot run a command, but it can be counted as it is
    /// dropped, which is the only thing the reaper does to one.
    struct Fake(Arc<AtomicUsize>);

    impl Drop for Fake {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    const TIMEOUT: Duration = Duration::from_secs(60);

    /// A pool of `open` sessions with one free session per entry in `ages`, idle for that long.
    fn pool(open: u32, ages: &[Duration]) -> (State<Fake>, Arc<AtomicUsize>, Instant) {
        // Built forwards from the present rather than backwards from it: an `Instant` far
        // enough in the past to subtract these ages from is not guaranteed to exist on a
        // machine that booted a moment ago.
        let base = Instant::now();
        let oldest = ages.iter().copied().max().unwrap_or(Duration::ZERO);
        let now = base + oldest;
        let closed = Arc::new(AtomicUsize::new(0));
        let idle = ages
            .iter()
            .map(|age| Idle {
                session: Fake(Arc::clone(&closed)),
                since: base + (oldest - *age),
            })
            .collect();
        (
            State {
                idle,
                open,
                closing: false,
            },
            closed,
            now,
        )
    }

    /// The same, wrapped as the reaper thread takes it.
    fn shared(open: u32, ages: &[Duration]) -> (Arc<Idles<Fake>>, Arc<AtomicUsize>) {
        let (state, closed, _) = pool(open, ages);
        (
            Arc::new(Idles {
                state: Mutex::new(state),
                returned: Condvar::new(),
                closing: Condvar::new(),
            }),
            closed,
        )
    }

    #[test]
    fn a_session_idle_past_the_timeout_is_closed() {
        let (mut state, closed, now) = pool(2, &[TIMEOUT, Duration::ZERO]);

        let (expired, _) = state.reap(1, TIMEOUT, now);

        assert_eq!(expired.len(), 1);
        assert_eq!(state.open, 1, "the closed session is no longer open");
        assert_eq!(state.idle.len(), 1, "the fresh session stays");
        drop(expired);
        assert_eq!(closed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn the_floor_is_kept_however_long_its_sessions_have_been_idle() {
        let (mut state, _, now) = pool(2, &[TIMEOUT * 10, TIMEOUT * 10]);

        let (expired, wait) = state.reap(2, TIMEOUT, now);

        assert!(
            expired.is_empty(),
            "a pool already at its floor has nothing to spare"
        );
        assert_eq!(state.open, 2);
        assert_eq!(
            wait, TIMEOUT,
            "with nothing to spare the reaper waits out a whole timeout rather than spinning"
        );
    }

    /// Checked-out sessions count against the floor: closing every idle one while the rest are
    /// busy would take the pool below the floor the moment they came back.
    #[test]
    fn sessions_out_on_loan_count_towards_the_floor() {
        let (mut state, _, now) = pool(4, &[TIMEOUT * 2, TIMEOUT * 2]);

        let (expired, _) = state.reap(3, TIMEOUT, now);

        assert_eq!(expired.len(), 1, "only one of the four is spare");
        assert_eq!(state.open, 3);
    }

    #[test]
    fn the_least_recently_used_session_is_the_first_to_go() {
        let (mut state, _, now) = pool(3, &[TIMEOUT * 3, TIMEOUT * 2, Duration::ZERO]);

        let (expired, _) = state.reap(1, TIMEOUT, now);

        assert_eq!(expired.len(), 2);
        assert_eq!(state.idle.len(), 1);
        assert_eq!(
            state.idle[0].since, now,
            "the session returned most recently is the one kept"
        );
    }

    #[test]
    fn nothing_expired_yet_asks_to_be_woken_when_the_oldest_comes_due() {
        let (mut state, _, now) = pool(2, &[TIMEOUT / 4, TIMEOUT / 2]);

        let (expired, wait) = state.reap(1, TIMEOUT, now);

        assert!(expired.is_empty());
        assert_eq!(
            wait,
            TIMEOUT - TIMEOUT / 2,
            "the wait is until the oldest spare session comes due, not a whole timeout"
        );
    }

    #[test]
    fn a_wait_is_never_zero_even_when_a_session_the_pool_cannot_spare_is_overdue() {
        let (mut state, _, now) = pool(1, &[TIMEOUT * 5]);

        let (expired, wait) = state.reap(1, TIMEOUT, now);

        assert!(expired.is_empty());
        assert!(
            !wait.is_zero(),
            "a zero wait would turn the reaper's timed wait into a spin"
        );
    }

    #[test]
    fn the_most_recently_returned_session_is_the_one_taken() {
        let (mut state, closed, _) = pool(2, &[TIMEOUT, Duration::ZERO]);

        let taken = state.take().expect("a free session");

        drop(taken);
        assert_eq!(closed.load(Ordering::Relaxed), 1);
        assert_eq!(
            state.idle.len(),
            1,
            "the session left behind is the older one, which is the one that may go stale"
        );
    }

    /// The thread's own contract rather than the clock's: it must stop when the pool says so,
    /// or the pool's drop would wait for it forever.
    #[test]
    fn the_reaper_stops_when_the_pool_closes() {
        let (idles, _) = shared(1, &[]);
        let reaper = {
            let idles = Arc::clone(&idles);
            std::thread::spawn(move || reap(&idles, 1, TIMEOUT))
        };

        idles.stop_reaping();

        reaper
            .join()
            .expect("the reaper stops rather than waiting out its timeout");
    }

    /// The other half of the same contract: the thread really does close what it finds, not
    /// merely compute that it should.
    #[test]
    fn the_reaper_closes_what_has_gone_stale() {
        let (idles, closed) = shared(2, &[TIMEOUT * 2, TIMEOUT * 2]);
        let reaper = {
            let idles = Arc::clone(&idles);
            std::thread::spawn(move || reap(&idles, 1, Duration::from_millis(1)))
        };

        while closed.load(Ordering::Relaxed) < 1 {
            std::hint::spin_loop();
        }
        idles.stop_reaping();
        reaper.join().expect("the reaper stops");

        let state = super::lock(&idles.state);
        assert_eq!(state.open, 1, "the pool shrank to its floor");
        assert_eq!(state.idle.len(), 1);
    }

    #[test]
    fn a_session_handed_back_is_free_again() {
        let (idles, _) = shared(1, &[]);

        idles.give_back(Fake(Arc::new(AtomicUsize::new(0))));

        assert!(super::lock(&idles.state).take().is_some());
    }
}
