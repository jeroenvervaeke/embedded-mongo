use crate::{Error, Result, ffi};

pub struct Client {
    inner: cxx::UniquePtr<ffi::bridge::EmbeddedMongo>,
}

// SAFETY: Runtime::runCommand holds a ClientStrand guard for the entire operation, and
// ClientStrand serializes bindings across threads. Closing requires exclusive Rust access.
// ponytail: one strand serializes commands; add native clients only if parallelism is needed.
unsafe impl Send for Client {}
unsafe impl Sync for Client {}

impl Client {
    pub fn open(path: &str) -> Result<Self> {
        let inner = ffi::bridge::open(path)?;
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
