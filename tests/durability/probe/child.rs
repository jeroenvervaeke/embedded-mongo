//! The child half of every probe: one entry point, one role, one line-oriented protocol.

use super::{Role, index, verify, workload};
use std::{
    env,
    fmt::{Arguments, Display},
    io::Write,
    path::PathBuf,
};

/// The libtest path of [`runs_the_role_named_in_the_environment`]. The parent passes it to
/// `--exact`, so a rename here has to be a rename there.
pub const ENTRY_POINT: &str = "probe::child::runs_the_role_named_in_the_environment";

/// Prefix on every protocol line. libtest prints `test <name> ... ` without a newline before
/// it runs a test, so the child's first line arrives glued to that prefix; the parent finds
/// the payload by this marker rather than by assuming the line starts with it.
pub const MARK: &str = "@probe ";

/// Set to any value to forward the engine's own log records to stderr. Off by default, but
/// worth knowing about: the engine reports an internal invariant failure through this channel
/// and nowhere else, so an abort with no subscriber installed is completely silent.
pub const LOG: &str = "EMBEDDED_MONGODB_PROBE_LOG";

pub const ROLE: &str = "EMBEDDED_MONGODB_PROBE_ROLE";
pub const DIRECTORY: &str = "EMBEDDED_MONGODB_PROBE_DIRECTORY";
pub const ARGUMENT: &str = "EMBEDDED_MONGODB_PROBE_ARGUMENT";

/// Exit code for a role that failed for a reason the probe was not asking about, kept
/// distinct from libtest's own panic code so the parent can tell the two apart.
pub const ROLE_FAILED: u8 = 3;

#[test]
#[ignore = "child-process entry point, re-executed by the probes under tests/durability"]
fn runs_the_role_named_in_the_environment() {
    install_engine_logging();
    let Ok(name) = env::var(ROLE) else {
        // A bare `cargo test -- --ignored` reaches this with no role to run.
        return;
    };
    let Some(role) = Role::parse(&name) else {
        panic!("unknown probe role {name:?}");
    };
    let directory = PathBuf::from(env::var_os(DIRECTORY).unwrap_or_default());
    let Some(argument) = env::var(ARGUMENT)
        .ok()
        .and_then(|raw| raw.parse::<i64>().ok())
    else {
        panic!("{ARGUMENT} must hold an integer");
    };

    let outcome = match role {
        Role::BuildIndex => workload::build_index(&directory, argument),
        Role::HoldOpen => workload::hold_open(&directory),
        Role::IndexExisting => workload::index_existing(&directory),
        Role::Insert => workload::insert_until_killed(&directory, workload::Journaling::Off),
        Role::InsertJournaled => {
            workload::insert_until_killed(&directory, workload::Journaling::On)
        }
        Role::InsertThenClose => workload::insert_then_close(&directory, argument),
        Role::OpenOnce => workload::open_once(&directory),
        Role::OpenTwice => workload::open_twice(&directory),
        Role::ReopenCycles => workload::reopen_cycles(&directory, argument),
        Role::VerifyIndex => index::verify_index(&directory, argument),
        Role::VerifyInserts => verify::verify_inserts(&directory, argument),
        Role::WriteAfterReopen => workload::write_after_reopen(&directory),
    };

    if let Err(error) = outcome {
        result("failed", format_args!("{error:#}"));
        // Straight out, rather than a panic libtest would turn into its own exit code: the
        // parent distinguishes "the role could not run" from "the role ran and reported".
        std::process::exit(i32::from(ROLE_FAILED));
    }
}

/// Engine logs go to stderr, never stdout, so they cannot be mistaken for protocol lines.
fn install_engine_logging() {
    if env::var_os(LOG).is_none() {
        return;
    }
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(std::io::stderr)
        .init();
}

pub fn phase(name: &str) {
    emit(format_args!("phase {name}"));
}

pub fn ack(sequence: i64) {
    emit(format_args!("ack {sequence}"));
}

pub fn result(key: &str, value: impl Display) {
    emit(format_args!("result {key}={value}"));
}

fn emit(line: Arguments<'_>) {
    let mut stdout = std::io::stdout().lock();
    // A write that fails here means the parent already tore the pipe down, which for the
    // kill probes is the expected end of this process rather than a failure to report.
    let _ = writeln!(stdout, "{MARK}{line}");
    let _ = stdout.flush();
}
