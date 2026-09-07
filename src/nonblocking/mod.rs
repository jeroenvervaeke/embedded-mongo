//! The async API: the blocking layer's types re-spoken in `async fn`, dispatched onto a pool
//! of worker threads following the session pool’s concurrency policy.
//!
//! Nothing here talks to the engine except through [`engine::Engine`], and everything here
//! builds its commands with the same functions the blocking layer runs -- `find`, `insert`
//! and `aggregate` in the crate root are the single place each command's shape is written
//! down. What this module adds is dispatch, and it adds it in one place.
//!
//! The job queue is unbounded, deliberately: sending a command must not block the runtime, and
//! a bounded queue that refuses or waits would do exactly that. Commands past the pool queue
//! rather than push back, so the ceiling on outstanding work is the caller's own concurrency.

mod aggregate;
mod client;
mod collection;
mod database;
mod engine;
mod find;
mod insert;
mod process;
mod shutdown;

pub use client::Client;
pub use collection::Collection;
pub use database::Database;
pub use find::Cursor;
pub use process::ProcessLimits;
