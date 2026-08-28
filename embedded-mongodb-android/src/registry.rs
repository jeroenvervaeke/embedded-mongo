use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use embedded_mongodb_sys::Client;

use crate::error::{BridgeError, Result};
use crate::handle::{Counter, HandleId};

/// The clients this process has open, keyed by an id that is never reused.
///
/// # Threading
///
/// Three locks, in a fixed order, and no native call happens under the first:
///
/// * The registry's own `Mutex` is held only long enough to allocate an id or to look one up
///   and clone its `Arc`. It is never held across a call into the engine, so a slow command
///   cannot block `open` or `close` on an unrelated handle.
/// * Each entry's `RwLock` decides what may run on that client. `run_command` takes it
///   *shared*: any number of Java threads may be inside `run_command` on the same handle at
///   once, which is what the engine expects -- it holds a `ClientStrand` guard internally and
///   serializes them itself. Turning that lock exclusive would serialize the callers twice.
/// * `close` takes the same lock *exclusive*, which is the whole close guarantee: acquiring
///   it waits until every in-flight command on that handle has returned, and it is held
///   across the native shutdown, so no command can start while the engine is stopping. A
///   command that was already holding the `Arc` when close removed it from the map then wakes
///   to find the slot empty and fails with [`ErrorCode::ClosedHandle`], rather than touching
///   a destroyed client. There is no path from inside the engine back into these locks, so
///   the wait cannot deadlock.
///
/// [`ErrorCode::ClosedHandle`]: crate::ErrorCode::ClosedHandle
pub struct Registry<C> {
    state: Mutex<State<C>>,
}

/// What the registry needs of a client, so the handle rules can be tested without starting
/// the engine -- only one engine runtime may exist per process, which would otherwise leave
/// every multi-handle case untestable.
pub trait EmbeddedClient: Send + Sync {
    fn run_command(&self, database: &str, command: &[u8]) -> Result<Vec<u8>>;

    /// Takes `self` because shutting the engine down consumes the client.
    fn close(self) -> Result<()>
    where
        Self: Sized;
}

/// The process-wide registry the JNI entry points use.
pub fn registry() -> &'static Registry<Client> {
    static REGISTRY: Registry<Client> = Registry::new();
    &REGISTRY
}

