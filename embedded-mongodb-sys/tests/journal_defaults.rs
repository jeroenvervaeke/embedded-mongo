//! What an untuned `open` costs on disk before the database holds anything.
//!
//! This is the measurement the mobile defaults exist for: mongod's own settings leave two
//! 100 MiB files here, which is two orders of magnitude more than a small offline dataset.
//! One test in its own binary, because the engine is one per process.

mod common;

use common::journal_files;
use embedded_mongodb_sys::Client;

/// Both defaults at once, because they are visible in the same directory listing: one journal
/// file, and no pre-allocated spare beside it.
#[test]
fn an_untuned_open_leaves_one_eight_mebibyte_journal_file() {
    let temporary = tempfile::tempdir().expect("a temporary directory");
    let path = temporary.path().join("database");

    Client::open(path.to_str().expect("a UTF-8 temporary path"))
        .expect("opening an empty directory")
        .close()
        .expect("closing cleanly");

    let files = journal_files(&path);
    let sizes: Vec<u64> = files.iter().map(|(_, size)| *size).collect();

    assert_eq!(
        files.len(),
        1,
        "expected exactly the log file being written, found {files:?}"
    );
    assert!(
        files[0].0.starts_with("WiredTigerLog."),
        "expected the active log file, found {files:?}"
    );
    assert_eq!(
        sizes,
        vec![8 * 1024 * 1024],
        "the default journal file is not 8 MiB: {files:?}"
    );
    assert!(
        !files
            .iter()
            .any(|(name, _)| name.starts_with("WiredTigerPreplog.")),
        "pre-allocation is on by default again, which doubles the idle journal: {files:?}"
    );
}
