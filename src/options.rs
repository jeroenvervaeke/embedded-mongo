//! Everything [`crate::Client::with_options`] can be told, in one object.
//!
//! Some of these reach the engine while WiredTiger is being opened and cannot be changed
//! afterwards; one is a pair of server parameters set on the running engine; and one --
//! [`OpenOptions::command_strands`] -- never reaches the engine at all. It sizes the pool of
//! sessions this crate opens over the engine, which is a decision the safe Rust layer makes
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

/// Storage limits for [`crate::Client::with_options`]. Anything left unset keeps the engine's
/// own default, so `Client::new(path)` and `Client::with_options(path, OpenOptions::new())`
/// open identically -- the free-disk floor included, which an open has to establish rather
/// than leave alone to make true.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenOptions {
    pub(crate) engine: EngineOptions,
    pub(crate) free_disk_floor: Option<FreeDiskFloor>,
    pub(crate) command_strands: Option<CommandStrands>,
}

/// How many commands the engine will run in parallel.
///
/// Each is a session -- a MongoDB client, the embedded equivalent of a connection -- so this
/// is the engine's connection count: parallel commands beyond it wait for a session to come
/// free rather than failing. It is a count of sessions, and a session only does work while a
/// thread drives it, so it is also how many worker threads the async [`Client`](crate::Client)
/// starts, and the most blocking callers that can run at once before one waits.
///
/// It is a decision of this crate, not of the native engine: the engine would run thousands of
/// sessions, and nothing about how many to open is written into the C ABI. So this is validated
/// and defaulted here, in Rust, and changing either costs no native rebuild.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CommandStrands(u32);

impl CommandStrands {
    /// The floor is one session -- an engine that runs no commands in parallel is still an
    /// engine. The ceiling is this crate's own: a session that no thread is driving is an idle
    /// client, and 256 is far past where more parallelism on an embedded engine pays for the
    /// threads it would take to use.
    pub const MIN_COUNT: u32 = 1;
    pub const MAX_COUNT: u32 = 256;

    /// What an open that names no count gets: eight, a small multiple of the cores on the
    /// devices this engine targets. Enough that a caller who fans work out is not quietly
    /// serialized, cheap enough that a caller who does not is out only a few idle clients.
    pub const DEFAULT_COUNT: u32 = 8;

    /// The default as a value, so that resolving an unset option yields a `CommandStrands` --
    /// carrying the "at least one" guarantee -- rather than a bare number.
    pub const DEFAULT: Self = Self(Self::DEFAULT_COUNT);

    pub fn from_count(count: u32) -> Result<Self, OutOfRange> {
        check_range(
            "command strands",
            "strands",
            count,
            Self::MIN_COUNT,
            Self::MAX_COUNT,
        )
        .map(Self)
    }

    pub fn count(self) -> u32 {
        self.0
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

    /// How many commands may run in parallel -- the session-pool size. It sizes the async
    /// [`Client`](crate::Client)'s worker threads, one per session, and bounds how many
    /// blocking callers run at once. Left unset it is [`CommandStrands::DEFAULT_COUNT`].
    pub fn command_strands(mut self, strands: CommandStrands) -> Self {
        self.command_strands = Some(strands);
        self
    }

    /// The session-pool size an open resolves to: what was asked for, or the default. Returns
    /// the newtype, so the "at least one session" guarantee travels to the pool and the worker
    /// count rather than being dropped for a bare `u32` at the boundary that relies on it.
    pub(crate) fn strand_count(options: Option<&Self>) -> CommandStrands {
        options
            .and_then(|options| options.command_strands)
            .unwrap_or(CommandStrands::DEFAULT)
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandStrands, OpenOptions};
    use crate::limits::FreeDiskFloor;
    use embedded_mongodb_sys::{CacheSize, EngineOptions};

    #[test]
    fn an_untouched_options_object_asks_for_nothing() {
        let options = OpenOptions::new();

        assert_eq!(options.engine, EngineOptions::new());
        assert_eq!(options.free_disk_floor, None);
        assert_eq!(options.command_strands, None);
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
        assert_eq!(OpenOptions::strand_count(None), CommandStrands::DEFAULT);

        let asked = OpenOptions::new()
            .command_strands(CommandStrands::from_count(3).expect("3 strands is in range"));
        assert_eq!(OpenOptions::strand_count(Some(&asked)).count(), 3);
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
}
