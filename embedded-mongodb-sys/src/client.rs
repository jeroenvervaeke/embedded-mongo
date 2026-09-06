use crate::{EngineOptions, Error, Result, ffi};

/// An open database directory. Runs no commands itself: open a [`Session`] and run commands on
/// that. One runtime per process; a second [`Client::open`] fails.
pub struct Client {
    inner: cxx::UniquePtr<ffi::bridge::EmbeddedMongo>,
}

// SAFETY: the handle is only read here to open sessions and to close, both of which the engine
// guards internally (session creation registers a client under the service's own lock, and
// close consumes the Client so nothing else can touch the handle concurrently). It carries no
// per-command state -- that lives on a Session -- so sharing it across threads is sound.
unsafe impl Send for Client {}
unsafe impl Sync for Client {}

/// One `mongo::Client` on an open [`Client`] -- the embedded equivalent of a connection.
///
/// A session runs one command at a time: [`run_command`](Session::run_command) binds the
/// session's own strand for the command's duration. Two *different* sessions running at once is
/// exactly how two commands run in parallel; the type system enforces the "one at a time" half
/// by making a session [`Send`] but not [`Sync`], so it moves between threads but is never
/// shared by two at once.
///
/// A session holds its runtime alive on the C++ side, so it can never run against a freed
/// engine. It must still be dropped before [`Client::close`], which tears the engine down.
pub struct Session {
    inner: cxx::UniquePtr<ffi::bridge::EmbeddedSession>,
}

// SAFETY: a session may move to the worker thread that will drive it. It is deliberately not
// Sync: run_command binds a single strand, which two threads calling at once would double-bind.
unsafe impl Send for Session {}

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
        Ok(Self { inner })
    }

    /// Opens a session on this runtime. Open one per thread that will run commands; how many to
    /// open is the caller's decision, made in Rust rather than baked into the engine.
    pub fn open_session(&self) -> Result<Session> {
        let inner = self.inner.as_ref().ok_or(Error::Closed)?.open_session()?;
        if inner.is_null() {
            return Err(Error::Closed);
        }
        Ok(Session { inner })
    }

    pub fn close(mut self) -> Result<()> {
        self.inner.pin_mut().close()?;
        Ok(())
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
