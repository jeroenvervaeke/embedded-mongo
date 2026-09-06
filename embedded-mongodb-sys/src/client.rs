use crate::{EngineOptions, Error, Result, ffi};
use std::sync::Arc;

/// An open database directory. Runs no commands itself: open a [`Session`] and run commands on
/// that. One runtime per process; a second [`Client::open`] fails.
///
/// The engine handle lives behind an [`Arc`] shared with every [`Session`] opened on it. That
/// is what guarantees, in Rust, that the engine outlives its sessions: the handle -- and with
/// it the `ServiceContext` a session's client belongs to -- is not dropped or closed while any
/// session holds a reference, so no session ever runs against, or is destroyed after, a
/// torn-down engine. The guarantee is the [`Arc`]'s, not a matter of field order or of the
/// caller closing in the right sequence.
///
/// Not `Clone`: there is one runtime handle, and [`close`](Client::close) is defined in terms
/// of being its sole owner. Sessions share the engine through their own [`Arc`], not by
/// cloning this.
pub struct Client {
    runtime: Arc<Runtime>,
}

/// The engine handle itself. Held only through the [`Arc`] in [`Client`] and [`Session`]; it is
/// closed -- eagerly by [`Client::close`], otherwise when the last of those references drops --
/// exactly once, and only when no session remains.
struct Runtime(cxx::UniquePtr<ffi::bridge::EmbeddedMongo>);

// SAFETY: the handle carries no per-command state -- that lives on a Session -- and the engine
// guards what this type touches: opening a session registers a client under the service's own
// lock, and closing goes through `Arc::try_unwrap`, which yields the handle only to the sole
// remaining owner. So sharing it across threads is sound.
unsafe impl Send for Runtime {}
unsafe impl Sync for Runtime {}

/// One `mongo::Client` on an open [`Client`] -- the embedded equivalent of a connection.
///
/// A session runs one command at a time: [`run_command`](Session::run_command) binds the
/// session's own strand for the command's duration. Two *different* sessions running at once is
/// exactly how two commands run in parallel; the type system enforces the "one at a time" half
/// by making a session [`Send`] but not [`Sync`], so it moves between threads but is never
/// shared by two at once.
///
/// It holds an [`Arc`] on the runtime, so it can neither run against nor be dropped after a
/// freed engine -- the engine cannot close while the session is alive.
pub struct Session {
    inner: cxx::UniquePtr<ffi::bridge::EmbeddedSession>,
    // Keeps the engine alive for as long as this session exists. Never read; its Drop is the
    // point of it.
    _runtime: Arc<Runtime>,
}

// SAFETY: a session may move to the worker thread that will drive it. It is deliberately not
// Sync: run_command binds a single strand, which two threads calling at once would double-bind.
unsafe impl Send for Session {}

impl Runtime {
    fn open_session(&self) -> Result<cxx::UniquePtr<ffi::bridge::EmbeddedSession>> {
        self.0
            .as_ref()
            .ok_or(Error::Closed)?
            .open_session()
            .map_err(Error::from)
    }

    /// Closes the engine and reports what the close said. Consumes the handle; the subsequent
    /// drop of the inner pointer is a no-op, because the native destructor sees an
    /// already-closed handle.
    fn close(mut self) -> Result<()> {
        self.0.pin_mut().close().map_err(Error::from)
    }
}

impl Client {
    pub fn open(path: &str) -> Result<Self> {
        Self::from_inner(ffi::bridge::open(path)?)
    }

    /// `open` with the engine's storage limits overridden. Anything the caller left unset in
    /// `options` stays the engine's own default.
    pub fn open_with_options(path: &str, options: EngineOptions) -> Result<Self> {
        Self::from_inner(ffi::bridge::open_with_options(path, &options.to_ffi())?)
    }

    fn from_inner(inner: cxx::UniquePtr<ffi::bridge::EmbeddedMongo>) -> Result<Self> {
        if inner.is_null() {
            return Err(Error::Closed);
        }
        Ok(Self {
            runtime: Arc::new(Runtime(inner)),
        })
    }

    /// Opens a session on this runtime. Open one per thread that will run commands; how many to
    /// open is the caller's decision, made in Rust rather than baked into the engine.
    pub fn open_session(&self) -> Result<Session> {
        let inner = self.runtime.open_session()?;
        if inner.is_null() {
            return Err(Error::Closed);
        }
        Ok(Session {
            inner,
            _runtime: Arc::clone(&self.runtime),
        })
    }

    /// Closes the engine, reporting what the close said. Closes eagerly only when this is the
    /// sole owner of the handle -- i.e. every session is already dropped, which is the contract
    /// for a clean close. If any session is somehow still alive the engine is left to close
    /// when the last reference drops, so this never tears an engine down under a live session.
    pub fn close(self) -> Result<()> {
        match Arc::try_unwrap(self.runtime) {
            Ok(runtime) => runtime.close(),
            Err(_still_shared) => Ok(()),
        }
    }
}

impl Session {
    pub fn run_command(&self, database: &str, command: &[u8]) -> Result<Vec<u8>> {
        self.inner
            .as_ref()
            .ok_or(Error::Closed)?
            .run_command(database, command)
            .map_err(Error::from)
    }
}
