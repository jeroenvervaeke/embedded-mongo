//! What a handle is, and where the next one comes from.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher};
use std::num::NonZeroI64;
use std::time::{SystemTime, UNIX_EPOCH};

use jni::sys::jlong;

/// Identity of one open client, as it crosses the boundary in a Java `long`.
///
/// Deliberately not a pointer. A Java `long` is trivially forged, survives the process that
/// produced it in a saved `Bundle`, and comes back after a configuration change or a process
/// restart pointing at nothing; dereferencing one would be a use-after-free. An id is looked
/// up instead, so every one of those cases is a miss in a map.
///
/// Two parts, so that a miss is what actually happens across a restart: a tag drawn once per
/// process in the high 31 bits, and a sequence in the low 32. A bare counter would restart at
/// 1 in the new process, and the handle Android restored from a `Bundle` would then name
/// whichever client this process opened first -- a different database, silently, which is
/// worse than a crash. The sign bit stays clear so every id survives the trip through Java's
/// signed `long`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HandleId(NonZeroI64);

impl HandleId {
    /// Accepts what Java handed over. `0` -- the default value of an uninitialised `long`
    /// field -- and negative values are rejected here; every other forgery misses the map.
    pub fn new(raw: jlong) -> Option<Self> {
        NonZeroI64::new(raw).filter(|raw| raw.get() > 0).map(Self)
    }

    /// The value Java holds.
    pub fn get(self) -> jlong {
        self.0.get()
    }

    /// `None` only for a zero tag, which [`process_tag`] never returns; propagating it keeps
    /// this total rather than asserting.
    fn from_parts(tag: i64, sequence: u32) -> Option<Self> {
        // The invariant belongs here rather than in the callers: `Counter` is visible to the
        // whole crate and its fields can be set directly, and a tag above `TAG_MASK` shifts
        // into the sign bit to make a negative id that `NonZeroI64` accepts happily and Java
        // can never quote back.
        debug_assert!(
            (1..=TAG_MASK).contains(&tag),
            "a handle tag must be 1..={TAG_MASK}, not {tag}"
        );
        NonZeroI64::new((tag << SEQUENCE_BITS) | i64::from(sequence)).map(Self)
    }
}

impl fmt::Display for HandleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// How many low bits of an id are the sequence; the rest, minus the sign, are the tag.
const SEQUENCE_BITS: u32 = 32;

/// The widest tag that leaves the sign bit clear.
const TAG_MASK: i64 = (1 << (i64::BITS - 1 - SEQUENCE_BITS)) - 1;

/// Where the next id comes from. Three states rather than a sentinel value, so that "no id
/// has been issued yet" and "this process has spent its ids" cannot be confused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Counter {
    /// Nothing has been opened yet, so the process tag has not been drawn.
    Unstarted,
    /// The tag and the sequence the next id will be composed from.
    Next { tag: i64, sequence: u32 },
    /// All 2^32 - 1 ids of this process's tag have been handed out.
    Spent,
}

impl Counter {
    /// Advances and returns the next id, or `None` once there are none left.
    pub(crate) fn take_next(&mut self) -> Option<HandleId> {
        let (tag, sequence) = match *self {
            Self::Unstarted => (process_tag(), 1),
            Self::Next { tag, sequence } => (tag, sequence),
            Self::Spent => return None,
        };
        let id = HandleId::from_parts(tag, sequence);
        *self = match (id, sequence.checked_add(1)) {
            (Some(_), Some(sequence)) => Self::Next { tag, sequence },
            _ => Self::Spent,
        };
        id
    }
}

/// The high bits of every id this process issues, drawn once from the operating system's
/// randomness. See [`HandleId`] for why a bare counter is not enough.
fn process_tag() -> i64 {
    // `RandomState` seeds itself per process from the OS. The pid and the clock are mixed in
    // as well, so a platform with a weak `RandomState` still separates two runs.
    let mut hasher = RandomState::new().build_hasher();
    std::process::id().hash(&mut hasher);
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default()
        .hash(&mut hasher);
    // A zero tag would put this process's ids back in the low range an older process handed
    // out, which is the whole thing the tag exists to prevent.
    ((hasher.finish() as i64) & TAG_MASK).max(1)
}

#[cfg(test)]
mod tests {
    use super::{Counter, HandleId, SEQUENCE_BITS, TAG_MASK, process_tag};
    use jni::sys::jlong;

    #[test]
    fn rejects_handles_java_can_produce_without_ever_calling_open() {
        // `0` is what an uninitialised Java `long` field holds, and a truncated or
        // sign-flipped id is what a corrupted `Bundle` produces.
        assert_eq!(HandleId::new(0), None);
        assert_eq!(HandleId::new(-1), None);
        assert_eq!(HandleId::new(jlong::MIN), None);
        assert!(HandleId::new(1).is_some());
    }

    #[test]
    fn every_id_stays_positive_at_the_widest_tag_and_sequence() {
        let widest = HandleId::from_parts(TAG_MASK, u32::MAX).expect("a non-zero tag composes");
        assert_eq!(widest.get(), jlong::MAX);
        assert!(
            HandleId::new(widest.get()).is_some(),
            "Java can hand it back"
        );
    }

    /// Only meaningful where `debug_assert!` is compiled in; in release the guard is not
    /// there to fire, so neither is this test.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a handle tag must be")]
    fn refuses_to_compose_an_id_from_a_tag_that_would_reach_the_sign_bit() {
        let _ = HandleId::from_parts(TAG_MASK + 1, 5);
    }

    #[test]
    fn the_sequence_occupies_the_low_bits_and_the_tag_the_rest() {
        let id = HandleId::from_parts(7, 9).expect("a non-zero tag composes");
        assert_eq!(id.get(), (7 << SEQUENCE_BITS) | 9);
    }

    #[test]
    fn a_process_tag_is_never_zero_and_never_touches_the_sign_bit() {
        for _ in 0..1_000 {
            let tag = process_tag();
            assert!((1..=TAG_MASK).contains(&tag), "{tag} is out of range");
        }
    }

    #[test]
    fn the_counter_walks_the_sequence_within_one_tag() {
        let mut counter = Counter::Unstarted;
        let first = counter.take_next().expect("a fresh counter has ids");
        let second = counter.take_next().expect("a fresh counter has ids");
        assert_eq!(
            second.get(),
            first.get() + 1,
            "the sequence advances by one"
        );
        assert_eq!(
            second.get() >> SEQUENCE_BITS,
            first.get() >> SEQUENCE_BITS,
            "the tag stays put"
        );
    }

    #[test]
    fn the_counter_stops_rather_than_wrapping_into_the_tag() {
        let mut counter = Counter::Next {
            tag: 3,
            sequence: u32::MAX,
        };
        let last = counter.take_next().expect("the final id is still issued");
        assert_eq!(last.get(), (3 << SEQUENCE_BITS) | i64::from(u32::MAX));
        assert_eq!(counter, Counter::Spent);
        assert_eq!(counter.take_next(), None, "and stays spent");
    }
}
