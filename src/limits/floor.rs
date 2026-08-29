//! The floor a caller names, and the pair of commands it turns into.

use super::knobs::{BYTES_PER_MEBIBYTE, IndexBuildFloor, QuerySpillingFloor, floor_commands};
use bson::Document;
use embedded_mongodb_sys::{OutOfRange, check_range};

/// How much free disk space an index build or a spilling query insists on before it starts.
///
/// Worth choosing deliberately rather than as low as it will go. The floor is a pre-flight
/// check and nothing else: it refuses a build that would start with too little room, and
/// nothing aborts one that runs out part-way. WiredTiger answers a genuinely full disk by
/// panicking, which MongoDB answers with `fassert` -- the host process is gone without an error
/// ever reaching the caller. So the floor is the only warning an application gets, and lowering
/// it trades a refusal it can report for a crash it cannot. Lower it to what the work about to
/// be done actually needs, not to what will fit.
///
/// A floor is a setting of the *process* rather than of the client that named it, which is
/// worth knowing before relying on one: [`ProcessLimits`](crate::ProcessLimits) has what that
/// means, and is where a floor is moved on a client that is already open.
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
    pub(crate) fn commands(self) -> [Document; 2] {
        let mebibytes = i64::from(self.0);
        floor_commands(
            IndexBuildFloor::from_mebibytes(mebibytes),
            QuerySpillingFloor::from_bytes(mebibytes * BYTES_PER_MEBIBYTE),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::FreeDiskFloor;
    use crate::limits::knobs::{INDEX_BUILD_FLOOR, QUERY_SPILLING_FLOOR};

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

        assert_eq!(index_build.get_i64(INDEX_BUILD_FLOOR).ok(), Some(32));
    }

    #[test]
    fn the_spilling_floor_is_set_in_bytes() {
        let [_, spilling] = FreeDiskFloor::from_mebibytes(32)
            .expect("32 MiB is in range")
            .commands();

        assert_eq!(
            spilling.get_i64(QUERY_SPILLING_FLOOR).ok(),
            Some(33_554_432)
        );
    }

    #[test]
    fn a_large_floor_does_not_overflow_the_byte_count() {
        let [_, spilling] = FreeDiskFloor::from_mebibytes(u32::MAX)
            .expect("the maximum is in range")
            .commands();

        assert_eq!(
            spilling.get_i64(QUERY_SPILLING_FLOOR).ok(),
            Some(i64::from(u32::MAX) * 1024 * 1024)
        );
    }
}
