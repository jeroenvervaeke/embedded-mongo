//! Everything [`crate::Client::with_options`] can be told, in one object.
//!
//! Some of these reach the engine while WiredTiger is being opened and cannot be changed
//! afterwards; one is a pair of server parameters set on the running engine; and one --
//! [`OpenOptions::concurrency`] -- never reaches the engine at all. It sizes the pool of
//! sessions this crate opens over the engine, which is a decision the safe Rust layer makes
//! rather than a knob the native library carries. The split matters to the implementation and
//! not to the caller, so it is hidden here.
//!
//! Being server parameters, the floors belong to the process rather than to one client, so
//! every open establishes them: naming no [`OpenOptions::free_disk_floor`] opens on MongoDB's
//! own, not on whatever a client closed earlier in this process left behind. [`FreeDiskFloor`]
//! has the whole of it.

use crate::{Error, Result, limits::FreeDiskFloor};
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

/// How sessions and async worker threads are allocated.
///
/// Fixed pools preallocate their count. Dynamic pools start at `min`, grow on demand up to
/// `max`, and release sessions idle for `idle_timeout` on the next checkout. Async workers
/// also retire after that timeout, reaping idle sessions even without another command.
/// Neither pool shrinks below `min`; commands beyond `max` wait for capacity.
///
/// Counts must be in `1..=256`, `min <= max`, and the timeout must be nonzero. These are
/// checked before opening the engine, including policies constructed directly as enum variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Concurrency {
    Fixed(u32),
    Dynamic {
        min: u32,
        max: u32,
        idle_timeout: Duration,
    },
}

/// Compatibility name for callers using `CommandStrands::from_count`.
pub type CommandStrands = Concurrency;

impl Concurrency {
    pub const MIN_COUNT: u32 = 1;
    pub const MAX_COUNT: u32 = 256;
    pub const DEFAULT_COUNT: u32 = 8;
    pub const DEFAULT: Self = Self::Dynamic {
        min: 1,
        max: Self::DEFAULT_COUNT,
        idle_timeout: Duration::from_secs(60),
    };

    /// Constructs a fixed pool, preserving the former `CommandStrands` constructor.
    pub fn from_count(count: u32) -> std::result::Result<Self, OutOfRange> {
        check_range(
            "command strands",
            "strands",
            count,
            Self::MIN_COUNT,
            Self::MAX_COUNT,
        )
        .map(Self::Fixed)
    }

    /// The maximum number of simultaneous commands.
    pub fn max(self) -> u32 {
        match self {
            Self::Fixed(count) => count,
            Self::Dynamic { max, .. } => max,
        }
    }

    pub(crate) fn min(self) -> u32 {
        match self {
            Self::Fixed(count) => count,
            Self::Dynamic { min, .. } => min,
        }
    }

    pub(crate) fn idle_timeout(self) -> Option<Duration> {
        match self {
            Self::Fixed(_) => None,
            Self::Dynamic { idle_timeout, .. } => Some(idle_timeout),
        }
    }

    fn validate(self) -> Result<Self> {
        if !(Self::MIN_COUNT..=Self::MAX_COUNT).contains(&self.min())
            || !(self.min()..=Self::MAX_COUNT).contains(&self.max())
        {
            return Err(Error::InvalidArgument(
                "concurrency must satisfy 1 <= min <= max <= 256",
            ));
        }
        if self.idle_timeout().is_some_and(|timeout| timeout.is_zero()) {
            return Err(Error::InvalidArgument(
                "concurrency idle timeout must be nonzero",
            ));
        }
        Ok(self)
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

    /// The session and async worker allocation policy. Unset uses [`Concurrency::DEFAULT`]:
    /// one session/worker initially, growing to eight, with a 60-second idle timeout.
    pub fn concurrency(mut self, concurrency: Concurrency) -> Self {
        self.concurrency = Some(concurrency);
        self
    }

    /// Compatibility spelling of [`Self::concurrency`].
    pub fn command_strands(self, strands: CommandStrands) -> Self {
        self.concurrency(strands)
    }

    pub(crate) fn concurrency_policy(options: Option<&Self>) -> Result<Concurrency> {
        options
            .and_then(|options| options.concurrency)
            .unwrap_or(Concurrency::DEFAULT)
            .validate()
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandStrands, Concurrency, OpenOptions};
    use crate::{Error, limits::FreeDiskFloor};
    use embedded_mongodb_sys::{CacheSize, EngineOptions};
    use std::time::Duration;

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
    fn the_strand_count_defaults_when_unset_and_takes_what_is_asked() {
        assert_eq!(
            OpenOptions::concurrency_policy(None).unwrap(),
            CommandStrands::DEFAULT
        );

        let asked = OpenOptions::new()
            .command_strands(CommandStrands::from_count(3).expect("3 strands is in range"));
        assert_eq!(
            OpenOptions::concurrency_policy(Some(&asked)).unwrap().max(),
            3
        );
    }

    #[test]
    fn a_strand_count_outside_the_range_is_refused() {
        let error = CommandStrands::from_count(0).expect_err("0 strands is below the minimum");
        assert_eq!(
            error.to_string(),
            "command strands must be between 1 and 256 strands, got 0"
        );
        assert!(CommandStrands::from_count(CommandStrands::MAX_COUNT + 1).is_err());
    }

    #[test]
    fn enum_variants_are_validated_before_opening() {
        for policy in [
            Concurrency::Fixed(0),
            Concurrency::Fixed(257),
            Concurrency::Dynamic {
                min: 0,
                max: 8,
                idle_timeout: Duration::from_secs(1),
            },
            Concurrency::Dynamic {
                min: 4,
                max: 3,
                idle_timeout: Duration::from_secs(1),
            },
            Concurrency::Dynamic {
                min: 1,
                max: 257,
                idle_timeout: Duration::from_secs(1),
            },
            Concurrency::Dynamic {
                min: 1,
                max: 8,
                idle_timeout: Duration::ZERO,
            },
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("must-not-be-created");
            assert!(
                matches!(
                    crate::blocking::Client::with_options(
                        &path,
                        OpenOptions::new().concurrency(policy)
                    ),
                    Err(Error::InvalidArgument(_))
                ),
                "accepted {policy:?}"
            );
            assert!(!path.exists(), "validation must precede opening storage");
        }
        for policy in [
            Concurrency::Fixed(1),
            Concurrency::Fixed(256),
            Concurrency::Dynamic {
                min: 1,
                max: 1,
                idle_timeout: Duration::from_nanos(1),
            },
            Concurrency::Dynamic {
                min: 256,
                max: 256,
                idle_timeout: Duration::MAX,
            },
        ] {
            assert_eq!(policy.validate().unwrap(), policy);
        }
        assert_eq!(
            OpenOptions::concurrency_policy(Some(&OpenOptions::new())).unwrap(),
            Concurrency::DEFAULT
        );
    }
}
