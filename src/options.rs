//! Everything [`crate::Client::with_options`] can be told, in one object.
//!
//! Some of these reach the engine while WiredTiger is being opened and cannot be changed
//! afterwards; one is a pair of server parameters set on the running engine; and one --
//! [`OpenOptions::concurrency`] -- never reaches the engine at all. It is the policy by which
//! this crate opens sessions over the engine, which is a decision the safe Rust layer makes
//! rather than a knob the native library carries. The split matters to the implementation and
//! not to the caller, so it is hidden here.
//!
//! Being server parameters, the floors belong to the process rather than to one client, so
//! every open establishes them: naming no [`OpenOptions::free_disk_floor`] opens on MongoDB's
//! own, not on whatever a client closed earlier in this process left behind. [`FreeDiskFloor`]
//! has the whole of it.

use crate::limits::FreeDiskFloor;
use embedded_mongodb_sys::{
    CacheSize, EngineOptions, JournalFileSize, OutOfRange, Preallocation, check_range,
};
use std::time::Duration;

/// Storage limits for [`crate::Client::with_options`]. Anything left unset keeps the engine's
/// own default, so `Client::new(path)` and `Client::with_options(path, OpenOptions::new())`
/// open identically -- the free-disk floor included, which an open has to establish rather
/// than leave alone to make true.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenOptions {
    pub(crate) engine: EngineOptions,
    pub(crate) free_disk_floor: Option<FreeDiskFloor>,
    pub(crate) concurrency: Option<Concurrency>,
}

/// How many commands the engine may run in parallel, and whether that number moves.
///
/// Each parallel command is a session -- a MongoDB client, the embedded equivalent of a
/// connection -- so this is the engine's connection policy: commands past the ceiling wait for
/// a session to come free rather than failing. A session only does work while a thread drives
/// it, so the same policy sizes the async [`Client`](crate::Client)'s worker threads.
///
/// There are two shapes.
///
/// - [`from_count`](Concurrency::from_count) is a **fixed** pool: exactly that many sessions,
///   opened at open time and kept for the life of the client. What to reach for when the
///   caller knows its own concurrency.
/// - [`dynamic`](Concurrency::dynamic) is an **elastic** pool: `min` sessions opened up front,
///   more opened on demand up to `max` while every session is busy, and sessions idle past
///   `idle_timeout` closed again down to `min`. What to reach for when the caller cannot
///   predict its concurrency -- the bindings especially -- because `max` is then a ceiling
///   rather than a cost.
///
/// It is a decision of this crate, not of the native engine: the engine would run thousands of
/// sessions, and nothing about how many to open is written into the C ABI. So this is validated
/// and defaulted here, in Rust, and changing either costs no native rebuild.
///
/// The fields are private because two of them constrain each other: `min` above `max` and a
/// zero idle timeout -- which would close a session the instant it came back, and spin the
/// reaper's timed wait doing it -- are refused by the constructors rather than left to be
/// discovered at run time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Concurrency {
    min: u32,
    max: u32,
    /// How long a session above `min` may sit idle before it is closed. `None` is a fixed
    /// pool: it never opens a session beyond `min == max`, so it has nothing to reap.
    idle_timeout: Option<Duration>,
}

impl Concurrency {
    /// The floor is one session -- an engine that runs no commands in parallel is still an
    /// engine. The ceiling is this crate's own: a session that no thread is driving is an idle
    /// client, and 256 is far past where more parallelism on an embedded engine pays for the
    /// threads it would take to use.
    pub const MIN_COUNT: u32 = 1;
    pub const MAX_COUNT: u32 = 256;

    /// What an open that names no policy gets: eight fixed sessions, a small multiple of the
    /// cores on the devices this engine targets. Enough that a caller who fans work out is not
    /// quietly serialized, cheap enough that a caller who does not is out only a few idle
    /// clients.
    pub const DEFAULT_COUNT: u32 = 8;

    /// The default as a value, so that resolving an unset option yields a `Concurrency` --
    /// carrying the "at least one" guarantee -- rather than a bare number.
    pub const DEFAULT: Self = Self::preallocated(Self::DEFAULT_COUNT);

    /// A fixed pool of `count` sessions: all of them opened at open time, none ever closed
    /// before the client is. Today's behaviour, and the name it had.
    pub fn from_count(count: u32) -> Result<Self, OutOfRange> {
        check_range(
            "concurrency",
            "sessions",
            count,
            Self::MIN_COUNT,
            Self::MAX_COUNT,
        )
        .map(Self::preallocated)
    }

