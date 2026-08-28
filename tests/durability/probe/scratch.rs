//! Where probe data directories live.
//!
//! Not the system temporary directory, which these probes must not use: it is a ramdisk on a
//! good many Linux machines, and this engine preallocates a WiredTiger journal of a couple of
//! hundred megabytes for *every* data directory it opens, however few documents go in. A
//! parallel run of this suite would put several of those in RAM at once.
//!
//! `CARGO_TARGET_TMPDIR` is a directory under `target`, on the same filesystem as the build
//! output — real storage, wiped by `cargo clean`, and no absolute path baked into the tests.
//! Cargo supplies it at compile time rather than in the environment, hence `env!` and not
//! `env::var`. Set `EMBEDDED_MONGODB_PROBE_TMPDIR` to put the directories somewhere else.

use std::{env, fs, path::PathBuf};
use tempfile::TempDir;

const OVERRIDE: &str = "EMBEDDED_MONGODB_PROBE_TMPDIR";

/// A directory that removes itself when the probe that made it is done with it.
///
/// The handle belongs to the parent, never to a child, so a probe that SIGKILLs its child still
/// cleans up — and so does one whose assertion panics, because the removal happens on unwind.
pub fn directory() -> TempDir {
    let base = base();
    fs::create_dir_all(&base)
        .unwrap_or_else(|error| panic!("creating {} failed: {error}", base.display()));
    tempfile::Builder::new()
        .prefix("durability-")
        .tempdir_in(&base)
        .unwrap_or_else(|error| panic!("creating a probe directory in {base:?} failed: {error}"))
}

fn base() -> PathBuf {
    let Some(chosen) = env::var_os(OVERRIDE) else {
        return PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    };
    PathBuf::from(chosen)
}
