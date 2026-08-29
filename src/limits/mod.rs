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
//! outlive the client that moved them. [`at_open`] is where that is dealt with.

pub(crate) mod at_open;

use crate::{Client, Error, Result};
use bson::{Document, doc};
use embedded_mongodb_sys::{OutOfRange, check_range};

/// How much free disk space an index build or a spilling query insists on before it starts.
///
/// Worth choosing deliberately rather than as low as it will go: see the module note on what
/// happens when a build actually runs out.
///
/// # It is process-global, not per-client
///
/// Both floors are MongoDB **server parameters**, and this engine keeps one runtime for the
/// whole life of the process. A floor is therefore a setting of the *process*, not of the
/// [`Client`] that named it: it survives that client's [`Client::close`], and left alone it
/// would still be in force for the next open. That is not guessable from an API where the
/// floor is named per-open, so this library does not leave it to be discovered -- **every open
/// establishes the floor**, putting MongoDB's own back where the caller named none. An
/// application that opens one database on a lowered floor, closes it and opens another gets
/// the defaults it asked for rather than the previous database's floor.
///
/// Two consequences worth knowing. A floor moved with [`set_free_disk_floor`] on a running
/// client lasts until the next open, which resets it -- it is not remembered for a directory,
/// so an application that wants it every time names it in
/// [`OpenOptions::free_disk_floor`](crate::OpenOptions::free_disk_floor) rather than setting
/// it afterwards. And while a client is open the floor is shared by every database name it
/// serves, because there is only ever one engine to set it on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FreeDiskFloor(u32);

impl FreeDiskFloor {
    /// One mebibyte is the lowest floor that can be asked for, and is in practice no floor at
    /// all. Zero is excluded because `indexBuildMinAvailableDiskSpaceMB` compares with `<=`:
    /// a floor of zero would still refuse a build on a disk with nothing left, while giving
    /// up every megabyte of warning before it.
    pub const MIN_MEBIBYTES: u32 = 1;
    /// The engine holds these as megabyte and byte counts in signed 64-bit integers, so the
    /// binding limit is this type's own, not MongoDB's.
    pub const MAX_MEBIBYTES: u32 = u32::MAX;

    pub fn from_mebibytes(mebibytes: u32) -> std::result::Result<Self, OutOfRange> {
        check_range(
            "free disk floor",
            "MiB",
            mebibytes,
            Self::MIN_MEBIBYTES,
            Self::MAX_MEBIBYTES,
        )
        .map(Self)
    }

    pub fn mebibytes(self) -> u32 {
        self.0
    }

    /// The two `setParameter` commands that move the floor, in the units each knob takes.
    ///
    /// Two commands rather than one: `setParameter` reports the previous value in a field
    /// named `was`, so a combined command answers with two fields of the same name and a
    /// parameter that was quietly rejected is indistinguishable from one that was applied.
    fn commands(self) -> [Document; 2] {
        let mebibytes = i64::from(self.0);
        floor_commands(mebibytes, mebibytes * BYTES_PER_MEBIBYTE)
    }
}

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
    apply_floor(client, floor)
}

/// The two floors as the engine currently reports them, each in the unit its own knob uses.
///
/// Not a `FreeDiskFloor`: these come back from the engine rather than going into it, they can
/// disagree with each other if something set them separately, and the spilling one is a byte
/// count that need not be a whole mebibyte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportedFloors {
    pub index_build_mebibytes: i64,
    pub query_spilling_bytes: i64,
}

