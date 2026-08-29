//! Moving the free-disk floors on an engine that is already running.

use super::{AdminCommands, FreeDiskFloor, ReportedFloors, knobs::reported_floors};
use crate::{Client, Error, Result};
use std::sync::{Mutex, PoisonError};

/// The limits that belong to this process rather than to the [`Client`] they are reached
/// through.
///
/// Both free-disk floors are MongoDB **server parameters**, and this engine keeps one runtime
/// for the whole life of the process. A floor is therefore a setting of the *process*, not of
/// the client that named it: it survives that client's [`Client::close`], and left alone it
/// would still be in force for the next open. That is not guessable from an API where the floor
/// is named per-open, so this library does not leave it to be discovered -- **every open
/// establishes the floor**, putting MongoDB's own back where the caller named none. An
/// application that opens one database on a lowered floor, closes it and opens another gets the
/// defaults it asked for rather than the previous database's floor.
///
/// Two consequences worth knowing. A floor moved through this handle lasts until the next open,
/// which resets it -- it is not remembered for a directory, so an application that wants it
/// every time names it in [`OpenOptions::free_disk_floor`](crate::OpenOptions::free_disk_floor)
/// rather than setting it afterwards. And while a client is open the floor is shared by every
/// database name that client serves, because there is only ever one engine to set it on.
///
/// A handle rather than two functions taking a `&Client`, because that is what puts the scope
/// in front of a reader at every call site instead of only where the function is defined:
///
/// ```no_run
/// use embedded_mongodb::{Client, FreeDiskFloor};
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let client = Client::new("./data")?;
///
/// let limits = client.process_limits();
/// limits.set_free_disk_floor(FreeDiskFloor::from_mebibytes(32)?)?;
/// let now = limits.free_disk_floors()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy)]
pub struct ProcessLimits<'client> {
    client: &'client Client,
}

impl<'client> ProcessLimits<'client> {
    pub(crate) fn new(client: &'client Client) -> Self {
        Self { client }
    }

    /// Applies `floor` to the engine this client has open, at any point in its life.
    ///
    /// [`Client::with_options`] establishes the floor during the open, which is the usual way to
    /// reach it. This is here as well because the floor is the one limit a caller may want to
    /// move while running -- raising it before a large index build and dropping it afterwards.
    ///
    /// Failures are returned rather than logged: a caller who asked for a floor and did not get
    /// it would otherwise find out at the index build, on a device where the build is the thing
    /// that was supposed to work. An unknown parameter name comes back as an error rather than
    /// being ignored, so a MongoDB that renames one of these is loud here.
    ///
    /// The two knobs take two commands, so a refusal of the second would otherwise leave the
    /// first already moved -- downward, in the case that matters, where a caller who caught the
    /// error and concluded nothing had happened would go on to build an index against a floor
    /// far below the one it believes is protecting it. So the floors are put back where they
    /// were, and a refusal leaves the engine as it was rather than half moved. If putting them
    /// back fails too, the floors are ones nobody chose and that is a different error:
    /// [`Error::FreeDiskFloorNotRestored`], which is worth catching separately by an application
    /// that would rather refuse the work than do it against an unknown floor.
    pub fn set_free_disk_floor(&self, floor: FreeDiskFloor) -> Result<()> {
        move_free_disk_floor(self.client, FloorMoves::process(), floor)
    }

    /// What the engine says the two floors are now. Useful to a caller that wants to check what
    /// it is running with, and to the tests that pin it.
    ///
    /// Taken behind the same lock a move holds, so this never reports the pair a mover is half
    /// way through. The two knobs can still disagree -- see [`ReportedFloors`] -- but not
    /// because of anything this library was in the middle of.
    pub fn free_disk_floors(&self) -> Result<ReportedFloors> {
        free_disk_floors(self.client, FloorMoves::process())
    }
}

/// Serialises movement of the two floors, so that a pair reaches the engine as a unit.
///
/// [`Client`] is `Send + Sync` and callers may share one. The two knobs take two commands and
/// express a single decision, so two threads moving the floor without this can interleave: the
/// engine ends up with one thread's index-build floor beside the other's spilling floor, a pair
/// neither caller asked for describing a policy nobody chose.
///
/// The critical section is the whole read-apply-put-back sequence rather than the sends alone.
/// The floors a move reads in order to put them back have to be the ones it is about to move,
/// and an open racing a move has the same hazard from the other side -- which is why
/// [`at_open`](super::at_open) establishes its floor under this lock too.
///
/// Nothing is held across anything that could wait on the floors again. The engine serialises
/// its own commands internally on a ClientStrand, which is a lock this one is always taken
/// before and never after, so the two cannot make a cycle.
///
/// Injected rather than reached for as a static, so that a test gets a fresh one: a test that
/// serialised against every other test in the binary would prove nothing about either.
pub(crate) struct FloorMoves {
    serialised: Mutex<()>,
}

