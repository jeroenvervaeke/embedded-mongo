//! The two server parameters a free-disk floor is made of, and the unit each of them takes.
//!
//! One decision, two knobs, two units: the index build holds a count of mebibytes and the
//! spilling query a count of bytes, both as signed 64-bit integers in the engine. They are told
//! apart by type here rather than by position, because two `i64` parameters in one signature
//! swap without a compiler noticing -- and a byte count written into the megabyte knob reads
//! back as a floor a millionfold too high, refusing every index build on the device.

use super::AdminCommands;
use crate::{Error, Result};
use bson::{Bson, Document, doc};

/// How much free disk an index build insists on before it starts, in mebibytes.
///
/// A count as `indexBuildMinAvailableDiskSpaceMB` holds it rather than a floor that was checked
/// on the way in: it carries what the engine reported, whatever that turns out to be.
/// [`FreeDiskFloor`](super::FreeDiskFloor) is the type a caller names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexBuildFloor(i64);

impl IndexBuildFloor {
    pub fn from_mebibytes(mebibytes: i64) -> Self {
        Self(mebibytes)
    }

    pub fn mebibytes(self) -> i64 {
        self.0
    }
}

/// How much free disk a query insists on before it spills to disk, in bytes.
///
/// Bytes rather than mebibytes because `internalQuerySpillingMinAvailableDiskSpaceBytes` is one,
/// and a value read back from the engine need not be a whole mebibyte at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct QuerySpillingFloor(i64);

impl QuerySpillingFloor {
    pub fn from_bytes(bytes: i64) -> Self {
        Self(bytes)
    }

    pub fn bytes(self) -> i64 {
        self.0
    }
}

/// The two floors as the engine currently reports them, each in the unit its own knob uses.
///
/// Not a [`FreeDiskFloor`](super::FreeDiskFloor): these come back from the engine rather than
/// going into it, they can disagree with each other if something set them separately, and the
/// spilling one is a byte count that need not be a whole mebibyte. Which is why putting a pair
/// back replays the two counts as they were read rather than a floor rounded out of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportedFloors {
    index_build: IndexBuildFloor,
    query_spilling: QuerySpillingFloor,
}

impl ReportedFloors {
    /// Constructable from outside so that a caller can write down the pair it expects and
    /// compare. Every pair this library hands out came from the engine.
    pub fn new(index_build: IndexBuildFloor, query_spilling: QuerySpillingFloor) -> Self {
        Self {
            index_build,
            query_spilling,
        }
    }

    pub fn index_build(self) -> IndexBuildFloor {
        self.index_build
    }

    pub fn query_spilling(self) -> QuerySpillingFloor {
        self.query_spilling
    }

    /// The `setParameter` commands that put these floors back, each in the unit its knob takes.
    ///
    /// Separate from [`FreeDiskFloor::commands`](super::FreeDiskFloor::commands) because a
    /// `ReportedFloors` is not a `FreeDiskFloor`: the spilling knob is a byte count that need
    /// not be a whole mebibyte, so floors read back from the engine have to be replayed as they
    /// were read rather than as a number rounded through this library's type.
    pub(crate) fn commands(self) -> [Document; 2] {
        floor_commands(self.index_build, self.query_spilling)
    }
}

pub(crate) const INDEX_BUILD_FLOOR: &str = "indexBuildMinAvailableDiskSpaceMB";

pub(crate) const QUERY_SPILLING_FLOOR: &str = "internalQuerySpillingMinAvailableDiskSpaceBytes";

pub(crate) const BYTES_PER_MEBIBYTE: i64 = 1024 * 1024;

/// The two `setParameter` commands that put this pair of knob values in force, in knob order.
///
/// Knob order -- index build first, spilling second -- is the order every array of commands in
/// this module is in, so a command that puts a knob back sits where the command that moved it
/// sat.
pub(crate) fn floor_commands(
    index_build: IndexBuildFloor,
    query_spilling: QuerySpillingFloor,
) -> [Document; 2] {
    [
        doc! { "setParameter": 1, INDEX_BUILD_FLOOR: index_build.mebibytes() },
        doc! { "setParameter": 1, QUERY_SPILLING_FLOOR: query_spilling.bytes() },
    ]
}

