//! The floor a caller names, and the pair of commands it turns into.

use super::knobs::{BYTES_PER_MEBIBYTE, IndexBuildFloor, QuerySpillingFloor, floor_commands};
use bson::Document;
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
/// [`Client`](crate::Client) that named it: it survives that client's
/// [`Client::close`](crate::Client::close), and left alone it would still be in force for the
/// next open. That is not guessable from an API where the floor is named per-open, so this
/// library does not leave it to be discovered -- **every open establishes the floor**, putting
/// MongoDB's own back where the caller named none. An application that opens one database on a
/// lowered floor, closes it and opens another gets the defaults it asked for rather than the
/// previous database's floor.
///
/// Two consequences worth knowing. A floor moved with
/// [`set_free_disk_floor`](crate::set_free_disk_floor) on a running client lasts until the next
/// open, which resets it -- it is not remembered for a directory, so an application that wants
/// it every time names it in
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
