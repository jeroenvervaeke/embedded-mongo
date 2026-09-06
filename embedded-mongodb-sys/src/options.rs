//! The two storage limits that can only be chosen while WiredTiger is being opened.
//!
//! Deliberately just two. MongoDB's other sizing knobs are server parameters, which a client
//! can set on a running engine, so they belong in the safe layer rather than in an FFI struct
//! that has to be rebuilt and re-released to grow a field. What is left here is what
//! `wiredtiger_open` reads once and never reconsiders: the cache ceiling and the journal.
//!
//! Every option left unset takes the native library's own default rather than a number
//! restated on this side, so there is one place to change a default.
//!
//! Each value is a type rather than a number: a `CacheSize` cannot be handed to the journal,
//! and neither can be constructed outside the range WiredTiger accepts.

/// A limit outside the range the engine accepts. Raised where the value is built, so an
/// unusable number never reaches `open`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{name} must be between {low} and {high} {unit}, got {value}")]
pub struct OutOfRange {
    name: &'static str,
    unit: &'static str,
    value: u32,
    low: u32,
    high: u32,
}

/// Overrides for [`crate::Client::open_with_options`]. Anything left unset is the engine's
/// default, so `EngineOptions::new()` and [`crate::Client::open`] open the same way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineOptions {
    cache: Option<CacheSize>,
    journal_file_size: Option<JournalFileSize>,
    journal_preallocation: Option<Preallocation>,
    command_strands: Option<CommandStrands>,
}

/// The ceiling on the WiredTiger cache. A ceiling rather than an allocation: the engine grows
/// into it as pages are read, so this decides how much resident memory a busy engine may
/// reach, not how much an idle one costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CacheSize(u32);

/// The size of one journal file. WiredTiger allocates each one in full the moment it creates
/// it, so this is what an otherwise empty database directory costs on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct JournalFileSize(u32);

/// Whether WiredTiger keeps a spare journal file ready ahead of the one it is writing.
///
/// The spare costs a second journal file on disk at all times and buys the writing thread the
/// latency of creating one at a rollover. Durability does not enter into it: the file is
/// created, extended and fsynced identically either way, only earlier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preallocation {
    Enabled,
    Disabled,
}

/// How many commands the engine will run in parallel.
///
/// Each strand is a MongoDB session -- what a connection would be to a server -- so this is
/// the embedded engine's connection count: parallel commands beyond it wait for a strand to
/// come free rather than failing. It is a count of sessions, not of threads. A command still
/// executes on the thread that called [`crate::Client::run_command`], so parallelism also
/// takes that many calling threads; a strand nobody is calling on costs one idle session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CommandStrands(u32);

impl EngineOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cache_size(mut self, cache: CacheSize) -> Self {
        self.cache = Some(cache);
        self
    }

    pub fn journal_file_size(mut self, size: JournalFileSize) -> Self {
        self.journal_file_size = Some(size);
        self
    }

    pub fn journal_preallocation(mut self, preallocation: Preallocation) -> Self {
        self.journal_preallocation = Some(preallocation);
        self
    }

    pub fn command_strands(mut self, strands: CommandStrands) -> Self {
        self.command_strands = Some(strands);
        self
    }

    /// What was asked for, if anything: callers sizing something to the pool -- a worker
    /// thread apiece, say -- read it here rather than restating it.
    pub fn requested_command_strands(&self) -> Option<CommandStrands> {
        self.command_strands
    }

    /// The shape `embedded_mongodb_open_with_options` reads, where zero means "the engine's
    /// default" in every field.
    pub(crate) fn to_ffi(self) -> crate::ffi::bridge::NativeOpenOptions {
        crate::ffi::bridge::NativeOpenOptions {
            cache_size_mb: self.cache.map_or(0, CacheSize::mebibytes),
            journal_file_max_kb: self.journal_file_size.map_or(0, JournalFileSize::kibibytes),
            journal_prealloc: self.journal_preallocation.map_or(0, Preallocation::native),
            command_strands: self.command_strands.map_or(0, CommandStrands::count),
        }
    }
}

impl CacheSize {
    /// WiredTiger's `cache_size` is `min=1MB,max=10TB`; MongoDB clamps the same value at
    /// 10,000,000 MB. Both are in src/third_party/wiredtiger/src/config/config_def.c and
    /// src/mongo/db/storage/wiredtiger/wiredtiger_util.cpp respectively.
    pub const MIN_MEBIBYTES: u32 = 1;
    pub const MAX_MEBIBYTES: u32 = 10_000_000;