impl FloorMoves {
    pub(crate) const fn new() -> Self {
        Self {
            serialised: Mutex::new(()),
        }
    }

    /// The one every caller but a test's reaches, because the engine behind it is one too.
    pub(crate) fn process() -> &'static Self {
        static PROCESS: FloorMoves = FloorMoves::new();
        &PROCESS
    }

    /// Runs `moving` with no other floor movement in this process interleaved.
    ///
    /// A panic under the lock takes it back rather than poisoning it. The lock guards no data of
    /// its own -- what it protects is the order of commands already on their way to the engine
    /// -- so there is no half-written state a later mover could observe, and refusing every
    /// later move would strand the process on whatever floors the panic interrupted with no way
    /// left to correct them.
    pub(crate) fn one_at_a_time<T>(&self, moving: impl FnOnce() -> Result<T>) -> Result<T> {
        let _in_order = self
            .serialised
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        moving()
    }
}

pub(crate) fn free_disk_floors(
    engine: &impl AdminCommands,
    moves: &FloorMoves,
) -> Result<ReportedFloors> {
    moves.one_at_a_time(|| reported_floors(engine))
}

pub(crate) fn move_free_disk_floor(
    engine: &impl AdminCommands,
    moves: &FloorMoves,
    floor: FreeDiskFloor,
) -> Result<()> {
    moves.one_at_a_time(|| {
        // Read inside the lock and immediately before the move, so that what a failure puts
        // back is what this move is about to disturb rather than what some other mover left.
        let before = reported_floors(engine)?;
        apply_or_put_back(engine, floor, before)
    })
}

