//! Establishing the free-disk floors on an engine that has just been opened.
//!
//! # Why an open sets a floor nobody asked for
//!
//! The two floors are server parameters, and this engine keeps one runtime for the whole life
//! of the process -- `embedded_mongodb_initialize` runs from a namespace-scope initializer in
//! `embedded-mongodb-sys/cpp/bridge.cc`, so it happens when the library is loaded and never
//! again. A floor therefore belongs to the *process*, not to the [`Client`] that named it, and
//! outlives that client's [`Client::close`] and its drop alike. A client that lowers the floor
//! to 32 MiB and closes would leave the next one -- opened by [`Client::new`], by a caller who
//! asked for nothing -- running on 32 MiB. Silently, because the API names the floor per-open
//! and nothing about that suggests it is process-wide.
//!
//! That is worth more than a tidy test. This engine runs no `DiskSpaceMonitor`, and WiredTiger
//! answers a genuinely full disk with `WT_PANIC`, which MongoDB answers with `fassert` and a
//! process abort no caller can catch. The floor is the only warning an application gets before
//! that, so running on a floor someone else lowered is exactly the failure
//! [`FreeDiskFloor`](super::FreeDiskFloor) exists to prevent.
//!
//! # Why here and not at close
//!
//! Putting the floor back when a client is closed would be symmetric with applying it at open,
//! and would be a guarantee nobody could rely on. A process can be killed, and this one can
//! `fassert` itself, so a restore that runs only sometimes leaves the floor correct only
//! sometimes. Rust's `Drop` does not change that -- and it is worse placed than Kotlin's
//! explicit close would be, because the same drop is what closes the engine the command would
//! have to go through. Establishing the floor at open holds however the last client ended, and
//! holds on the first open of a fresh process just the same.
//!
//! The cost is that an open naming no floor now depends on two more commands, so a MongoDB
//! that renamed a knob fails every open rather than only the opens that asked for a floor.
//! That is the right way round: a knob this library cannot find is a floor it cannot promise,
//! and a loud failure at open beats a silent wrong floor at the index build, on the device
//! where the index build was the thing that had to work.

use super::{
    AdminCommands, FreeDiskFloor, ReportedFloors,
    knobs::{reported_floors, send},
    process::FloorMoves,
};
use crate::{Client, Result};
use std::sync::{Mutex, PoisonError};

/// Puts `requested` in force on an engine that has just opened, or MongoDB's own floors where
/// the caller named none.
///
/// A failure here fails the open. The half-built client is dropped on the way out, which
/// closes the engine behind it -- only one runtime may exist per process, so an engine nobody
/// holds a handle to is one this process could never open a database in again.
///
/// Which is why a floor half applied here is not put back, though
/// [`ProcessLimits::set_free_disk_floor`](crate::ProcessLimits::set_free_disk_floor) does put
/// one back: there is no engine left to read the floors from, and no other client that could
/// have one. `Runtime::Runtime` in `embedded-mongodb-sys/native/engine_runtime.cpp` refuses a
/// second runtime outright -- `uassert` 13180000, "only one embedded MongoDB runtime may be
/// open per process" -- so an open that got as far as this one is the only open there was, and
/// the engine it is dropping is the only engine. Putting the floors back would also mean
/// putting back whatever the client that ran last left behind, which is the very thing this
/// open exists to get away from.
pub(crate) fn establish_free_disk_floor(
    client: &Client,
    requested: Option<FreeDiskFloor>,
) -> Result<()> {
    establish(
        client,
        requested,
        EngineFloorDefaults::process(),
        FloorMoves::process(),
    )
}

fn establish(
    engine: &impl AdminCommands,
    requested: Option<FreeDiskFloor>,
    defaults: &EngineFloorDefaults,
    moves: &FloorMoves,
) -> Result<()> {
    // Under the same lock a move on a running client takes, and around the read as well as the
    // writes. An open racing a `set_free_disk_floor` is the same hazard from the other side:
    // the pair this open leaves behind would otherwise be half its own and half the mover's,
    // and the pair it records as MongoDB's own could be one the mover was half way through --
    // which this process would then hand to every later open, for as long as it lives.
    moves.one_at_a_time(|| {
        // Read unconditionally, and before anything is applied: the first open in a process may
        // well be one that names a floor, and recording afterwards would take that caller's
        // floor for MongoDB's and hand it to every later open that asked for the default.
        let engine_own = defaults.of(engine)?;
        match requested {
            Some(floor) => send(engine, floor.commands()),
            None => send(engine, engine_own.commands()),
        }
    })
}

/// The free-disk floors MongoDB itself starts with, read from the engine once and remembered
/// for the life of the process.
///
/// Read rather than written down as a constant. A constant would make this library the
/// authority on a number it does not own: a MongoDB whose default moved would be quietly
/// overridden with the old one on every open, and the test that pins the default would go on
/// passing because it would be checking this library's constant against itself rather than
/// against the engine.
///
/// The first open in the process is the only moment the defaults are knowable, and it is a
/// reliable one. The floors are server parameters, so nothing can have moved them before an
/// engine exists to move them through, and [`establish`] reads them before it applies anything
/// and before the caller who could move them is handed the client.
///
/// Injected rather than reached for as a static, so that a test gets a fresh one: floors
/// recorded by one test and read by the next are the very defect this exists to fix.
struct EngineFloorDefaults {
    recorded: Mutex<Option<ReportedFloors>>,
}