    pub fn from_mebibytes(mebibytes: u32) -> Result<Self, OutOfRange> {
        check_range(
            "cache size",
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
}

impl JournalFileSize {
    /// WiredTiger's `log.file_max` is `min=100KB,max=2GB`, from the same config_def.c.
    pub const MIN_KIBIBYTES: u32 = 100;
    pub const MAX_KIBIBYTES: u32 = 2 * 1024 * 1024;

    pub fn from_kibibytes(kibibytes: u32) -> Result<Self, OutOfRange> {
        check_range(
            "journal file size",
            "KiB",
            kibibytes,
            Self::MIN_KIBIBYTES,
            Self::MAX_KIBIBYTES,
        )
        .map(Self)
    }

    pub fn kibibytes(self) -> u32 {
        self.0
    }
}

impl Preallocation {
    /// The `embedded_mongodb_journal_prealloc` value for this policy. Zero is reserved for
    /// "unset", which is why the enum starts at one.
    fn native(self) -> u32 {
        match self {
            Self::Enabled => 1,
            Self::Disabled => 2,
        }
    }
}

impl CommandStrands {
    /// This library's own bound rather than MongoDB's: the engine runs thousands of sessions
    /// happily, but a strand is only useful with a calling thread parked in
    /// `run_command` on it, and 256 is far past where parallelism on an embedded engine
    /// stops paying. Must match `kMaxCommandStrands` in `native/engine_options.cpp`.
    pub const MIN_COUNT: u32 = 1;
    pub const MAX_COUNT: u32 = 256;

    /// What an open that asks for nothing gets, restated from `kDefaultCommandStrands` in
    /// `native/engine_options.cpp` because callers sizing threads to the pool need the
    /// number before the engine exists to ask.
    pub const DEFAULT_COUNT: u32 = 8;

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

/// Checks one limit against its range, so that every newtype here and every one built on top
/// of this crate reports a range violation the same way and from the same code.
pub fn check_range(
    name: &'static str,
    unit: &'static str,
    value: u32,
    low: u32,
    high: u32,
) -> Result<u32, OutOfRange> {
    match (low..=high).contains(&value) {
        true => Ok(value),
        false => Err(OutOfRange {
            name,
            unit,
            value,
            low,
            high,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheSize, CommandStrands, EngineOptions, JournalFileSize, Preallocation};

    #[test]
    fn unset_options_ask_the_engine_for_its_defaults() {
        let ffi = EngineOptions::new().to_ffi();

        assert_eq!(ffi.cache_size_mb, 0);
        assert_eq!(ffi.journal_file_max_kb, 0);
        assert_eq!(ffi.journal_prealloc, 0);
        assert_eq!(ffi.command_strands, 0);
    }

    #[test]
    fn set_options_reach_the_engine_in_its_own_units() {
        let ffi = EngineOptions::new()
            .cache_size(CacheSize::from_mebibytes(48).expect("48 MiB is in range"))
            .journal_file_size(JournalFileSize::from_kibibytes(4096).expect("4 MiB is in range"))
            .journal_preallocation(Preallocation::Enabled)
            .to_ffi();

        assert_eq!(ffi.cache_size_mb, 48);
        assert_eq!(ffi.journal_file_max_kb, 4096);
        assert_eq!(ffi.journal_prealloc, 1);
    }

    #[test]
    fn disabled_preallocation_is_distinct_from_unset() {
        let ffi = EngineOptions::new()
            .journal_preallocation(Preallocation::Disabled)
            .to_ffi();

        assert_eq!(ffi.journal_prealloc, 2);
    }

    #[test]
    fn a_cache_below_wiredtigers_minimum_is_refused() {
        let error = CacheSize::from_mebibytes(0).expect_err("0 MiB is below the minimum");

        assert_eq!(
            error.to_string(),
            "cache size must be between 1 and 10000000 MiB, got 0"
        );
    }

    #[test]
    fn a_cache_above_wiredtigers_maximum_is_refused() {
        assert!(CacheSize::from_mebibytes(CacheSize::MAX_MEBIBYTES + 1).is_err());
    }

    #[test]
    fn a_journal_file_below_wiredtigers_minimum_is_refused() {
        assert!(JournalFileSize::from_kibibytes(JournalFileSize::MIN_KIBIBYTES - 1).is_err());
    }

    #[test]
    fn a_journal_file_above_wiredtigers_maximum_is_refused() {
        assert!(JournalFileSize::from_kibibytes(JournalFileSize::MAX_KIBIBYTES + 1).is_err());
    }

    #[test]
    fn the_boundary_values_themselves_are_accepted() {
        assert!(CacheSize::from_mebibytes(CacheSize::MIN_MEBIBYTES).is_ok());
        assert!(CacheSize::from_mebibytes(CacheSize::MAX_MEBIBYTES).is_ok());
        assert!(JournalFileSize::from_kibibytes(JournalFileSize::MIN_KIBIBYTES).is_ok());
        assert!(JournalFileSize::from_kibibytes(JournalFileSize::MAX_KIBIBYTES).is_ok());
        assert!(CommandStrands::from_count(CommandStrands::MIN_COUNT).is_ok());
        assert!(CommandStrands::from_count(CommandStrands::MAX_COUNT).is_ok());
    }

    #[test]
    fn command_strands_reach_the_engine_as_a_count() {
        let ffi = EngineOptions::new()
            .command_strands(CommandStrands::from_count(4).expect("4 strands is in range"))
            .to_ffi();

        assert_eq!(ffi.command_strands, 4);
    }

    #[test]
    fn a_strand_pool_outside_the_range_is_refused() {
        let error = CommandStrands::from_count(0).expect_err("0 strands is below the minimum");

        assert_eq!(
            error.to_string(),
            "command strands must be between 1 and 256 strands, got 0"
        );
        assert!(CommandStrands::from_count(CommandStrands::MAX_COUNT + 1).is_err());
    }
}