impl<C: EmbeddedClient> Registry<C> {
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(State {
                counter: Counter::Unstarted,
                entries: BTreeMap::new(),
            }),
        }
    }

    /// Takes ownership of an open client and returns the id Java will quote back.
    ///
    /// Ids are never reused, not even after `close`, so a stale one can never name a client
    /// that happens to be open now.
    pub fn insert(&self, client: C) -> Result<HandleId> {
        let mut state = self.state();
        let Some(id) = state.counter.take_next() else {
            // Released before `spent` shuts the engine down: no native call may run under
            // the registry lock.
            drop(state);
            return Err(spent(client));
        };
        state.entries.insert(id, Arc::new(Entry::new(client)));
        Ok(id)
    }

    pub fn run_command(&self, id: HandleId, database: &str, command: &[u8]) -> Result<Vec<u8>> {
        let entry = self.lookup(id)?;
        let client = entry.read();
        let Some(client) = client.as_ref() else {
            return Err(closed(id));
        };
        client.run_command(database, command)
    }

    /// Closes the client and retires its id. A second call, or a call racing the first, finds
    /// nothing to remove and fails cleanly.
    pub fn close(&self, id: HandleId) -> Result<()> {
        // Removing under the registry lock is what makes double close safe: exactly one
        // caller can win, whatever the other threads are doing.
        let Some(entry) = self.state().entries.remove(&id) else {
            return Err(closed(id));
        };
        let mut slot = entry.write();
        let Some(client) = slot.take() else {
            return Err(closed(id));
        };
        // Still holding the exclusive guard: commands that reached the entry before it was
        // removed wait here rather than run against an engine that is shutting down.
        client.close()
    }

    /// How many clients are open. Only the tests have a use for this.
    pub fn len(&self) -> usize {
        self.state().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<C: EmbeddedClient> Default for Registry<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddedClient for Client {
    fn run_command(&self, database: &str, command: &[u8]) -> Result<Vec<u8>> {
        Client::run_command(self, database, command).map_err(BridgeError::from)
    }

    fn close(self) -> Result<()> {
        Client::close(self).map_err(BridgeError::from)
    }
}

struct State<C> {
    counter: Counter,
    entries: BTreeMap<HandleId, Arc<Entry<C>>>,
}

struct Entry<C> {
    /// `None` once `close` has taken the client out, which only happens after the entry has
    /// already left the map -- so a command can observe it only by having raced the removal.
    client: RwLock<Option<C>>,
}

impl<C> Entry<C> {
    fn new(client: C) -> Self {
        Self {
            client: RwLock::new(Some(client)),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, Option<C>> {
        self.client.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Option<C>> {
        self.client.write().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<C: EmbeddedClient> Registry<C> {
    /// Poison is recovered from rather than reported, in all three accessors. A panic under
    /// one of these locks is already on its way to Java as an `EmbeddedMongoException`, and
    /// nothing it can interrupt leaves the guarded state inconsistent: readers do not touch
    /// the `Option` at all, and the only writer replaces it with `None`, which is exactly the
    /// "closed" state. Reporting poison instead would brick the whole binding for the life of
    /// the process over a failure that was already reported once.
    fn state(&self) -> MutexGuard<'_, State<C>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Clones the `Arc` and releases the registry lock, so the native call that follows runs
    /// without it.
    fn lookup(&self, id: HandleId) -> Result<Arc<Entry<C>>> {
        let Some(entry) = self.state().entries.get(&id).map(Arc::clone) else {
            return Err(closed(id));
        };
        Ok(entry)
    }
}

fn closed(id: HandleId) -> BridgeError {
    BridgeError::closed_handle(format!(
        "embedded MongoDB handle {id} is unknown or already closed"
    ))
}

/// Reports that no id could be issued, having first shut the client down.
///
/// `Drop` would close it too, but discards whatever the engine said on the way out, and by
/// this point `open` has already taken this process's one and only runtime -- so this is the
/// last chance to report anything about it at all.
fn spent<C: EmbeddedClient>(client: C) -> BridgeError {
    let detail = match client.close() {
        Ok(()) => String::new(),
        Err(failure) => format!("; closing it again failed: {failure}"),
    };
    BridgeError::closed_handle(format!(
        "embedded MongoDB handle ids are exhausted; this process cannot open another \
         client{detail}"
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::{Condvar, PoisonError};
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::ErrorCode;

    /// Long enough that a loaded machine cannot mistake scheduling for a deadlock.
    const PATIENCE: Duration = Duration::from_secs(30);

    #[test]
    fn forwards_the_command_and_returns_the_response() {
        let registry = Registry::new();
        let probe = Arc::new(Probe::default());
        let id = insert(&registry, &probe, 1);

        let response = registry
            .run_command(id, "admin", b"ping")
            .expect("the fake client always answers");

        assert_eq!(response, b"admin/ping");
        assert_eq!(probe.snapshot().commands, 1);
    }

    #[test]
    fn issues_increasing_ids_and_never_reuses_a_closed_one() {
        let registry = Registry::new();
        let probe = Arc::new(Probe::default());
        let mut issued = Vec::new();

        for _ in 0..64 {
            let id = insert(&registry, &probe, 1);
            issued.push(id);
            close(&registry, id);
        }

        let mut sorted = issued.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, issued, "ids must be unique and monotonic");
        assert!(registry.is_empty());
    }

    #[test]
    fn rejects_a_handle_this_process_never_issued() {
        let registry = Registry::new();
        let probe = Arc::new(Probe::default());
        let live = insert(&registry, &probe, 1);

        let forged = HandleId::new(live.get() + 4_242).expect("a positive id");
        assert_closed_handle(registry.run_command(forged, "admin", b"ping"));
        assert_closed_handle(registry.close(forged));
    }

    #[test]
    fn refuses_every_call_on_a_closed_handle() {
        let registry = Registry::new();
        let probe = Arc::new(Probe::default());
        let id = insert(&registry, &probe, 1);
        close(&registry, id);

        assert_closed_handle(registry.run_command(id, "admin", b"ping"));
        assert_closed_handle(registry.close(id));
        assert_eq!(
            probe.snapshot().closes,
            1,
            "the client is closed exactly once"
        );
    }

    #[test]
    fn closing_twice_concurrently_closes_the_client_once() {
        let registry = Arc::new(Registry::new());
        let probe = Arc::new(Probe::default());
        let id = insert(&registry, &probe, 1);

        let closers: Vec<_> = (0..8)
            .map(|_| {
                let registry = Arc::clone(&registry);
                thread::spawn(move || registry.close(id))
            })
            .collect();
        let succeeded = closers
            .into_iter()
            .map(|closer| closer.join().expect("no close thread may panic"))
            .filter(Result::is_ok)
            .count();

        assert_eq!(succeeded, 1, "exactly one caller may win the close");
        assert_eq!(probe.snapshot().closes, 1);
    }

    #[test]
    fn runs_commands_from_several_threads_at_once() {
        const THREADS: usize = 4;
        let registry = Arc::new(Registry::new());
        let probe = Arc::new(Probe::default());
        // The fake refuses to return until all four callers are inside it, so this can only
        // pass if the entry lock lets commands overlap.
        let id = insert(&registry, &probe, THREADS);

        let callers: Vec<_> = (0..THREADS)
            .map(|_| {
                let registry = Arc::clone(&registry);
                thread::spawn(move || registry.run_command(id, "admin", b"ping"))
            })
            .collect();
        for caller in callers {
            let outcome = caller.join().expect("no command thread may panic");
            assert!(outcome.is_ok(), "{outcome:?}");
        }

        assert_eq!(probe.snapshot().peak_in_flight, THREADS);
    }

    #[test]
    fn close_waits_for_a_command_in_flight() {
        let registry = Arc::new(Registry::new());
        let probe = Arc::new(Probe::default());
        // The command returns once one further party arrives -- this thread, below.
        let id = insert(&registry, &probe, 2);

        let commander = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.run_command(id, "admin", b"ping"))
        };
        assert!(probe.wait_until(|meeting| meeting.in_flight == 1));

        let (sender, closed) = mpsc::channel();
        let closer = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || {
                let _ = sender.send(registry.close(id));
            })
        };
        // Corroboration, not proof: a slow machine can make this pass for the wrong reason,
        // but it can never fail for one -- a close that did not wait would answer at once.
        assert!(
            closed.recv_timeout(Duration::from_millis(200)).is_err(),
            "close must not finish while a command is still running"
        );

        probe.arrive();
        let outcome = closed
            .recv_timeout(PATIENCE)
            .expect("close must finish once the command returns");
        assert!(outcome.is_ok(), "{outcome:?}");
        let commanded = commander.join().expect("no command thread may panic");
        assert!(commanded.is_ok(), "{commanded:?}");
        closer.join().expect("no close thread may panic");
        // This, not the timeout above, is the proof: a close that had skipped the exclusive
        // guard would have recorded the command still running.
        assert_eq!(
            probe.snapshot().in_flight_at_close,
            vec![0],
            "the engine must be shut down with nothing running on it"
        );
    }

    #[test]
    fn a_command_in_flight_does_not_block_another_handle() {
        let registry = Arc::new(Registry::new());
        let busy = Arc::new(Probe::default());
        let idle = Arc::new(Probe::default());
        let held = insert(&registry, &busy, 2);

        let commander = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.run_command(held, "admin", b"ping"))
        };
        assert!(busy.wait_until(|meeting| meeting.in_flight == 1));

        // The registry lock is not held across the native call, so this must not wait for the
        // command above to finish.
        let (sender, done) = mpsc::channel();
        let other = {
            let registry = Arc::clone(&registry);
            let idle = Arc::clone(&idle);
            thread::spawn(move || {
                let id = registry.insert(FakeClient::new(&idle, 1));
                let _ = sender.send(id.and_then(|id| registry.close(id)));
            })
        };
        let outcome = done
            .recv_timeout(PATIENCE)
            .expect("an unrelated handle must not wait behind a running command");
        assert!(outcome.is_ok(), "{outcome:?}");

        busy.arrive();
        let commanded = commander.join().expect("no command thread may panic");
        assert!(commanded.is_ok(), "{commanded:?}");
        other.join().expect("no insert thread may panic");
    }

    #[test]
    fn a_command_racing_a_close_never_reports_anything_but_a_closed_handle() {
        let registry = Arc::new(Registry::new());
        for _ in 0..256 {
            let probe = Arc::new(Probe::default());
            let id = insert(&registry, &probe, 1);

            let commander = {
                let registry = Arc::clone(&registry);
                thread::spawn(move || registry.run_command(id, "admin", b"ping"))
            };
            let closer = {
                let registry = Arc::clone(&registry);
                thread::spawn(move || registry.close(id))
            };

            match commander.join().expect("no command thread may panic") {
                Ok(response) => assert_eq!(response, b"admin/ping"),
                Err(error) => assert_eq!(error.code(), ErrorCode::ClosedHandle, "{error}"),
            }
            assert!(closer.join().expect("no close thread may panic").is_ok());
            assert_eq!(probe.snapshot().closes, 1);
        }
        assert!(registry.is_empty());
    }

    #[test]
    fn refuses_to_issue_an_id_twice_once_this_process_has_spent_its_block() {
        let registry = Registry::new();
        let probe = Arc::new(Probe::default());
        let first = insert(&registry, &probe, 1);

        let Counter::Next { tag, .. } = registry.state().counter else {
            panic!("the counter must be running after the first insert");
        };
        registry.state().counter = Counter::Next {
            tag,
            sequence: u32::MAX,
        };
        let last = insert(&registry, &probe, 1);
        assert!(last.get() > first.get());

        // Wrapping would hand out the first id again, which some Java object may still hold.
        let error = registry
            .insert(FakeClient::new(&probe, 1))
            .expect_err("the id space is exhausted");
        assert_eq!(error.code(), ErrorCode::ClosedHandle);
        assert!(error.message().contains("exhausted"), "{error}");
        assert_eq!(
            probe.snapshot().closes,
            1,
            "the client that could not be registered must still be shut down"
        );
    }

    #[test]
    fn a_handle_saved_by_an_earlier_process_names_nothing_here() {
        let probe = Arc::new(Probe::default());
        // Two registries stand in for two runs of the process: each draws its own tag.
        let earlier = Registry::new();
        let restarted = Registry::new();

        let stale = insert(&earlier, &probe, 1);
        let fresh = insert(&restarted, &probe, 1);

        // Deterministic: a bare counter would make both of these 1.
        assert!(
            stale.get() > i64::from(u32::MAX) && fresh.get() > i64::from(u32::MAX),
            "ids must carry a per-process tag, not start at 1: {stale} and {fresh}"
        );
        // Probabilistic, at one chance in 2^31 of a shared tag -- which is the design.
        assert_ne!(stale, fresh, "two runs must not issue the same first id");
        assert_closed_handle(restarted.run_command(stale, "admin", b"ping"));
        assert_closed_handle(restarted.close(stale));
    }

    fn insert(registry: &Registry<FakeClient>, probe: &Arc<Probe>, rendezvous: usize) -> HandleId {
        registry
            .insert(FakeClient::new(probe, rendezvous))
            .expect("a fresh registry always has ids left")
    }

    fn close(registry: &Registry<FakeClient>, id: HandleId) {
        registry.close(id).expect("the handle is open");
    }

    fn assert_closed_handle<T: std::fmt::Debug>(outcome: Result<T>) {
        let error = outcome.expect_err("the handle is not usable");
        assert_eq!(error.code(), ErrorCode::ClosedHandle, "{error}");
    }

    /// A client that reports what the registry let happen, and can be held inside a command
    /// for as long as a test needs.
    struct FakeClient {
        probe: Arc<Probe>,
        /// How many parties must reach the rendezvous before a command may return; `1` means
        /// the command itself is enough and it never blocks.
        rendezvous: usize,
    }

    #[derive(Clone, Debug, Default)]
    struct Meeting {
        arrived: usize,
        in_flight: usize,
        peak_in_flight: usize,
        commands: usize,
        closes: usize,
        in_flight_at_close: Vec<usize>,
    }

    #[derive(Default)]
    struct Probe {
        meeting: Mutex<Meeting>,
        progress: Condvar,
    }

    impl FakeClient {
        fn new(probe: &Arc<Probe>, rendezvous: usize) -> Self {
            Self {
                probe: Arc::clone(probe),
                rendezvous,
            }
        }
    }

    impl EmbeddedClient for FakeClient {
        fn run_command(&self, database: &str, command: &[u8]) -> Result<Vec<u8>> {
            self.probe.enter();
            self.probe.arrive();
            // `arrived` only ever grows, so a caller that is woken late still sees that the
            // rendezvous happened. Comparing against `in_flight` instead would let a thread
            // sleep through the moment the others were all inside.
            let met = self
                .probe
                .wait_until(|meeting| meeting.arrived >= self.rendezvous);
            self.probe.leave();
            if !met {
                return Err(BridgeError::invalid_argument(
                    "the registry serialized commands that were supposed to overlap",
                ));
            }
            Ok(format!("{database}/{}", String::from_utf8_lossy(command)).into_bytes())
        }

        fn close(self) -> Result<()> {
            self.probe.record_close();
            Ok(())
        }
    }

    impl Probe {
        fn snapshot(&self) -> Meeting {
            self.meeting().clone()
        }

        /// Counts one more party at the rendezvous a held command is waiting for. Every
        /// command counts itself; a test adds itself to release a command it is holding.
        fn arrive(&self) {
            self.meeting().arrived += 1;
            self.progress.notify_all();
        }

        /// Returns false rather than hanging when the condition never comes true, so a
        /// regression fails the test instead of stalling it.
        fn wait_until(&self, condition: impl Fn(&Meeting) -> bool) -> bool {
            let Ok((_meeting, timeout)) =
                self.progress
                    .wait_timeout_while(self.meeting(), PATIENCE, |meeting| !condition(meeting))
            else {
                return false;
            };
            !timeout.timed_out()
        }

        fn enter(&self) {
            let mut meeting = self.meeting();
            meeting.in_flight += 1;
            meeting.commands += 1;
            meeting.peak_in_flight = meeting.peak_in_flight.max(meeting.in_flight);
            self.progress.notify_all();
        }

        fn leave(&self) {
            self.meeting().in_flight -= 1;
            self.progress.notify_all();
        }

        fn record_close(&self) {
            let mut meeting = self.meeting();
            meeting.closes += 1;
            let in_flight = meeting.in_flight;
            meeting.in_flight_at_close.push(in_flight);
            self.progress.notify_all();
        }

        fn meeting(&self) -> MutexGuard<'_, Meeting> {
            self.meeting.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }
}
