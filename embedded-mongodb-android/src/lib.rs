//! JNI bindings for the embedded MongoDB engine.
//!
//! The shared library is `libembedded_mongodb_android.so` and it answers exactly one Java
//! class:
//!
//! ```java
//! package io.github.jeroenvervaeke.embeddedmongodb;
//!
//! final class NativeBridge {
//!     static native long open(String path);
//!     static native long openWithOptions(String path, long[] options);
//!     static native byte[] command(long handle, String database, byte[] command);
//!     static native void close(long handle);
//! }
//! ```
//!
//! Those four names and signatures are the contract. `openWithOptions` was added to it rather
//! than folded into `open`, and it takes a self-describing array rather than one parameter per
//! limit, so that the contract can grow another storage limit without any of the four
//! changing: [`options`] has the whole argument. It is the same promise the C ABI makes with
//! `embedded_mongodb_open_with_options` and its size-prefixed struct, kept in the shape JNI
//! can express.
//!
//! Only the limits WiredTiger reads while it is being opened need a native entry point at all.
//! The free-disk floors are server parameters a `setParameter` command sets on a running
//! engine, so the Kotlin side reaches them over `command` and this library knows nothing about
//! them.
//!
//! Every failure arrives as
//! `io.github.jeroenvervaeke.embeddedmongodb.EmbeddedMongoException(String message, int code)`,
//! including a Rust panic, which is caught at the boundary rather than allowed to unwind into
//! the JVM. `code` is a MongoDB error code when the engine reported a number, and otherwise
//! one of the negative sentinels in [`ErrorCode`].
//!
//! A handle is an id in a process-wide [`Registry`], not a pointer: see its documentation for
//! what a stale, forged or double-closed handle does, and for what `close` guarantees against
//! a command running on another thread.

mod bridge;
mod error;
mod handle;
mod jvm;
pub mod options;
mod registry;

pub use error::{BridgeError, ErrorCode, Result};
pub use handle::HandleId;
pub use jvm::ThrowEmbeddedMongoException;
pub use registry::{EmbeddedClient, Registry, registry};