    /// An elastic pool: `min` sessions up front, up to `max` while callers are waiting, and
    /// sessions idle longer than `idle_timeout` closed back down to `min`.
    ///
    /// Refuses a `min` above `max`, either outside [`MIN_COUNT`](Concurrency::MIN_COUNT) and
    /// [`MAX_COUNT`](Concurrency::MAX_COUNT), and a zero idle timeout.
    pub fn dynamic(min: u32, max: u32, idle_timeout: Duration) -> Result<Self, OutOfRange> {
        let max = check_range(
            "maximum concurrency",
            "sessions",
            max,
            Self::MIN_COUNT,
            Self::MAX_COUNT,
        )?;
        // Checked against the ceiling just validated, so `min > max` comes back as the range
        // error it is rather than needing an error case of its own.
        let min = check_range("minimum concurrency", "sessions", min, Self::MIN_COUNT, max)?;
        // Saturating rather than wrapping: the floor exists to refuse zero, and a timeout past
        // the 49 days a `u32` of milliseconds holds is nobody's mistake.
        let millis = u32::try_from(idle_timeout.as_millis()).unwrap_or(u32::MAX);
        check_range("idle timeout", "milliseconds", millis, 1, u32::MAX)?;
        Ok(Self {
            min,
            max,
            idle_timeout: Some(idle_timeout),
        })
    }

    /// Sessions opened before the first command runs.
    pub fn min(self) -> u32 {
        self.min
    }

    /// The ceiling: the most sessions that will ever be open at once, and so the most commands
    /// that ever run in parallel.
    pub fn max(self) -> u32 {
        self.max
    }

    /// How long an idle session above [`min`](Concurrency::min) is kept before being closed,
    /// or `None` for a fixed pool, which closes none.
    pub fn idle_timeout(self) -> Option<Duration> {
        self.idle_timeout
    }

    /// The unchecked fixed constructor, so that [`DEFAULT`](Concurrency::DEFAULT) can be a
    /// `const` rather than an unwrap.
    const fn preallocated(count: u32) -> Self {
        Self {
            min: count,
            max: count,
            idle_timeout: None,
        }
    }
}

impl OpenOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// The ceiling on WiredTiger's cache.
    pub fn cache_size(mut self, cache: CacheSize) -> Self {
        self.engine = self.engine.cache_size(cache);
        self
    }

    /// The size of one journal file, and so what an empty directory costs on disk.
    pub fn journal_file_size(mut self, size: JournalFileSize) -> Self {
        self.engine = self.engine.journal_file_size(size);
        self
    }

    /// Whether a spare journal file is kept ready ahead of the one being written.
    pub fn journal_preallocation(mut self, preallocation: Preallocation) -> Self {
        self.engine = self.engine.journal_preallocation(preallocation);
        self
    }

    /// How much free disk space an index build or a spilling query insists on. Read
    /// [`FreeDiskFloor`] before lowering it: it is a server parameter of the whole process,
    /// which is why it is named at every open rather than remembered for a directory.
    pub fn free_disk_floor(mut self, floor: FreeDiskFloor) -> Self {
        self.free_disk_floor = Some(floor);
        self
    }

    /// How many commands may run in parallel, and whether that number moves -- the
    /// session-pool policy. It sizes the async [`Client`](crate::Client)'s worker threads, one
    /// per session, and bounds how many blocking callers run at once. Left unset it is
    /// [`Concurrency::DEFAULT`].
    pub fn concurrency(mut self, concurrency: Concurrency) -> Self {
        self.concurrency = Some(concurrency);
        self
    }

    /// The session-pool policy an open resolves to: what was asked for, or the default.
    /// Returns the newtype, so the "at least one session" guarantee travels to the pool and
    /// the worker count rather than being dropped for a bare `u32` at the boundary that relies
    /// on it.
    pub(crate) fn resolve_concurrency(options: Option<&Self>) -> Concurrency {
        options
            .and_then(|options| options.concurrency)
            .unwrap_or(Concurrency::DEFAULT)
    }
}

#[cfg(test)]
mod tests {
    use super::{Concurrency, OpenOptions};
    use crate::limits::FreeDiskFloor;
    use embedded_mongodb_sys::{CacheSize, EngineOptions};
    use std::time::Duration;

    fn a_minute() -> Duration {
        Duration::from_secs(60)
    }

    #[test]
    fn an_untouched_options_object_asks_for_nothing() {
        let options = OpenOptions::new();

        assert_eq!(options.engine, EngineOptions::new());
        assert_eq!(options.free_disk_floor, None);
        assert_eq!(options.concurrency, None);
    }

