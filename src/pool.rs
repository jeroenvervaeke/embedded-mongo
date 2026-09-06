//! The sessions a [`Client`](crate::blocking::Client) runs commands on, and the interrupt that
//! makes a command abandonable.
//!
//! A session runs one command at a time, so a client shared across threads needs a pool: a
//! caller takes one for the length of its command and hands it back. Callers past the pool wait
//! here for one to come free rather than failing -- the pool bounds how many commands run at
//! once, not how many may be asked for. This is the safe-Rust counterpart of what a lock in the
//! engine would otherwise be, and it is here so that it can be read, tested and changed without
//! touching the native library.
//!
//! [`CommandSlot`] is the other half: it lets a caller that has stopped waiting interrupt the
//! command it started. It lives here rather than in the async layer because the hazard it
//! solves is the pool's -- an interrupt must never land on the *next* command to borrow the
//! session -- and only code holding the checkout can rule that out.

use crate::{Error, Result};
use embedded_mongodb_sys::{Client as NativeClient, Killer, Session as NativeSession};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

pub(crate) struct SessionPool {
    idle: Mutex<Vec<NativeSession>>,
    returned: Condvar,
}

/// One dispatched command, and whether it may still be interrupted.
///
/// The states are walked once, in order: a command is `Waiting` while queued, `Running` while a
/// session is executing it, and then either `Finished` or `Cancelled`. Every transition happens
/// under the one mutex, which is what makes the interrupt safe -- see [`CommandSlot::cancel`].
pub(crate) struct CommandSlot {
    state: Mutex<Slot>,
}

enum Slot {
    Waiting,
    Running(Interrupt),
    Finished,
    Cancelled,
}

/// What a slot calls to stop the command it is holding open.
///
/// A boxed closure rather than the [`Killer`] itself, so the state machine below can be tested
/// without an engine: the transition that matters most -- that a cancel arriving after the
/// command finished interrupts *nothing* -- is a property of these states alone, and proving it
/// should not require racing a real session.
type Interrupt = Box<dyn Fn() + Send>;

impl SessionPool {
    /// Opens `count` sessions. The caller has already turned a requested pool size into a
    /// count that cannot be zero -- a pool with no sessions would leave every
    /// [`checkout`](SessionPool::checkout) waiting forever.
    pub(crate) fn open(runtime: &NativeClient, count: u32) -> Result<Self> {
        let mut idle = Vec::with_capacity(count as usize);
        for _ in 0..count {
            idle.push(runtime.open_session()?);
        }
        Ok(Self {
            idle: Mutex::new(idle),
            returned: Condvar::new(),
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
        slot: &CommandSlot,
        database: &str,
        command: &[u8],
    ) -> Option<Result<Vec<u8>>> {
        let session = self.checkout();
        let killer = session.killer();
        if !slot.start(Box::new(move || {
            // Nothing to do about a failure: the caller is gone, and a session that refuses to
            // be interrupted simply finishes its command as it would have anyway.
            let _ = killer.kill();
        })) {
            return None;
        }
        let result = session.run_command(database, command);
        slot.finish();
        Some(result.map_err(Error::from))
    }

    /// Takes a session, waiting while every one is running a command. The returned guard hands
    /// the session back on drop.
    fn checkout(&self) -> Checkout<'_> {
        let mut idle = lock(&self.idle);
        while idle.is_empty() {
            idle = self
                .returned
                .wait(idle)
                .unwrap_or_else(PoisonError::into_inner);
        }
        let session = idle.pop().expect("waited until a session was free");
        Checkout {
            pool: self,
            session: Some(session),
        }
    }
}

impl CommandSlot {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(Slot::Waiting),
        }
    }

    /// Interrupts the command, if one is running, and refuses it a session if one has not yet
    /// picked it up. Called when the caller stops waiting for the answer.
    ///
    /// The kill happens under the slot's mutex on purpose: it is what stops the running command
    /// from finishing, returning its session and having it taken by another caller in between
    /// this deciding to interrupt and actually doing so.
    pub(crate) fn cancel(&self) {
        let mut state = lock(&self.state);
        if let Slot::Running(interrupt) = &*state {
            interrupt();
        }
        *state = Slot::Cancelled;
    }

    /// Hands the slot the means to interrupt the session now running its command. Answers
    /// whether the command should run at all -- `false` once it has been cancelled.
    fn start(&self, interrupt: Interrupt) -> bool {
        let mut state = lock(&self.state);
        match *state {
            Slot::Waiting => {
                *state = Slot::Running(interrupt);
                true
            }
            _ => false,
        }
    }

    /// Gives up the interrupt. From here a cancel does nothing, which is correct: the command
    /// is over, and the session is about to belong to somebody else.
    fn finish(&self) {
        let mut state = lock(&self.state);
        if matches!(*state, Slot::Running(_)) {
            *state = Slot::Finished;
        }
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
            lock(&self.pool.idle).push(session);
            self.pool.returned.notify_one();
        }
    }
}

/// The pool's mutexes guard a plain list and a small enum; a panic under either leaves both
/// sound, and refusing them would strand every other caller. So the poison is shrugged off, as
/// `limits::process` does with its own.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::{CommandSlot, Interrupt};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An interrupt that counts rather than kills, so the transitions can be checked without a
    /// session to interrupt.
    fn counting() -> (Interrupt, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        (
            Box::new(move || {
                counted.fetch_add(1, Ordering::Relaxed);
            }),
            calls,
        )
    }

    #[test]
    fn a_running_command_is_interrupted_when_the_caller_stops_waiting() {
        let slot = CommandSlot::new();
        let (interrupt, calls) = counting();

        assert!(
            slot.start(interrupt),
            "a fresh slot should accept a command"
        );
        slot.cancel();

        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    /// The safety property the whole design turns on. A session goes back into the pool the
    /// moment its command ends, so an interrupt that arrived a moment later would land on
    /// whatever borrowed it next. Disarming first is what rules that out.
    #[test]
    fn a_command_that_already_finished_is_not_interrupted() {
        let slot = CommandSlot::new();
        let (interrupt, calls) = counting();

        assert!(slot.start(interrupt));
        slot.finish();
        slot.cancel();

        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "cancelling after the command finished must interrupt nothing -- the session may \
             already be running somebody else's command"
        );
    }

    #[test]
    fn a_command_cancelled_before_it_starts_is_never_run() {
        let slot = CommandSlot::new();
        let (interrupt, calls) = counting();

        slot.cancel();

        assert!(
            !slot.start(interrupt),
            "a slot cancelled while queued should refuse to run its command at all"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "a command that never ran has nothing to interrupt"
        );
    }

    /// Cancelling twice is ordinary: the drop guard fires on every exit, including after a
    /// caller has already given up.
    #[test]
    fn cancelling_twice_interrupts_once() {
        let slot = CommandSlot::new();
        let (interrupt, calls) = counting();

        assert!(slot.start(interrupt));
        slot.cancel();
        slot.cancel();

        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn finishing_a_cancelled_command_does_not_revive_it() {
        let slot = CommandSlot::new();
        let (interrupt, calls) = counting();

        assert!(slot.start(interrupt));
        slot.cancel();
        slot.finish();
        slot.cancel();

        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "a disarm arriving after the cancel must not put the slot back in a killable state"
        );
    }
}