impl ReportedFloors {
    /// The `setParameter` commands that put these floors back, each in the unit its knob
    /// takes.
    ///
    /// Separate from [`FreeDiskFloor::commands`] because a `ReportedFloors` is not a
    /// `FreeDiskFloor`: the spilling knob is a byte count that need not be a whole mebibyte,
    /// so floors read back from the engine have to be replayed as they were read rather than
    /// as a number rounded through this library's type.
    fn commands(self) -> [Document; 2] {
        floor_commands(self.index_build_mebibytes, self.query_spilling_bytes)
    }
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

pub(crate) fn apply_floor(engine: &impl AdminCommands, floor: FreeDiskFloor) -> Result<()> {
    send(engine, floor.commands())
}

pub(crate) fn restore_floors(engine: &impl AdminCommands, floors: ReportedFloors) -> Result<()> {
    send(engine, floors.commands())
}

pub(crate) fn reported_floors(engine: &impl AdminCommands) -> Result<ReportedFloors> {
    let reported = engine.run_on_admin(&doc! {
        "getParameter": 1,
        INDEX_BUILD_FLOOR: 1,
        QUERY_SPILLING_FLOOR: 1,
    })?;
    // A missing knob is raised rather than defaulted: a MongoDB that renamed one of these
    // would otherwise report a floor this library never set, and a caller would size its work
    // against a number that is not the one in force.
    let read = |name: &str| {
        reported
            .get_i64(name)
            .map_err(|_| Error::InvalidResponse(format!("getParameter has no {name}")))
    };
    Ok(ReportedFloors {
        index_build_mebibytes: read(INDEX_BUILD_FLOOR)?,
        query_spilling_bytes: read(QUERY_SPILLING_FLOOR)?,
    })
}

const ADMIN: &str = "admin";

pub(crate) const INDEX_BUILD_FLOOR: &str = "indexBuildMinAvailableDiskSpaceMB";

pub(crate) const QUERY_SPILLING_FLOOR: &str = "internalQuerySpillingMinAvailableDiskSpaceBytes";

const BYTES_PER_MEBIBYTE: i64 = 1024 * 1024;

fn floor_commands(mebibytes: i64, bytes: i64) -> [Document; 2] {
    [
        doc! { "setParameter": 1, INDEX_BUILD_FLOOR: mebibytes },
        doc! { "setParameter": 1, QUERY_SPILLING_FLOOR: bytes },
    ]
}

fn send(engine: &impl AdminCommands, commands: [Document; 2]) -> Result<()> {
    for command in commands {
        engine.run_on_admin(&command)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FreeDiskFloor, ReportedFloors};

    #[test]
    fn a_floor_of_zero_is_refused_because_it_is_not_a_floor() {
        assert!(FreeDiskFloor::from_mebibytes(0).is_err());
    }

    #[test]
    fn the_smallest_floor_is_accepted() {
        assert_eq!(
            FreeDiskFloor::from_mebibytes(1)
                .expect("1 MiB is the minimum")
                .mebibytes(),
            1
        );
    }

    #[test]
    fn the_index_build_floor_is_set_in_mebibytes() {
        let [index_build, _] = FreeDiskFloor::from_mebibytes(32)
            .expect("32 MiB is in range")
            .commands();

        assert_eq!(
            index_build
                .get_i64("indexBuildMinAvailableDiskSpaceMB")
                .ok(),
            Some(32)
        );
    }

    #[test]
    fn the_spilling_floor_is_set_in_bytes() {
        let [_, spilling] = FreeDiskFloor::from_mebibytes(32)
            .expect("32 MiB is in range")
            .commands();

        assert_eq!(
            spilling
                .get_i64("internalQuerySpillingMinAvailableDiskSpaceBytes")
                .ok(),
            Some(33_554_432)
        );
    }

    #[test]
    fn a_large_floor_does_not_overflow_the_byte_count() {
        let [_, spilling] = FreeDiskFloor::from_mebibytes(u32::MAX)
            .expect("the maximum is in range")
            .commands();

        assert_eq!(
            spilling
                .get_i64("internalQuerySpillingMinAvailableDiskSpaceBytes")
                .ok(),
            Some(i64::from(u32::MAX) * 1024 * 1024)
        );
    }

    /// The spilling knob is a byte count the engine need not report as a whole mebibyte, so
    /// what was read has to go back as it was read rather than through [`FreeDiskFloor`].
    #[test]
    fn floors_read_back_are_replayed_in_the_units_they_were_read_in() {
        let [index_build, spilling] = ReportedFloors {
            index_build_mebibytes: 500,
            query_spilling_bytes: 123_456_789,
        }
        .commands();

        assert_eq!(
            index_build
                .get_i64("indexBuildMinAvailableDiskSpaceMB")
                .ok(),
            Some(500)
        );
        assert_eq!(
            spilling
                .get_i64("internalQuerySpillingMinAvailableDiskSpaceBytes")
                .ok(),
            Some(123_456_789)
        );
    }
}
