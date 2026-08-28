use crate::{Database, Error, Result, error::validate_response, repair};
use bson::Document;
use embedded_mongodb_sys::Client as NativeClient;
use std::path::Path;

pub struct Client {
    inner: NativeClient,
}

impl Client {
    /// Opens the database directory at `path`, creating it if it is not there.
    ///
    /// A directory written to by a build from before the `DatabaseHolder::openDb` fix is
    /// checked once for missing index entries and repaired where it has them, which is a full
    /// scan of every collection in it. Only the first open after upgrading pays for that, and
    /// a directory this build created is never scanned at all. Set
    /// `EMBEDDED_MONGODB_SKIP_INDEX_REPAIR` to leave the check out.
    #[tracing::instrument(
        name = "embedded_mongodb.open",
        level = "debug",
        skip_all,
        fields(path = %path.as_ref().display()),
        err
    )]
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let Some(text) = path.to_str() else {
            return Err(Error::NonUtf8Path);
        };
        // Asked before the engine starts: afterwards every directory holds a database, and
        // the one this process just created would be indistinguishable from one that predates
        // the fix and has to be scanned.
        let origin = repair::origin(path);

        let inner = NativeClient::open(text)?;
        let client = Self { inner };
        repair::run(&client, path, origin);
        Ok(client)
    }

    #[tracing::instrument(
        name = "embedded_mongodb.command",
        level = "debug",
        skip_all,
        fields(
            database = database,
            command = command.keys().next().map_or("unknown", String::as_str)
        ),
        err
    )]
    pub fn run_command(&self, database: &str, command: &Document) -> Result<Document> {
        let command = command.to_vec()?;
        let response = self.inner.run_command(database, &command)?;
        validate_response(Document::from_reader(response.as_slice())?)
    }

    pub fn database(&self, name: &str) -> Database<'_> {
        Database::new(self, name)
    }

    #[tracing::instrument(name = "embedded_mongodb.close", level = "debug", skip_all, err)]
    pub fn close(self) -> Result<()> {
        self.inner.close().map_err(Error::from)
    }
}
