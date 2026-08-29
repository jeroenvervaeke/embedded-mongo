//! Moving the free-disk floors on an engine that is already running.

use super::{AdminCommands, FreeDiskFloor, ReportedFloors, knobs::reported_floors};
use crate::{Client, Result};

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
    /// were, and a refusal leaves the engine as it was rather than half moved.
    pub fn set_free_disk_floor(&self, floor: FreeDiskFloor) -> Result<()> {
        move_free_disk_floor(self.client, floor)
    }

    /// What the engine says the two floors are now. Useful to a caller that wants to check what
    /// it is running with, and to the tests that pin it.
    pub fn free_disk_floors(&self) -> Result<ReportedFloors> {
        reported_floors(self.client)
    }
}

pub(crate) fn move_free_disk_floor(
    engine: &impl AdminCommands,
    floor: FreeDiskFloor,
) -> Result<()> {
    // Read immediately before the move, so that what a failure puts back is what this move is
    // about to disturb.
    let before = reported_floors(engine)?;
    apply_or_put_back(engine, floor, before)
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
/// sending a command already known to be refused would report a floor as disturbed where in
/// fact it is exactly where it was.
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
    let _put_back = engine.run_on_admin(&put_index_build_back);
    Err(cause)
}

#[cfg(test)]
mod tests {
    use super::move_free_disk_floor;
    use crate::{
        Error, IndexBuildFloor, QuerySpillingFloor,
        limits::{
            ReportedFloors,
            fake::{DEFAULT_BYTES, DEFAULT_MEBIBYTES, FakeEngine, floor, floors_set},
            knobs::{BYTES_PER_MEBIBYTE, QUERY_SPILLING_FLOOR},
        },
    };

    #[test]
    fn the_floor_a_caller_named_is_the_one_applied() {
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES);

        move_free_disk_floor(&engine, floor(32)).expect("moving the floor");

        assert_eq!(engine.floors_set(), floors_set(32, 32 * BYTES_PER_MEBIBYTE));
    }

    /// The defect. The two knobs take two commands, so a refusal of the second used to leave the
    /// first moved -- downward, in the case that matters -- while the caller was told the move
    /// had failed and could reasonably conclude that nothing had happened.
    #[test]
    fn a_move_the_engine_refuses_part_way_leaves_the_floors_where_they_were() {
        let engine = FakeEngine::reporting(DEFAULT_MEBIBYTES).refusing(QUERY_SPILLING_FLOOR);

        move_free_disk_floor(&engine, floor(32)).expect_err("the engine refused the spilling knob");

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

        let failure = move_free_disk_floor(&engine, floor(32))
            .expect_err("the engine refused the spilling knob");

        assert!(
            matches!(&failure, Error::Server { message, .. }
                     if message.contains(QUERY_SPILLING_FLOOR)),
            "{failure}"
        );
    }
}
