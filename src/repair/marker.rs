//! The per-directory record that says the repair pass has already run here.
//!
//! A file in the data directory rather than a document in the database: the pass has to be
//! able to answer "has this run?" for a directory whose collections are exactly what is in
//! doubt, and writing bookkeeping into user data to record a repair of user data is a worse
//! trade than one dotfile. The engine ignores files it does not recognise in its dbpath, which
//! `examples/places-benchmark` has relied on since before this existed.

use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Whether this directory has already been through a completed pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkerState {
    Recorded,
    Missing,
}

pub(crate) struct Marker {
    directory: PathBuf,
}

impl Marker {
    pub(crate) fn in_data_directory(directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
        }
    }

    /// `Missing` for anything this build did not write, an unreadable file and a truncated one
    /// included. Every uncertain answer has to come out `Missing`: the cost of re-running a
    /// pass that already ran is one scan, and the cost of skipping one that never finished is
    /// leaving a collection damaged for good.
    pub(crate) fn state(&self) -> MarkerState {
        match fs::read_to_string(self.path()) {
            Ok(contents) if contents.lines().next() == Some(RECORD) => MarkerState::Recorded,
            _ => MarkerState::Missing,
        }
    }

    /// Writes the marker so that it exists only once it is complete.
    ///
    /// Through a temporary name and a rename, because the failure this guards against is a
    /// process that dies partway through the write: `rename` is atomic, so a reader sees either
    /// no marker or the whole one, never a first line that happened to land while the pass
    /// behind it did not finish. `sync_all` puts the bytes on the device before the rename can
    /// publish the name, so the same holds after power loss.
    ///
    /// It does not promise the marker itself survives power loss -- that would need the
    /// containing directory fsynced too, which is not portable and is not done here. Losing it
    /// costs one repeat of the pass, which is the harmless direction.
    pub(crate) fn record(&self) -> io::Result<()> {
        let partial = self.partial_path();
        match self.write_partial(&partial) {
            Ok(()) => fs::rename(&partial, self.path()),
            Err(error) => {
                // Nothing else will ever look at a partial file, so leaving one in a user's
                // data directory is litter with no owner.
                let _ = fs::remove_file(&partial);
                Err(error)
            }
        }
    }

    fn write_partial(&self, partial: &Path) -> io::Result<()> {
        let mut file = File::create(partial)?;
        file.write_all(format!("{RECORD}\n{EXPLANATION}").as_bytes())?;
        file.sync_all()?;
        // Closed before the caller renames, not at the end of the enclosing scope: Windows
        // refuses to rename a path while a handle to it is still open.
        drop(file);
        Ok(())
    }

    fn path(&self) -> PathBuf {
        self.directory.join(FILE_NAME)
    }

    fn partial_path(&self) -> PathBuf {
        self.directory.join(PARTIAL_FILE_NAME)
    }
}

/// A leading dot keeps this clear of every name the storage engine uses -- `WiredTiger*`,
/// `collection-*`, `index-*`, `internal-*`, `_mdb_catalog`, `sizeStorer`, `storage.bson`,
/// `mongod.lock`, `journal/`, `_tmp/` -- none of which start with one.
const FILE_NAME: &str = ".embedded-mongodb-index-repair";

const PARTIAL_FILE_NAME: &str = ".embedded-mongodb-index-repair.partial";

/// First line of a marker this build wrote, and the only part [`Marker::state`] reads. A later
/// pass that has to run again everywhere changes this string and every existing marker stops
/// counting, with no migration of the migration.
const RECORD: &str = "embedded-mongodb index repair v1";

const EXPLANATION: &str = "\
Written by embedded-mongodb after checking this directory for the missing index entries that
builds before the DatabaseHolder::openDb fix left behind. See the README section `Repairing a
directory an older build damaged`. Delete this file to have the next open check again.
";

#[cfg(test)]
mod tests {
    use super::{FILE_NAME, Marker, MarkerState, RECORD};
    use std::fs;

    #[test]
    fn a_directory_that_was_never_checked_reports_missing() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(
            Marker::in_data_directory(directory.path()).state(),
            MarkerState::Missing
        );
    }

    #[test]
    fn a_recorded_marker_is_read_back() {
        let directory = tempfile::tempdir().unwrap();
        let marker = Marker::in_data_directory(directory.path());

        marker.record().unwrap();

        assert_eq!(marker.state(), MarkerState::Recorded);
    }

    #[test]
    fn recording_leaves_no_partial_file_behind() {
        let directory = tempfile::tempdir().unwrap();

        Marker::in_data_directory(directory.path())
            .record()
            .unwrap();

        let names = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![FILE_NAME.to_owned()]);
    }

    #[test]
    fn a_marker_from_another_version_does_not_count() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(FILE_NAME),
            "embedded-mongodb index repair v0\n",
        )
        .unwrap();

        assert_eq!(
            Marker::in_data_directory(directory.path()).state(),
            MarkerState::Missing
        );
    }

    #[test]
    fn an_empty_marker_does_not_count() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(FILE_NAME), "").unwrap();

        assert_eq!(
            Marker::in_data_directory(directory.path()).state(),
            MarkerState::Missing
        );
    }

    /// The half-written case the rename exists for: the first line is there and the rest is
    /// not, which is exactly what a marker looks like if it were written in place and the
    /// process died. It still has to count, because this content is what `record` produces
    /// atomically -- what must not count is a *different* first line, covered above.
    #[test]
    fn the_first_line_is_what_identifies_a_marker() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(FILE_NAME), format!("{RECORD}\n")).unwrap();

        assert_eq!(
            Marker::in_data_directory(directory.path()).state(),
            MarkerState::Recorded
        );
    }

    #[test]
    fn recording_over_an_existing_marker_succeeds() {
        let directory = tempfile::tempdir().unwrap();
        let marker = Marker::in_data_directory(directory.path());
        marker.record().unwrap();

        marker.record().unwrap();

        assert_eq!(marker.state(), MarkerState::Recorded);
    }

    #[test]
    fn recording_into_a_directory_that_does_not_exist_fails() {
        let directory = tempfile::tempdir().unwrap();

        let recorded = Marker::in_data_directory(&directory.path().join("absent")).record();

        assert!(recorded.is_err());
    }
}
