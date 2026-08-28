//! Everything [`crate::Client::with_options`] can be told, in one object.
//!
//! Two of these reach the engine while WiredTiger is being opened and cannot be changed
//! afterwards; the third is a pair of server parameters set on the running engine. The split
//! matters to the implementation and not to the caller, so it is hidden here.

use crate::limits::FreeDiskFloor;
use embedded_mongodb_sys::{CacheSize, EngineOptions, JournalFileSize, Preallocation};

/// Storage limits for [`crate::Client::with_options`]. Anything left unset keeps the engine's
/// own default, so `Client::new(path)` and `Client::with_options(path, OpenOptions::new())`
/// open identically.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenOptions {
    pub(crate) engine: EngineOptions,
    pub(crate) free_disk_floor: Option<FreeDiskFloor>,
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
    /// [`FreeDiskFloor`] before lowering it.
    pub fn free_disk_floor(mut self, floor: FreeDiskFloor) -> Self {
        self.free_disk_floor = Some(floor);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::OpenOptions;
    use crate::limits::FreeDiskFloor;
    use embedded_mongodb_sys::{CacheSize, EngineOptions};

    #[test]
    fn an_untouched_options_object_asks_for_nothing() {
        let options = OpenOptions::new();

        assert_eq!(options.engine, EngineOptions::new());
        assert_eq!(options.free_disk_floor, None);
    }

    #[test]
    fn the_engine_limits_are_kept_apart_from_the_floor() {
        let cache = CacheSize::from_mebibytes(32).expect("32 MiB is in range");
        let floor = FreeDiskFloor::from_mebibytes(16).expect("16 MiB is in range");

        let options = OpenOptions::new().cache_size(cache).free_disk_floor(floor);

        assert_eq!(options.engine, EngineOptions::new().cache_size(cache));
        assert_eq!(options.free_disk_floor, Some(floor));
    }
}
