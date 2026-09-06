use crate::{EngineOptions, Error, Result, ffi};

pub struct Client {
    inner: cxx::UniquePtr<ffi::bridge::EmbeddedMongo>,
}

// SAFETY: Runtime::runCommand takes a strand from its pool and holds that strand's guard for
// the entire operation, so up to `CommandStrands` commands run in parallel, each on its own
// mongo::Client, and callers past the pool wait inside the FFI for a strand to come back.
// Closing requires exclusive Rust access.
unsafe impl Send for Client {}
unsafe impl Sync for Client {}

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

    pub fn run_command(&self, database: &str, command: &[u8]) -> Result<Vec<u8>> {
        self.inner
            .as_ref()
            .ok_or(Error::Closed)?
            .run_command(database, command)
            .map_err(Error::from)
    }

    pub fn close(mut self) -> Result<()> {
        self.inner.pin_mut().close()?;
        Ok(())
    }
}
