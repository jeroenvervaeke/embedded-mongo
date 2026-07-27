use crate::{Database, Error, Result, error::validate_response};
use bson::Document;
use embedded_mongodb_sys::Client as NativeClient;
use std::path::Path;

pub struct Client {
    inner: NativeClient,
}

impl Client {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_str().ok_or(Error::NonUtf8Path)?;
        let inner = NativeClient::open(path)?;
        Ok(Self { inner })
    }

    pub fn run_command(&self, database: &str, command: &Document) -> Result<Document> {
        let command = command.to_vec()?;
        let response = self.inner.run_command(database, &command)?;
        validate_response(Document::from_reader(response.as_slice())?)
    }

    pub fn database(&self, name: &str) -> Database<'_> {
        Database::new(self, name)
    }

    pub fn close(self) -> Result<()> {
        self.inner.close().map_err(Error::from)
    }
}
