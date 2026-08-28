//! What a data directory looked like before this process opened it.
//!
//! One question, asked once, and it has to be asked before the engine starts: afterwards every
//! directory holds a database, and the one this process just created is indistinguishable from
//! one written years ago by a build that predates the fix.

use std::path::Path;

/// Whether the data directory already held a database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    PreExisting,
    Fresh,
}

pub(crate) fn origin(data_directory: &Path) -> Origin {
    // WiredTiger writes this on the first open of a directory and never removes it, so its
    // absence is the one thing that says "nothing here yet" without opening anything.
    if data_directory.join("WiredTiger").is_file() {
        Origin::PreExisting
    } else {
        Origin::Fresh
    }
}

#[cfg(test)]
mod tests {
    use super::{Origin, origin};

    #[test]
    fn a_directory_that_does_not_exist_is_fresh() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(origin(&directory.path().join("absent")), Origin::Fresh);
    }

    #[test]
    fn an_empty_directory_is_fresh() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(origin(directory.path()), Origin::Fresh);
    }

    /// The engine's own metadata file is the signal, not merely a non-empty directory: the
    /// benchmark example drops a marker of its own into a dbpath before the engine ever sees
    /// it, and that must still count as a directory this build is about to create.
    #[test]
    fn a_directory_with_only_foreign_files_is_fresh() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(".places-benchmark"), b"x").unwrap();

        assert_eq!(origin(directory.path()), Origin::Fresh);
    }

    #[test]
    fn a_directory_holding_a_wiredtiger_database_is_pre_existing() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("WiredTiger"), b"WiredTiger\n").unwrap();

        assert_eq!(origin(directory.path()), Origin::PreExisting);
    }
}
