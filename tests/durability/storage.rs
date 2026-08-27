//! Opening where the engine cannot write, which is what an Android device with no space left
//! or an app whose storage has been taken away looks like from in here.
//!
//! One of the two probes here pins a defect rather than a guarantee — the fifth of the five
//! pinned defects, the other four being everything in `reopened`. A failure in
//! [`an_unwritable_database_directory_aborts_the_process`] means the engine was fixed, not that
//! someone broke it.

use crate::probe::{Role, harness::Child, outcome::Outcome, scratch};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

#[test]
fn opening_under_an_unwritable_parent_fails_with_an_error() {
    let directory = scratch::directory();
    let parent = directory.path().join("read-only");
    fs::create_dir(&parent).unwrap();
    require_enforced_permissions(directory.path());

    let outcome = open_with_permissions(&parent, &parent.join("database"));
    outcome.assert_exited_cleanly().report();
    assert_eq!(
        outcome.get("open"),
        "error",
        "the engine claimed to open a directory it cannot create\n{}",
        outcome.transcript()
    );
    assert_eq!(outcome.get("open_variant"), "Native");
}

/// A defect, pinned rather than blessed.
///
/// Opening a database directory the process cannot write to gets as far as the startup
/// checkpoint, fails to create `WiredTiger.turtle.set` with EPERM, and WiredTiger declares
/// `WT_PANIC: the process must exit and restart`. MongoDB answers a panic with `fassert`,
/// which calls `abort()`. The caller never sees an `Error` — the process is simply gone.
///
/// A full disk was confirmed to end the same way, by filling a small tmpfs under the running
/// engine: `__evict_thread_run: eviction thread error` with `No space left on device`, then
/// the same `WT_PANIC` and the same abort. That check needs a private mount namespace, so it
/// is not part of this suite; permissions reach the same code path without one.
#[test]
fn an_unwritable_database_directory_aborts_the_process() {
    let directory = scratch::directory();
    require_enforced_permissions(directory.path());
    let path = directory.path().join("database");
    Child::spawn(Role::ReopenCycles, &path, 1)
        .finish()
        .assert_exited_cleanly();

    let outcome = open_with_permissions(&path, &path);
    outcome.report();
    assert!(
        outcome.was_aborted(),
        "an unwritable database directory no longer takes the process down — THE ENGINE HAS BEEN FIXED; make this \
         probe assert that the open fails with an Error\n{}",
        outcome.transcript()
    );
}

/// Fails the probe outright when file permissions do not apply to this process, which is what
/// running as root looks like. Skipping quietly would leave a probe that passes without having
/// simulated anything, and these two are the only evidence that an unwritable directory is
/// handled at all.
fn require_enforced_permissions(directory: &Path) {
    let probe = directory.join("permission-probe");
    fs::write(&probe, b"").unwrap();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).unwrap();
    let enforced = fs::File::open(&probe).is_err();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&probe).unwrap();

    assert!(
        enforced,
        "this probe makes a directory unwritable and then opens it, which proves nothing for a \
         process file permissions do not apply to; run the suite as an ordinary user"
    );
}

/// Opens `path` while `locked` is read-only, restoring the mode before anything is asserted so
/// that a failing probe still leaves a directory `tempfile` can delete.
fn open_with_permissions(locked: &Path, path: &Path) -> Outcome {
    fs::set_permissions(locked, fs::Permissions::from_mode(0o555)).unwrap();
    let outcome = Child::spawn(Role::OpenOnce, path, 0).finish();
    fs::set_permissions(locked, fs::Permissions::from_mode(0o755)).unwrap();
    outcome
}
