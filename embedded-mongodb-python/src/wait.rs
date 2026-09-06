//! Driving one future to completion on the calling thread.
//!
//! The async engine is opened from `AsyncNativeClient::__init__`, which Python calls
//! synchronously -- `AsyncMongoClient(...)` is a constructor in PyMongo's API, not something to
//! await -- so the open has to finish before it returns. There is no runtime to hand it to
//! either: this crate's async layer runs on `tokio::sync` alone and starts its own threads, so
//! nothing here has an executor and pulling one in to poll a single future would be a
//! dependency bought for one call.
//!
//! Park and unpark is the whole of it. The engine's open resolves when a worker thread reports
//! back, so the thread that opened it has nothing to do but wait, which is what a Python
//! constructor is doing anyway.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

/// Runs `future` on this thread, parking between polls. Blocks, so callers holding the GIL
/// must release it first.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        // A wake that lands between the poll and here is not lost: `unpark` on a thread that is
        // not parked yet leaves a permit behind, and the next `park` consumes it and returns.
        thread::park();
    }
}

struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

#[cfg(test)]
mod tests {
    use super::block_on;
    use std::task::Poll;
    use std::thread;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[test]
    fn a_ready_future_needs_no_parking() {
        assert_eq!(block_on(std::future::ready(7)), 7);
    }

    /// The case the engine's open actually is: the answer arrives from another thread, so the
    /// waker has to be the thing that ends the wait.
    #[test]
    fn a_future_woken_from_another_thread_completes() {
        let (sender, receiver) = oneshot::channel();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            sender.send("opened").expect("the receiver is still parked");
        });

        assert_eq!(
            block_on(receiver).expect("the sender was not dropped"),
            "opened"
        );
    }

    /// A wake that arrives while the poll is still running must not be missed: `park` has to
    /// return immediately on the permit `unpark` left behind, or this hangs rather than fails.
    #[test]
    fn a_wake_racing_the_poll_is_not_lost() {
        let mut polled = false;
        let future = std::future::poll_fn(move |context| {
            if polled {
                return Poll::Ready(());
            }
            polled = true;
            // Woken from inside the poll, which is as tight as the race gets.
            context.waker().wake_by_ref();
            Poll::Pending
        });

        block_on(future);
    }
}
