//! That a free-disk floor one client lowered is not still in force for the next one.
//!
//! The floors are server parameters, and this engine keeps one runtime for the whole life of
//! the process, so a floor belongs to the process rather than to the [`Client`] that named it
//! and outlives that client's close. An open that named no floor used to inherit whatever the
//! last one left behind -- silently, because the API names the floor per-open and nothing
//! about that suggests it is process-wide.
//!
//! Driven from a child process, and not because anything here kills one. The property under
//! test is a property of a *process*: it needs the first open in that process to be the one
//! that lowers the floor, since that is the open at which the library records what MongoDB's
//! own floors are. A test in the parent would have that only when libtest happened to schedule
//! it before every other engine test in the binary, which is the accident that let this defect
//! sit in the Android suite. A child of its own is that arrangement made deliberate.

#[path = "scratch/mod.rs"]
mod scratch;

use embedded_mongodb::{Client, FreeDiskFloor, OpenOptions};
use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// The libtest path of [`opens_two_databases_in_a_process_of_its_own`]. The parent passes it
/// to `--exact`, so a rename here has to be a rename there.
const ENTRY_POINT: &str = "opens_two_databases_in_a_process_of_its_own";

/// Where the child puts its two data directories.
const DIRECTORY: &str = "EMBEDDED_MONGODB_FLOOR_DIRECTORY";

/// Prefix on every line the child means the parent to read. libtest prints `test <name> ... `
/// without a newline before it runs a test, so the child's first line arrives glued to that
/// prefix; the parent finds the payload by this marker rather than by the start of the line.
const MARK: &str = "@floor ";

/// The floor the first database asks for. Nothing like MongoDB's own, so a reading of it
/// cannot be mistaken for a default that was never moved.
const LOWERED: u32 = 32;

/// MongoDB's own floors, written out here rather than asked of the library. The library reads
/// them from the engine, so an expectation taken from the library would agree with itself
/// whatever the engine reported, and would go on agreeing if the recording broke.
const DEFAULT_MEBIBYTES: i64 = 500;
const BYTES_PER_MEBIBYTE: i64 = 1024 * 1024;

#[test]
fn a_floor_one_client_lowered_does_not_reach_the_next_open() {
    let directory = scratch::directory("floor-at-open-");
    let output = child(directory.path());

    assert_eq!(
        reading(&output, "lowered"),
        floors(i64::from(LOWERED), i64::from(LOWERED) * BYTES_PER_MEBIBYTE),
        "the floor that was to be left behind was never lowered, so this proves nothing\n{}",
        transcript(&output)
    );
    assert_eq!(
        reading(&output, "inherited"),
        floors(DEFAULT_MEBIBYTES, DEFAULT_MEBIBYTES * BYTES_PER_MEBIBYTE),
        "the second open ran on the floor the first one left behind\n{}",
        transcript(&output)
    );
}

/// Opens two databases in a row, reporting the floors each of them ran on.
///
/// The first open names a floor, which makes it the open the defaults have to be recorded
/// *before*: a library that recorded them afterwards would take this caller's 32 MiB for
/// MongoDB's own and hand it back to the second open, which asked for nothing.
#[test]
#[ignore = "child-process entry point, re-executed by the test above"]
fn opens_two_databases_in_a_process_of_its_own() {
    let Some(root) = env::var_os(DIRECTORY) else {
        // A bare `cargo test -- --ignored` reaches this with nowhere to put a database.
        return;
    };
    let root = PathBuf::from(root);
    let lowered = FreeDiskFloor::from_mebibytes(LOWERED).expect("32 MiB is in range");

    let client = Client::with_options(
        root.join("lowered"),
        OpenOptions::new().free_disk_floor(lowered),
    )
    .expect("opening the first database");
    report("lowered", &client);
    client.close().expect("closing the first database");

    // A second directory rather than a reopen of the first, so that the floor is the only
    // thing on trial: reopening would put the index repair pass beside it.
    let client = Client::new(root.join("inheriting")).expect("opening the second database");
    report("inherited", &client);
    client.close().expect("closing the second database");
}

/// Re-executes this test binary in child mode, on `directory`.
fn child(directory: &Path) -> Output {
    let executable = env::current_exe().expect("the test binary knows its own path");
    let output = Command::new(executable)
        .args([
            "--exact",
            ENTRY_POINT,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(DIRECTORY, directory)
        .output()
        .unwrap_or_else(|error| panic!("spawning the child failed: {error}"));
    assert!(
        output.status.success(),
        "the child could not run the two opens at all\n{}",
        transcript(&output)
    );
    output
}

fn report(key: &str, client: &Client) {
    let reported = client
        .process_limits()
        .free_disk_floors()
        .expect("the engine reports its floors");
    println!(
        "{MARK}{key}={}",
        floors(
            reported.index_build().mebibytes(),
            reported.query_spilling().bytes()
        )
    );
}

fn floors(mebibytes: i64, bytes: i64) -> String {
    format!("index_build={mebibytes} query_spilling={bytes}")
}

/// What the child reported for `key`, or a panic naming everything it did say.
fn reading(output: &Output, key: &str) -> String {
    let wanted = format!("{MARK}{key}=");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(reading) = stdout
        .lines()
        .find_map(|line| line.rsplit_once(&wanted).map(|(_, value)| value.to_owned()))
    else {
        panic!("the child never reported `{key}`\n{}", transcript(output));
    };
    reading
}

fn transcript(output: &Output) -> String {
    format!(
        "child exited with {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
