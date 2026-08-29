//! Moving the free-disk floors on an engine that is already running.

use super::{
    FreeDiskFloor, ReportedFloors,
    knobs::{reported_floors, send},
};
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
    pub fn set_free_disk_floor(&self, floor: FreeDiskFloor) -> Result<()> {
        send(self.client, floor.commands())
    }

    /// What the engine says the two floors are now. Useful to a caller that wants to check what
    /// it is running with, and to the tests that pin it.
    pub fn free_disk_floors(&self) -> Result<ReportedFloors> {
        reported_floors(self.client)
    }
}
