//! Shared plumbing for the probes under `tests/durability`.
//!
//! The parent test process never opens the engine. Every probe drives it from a child, which
//! keeps `cargo test`'s parallel test threads from tripping over the one-runtime-per-process
//! rule and makes SIGKILL a normal thing to do rather than something that would take the test
//! harness down with it.

pub mod child;
pub mod harness;
pub mod index;
pub mod inspect;
pub mod outcome;
pub mod scratch;
pub mod verify;
pub mod workload;

/// The database and collection every role works in, so a parent and its verifier agree
/// without passing names around.
pub const DATABASE: &str = "probe";
pub const COLLECTION: &str = "records";
pub const INDEX: &str = "k_1";

/// What a child process was asked to do.
///
/// The name travels through an environment variable, so this is the parse-at-the-boundary
/// point: past `Role::parse` nothing works with the string again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Load a collection, then build a secondary index on it.
    BuildIndex,
    /// Open, then block until the parent releases stdin, so a second process meets a live
    /// lock file.
    HoldOpen,
    /// Build a secondary index on a collection this process did not create.
    IndexExisting,
    /// Insert one document at a time forever, acknowledging each.
    Insert,
    /// The same, with `j: true` on every write.
    InsertJournaled,
    /// Load a collection, then call `close()`.
    InsertThenClose,
    /// Try to open once and report what came back.
    OpenOnce,
    /// Open a second client while the first is still alive.
    OpenTwice,
    /// open -> insert -> close, repeatedly.
    ReopenCycles,
    /// Reopen and check the secondary index against a collection scan.
    VerifyIndex,
    /// Reopen and check the documents and the catalog.
    VerifyInserts,
    /// Reopen, write one document, and check whether the indexes noticed.
    WriteAfterReopen,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BuildIndex => "build-index",
            Self::HoldOpen => "hold-open",
            Self::IndexExisting => "index-existing",
            Self::Insert => "insert",
            Self::InsertJournaled => "insert-journaled",
            Self::InsertThenClose => "insert-then-close",
            Self::OpenOnce => "open-once",
            Self::OpenTwice => "open-twice",
            Self::ReopenCycles => "reopen-cycles",
            Self::VerifyIndex => "verify-index",
            Self::VerifyInserts => "verify-inserts",
            Self::WriteAfterReopen => "write-after-reopen",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        let roles = [
            Self::BuildIndex,
            Self::HoldOpen,
            Self::IndexExisting,
            Self::Insert,
            Self::InsertJournaled,
            Self::InsertThenClose,
            Self::OpenOnce,
            Self::OpenTwice,
            Self::ReopenCycles,
            Self::VerifyIndex,
            Self::VerifyInserts,
            Self::WriteAfterReopen,
        ];
        roles.into_iter().find(|role| role.as_str() == name)
    }
}