/// What the engine says the two floors are now.
pub(crate) fn reported_floors(engine: &impl AdminCommands) -> Result<ReportedFloors> {
    let reported = engine.run_on_admin(&doc! {
        "getParameter": 1,
        INDEX_BUILD_FLOOR: 1,
        QUERY_SPILLING_FLOOR: 1,
    })?;
    Ok(ReportedFloors {
        index_build: IndexBuildFloor::from_mebibytes(floor_in(&reported, INDEX_BUILD_FLOOR)?),
        query_spilling: QuerySpillingFloor::from_bytes(floor_in(&reported, QUERY_SPILLING_FLOOR)?),
    })
}

/// Sends `commands` in order, stopping at the first one the engine refuses.
///
/// Nothing is put back here. The callers that must put a half-moved floor back read what was
/// there first and do it themselves, because what they put back is what they read and not
/// anything this function could work out from the commands it was handed.
pub(crate) fn send(engine: &impl AdminCommands, commands: [Document; 2]) -> Result<()> {
    for command in commands {
        engine.run_on_admin(&command)?;
    }
    Ok(())
}

/// The count the engine reported for `knob`, or why it cannot be read as one.
///
/// A missing knob is raised rather than defaulted: a MongoDB that renamed one of these would
/// otherwise report a floor this library never set, and a caller would size its work against a
/// number that is not the one in force.
///
/// A knob that is present but holds something else is a different failure and says so, naming
/// what came back. Reporting it as absent would be false, and would send whoever has to fix it
/// looking for a knob the reply plainly contains.
fn floor_in(reported: &Document, knob: &str) -> Result<i64> {
    match reported.get(knob) {
        Some(Bson::Int64(floor)) => Ok(*floor),
        Some(found) => Err(Error::InvalidResponse(format!(
            "getParameter answered {knob} with {found:?}, not the 64-bit integer the knob holds"
        ))),
        None => Err(Error::InvalidResponse(format!(
            "getParameter has no {knob}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        INDEX_BUILD_FLOOR, IndexBuildFloor, QUERY_SPILLING_FLOOR, QuerySpillingFloor,
        ReportedFloors, reported_floors,
    };
    use crate::{Error, limits::fake::FakeEngine};

    /// The spilling knob is a byte count the engine need not report as a whole mebibyte, so what
    /// was read has to go back as it was read rather than through
    /// [`FreeDiskFloor`](crate::FreeDiskFloor).
    #[test]
    fn floors_read_back_are_replayed_in_the_units_they_were_read_in() {
        let [index_build, spilling] = ReportedFloors::new(
            IndexBuildFloor::from_mebibytes(500),
            QuerySpillingFloor::from_bytes(123_456_789),
        )
        .commands();

        assert_eq!(index_build.get_i64(INDEX_BUILD_FLOOR).ok(), Some(500));
        assert_eq!(
            spilling.get_i64(QUERY_SPILLING_FLOOR).ok(),
            Some(123_456_789)
        );
    }

    #[test]
    fn a_knob_the_engine_leaves_out_is_reported_as_missing() {
        let engine = FakeEngine::reporting(500).missing(QUERY_SPILLING_FLOOR);

        let failure = reported_floors(&engine).expect_err("a knob that is not there");

        assert!(
            matches!(&failure, Error::InvalidResponse(message)
                     if message.contains(QUERY_SPILLING_FLOOR) && message.contains("has no")),
            "{failure}"
        );
    }

    /// The defect: a knob answered with the wrong type used to be reported as one the reply did
    /// not contain, which is false and sends the reader looking for the wrong thing.
    #[test]
    fn a_knob_the_engine_answers_with_something_else_is_not_reported_as_missing() {
        let engine = FakeEngine::reporting(500).mistyping(INDEX_BUILD_FLOOR);

        let failure = reported_floors(&engine).expect_err("a knob holding the wrong type");

        assert!(
            matches!(&failure, Error::InvalidResponse(message)
                     if !message.contains("has no")),
            "{failure}"
        );
    }

    #[test]
    fn a_knob_the_engine_answers_with_something_else_reports_what_came_back() {
        let engine = FakeEngine::reporting(500).mistyping(INDEX_BUILD_FLOOR);

        let failure = reported_floors(&engine).expect_err("a knob holding the wrong type");

        assert!(
            matches!(&failure, Error::InvalidResponse(message)
                     if message.contains(INDEX_BUILD_FLOOR)
                        && message.contains(FakeEngine::MISTYPED_AS)),
            "{failure}"
        );
    }
}
