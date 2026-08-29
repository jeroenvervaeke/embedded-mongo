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
//! outlive the client that moved them. [`process`] is where a caller meets that, [`at_open`] is
//! where every open is made to establish a floor because of it, and [`knobs`] is the pair of
//! parameters underneath both.

pub(crate) mod at_open;

mod floor;
mod knobs;
mod process;

pub use floor::FreeDiskFloor;
pub use knobs::{IndexBuildFloor, QuerySpillingFloor, ReportedFloors};
pub use process::ProcessLimits;

use crate::{Client, Result};
use bson::Document;

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
