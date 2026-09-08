//! The interrupt that lets a caller stop the command it started.

use super::lock;
use std::sync::Mutex;

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
/// A boxed closure rather than the [`Killer`](embedded_mongodb_sys::Killer) itself, so the
/// state machine below can be tested without an engine: the transition that matters most --
/// that a cancel arriving after the command finished interrupts *nothing* -- is a property of
/// these states alone, and proving it should not require racing a real session.
type Interrupt = Box<dyn Fn() + Send>;

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
    pub(super) fn start(&self, interrupt: Interrupt) -> bool {
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

/// Gives the interrupt up, however the command it was armed for ends.
///
/// A guard rather than a call after the command, because the command can unwind -- the async
/// layer catches exactly that so a panicking command fails only itself -- and an interrupt left
/// armed on a session already back in the pool would kill whoever borrowed it next.
pub(super) struct Disarm<'slot>(&'slot CommandSlot);

impl<'slot> Disarm<'slot> {
    pub(super) fn new(slot: &'slot CommandSlot) -> Self {
        Self(slot)
    }
}

impl Drop for Disarm<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandSlot, Disarm, Interrupt};
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

    /// The guard's whole reason for being. A command that unwinds still ends, and the session
    /// it was running on goes straight back into the pool -- so an interrupt still armed at
    /// that point would land on whatever borrowed the session next.
    #[test]
    fn a_command_that_unwound_is_not_interrupted_either() {
        let slot = CommandSlot::new();
        let (interrupt, calls) = counting();

        assert!(slot.start(interrupt));
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _disarm = Disarm::new(&slot);
            panic!("a command that fails the way the async layer expects one to");
        }));
        slot.cancel();

        assert!(
            unwound.is_err(),
            "the panic should have been caught, not avoided"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "an interrupt left armed by an unwind would kill the next command on that session"
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
