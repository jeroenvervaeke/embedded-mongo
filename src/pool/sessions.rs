//! The pool itself: which session a caller gets, and when the pool opens another.
//!
//! The pool is elastic or fixed depending on the [`Concurrency`] it was opened with, and the
//! difference is only in the numbers: a fixed pool has `min == max` and no idle timeout, so the
//! growth branch below never fires and no reaper is started. One implementation therefore
//! covers both, and the fixed case pays nothing for the elastic one.

use super::reaper::{Idles, start_reaper};
use super::slot::Disarm;
use super::{CommandSlot, lock};
use crate::{Concurrency, Error, Result};
use embedded_mongodb_sys::{Client as NativeClient, Killer, Session as NativeSession};
use std::sync::{Arc, PoisonError};
use std::thread::JoinHandle;

pub(crate) struct SessionPool {
    limits: Concurrency,
    idles: Arc<Idles<NativeSession>>,
    /// The thread that closes sessions idle past the timeout, joined before the pool's own
    /// sessions drop. `None` for a fixed pool, which has nothing to reap, and for one whose
    /// reaper thread could not be started.
    reaper: Option<JoinHandle<()>>,
}

impl SessionPool {
    /// Opens the pool's floor of sessions and, for an elastic pool, starts the thread that
    /// closes the ones that go idle.
    ///
    /// The floor cannot be zero -- [`Concurrency`] guarantees at least one -- because a pool
    /// with no sessions and no caller to grow it would leave every
    /// [`checkout`](SessionPool::checkout) waiting forever.
    pub(crate) fn open(runtime: &NativeClient, limits: Concurrency) -> Result<Self> {
        let mut sessions = Vec::with_capacity(limits.min() as usize);
        for _ in 0..limits.min() {
            sessions.push(runtime.open_session()?);
        }
        let idles = Arc::new(Idles::new(sessions, limits.min()));
        let reaper = limits
            .idle_timeout()
            .and_then(|timeout| start_reaper(&idles, limits.min(), timeout));
        Ok(Self {
            limits,
            idles,
            reaper,
        })
    }

    /// Runs one command on a borrowed session, letting `slot` interrupt it while it runs.
    ///
    /// Answers `None` when the command was cancelled before a session ever picked it up: there
    /// is then nothing to run and nobody to tell, so it is not run.
    ///
    /// The arming and the disarming both happen while the checkout is still held, which is the
    /// whole of the safety argument. A cancel that arrives after the command finished finds the
    /// slot `Finished` and does nothing; a cancel that arrives during it holds the slot's mutex
    /// while it kills, and the disarm below cannot proceed until that is done. So an interrupt
    /// can never reach the session after it has gone back into the pool and been taken by
    /// somebody else.
    pub(crate) fn run(
        &self,
        runtime: &NativeClient,
        slot: &CommandSlot,
        database: &str,
        command: &[u8],
    ) -> Option<Result<Vec<u8>>> {
        let session = match self.checkout(runtime) {
            Ok(session) => session,
            Err(error) => return Some(Err(error)),
        };
        let killer = session.killer();
        if !slot.start(Box::new(move || {
            // Nothing to do about a failure: the caller is gone, and a session that refuses to
            // be interrupted simply finishes its command as it would have anyway.
            let _ = killer.kill();
        })) {
            return None;
        }
        // Declared after the checkout so that it drops first: the interrupt has to be given up
        // while the session is still held, however the command below ends. See [`Disarm`].
        let _disarm = Disarm::new(slot);
        let result = session.run_command(database, command);
        Some(result.map_err(Error::from))
    }

    /// Takes a session: a free one, a newly opened one while the pool is under its ceiling, or
    /// the next one to come back. The returned guard hands it back on drop.
    fn checkout<'pool>(&'pool self, runtime: &NativeClient) -> Result<Checkout<'pool>> {
        let mut state = lock(&self.idles.state);
        loop {
            if let Some(session) = state.take() {
                return Ok(self.holding(session));
            }
            if state.open < self.limits.max() {
                // Reserved before the lock is dropped, so the open below happens with the pool
                // free for other callers and yet cannot overshoot the ceiling.
                state.open += 1;
                drop(state);
                return self.grow(runtime);
            }
            state = self
                .idles
                .returned
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Opens the session a checkout reserved. A failure is answered rather than waited out: an
    /// engine that refuses a session is reporting something the caller should hear, and the
    /// wait it would otherwise fall back into has no bound on it.
    fn grow(&self, runtime: &NativeClient) -> Result<Checkout<'_>> {
        match runtime.open_session() {
            Ok(session) => Ok(self.holding(session)),
            Err(error) => {
                lock(&self.idles.state).open -= 1;
                // The reservation going back is a waiting caller's chance to make one of its
                // own.
                self.idles.returned.notify_one();
                Err(Error::from(error))
            }
        }
    }

    fn holding(&self, session: NativeSession) -> Checkout<'_> {
        Checkout {
            pool: self,
            session: Some(session),
        }
    }
}

/// Joins the reaper before the sessions it shares go anywhere: the thread holds a reference to
/// every free session, and the engine handle beside this pool is closed the moment the last
/// session is gone.
impl Drop for SessionPool {
    fn drop(&mut self) {
        let Some(reaper) = self.reaper.take() else {
            return;
        };
        self.idles.stop_reaping();
        // A reaper that panicked is a reaper that has stopped, which is all this waits for.
        let _ = reaper.join();
    }
}

/// A session on loan from the pool. Returns to the pool when dropped.
struct Checkout<'pool> {
    pool: &'pool SessionPool,
    session: Option<NativeSession>,
}

impl Checkout<'_> {
    fn session(&self) -> &NativeSession {
        self.session
            .as_ref()
            .expect("a checked-out session is present until it is returned")
    }

    fn killer(&self) -> Killer {
        self.session().killer()
    }

    fn run_command(
        &self,
        database: &str,
        command: &[u8],
    ) -> std::result::Result<Vec<u8>, embedded_mongodb_sys::Error> {
        // The pool is unlocked for the whole command, so other threads run theirs on other
        // sessions meanwhile -- which is the parallelism the pool exists to allow.
        self.session().run_command(database, command)
    }
}

impl Drop for Checkout<'_> {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            self.pool.idles.give_back(session);
        }
    }
}
