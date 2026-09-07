//! Closing the engine once every worker has let go of it.
//!
//! Kept apart from the dispatch loop because it answers a different question: dispatch is
//! about getting one command onto a worker, this is about the one moment no command is on
//! any of them.

use crate::{Error, Result, client::Client as BlockingClient};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::oneshot;

/// The rendezvous that turns N retiring workers into one engine close.
///
/// Closing needs what no single worker has: certainty that every other worker has let go of
/// the engine. Each worker deposits its handle here before counting itself out, so the worker
/// whose decrement hits zero knows every handle is in the vault, drains it down to one, and
/// closes the engine through that -- on a worker thread, which is where blocking work lives.
pub(super) struct Shutdown {
    remaining: AtomicUsize,
    handles: Mutex<Vec<Arc<BlockingClient>>>,
    /// Reached only by the single worker whose decrement hits zero, so it never contends. The
    /// `Mutex` is here only to get interior mutability for the `take` through the shared `Arc`,
    /// not to guard against a race.
    reply: Mutex<Option<oneshot::Sender<Result<()>>>>,
}

impl Shutdown {
    pub(super) fn new(workers: usize, reply: oneshot::Sender<Result<()>>) -> Self {
        Self {
            remaining: AtomicUsize::new(workers),
            handles: Mutex::new(Vec::with_capacity(workers)),
            reply: Mutex::new(Some(reply)),
        }
    }

    /// Called once by every worker as it retires. The deposit has to precede the decrement:
    /// the last worker's claim to the engine is that the vault is complete, and a worker that
    /// counted out before depositing would leave that claim briefly false.
    pub(super) fn retire(&self, client: Arc<BlockingClient>) {
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

/// A poisoned lock here means a worker panicked with the guard held; the data under every one
/// of these locks is still sound -- a queue, a vault of handles, a reply slot -- and refusing
/// it would strand every other worker, so the poison is shrugged off the way
/// `limits::process` does.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
