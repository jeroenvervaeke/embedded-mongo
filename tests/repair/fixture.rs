//! The committed remains of a directory the pre-fix engine damaged, and where tests unpack it.
//!
//! ## Where this came from, and why it is committed
//!
//! The engine cannot produce this directory any more. Building one takes a library from before
//! `fix(engine): register databases already on disk at startup`, which is exactly what a user
//! upgrading from a published build is coming off. It was made by driving such a library
//! through the sequence that causes the damage -- create, close, reopen, write -- and the
//! result is checked in because regenerating it needs an engine no checkout still builds.
//!
//! What is in it:
//!
//! * `shop.orders`: four documents and a `customer_1` index written before the reopen, then
//!   `_id` 5 and 6 and a second copy of `_id` 1 written after it. The three later documents are
//!   in neither index, in `_id_` nor in `customer_1`, which is six missing entries and one
//!   duplicate `_id` the engine of the day accepted.
//! * `shop.untouched`: one document, written before the reopen only. Sound, and the control for
//!   "a healthy collection in a damaged directory is left alone".
//! * `catalog.items`: a second database, so the pass is seen to cross database boundaries.
//!
//! The journal, `mongod.lock`, `WiredTiger.lock` and `_tmp` are left out: the engine recreates
//! all four, and the two preallocated journal files are a hundred megabytes each. Everything
//! else is stored gzipped one file per file -- the tables are mostly page padding, so 300 KB of
//! directory becomes 16 KB of repository, with no archive format to hand-roll and no dependency
//! this crate did not already have.

use flate2::read::GzDecoder;
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

/// Where the fixture lives, relative to the workspace root.
const FIXTURE: &str = "tests/fixtures/damaged-reopen";

/// How many files the fixture is made of. Asserted on unpacking, so a fixture that arrived
/// half-committed fails as a fixture problem instead of as an unexplained engine error.
const FIXTURE_FILES: usize = 15;

/// Set to put test data directories somewhere other than `target`.
const TMPDIR_OVERRIDE: &str = "EMBEDDED_MONGODB_PROBE_TMPDIR";

/// A directory that removes itself when the test is done with it.
///
/// Under `target`, never the system temporary directory: that is a ramdisk on a good many
/// Linux machines, and this engine preallocates a couple of hundred megabytes of WiredTiger
/// journal for every data directory it opens, however few documents go in.
pub fn directory() -> TempDir {
    let base = env::var_os(TMPDIR_OVERRIDE)
        .map_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")), PathBuf::from);
    fs::create_dir_all(&base)
        .unwrap_or_else(|error| panic!("creating {} failed: {error}", base.display()));
    tempfile::Builder::new()
        .prefix("repair-")
        .tempdir_in(&base)
        .unwrap_or_else(|error| panic!("creating a test directory in {base:?} failed: {error}"))
}

/// Unpacks the damaged directory into `path`, which must not exist yet.
pub fn unpack_damaged(path: &Path) {
    let fixture = fixture();
    fs::create_dir_all(path).unwrap();
    let mut unpacked = 0;
    for entry in fs::read_dir(&fixture).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().into_string().unwrap();
        let Some(name) = name.strip_suffix(".gz") else {
            continue;
        };
        let source = fs::File::open(entry.path()).unwrap();
        let mut target = fs::File::create(path.join(name)).unwrap();
        io::copy(&mut GzDecoder::new(source), &mut target).unwrap();
        unpacked += 1;
    }
    assert_eq!(
        unpacked,
        FIXTURE_FILES,
        "unpacked {unpacked} files from {}, expected {FIXTURE_FILES}",
        fixture.display()
    );
}

/// The marker the repair pass writes. Named here rather than reached for through the crate,
/// because its name is part of what a data directory looks like from the outside: a rename is
/// a change users can see, and this is where that shows up.
pub const MARKER: &str = ".embedded-mongodb-index-repair";

pub fn marker_exists(path: &Path) -> bool {
    path.join(MARKER).is_file()
}

/// The committed fixture, found by walking up from whichever crate is compiling this file.
///
/// Not `CARGO_MANIFEST_DIR` joined to the relative path: `embedded-mongodb-android/tests`
/// includes this same file to prove the pass reaches the JNI bridge, and there the manifest
/// directory is one level below the workspace root the fixture hangs off. Sharing the file is
/// the point -- a second copy of the unpacking would drift from the fixture it unpacks.
fn fixture() -> PathBuf {
    let mut directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = directory.join(FIXTURE);
        if candidate.is_dir() {
            return candidate;
        }
        let Some(parent) = directory.parent() else {
            panic!(
                "no {FIXTURE} in {} or any directory above it",
                env!("CARGO_MANIFEST_DIR")
            );
        };
        directory = parent;
    }
}