/// Sends the two commands `floor` takes, putting the first knob back if the second is refused.
///
/// What goes back comes from `before` rather than from anything derived through
/// [`FreeDiskFloor`]: the spilling knob is a byte count that need not be a whole mebibyte, so a
/// pair read off the engine has to be replayed as it was read. Both arrays are in knob order,
/// which is what makes the command that puts a knob back the one at the same position as the
/// command that moved it.
///
/// A refusal of the first command puts nothing back, because nothing has moved yet: a
/// `setParameter` the engine answered `ok: 0` to is one it did not apply, and a failure of the
/// engine itself is one no restore could reach either. Nor is the refused knob replayed --
/// sending a command already known to be refused would turn a floor that is exactly where it
/// was into one this library reports as unknown.
fn apply_or_put_back(
    engine: &impl AdminCommands,
    floor: FreeDiskFloor,
    before: ReportedFloors,
) -> Result<()> {
    let [move_index_build, move_spilling] = floor.commands();
    let [put_index_build_back, _] = before.commands();

    engine.run_on_admin(&move_index_build)?;
    let Err(cause) = engine.run_on_admin(&move_spilling) else {
        return Ok(());
    };
    Err(match engine.run_on_admin(&put_index_build_back) {
        Ok(_) => cause,
        Err(rollback) => Error::FreeDiskFloorNotRestored {
            requested: floor,
            cause: Box::new(cause),
            rollback: Box::new(rollback),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{FloorMoves, free_disk_floors, move_free_disk_floor};
    use crate::{
        Error, IndexBuildFloor, QuerySpillingFloor,
        limits::{
            ReportedFloors,
            fake::{DEFAULT_BYTES, DEFAULT_MEBIBYTES, FakeEngine, floor, floors_set},
            knobs::{BYTES_PER_MEBIBYTE, INDEX_BUILD_FLOOR, QUERY_SPILLING_FLOOR},
        },
    };
    use std::thread;

    #[test]
    fn the_floor_a_caller_named_is_the_one_applied() {
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES);

        move_free_disk_floor(&engine, &FloorMoves::new(), floor(32)).expect("moving the floor");

        assert_eq!(engine.floors_set(), floors_set(32, 32 * BYTES_PER_MEBIBYTE));
    }

    /// The defect. The two knobs take two commands, so a refusal of the second used to leave the
    /// first moved -- downward, in the case that matters -- while the caller was told the move
    /// had failed and could reasonably conclude that nothing had happened.
    #[test]
    fn a_move_the_engine_refuses_part_way_leaves_the_floors_where_they_were() {
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES).refusing(QUERY_SPILLING_FLOOR);

        move_free_disk_floor(&engine, &FloorMoves::new(), floor(32))
            .expect_err("the engine refused the spilling knob");

        assert_eq!(
            engine.reported(),
            ReportedFloors::new(
                IndexBuildFloor::from_mebibytes(DEFAULT_MEBIBYTES),
                QuerySpillingFloor::from_bytes(DEFAULT_BYTES),
            )
        );
    }

    /// The caller is owed the reason their floor was refused, not the mechanics of the retreat:
    /// a move that was put back failed for the reason the engine gave.
    #[test]
    fn a_move_that_was_put_back_reports_why_the_engine_refused_it() {
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES).refusing(QUERY_SPILLING_FLOOR);

        let failure = move_free_disk_floor(&engine, &FloorMoves::new(), floor(32))
            .expect_err("the engine refused the spilling knob");

        assert!(
            matches!(&failure, Error::Server { message, .. }
                     if message.contains(QUERY_SPILLING_FLOOR)),
            "{failure}"
        );
    }

    /// A refusal of the first command has moved nothing, so there is nothing to put back and the
    /// caller gets the plain refusal rather than a report that the floors are unknown.
    #[test]
    fn a_move_the_engine_refuses_outright_is_not_reported_as_floors_nobody_chose() {
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES).refusing(INDEX_BUILD_FLOOR);

        let failure = move_free_disk_floor(&engine, &FloorMoves::new(), floor(32))
            .expect_err("the engine refused the index build knob");

        assert!(
            matches!(&failure, Error::Server { message, .. }
                     if message.contains(INDEX_BUILD_FLOOR)),
            "{failure}"
        );
    }

    /// An engine that stops taking parameters half way through leaves floors nobody chose, and
    /// that is materially different from a request that failed: a caller who catches an ordinary
    /// error reasonably assumes nothing happened.
    #[test]
    fn a_move_whose_retreat_also_fails_is_a_failure_of_its_own() {
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES).refusing_from(1);

        let failure = move_free_disk_floor(&engine, &FloorMoves::new(), floor(32))
            .expect_err("the engine refused the move and the retreat");

        assert!(
            matches!(&failure, Error::FreeDiskFloorNotRestored { requested, .. }
                     if *requested == floor(32)),
            "{failure}"
        );
    }

    #[test]
    fn floors_nobody_chose_are_reported_with_both_reasons() {
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES).refusing_from(1);

        let failure = move_free_disk_floor(&engine, &FloorMoves::new(), floor(32))
            .expect_err("the engine refused the move and the retreat");

        let reported = failure.to_string();
        assert!(reported.contains(QUERY_SPILLING_FLOOR), "{reported}");
        assert!(reported.contains(INDEX_BUILD_FLOOR), "{reported}");
    }

    /// A reading taken while a mover is between its two commands would otherwise report one
    /// knob's new value beside the other's old one -- a pair the engine holds for an instant and
    /// nobody chose, handed to a caller as the floors it is running on.
    ///
    /// The mover is let into the index-build knob and left there, so the reading is taken at
    /// exactly the moment the pair is half moved rather than whenever the scheduler allows.
    #[test]
    fn a_reading_taken_while_a_move_is_half_way_through_waits_for_the_whole_pair() {
        let engine = &FakeEngine::reporting(DEFAULT_MEBIBYTES).pausing_in_the_index_build_knob();
        let moves = &FloorMoves::new();

        let reading = thread::scope(|threads| {
            threads.spawn(move || {
                move_free_disk_floor(engine, moves, floor(16)).expect("moving the floor")
            });
            while engine.floors_set().len() != 1 {
                thread::yield_now();
            }
            free_disk_floors(engine, moves).expect("reading the floors")
        });

        assert_eq!(
            reading,
            ReportedFloors::new(
                IndexBuildFloor::from_mebibytes(16),
                QuerySpillingFloor::from_bytes(16 * BYTES_PER_MEBIBYTE),
            )
        );
    }

    /// Two movers, and a fake that holds the first one inside the index-build knob until a
    /// second mover reaches the same knob. Without the serialisation the second gets in between
    /// the first one's two commands every time rather than when the scheduler happens to allow
    /// it, and the four commands arrive interleaved. With it the second mover is still waiting
    /// for the lock while the first finishes, the fake's wait times out, and the commands arrive
    /// as two whole pairs.
    #[test]
    fn two_movers_do_not_interleave_the_two_commands_of_a_pair() {
        let engine = &FakeEngine::reporting(DEFAULT_MEBIBYTES).pausing_in_the_index_build_knob();
        let moves = &FloorMoves::new();

        thread::scope(|movers| {
            for mebibytes in [16, 64] {
                movers.spawn(move || {
                    move_free_disk_floor(engine, moves, floor(mebibytes)).expect("moving the floor")
                });
            }
        });

        let sixteen = floors_set(16, 16 * BYTES_PER_MEBIBYTE);
        let sixty_four = floors_set(64, 64 * BYTES_PER_MEBIBYTE);
        let sent = engine.floors_set();
        assert!(
            sent == [sixteen.clone(), sixty_four.clone()].concat()
                || sent == [sixty_four, sixteen].concat(),
            "the two movers interleaved their commands: {sent:#?}"
        );
    }
}
