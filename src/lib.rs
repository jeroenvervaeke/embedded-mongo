//! An embedded MongoDB engine with an async face and a blocking core.
//!
//! The engine underneath is synchronous: a command occupies the thread that entered the FFI
//! until it is done. What makes the crate-root API genuinely async anyway is a pool of
//! dedicated worker threads, governed by [`Concurrency`], that do that occupying on the caller's
//! behalf -- an `await` here parks a task, never a runtime thread. [`blocking`] is the same
//! engine without the workers, for callers that have no runtime.

mod aggregate;
mod client;
mod collection;
mod database;
mod error;
mod find;
mod insert;
mod limits;
mod nonblocking;
mod options;
mod pool;
mod repair;

pub use bson;
// The limits the native library validates, re-exported rather than redefined: a second copy
// here would be a second place for the engine's bounds to be written down. Concurrency is
// not among them -- it is this crate's own pool size, defined in `options`.
pub use embedded_mongodb_sys::{CacheSize, JournalFileSize, OutOfRange, Preallocation};
pub use error::{Error, Result};
pub use insert::{InsertManyResult, InsertOneResult};
pub use limits::{FreeDiskFloor, IndexBuildFloor, QuerySpillingFloor, ReportedFloors};
pub use nonblocking::{Client, Collection, Cursor, Database, ProcessLimits};
pub use options::{CommandStrands, Concurrency, OpenOptions};

/// The synchronous API: every type here works exactly as its crate-root namesake does, minus
/// the worker threads -- a call occupies the calling thread for the length of the command.
///
/// It exists because not every caller has an async runtime: the Python and Android bindings
/// are synchronous surfaces, and an application that wants one database read at startup has
/// no reason to start workers to get it. It is the same engine either way -- the async layer
/// runs on this one -- so the two share every limit, option and error type, and a process may
/// hold only one open [`Client`](crate::blocking::Client) across both.
pub mod blocking {
    pub use crate::client::Client;
    pub use crate::collection::Collection;
    pub use crate::database::Database;
    pub use crate::find::Cursor;
    pub use crate::limits::ProcessLimits;
}

// Only the pool/worker tests open a real engine; the native runtime permits one at a time.
#[cfg(test)]
static TEST_ENGINE: std::sync::Mutex<()> = std::sync::Mutex::new(());
