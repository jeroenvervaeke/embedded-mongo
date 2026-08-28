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

use crate::{Client, Error, Result};
use bson::{Document, doc};
use embedded_mongodb_sys::{OutOfRange, check_range};

/// How much free disk space an index build or a spilling query insists on before it starts.
///
/// Worth choosing deliberately rather than as low as it will go: see the module note on what
/// happens when a build actually runs out.
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
        [
            doc! { "setParameter": 1, "indexBuildMinAvailableDiskSpaceMB": mebibytes },
            doc! {
                "setParameter": 1,
                "internalQuerySpillingMinAvailableDiskSpaceBytes": mebibytes * 1024 * 1024,
            },
        ]
    }
}

/// Applies `floor` to the engine `client` has open, at any point in its life.
///
/// [`crate::Client::with_options`] calls this during `open`, which is the usual way to reach
/// it. It is public as well because the floor is the one limit here that a caller may want to
/// move while running -- raising it before a large build and dropping it afterwards.
///
/// Failures are returned rather than logged: a caller who asked for a floor and did not get
/// it would otherwise find out at the index build, on a device where the build is the thing
/// that was supposed to work. An unknown parameter name comes back as `InvalidOptions` rather
/// than being ignored, so a MongoDB that renames one of these is a loud error here.
pub fn set_free_disk_floor(client: &Client, floor: FreeDiskFloor) -> Result<()> {
    for command in floor.commands() {
        client.database("admin").run_command(&command)?;
    }
    Ok(())
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

/// What the engine says the two floors are now. Useful to a caller that wants to check what
/// it is running with, and to the tests that pin it.
pub fn free_disk_floors(client: &Client) -> Result<ReportedFloors> {
    let reported = client.database("admin").run_command(&doc! {
        "getParameter": 1,
        "indexBuildMinAvailableDiskSpaceMB": 1,
        "internalQuerySpillingMinAvailableDiskSpaceBytes": 1,
    })?;
    let read = |name: &str| {
        reported
            .get_i64(name)
            .map_err(|_| Error::InvalidResponse(format!("getParameter has no {name}")))
    };
    Ok(ReportedFloors {
        index_build_mebibytes: read("indexBuildMinAvailableDiskSpaceMB")?,
        query_spilling_bytes: read("internalQuerySpillingMinAvailableDiskSpaceBytes")?,
    })
}

#[cfg(test)]
mod tests {
    use super::FreeDiskFloor;

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
}
