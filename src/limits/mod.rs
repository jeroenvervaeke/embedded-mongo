//! The free-disk floors, lowered on a running engine rather than baked into the library.
//!
//! MongoDB refuses to start an index build, or to spill a query to disk, when the data
//! directory has less than 500 MB free. That is sized for a server. A phone near its limit
//! does not have 500 MB free at all, so on such a device an application that can open and
//! read its database still cannot build an index over it -- and a query that has to spill
//! fails with the same `OutOfDiskSpace`.
//!
//! Both are `set_at: [startup, runtime]` server parameters, so a client can move them on an
//! engine that is already up. That is why none of this is in the native library: it needs no
//! C ABI, no rebuild and no release to change.
//!
//! It is not lowered by default. Nothing aborts a build that runs out of space part-way, and
//! WiredTiger answers a full disk by panicking, which MongoDB answers with `fassert` -- the
//! host process is gone without an error ever reaching the caller. How much headroom is
//! enough depends on how much data is about to be indexed, which the caller knows and this
//! library does not.
//!
//! Being server parameters, the floors belong to the process rather than to a [`Client`], and
//! outlive the client that moved them. [`at_open`] is where that is dealt with, and [`knobs`]
//! is the pair of parameters underneath it.

pub(crate) mod at_open;

mod floor;
mod knobs;

pub use floor::FreeDiskFloor;
pub use knobs::{IndexBuildFloor, QuerySpillingFloor, ReportedFloors};

use crate::{Client, Result};
use bson::Document;
use knobs::{reported_floors, send};

/// Applies `floor` to the engine `client` has open, at any point in its life.
///
/// [`crate::Client::with_options`] calls this during `open`, which is the usual way to reach
/// it. It is public as well because the floor is the one limit here that a caller may want to
/// move while running -- raising it before a large build and dropping it afterwards.
///
/// What moves is a pair of server parameters belonging to the process rather than to this
/// client -- see [`FreeDiskFloor`]. It reaches every database name the engine serves, and the
/// next open puts it back to whatever that open asks for.
///
/// Failures are returned rather than logged: a caller who asked for a floor and did not get
/// it would otherwise find out at the index build, on a device where the build is the thing
/// that was supposed to work. An unknown parameter name comes back as `InvalidOptions` rather
/// than being ignored, so a MongoDB that renames one of these is a loud error here.
pub fn set_free_disk_floor(client: &Client, floor: FreeDiskFloor) -> Result<()> {
    send(client, floor.commands())
}

/// What the engine says the two floors are now. Useful to a caller that wants to check what
/// it is running with, and to the tests that pin it.
pub fn free_disk_floors(client: &Client) -> Result<ReportedFloors> {
    reported_floors(client)
}

/// The one thing the floors need of an open engine: a command on `admin`, and its reply.
///
/// A seam rather than a `&Client` everywhere, so that [`at_open`] can be put in front of an
/// engine that remembers what was set on it without one being started. The floors are the
/// process's rather than a client's, and a test of that has to be able to arrange a process
/// whose floors have already been moved.
pub(crate) trait AdminCommands {
    fn run_on_admin(&self, command: &Document) -> Result<Document>;
}

impl AdminCommands for Client {
    fn run_on_admin(&self, command: &Document) -> Result<Document> {
        self.database(ADMIN).run_command(command)
    }
}

const ADMIN: &str = "admin";
