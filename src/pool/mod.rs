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
//! [`sessions`] holds the pool itself and [`reaper`] the sessions it is not using;
//! [`slot`] is the other half: a caller that has stopped
//! waiting can interrupt the command it started. The interrupt lives beside the pool rather
//! than in the async layer because the hazard it solves is the pool's -- an interrupt must
//! never land on the *next* command to borrow the session -- and only code holding the
//! checkout can rule that out.

mod reaper;
mod sessions;
mod slot;

pub(crate) use sessions::SessionPool;
pub(crate) use slot::CommandSlot;

use std::sync::{Mutex, MutexGuard, PoisonError};

/// The pool's mutexes guard a plain list and a small enum; a panic under either leaves both
/// sound, and refusing them would strand every other caller. So the poison is shrugged off, as
/// `limits::process` does with its own.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
