//! Shared by the two journal tests, which are separate binaries because the engine is a
//! process-global singleton: one open per test process, so one test per file.

use std::path::Path;

/// Every file WiredTiger left in the journal directory, as (name, byte size), sorted by name.
pub fn journal_files(directory: &Path) -> Vec<(String, u64)> {
    let mut files: Vec<(String, u64)> = std::fs::read_dir(directory.join("journal"))
        .expect("the engine creates a journal directory when it opens")
        .map(|entry| {
            let entry = entry.expect("reading the journal directory");
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.metadata().expect("stat of a journal file").len(),
            )
        })
        .collect();
    files.sort();
    files
}