    #[test]
    fn the_engine_limits_are_kept_apart_from_the_floor() {
        let cache = CacheSize::from_mebibytes(32).expect("32 MiB is in range");
        let floor = FreeDiskFloor::from_mebibytes(16).expect("16 MiB is in range");

        let options = OpenOptions::new().cache_size(cache).free_disk_floor(floor);

        assert_eq!(options.engine, EngineOptions::new().cache_size(cache));
        assert_eq!(options.free_disk_floor, Some(floor));
    }

    #[test]
    fn the_concurrency_defaults_when_unset_and_takes_what_is_asked() {
        assert_eq!(OpenOptions::resolve_concurrency(None), Concurrency::DEFAULT);

        let asked = OpenOptions::new()
            .concurrency(Concurrency::from_count(3).expect("3 sessions is in range"));
        assert_eq!(OpenOptions::resolve_concurrency(Some(&asked)).max(), 3);
    }

    #[test]
    fn a_fixed_pool_never_grows_and_never_reaps() {
        let fixed = Concurrency::from_count(4).expect("4 sessions is in range");

        assert_eq!(fixed.min(), 4);
        assert_eq!(fixed.max(), 4);
        assert_eq!(fixed.idle_timeout(), None);
    }

    #[test]
    fn the_default_is_a_fixed_pool_of_the_default_count() {
        assert_eq!(
            Concurrency::DEFAULT,
            Concurrency::from_count(Concurrency::DEFAULT_COUNT).expect("the default is in range")
        );
    }

    #[test]
    fn a_dynamic_pool_keeps_its_floor_ceiling_and_timeout() {
        let elastic = Concurrency::dynamic(2, 16, a_minute()).expect("2..16 is in range");

        assert_eq!(elastic.min(), 2);
        assert_eq!(elastic.max(), 16);
        assert_eq!(elastic.idle_timeout(), Some(a_minute()));
    }

    #[test]
    fn a_fixed_count_outside_the_range_is_refused() {
        let error = Concurrency::from_count(0).expect_err("0 sessions is below the minimum");
        assert_eq!(
            error.to_string(),
            "concurrency must be between 1 and 256 sessions, got 0"
        );
        assert!(Concurrency::from_count(Concurrency::MAX_COUNT + 1).is_err());
    }

    #[test]
    fn a_dynamic_ceiling_outside_the_range_is_refused() {
        let error = Concurrency::dynamic(1, Concurrency::MAX_COUNT + 1, a_minute())
            .expect_err("a ceiling above the maximum is out of range");
        assert_eq!(
            error.to_string(),
            "maximum concurrency must be between 1 and 256 sessions, got 257"
        );
        assert!(Concurrency::dynamic(1, 0, a_minute()).is_err());
    }

    #[test]
    fn a_dynamic_floor_above_its_ceiling_is_refused() {
        let error = Concurrency::dynamic(9, 8, a_minute())
            .expect_err("a floor above the ceiling is out of range");
        assert_eq!(
            error.to_string(),
            "minimum concurrency must be between 1 and 8 sessions, got 9"
        );
    }

    #[test]
    fn a_dynamic_floor_below_the_minimum_is_refused() {
        let error =
            Concurrency::dynamic(0, 8, a_minute()).expect_err("0 sessions is below the minimum");
        assert_eq!(
            error.to_string(),
            "minimum concurrency must be between 1 and 8 sessions, got 0"
        );
    }

    #[test]
    fn a_zero_idle_timeout_is_refused() {
        let error = Concurrency::dynamic(1, 8, Duration::ZERO)
            .expect_err("a session cannot be reaped the instant it is returned");
        assert_eq!(
            error.to_string(),
            "idle timeout must be between 1 and 4294967295 milliseconds, got 0"
        );
    }

    #[test]
    fn an_idle_timeout_past_what_the_check_counts_is_still_accepted() {
        let century = Duration::from_secs(60 * 60 * 24 * 365 * 100);

        let elastic =
            Concurrency::dynamic(1, 8, century).expect("a very long timeout is not a mistake");

        assert_eq!(elastic.idle_timeout(), Some(century));
    }

    #[test]
    fn a_floor_equal_to_its_ceiling_is_accepted() {
        let elastic = Concurrency::dynamic(4, 4, a_minute()).expect("4..4 is in range");

        assert_eq!(elastic.min(), 4);
        assert_eq!(elastic.max(), 4);
    }
}