impl EngineFloorDefaults {
    const fn new() -> Self {
        Self {
            recorded: Mutex::new(None),
        }
    }

    /// The one every open but a test's reaches, because the engine behind it is one too.
    fn process() -> &'static Self {
        static PROCESS: EngineFloorDefaults = EngineFloorDefaults::new();
        &PROCESS
    }

    /// What the floors were before anything moved them, asking `engine` the first time only.
    ///
    /// Reached from [`establish`], which holds the lock that keeps floor movement in order, so
    /// this lock is only ever taken second and the two cannot make a cycle.
    fn of(&self, engine: &impl AdminCommands) -> Result<ReportedFloors> {
        // The read happens under the lock rather than before it, so that "recorded once" is a
        // property of this type and not a loan against the engine's one-runtime rule.
        //
        // A panic can only reach the lock from the read below, which is before anything is
        // written, so there is no half-recorded state for a poisoned lock to protect anyone
        // from -- the alternative to taking it back is failing every later open of a process
        // that already survived the panic.
        let mut recorded = self.recorded.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(floors) = *recorded {
            return Ok(floors);
        }
        let floors = reported_floors(engine)?;
        *recorded = Some(floors);
        Ok(floors)
    }
}

#[cfg(test)]
mod tests {
    use super::{EngineFloorDefaults, establish};
    use crate::{
        Error, IndexBuildFloor, QuerySpillingFloor,
        limits::{
            ReportedFloors,
            fake::{DEFAULT_BYTES, DEFAULT_MEBIBYTES, FakeEngine, floor, floors_set},
            knobs::{INDEX_BUILD_FLOOR, QUERY_SPILLING_FLOOR, send},
            process::{FloorMoves, move_free_disk_floor},
        },
    };
    use std::thread;

    #[test]
    fn a_client_opened_without_a_floor_is_put_on_the_engines_own_floors() {
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES);

        establish(
            &engine,
            None,
            &EngineFloorDefaults::new(),
            &FloorMoves::new(),
        )
        .expect("establishing the floor");

