//! The storage limits, as they cross the JNI boundary.
//!
//! Only the limits `wiredtiger_open` reads are here. The free-disk floors are `setParameter`
//! server parameters, which the Kotlin side applies over the `command` entry point that
//! already exists -- see `FreeDiskFloor.kt`. Nothing about them needs a native symbol, and a
//! knob that needs no native symbol does not get one.
//!
//! That absence is now load-bearing, and a slot for the floor would be a defect rather than a
//! feature. Both layers establish the floor at every open, each recording MongoDB's own floors
//! at the first one -- `embedded_mongodb::FreeDiskFloor` and `FreeDiskFloorAtOpen.kt` say why.
//! Because the floor never reaches this vector, every open through `Client::with_options` here
//! names no floor, so the crate below has already put MongoDB's own back by the time the Kotlin
//! layer takes its reading, and the two agree on what the defaults are. Route the floor through
//! a slot and that stops holding: the crate would apply the caller's floor during the open, and
//! the Kotlin layer would read it back and remember 32 MiB as MongoDB's own for the life of the
//! process -- the very defect both layers exist to prevent.
//!
//! # Why a `long[]` and not four parameters
//!
//! The same reason `embedded_mongodb_open_options` carries its own `size`: one entry point has
//! to survive every limit added later. A Java array reports its own length, so it is that
//! struct with the size field already filled in. A caller built against an older library
//! passes fewer slots and the rest read as unset; a caller built against a newer one passes
//! more and this build ignores what it does not know. Either way the symbol and its descriptor
//! stay put, which a fourth `long` parameter would not: changing the descriptor of a native
//! method leaves its C symbol name untouched, so the JVM would go on binding this function and
//! start calling it with a stack it does not describe.
//!
//! Zero is "the engine's default" in every slot, exactly as in the C struct, which is what
//! lets an unset limit stay unset the whole way down rather than being answered with a number
//! restated here.
//!
//! A slot past the end of what this build reads is ignored rather than refused, which means a
//! caller newer than the library it is loaded against is told its limit was applied when it was
//! not. That is deliberate, and it is the C struct's rule rather than one invented here --
//! `requested()` in `engine_options.cpp` copies `min(options->size, sizeof(copy))` and never
//! looks at the rest. Making this layer stricter would put the two contracts out of step, which
//! costs more than it buys while the Kotlin classes and this library ship in one AAR and cannot
//! be skewed. If they are ever published apart, refusing a non-zero unknown slot is the change
//! to make, and it belongs in both layers at once.

use embedded_mongodb::{CacheSize, JournalFileSize, OpenOptions, OutOfRange, Preallocation};
use jni::sys::jlong;

use crate::error::{BridgeError, Result};

/// How many slots this build reads. A caller may hand over fewer or more.
pub const SLOTS: usize = 3;

/// Builds the options one `openWithOptions` call asked for.
///
/// `slots` is what the caller's array held, already padded with zeros to [`SLOTS`], so the
/// three reads below cannot be out of bounds.
///
/// Every bound is checked here rather than left to the engine. The Kotlin types make an
/// out-of-range value unconstructable, so reaching this is either a caller that bypassed them
/// or a bug in the encoding -- and both are better as a named `InvalidArgument` than as an
/// opaque `EINVAL` out of `wiredtiger_open`.
pub fn open_options(slots: [jlong; SLOTS]) -> Result<OpenOptions> {
    let [cache, journal, preallocation] = slots;
    let mut options = OpenOptions::new();
    if let Some(mebibytes) = size_slot(cache, "cache_size_mebibytes")? {
        options = options.cache_size(range(CacheSize::from_mebibytes(mebibytes))?);
    }
    if let Some(kibibytes) = size_slot(journal, "journal_file_kibibytes")? {
        options = options.journal_file_size(range(JournalFileSize::from_kibibytes(kibibytes))?);
    }
    if let Some(preallocation) = PreallocationSlot::read(preallocation)?.requested() {
        options = options.journal_preallocation(preallocation);
    }
    Ok(options)
}

/// What the journal pre-allocation slot may hold.
///
/// The same numbering as `embedded_mongodb_journal_prealloc` in the C header, and for the same
/// reason: zero has to mean "not set" so that a caller who filled in two slots and left this
/// one alone is not read as having asked for one of the two answers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreallocationSlot {
    Unset,
    Enabled,
    Disabled,
}

impl PreallocationSlot {
    fn read(value: jlong) -> Result<Self> {
        match value {
            0 => Ok(Self::Unset),
            1 => Ok(Self::Enabled),
            2 => Ok(Self::Disabled),
            _ => Err(BridgeError::invalid_argument(format!(
                "journal_preallocation must be 0 (the engine's default), 1 (enabled) or 2 \
                 (disabled), got {value}"
            ))),
        }
    }

