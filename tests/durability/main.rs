#![cfg(unix)]
//! Does the engine survive having its process taken away without warning?
//!
//! Android reclaims an app process whenever it wants to. `close()` does not run, destructors do
//! not run, nothing unwinds — the process is simply gone, exactly as if someone had sent it
//! SIGKILL. Everything here answers the question that has to be settled before a library is
//! built on this engine for that platform: what does the data directory look like afterwards,
//! and does reopening it work.
//!
//! Every probe drives the engine from a child process (see `probe`) while the parent only
//! watches, kills and asserts. That keeps `cargo test`'s parallel test threads away from the
//! one-runtime-per-process rule, and it means a probe can kill an engine without taking the
//! test run down with it.
//!
//! `crash` kills the engine, `contention` gives it a rival, `lifecycle` restarts it twenty
//! times and `storage` takes its disk away. `reopened` is the odd one out: those probes pin
//! defects this exercise turned up rather than guarantees, and they are written so that fixing
//! the engine makes them fail.

mod contention;
mod crash;
mod lifecycle;
mod probe;
mod reopened;
mod storage;
