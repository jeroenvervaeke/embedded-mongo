//! Data directories for the durability probes.
//!
//! The implementation is shared with every other integration target that opens an engine;
//! see `tests/scratch/mod.rs` for why the system temporary directory is not used and why
//! the override matters on a device.

#[path = "../../scratch/mod.rs"]
mod shared;

use tempfile::TempDir;

/// A directory that removes itself when the probe that made it is done with it.
///
/// The handle belongs to the parent, never to a child, so a probe that SIGKILLs its child
/// still cleans up — and so does one whose assertion panics, because the removal happens on
/// unwind.
pub fn directory() -> TempDir {
    shared::directory("durability-")
}