    /// `None` where the caller asked for nothing, which is not the same as asking for
    /// [`Preallocation::Disabled`]: unset leaves the engine's default in place, and the
    /// engine is free to change it.
    fn requested(self) -> Option<Preallocation> {
        match self {
            Self::Unset => None,
            Self::Enabled => Some(Preallocation::Enabled),
            Self::Disabled => Some(Preallocation::Disabled),
        }
    }
}

/// The value a size slot asked for, or `None` for the engine's default.
///
/// Java has no unsigned integers, so a slot arrives as a `jlong` that can hold values the
/// limits behind it cannot. Narrowing here means the range error names the slot rather than
/// wrapping into some other number first.
fn size_slot(value: jlong, name: &str) -> Result<Option<u32>> {
    if value == 0 {
        return Ok(None);
    }
    let Ok(value) = u32::try_from(value) else {
        return Err(BridgeError::invalid_argument(format!(
            "{name} must fit an unsigned 32-bit integer, got {value}"
        )));
    };
    Ok(Some(value))
}

fn range<T>(checked: std::result::Result<T, OutOfRange>) -> Result<T> {
    checked.map_err(|error| BridgeError::invalid_argument(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{PreallocationSlot, SLOTS, open_options};
    use crate::ErrorCode;
    use embedded_mongodb::{CacheSize, JournalFileSize, OpenOptions, Preallocation};

    #[test]
    fn an_all_zero_vector_asks_for_nothing() {
        let options = open_options([0; SLOTS]).expect("zeros are the engine's defaults");

        assert_eq!(options, OpenOptions::new());
    }

    #[test]
    fn every_slot_reaches_the_limit_it_names() {
        let options = open_options([64, 512, 1]).expect("all three are in range");

        assert_eq!(
            options,
            OpenOptions::new()
                .cache_size(CacheSize::from_mebibytes(64).expect("64 MiB is in range"))
                .journal_file_size(
                    JournalFileSize::from_kibibytes(512).expect("512 KiB is in range")
                )
                .journal_preallocation(Preallocation::Enabled)
        );
    }

    /// The slot a caller left alone must stay unset rather than become a number chosen here:
    /// the engine owns its defaults, and this vector is how a caller declines to name one.
    #[test]
    fn a_slot_left_at_zero_leaves_the_others_alone() {
        let options = open_options([0, 512, 0]).expect("one slot in range");

        assert_eq!(
            options,
            OpenOptions::new().journal_file_size(
                JournalFileSize::from_kibibytes(512).expect("512 KiB is in range")
            )
        );
    }

    #[test]
    fn disabled_preallocation_is_distinct_from_unset() {
        assert_eq!(
            open_options([0, 0, 2]).expect("2 is disabled"),
            OpenOptions::new().journal_preallocation(Preallocation::Disabled)
        );
    }

    #[test]
    fn a_preallocation_slot_that_names_no_policy_is_refused() {
        let error = open_options([0, 0, 3]).expect_err("3 is not a policy");

        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(error.message().contains("journal_preallocation"), "{error}");
    }

    #[test]
    fn a_cache_outside_wiredtigers_range_is_refused_before_the_engine_is_opened() {
        let error = open_options([20_000_000, 0, 0]).expect_err("20 TB is above the maximum");

        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(error.message().contains("cache size"), "{error}");
    }

    #[test]
    fn a_journal_file_outside_wiredtigers_range_is_refused() {
        let error = open_options([0, 1, 0]).expect_err("1 KiB is below the minimum");

        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(error.message().contains("journal file size"), "{error}");
    }

    /// Java's `long` is signed and wider than the limits behind these slots, so both ends of
    /// what it can hold have to be refused by name rather than truncated into some other
    /// number.
    #[test]
    fn a_slot_java_could_hold_but_the_limit_could_not_is_refused_by_name() {
        for slot in [-1, i64::from(u32::MAX) + 1, i64::MAX] {
            let Err(error) = open_options([slot, 0, 0]) else {
                panic!("{slot} is not a cache size and has to be refused");
            };

            assert_eq!(error.code(), ErrorCode::InvalidArgument);
            assert!(
                error.message().contains("cache_size_mebibytes"),
                "{slot} was not reported against its own slot: {error}"
            );
        }
    }

    #[test]
    fn the_preallocation_slot_tells_unset_from_the_two_policies() {
        assert_eq!(
            PreallocationSlot::read(0).map(PreallocationSlot::requested),
            Ok(None)
        );
        assert_eq!(
            PreallocationSlot::read(1).map(PreallocationSlot::requested),
            Ok(Some(Preallocation::Enabled))
        );
        assert_eq!(
            PreallocationSlot::read(2).map(PreallocationSlot::requested),
            Ok(Some(Preallocation::Disabled))
        );
    }
}