        assert_eq!(
            engine.floors_set(),
            floors_set(DEFAULT_MEBIBYTES, DEFAULT_BYTES)
        );
    }

    #[test]
    fn the_floor_a_caller_named_is_the_one_applied() {
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES);

        establish(
            &engine,
            Some(floor(64)),
            &EngineFloorDefaults::new(),
            &FloorMoves::new(),
        )
        .expect("establishing the floor");

        assert_eq!(engine.floors_set(), floors_set(64, 64 * 1024 * 1024));
    }

    /// The defect this module exists for. The floors are server parameters and the engine is
    /// one runtime per process, so a floor a client lowered is still in force after its close.
    /// A caller who names none must be given MongoDB's floors, not the last client's.
    #[test]
    fn a_floor_left_behind_by_a_closed_client_does_not_reach_the_next_open() {
        let defaults = EngineFloorDefaults::new();
        let moves = FloorMoves::new();
        let lowered = FakeEngine::reporting(DEFAULT_MEBIBYTES);
        establish(&lowered, Some(floor(32)), &defaults, &moves).expect("the first open");

        // The next client opens on an engine still holding the 32 MiB the first one set.
        let next = FakeEngine::reporting(32);
        establish(&next, None, &defaults, &moves).expect("the second open");

        assert_eq!(
            next.floors_set(),
            floors_set(DEFAULT_MEBIBYTES, DEFAULT_BYTES)
        );
    }

    /// The first open in a process may be one that names a floor, so the defaults have to be
    /// recorded before that floor is applied. Recording them afterwards would take the
    /// caller's floor for MongoDB's and hand it to every later open that asked for the default
    /// -- the same defect as inheriting one, moved a step earlier.
    ///
    /// The fake answers `getParameter` with whatever was last set on it, so a read taken after
    /// the write reports 32 and this fails. A fake that answered with a constant could not
    /// tell the two orders apart at all.
    #[test]
    fn the_floors_recorded_are_the_ones_from_before_the_first_caller_moved_them() {
        let defaults = EngineFloorDefaults::new();
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES);

        establish(&engine, Some(floor(32)), &defaults, &FloorMoves::new()).expect("the first open");

        assert_eq!(
            defaults
                .of(&FakeEngine::never_read())
                .expect("the floors were recorded at the first open"),
            engine_own_floors()
        );
    }

    /// Re-reading them at the second open would read the floor the first client left behind,
    /// which is the defect wearing a different hat.
    #[test]
    fn the_engines_own_floors_are_read_once_and_not_asked_for_again() {
        let defaults = EngineFloorDefaults::new();
        let moves = FloorMoves::new();
        establish(
            &FakeEngine::reporting(DEFAULT_MEBIBYTES),
            None,
            &defaults,
            &moves,
        )
        .expect("the first open");

        // Answers the restore but refuses to be read.
        let next = FakeEngine::never_read();
        establish(&next, None, &defaults, &moves).expect("the second open");

        assert_eq!(
            next.floors_set(),
            floors_set(DEFAULT_MEBIBYTES, DEFAULT_BYTES)
        );
    }

    /// The knobs are read back separately and can disagree, and the spilling one is a byte
    /// count that need not be a whole mebibyte -- so an open replays what was read rather than
    /// a floor rounded through [`FreeDiskFloor`](crate::FreeDiskFloor).
    #[test]
    fn floors_the_engine_reported_separately_are_put_back_separately() {
        let engine = FakeEngine::new(DEFAULT_MEBIBYTES, 123_456_789);

        establish(
            &engine,
            None,
            &EngineFloorDefaults::new(),
            &FloorMoves::new(),
        )
        .expect("establishing the floor");

        assert_eq!(
            engine.floors_set(),
            floors_set(DEFAULT_MEBIBYTES, 123_456_789)
        );
    }

    /// A knob this library cannot find is a floor it cannot promise. Failing the open is the
    /// loud end of that trade; the silent end is an index build refused on a device months
    /// later.
    #[test]
    fn an_open_whose_floors_cannot_be_read_fails() {
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES).missing(QUERY_SPILLING_FLOOR);

        let failure = establish(
            &engine,
            None,
            &EngineFloorDefaults::new(),
            &FloorMoves::new(),
        )
        .expect_err("an engine that hides a knob cannot promise a floor");

        assert!(
            matches!(&failure, Error::InvalidResponse(message)
                     if message.contains(QUERY_SPILLING_FLOOR)),
            "{failure}"
        );
    }

    /// A caller who named no floor is relying on this open just as much as one who did, so a
    /// refusal cannot be swallowed here either.
    #[test]
    fn an_open_the_engine_refuses_to_put_the_floors_back_on_fails() {
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES).refusing(INDEX_BUILD_FLOOR);

        let failure = establish(
            &engine,
            None,
            &EngineFloorDefaults::new(),
            &FloorMoves::new(),
        )
        .expect_err("the engine refused the knob");

        assert!(
            matches!(&failure, Error::Server { message, .. }
                     if message.contains(INDEX_BUILD_FLOOR)),
            "{failure}"
        );
    }

    #[test]
    fn an_open_the_engine_refuses_the_named_floor_of_fails() {
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES).refusing(QUERY_SPILLING_FLOOR);

        let failure = establish(
            &engine,
            Some(floor(64)),
            &EngineFloorDefaults::new(),
            &FloorMoves::new(),
        )
        .expect_err("the engine refused the knob");

        assert!(
            matches!(&failure, Error::Server { message, .. }
                     if message.contains(QUERY_SPILLING_FLOOR)),
            "{failure}"
        );
    }

    /// An engine whose floors moved after they were recorded is still put back on the
    /// recorded ones, which is what makes a floor moved on a running client last no longer
    /// than the next open.
    #[test]
    fn a_floor_moved_while_running_lasts_until_the_next_open() {
        let defaults = EngineFloorDefaults::new();
        let moves = FloorMoves::new();
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES);
        establish(&engine, None, &defaults, &moves).expect("the first open");
        send(&engine, floor(16).commands()).expect("lowering it while running");

        establish(&engine, None, &defaults, &moves).expect("the next open");

        assert_eq!(engine.reported(), engine_own_floors());
    }

    /// An open racing a move on a running client is the same hazard as two movers racing each
    /// other: four commands for two decisions, and an engine left holding one knob from each.
    /// So the open takes the lock the mover takes, and the fake holds whichever gets there first
    /// inside the index-build knob until the other reaches it.
    #[test]
    fn an_open_does_not_interleave_its_commands_with_a_move_on_a_running_client() {
        let engine = &FakeEngine::reporting(DEFAULT_MEBIBYTES).pausing_in_the_index_build_knob();
        let moves = &FloorMoves::new();
        let defaults = &EngineFloorDefaults::new();

        thread::scope(|threads| {
            threads.spawn(move || {
                establish(engine, Some(floor(16)), defaults, moves).expect("the open")
            });
            threads
                .spawn(move || move_free_disk_floor(engine, moves, floor(64)).expect("the move"));
        });

        let opened = floors_set(16, 16 * 1024 * 1024);
        let moved = floors_set(64, 64 * 1024 * 1024);
        let sent = engine.floors_set();
        assert!(
            sent == [opened.clone(), moved.clone()].concat() || sent == [moved, opened].concat(),
            "the open and the move interleaved their commands: {sent:#?}"
        );
    }

    fn engine_own_floors() -> ReportedFloors {
        ReportedFloors::new(
            IndexBuildFloor::from_mebibytes(DEFAULT_MEBIBYTES),
            QuerySpillingFloor::from_bytes(DEFAULT_BYTES),
        )
    }
}
