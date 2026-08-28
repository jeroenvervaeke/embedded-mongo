//! That `JournalFileSize` reaches WiredTiger's `log.file_max`, measured on disk rather than
//! trusted. One test in its own binary, because the engine is one per process.

mod common;

use common::journal_files;
use embedded_mongodb_sys::{Client, EngineOptions, JournalFileSize, Preallocation};

/// WiredTiger's own floor, so this also pins the bottom of the range `JournalFileSize`
/// accepts: a value it rejects fails inside `wiredtiger_open`, not here.
const SMALLEST_KIBIBYTES: u32 = JournalFileSize::MIN_KIBIBYTES;

#[test]
fn the_journal_file_size_option_sizes_the_file_wiredtiger_allocates() {
    let temporary = tempfile::tempdir().expect("a temporary directory");
    let path = temporary.path().join("database");
    let options = EngineOptions::new()
        .journal_file_size(
            JournalFileSize::from_kibibytes(SMALLEST_KIBIBYTES).expect("WiredTiger's own floor"),
        )
        // Explicitly on rather than left unset, so the option travels the same path a caller
        // setting it would. What it produces is not asserted below: WiredTiger creates the
        // spare on its own thread, so whether it exists by the time the engine closes is a
        // race. `journal_defaults` covers the other direction, where "never created" is
        // deterministic.
        .journal_preallocation(Preallocation::Enabled);

    Client::open_with_options(path.to_str().expect("a UTF-8 temporary path"), options)
        .expect("opening an empty directory")
        .close()
        .expect("closing cleanly");

    let files = journal_files(&path);

    assert!(
        !files.is_empty(),
        "the engine left no journal file behind at all"
    );
    // Not an exact file count, for the reason given above. Every file WiredTiger does create
    // is allocated at file_max, and that is what is being checked.
    for (name, size) in &files {
        assert_eq!(
            *size,
            u64::from(SMALLEST_KIBIBYTES) * 1024,
            "{name} is not the {SMALLEST_KIBIBYTES} KiB the options asked for: {files:?}"
        );
    }
}
