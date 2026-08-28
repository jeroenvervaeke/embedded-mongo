//! Two clients on one directory. An Android app that restarts an activity, or runs a
//! background worker beside its UI, will try this by accident sooner or later; it has to fail
//! with an error rather than with a second engine quietly writing over the first.

use crate::probe::{Role, harness::Child, scratch};

#[test]
fn a_second_client_in_the_same_process_fails_with_an_error() {
    let directory = scratch::directory();
    let path = directory.path().join("database");

    let outcome = Child::spawn(Role::OpenTwice, &path, 0).finish();
    outcome.assert_exited_cleanly().report();

    assert_eq!(outcome.get("second_open"), "error");
    assert_eq!(outcome.get("second_open_variant"), "Native");
    // The message, not just the variant: this failure and the two-process one are different
    // mechanisms, and asserting only "some native error" would not tell them apart.
    assert!(
        outcome
            .get("second_open_message")
            .contains("only one embedded MongoDB runtime may be open per process"),
        "the second open failed for some other reason\n{}",
        outcome.transcript()
    );
}

#[test]
fn a_refused_second_client_leaves_the_first_one_usable() {
    let directory = scratch::directory();
    let path = directory.path().join("database");

    let outcome = Child::spawn(Role::OpenTwice, &path, 0).finish();
    outcome.assert_exited_cleanly();

    assert_eq!(
        outcome.get("reopen_after_close"),
        "ok",
        "the refused open consumed the runtime slot it never got\n{}",
        outcome.transcript()
    );
}

#[test]
fn a_second_process_on_the_same_directory_fails_with_an_error() {
    let directory = scratch::directory();
    let path = directory.path().join("database");

    let mut holder = Child::spawn(Role::HoldOpen, &path, 0);
    holder.wait_for_phase("ready");
    let intruder = Child::spawn(Role::OpenOnce, &path, 0).finish();
    holder.release();
    holder.finish().assert_exited_cleanly();

    intruder.assert_exited_cleanly().report();
    assert_eq!(
        intruder.get("open"),
        "error",
        "two processes opened the same directory at once\n{}",
        intruder.transcript()
    );
    assert_eq!(intruder.get("open_variant"), "Native");
    // The lock file specifically, so a failure for any other reason cannot pass for one.
    assert!(
        intruder.get("open_message").contains("DBPathInUse")
            && intruder.get("open_message").contains("mongod.lock"),
        "the second process failed for some reason other than the lock file\n{}",
        intruder.transcript()
    );
}
