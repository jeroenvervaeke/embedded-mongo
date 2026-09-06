use super::{Database, ProcessLimits, engine::Engine};
use crate::{OpenOptions, Result};
use bson::Document;
use std::path::Path;

/// The async face of [`blocking::Client`](crate::blocking::Client): same engine, same
/// directory format, same one-runtime-per-process rule, with every command dispatched to a
/// worker thread so an `await` parks a task rather than a runtime thread.
///
/// Commands run in parallel up to the session-pool size --
/// [`OpenOptions::command_strands`], eight unless asked otherwise -- because there is one
/// worker thread per session, each holding its session for the length of a command. A `Client`
/// is `Send + Sync` and futures from different tasks proceed independently; what they contend
/// on is what mongod's own sessions contend on.
pub struct Client {
    engine: Engine,
}

impl Client {
    /// Opens the database directory at `path`, creating it if it is not there.
    ///
    /// The open runs on the engine's first worker thread, so the recovery and the one-time
    /// index repair scan documented on [`blocking::Client::new`](crate::blocking::Client::new)
    /// -- everything that makes an open slow -- happens off the runtime. Every behavior
    /// documented there holds here, the free-disk floors included.
    pub async fn new(path: impl AsRef<Path>) -> Result<Self> {
        Self::open(path, None).await
    }

    /// [`Client::new`] with the engine's storage limits overridden; see
    /// [`blocking::Client::with_options`](crate::blocking::Client::with_options).
    /// [`OpenOptions::command_strands`] sizes the session pool for both, and here also the
    /// worker threads that drive it -- one per session.
    pub async fn with_options(path: impl AsRef<Path>, options: OpenOptions) -> Result<Self> {
        Self::open(path, Some(options)).await
    }

    async fn open(path: impl AsRef<Path>, options: Option<OpenOptions>) -> Result<Self> {
        let engine = Engine::open(path.as_ref().to_path_buf(), options).await?;
        Ok(Self { engine })
    }

    /// Runs `command` against `database` and checks the reply, as
    /// [`blocking::Client::run_command`](crate::blocking::Client::run_command) does.
    ///
    /// Takes the command by value rather than by reference: it has to cross to a worker
    /// thread, and taking ownership makes that a move instead of a clone.
    pub async fn run_command(&self, database: &str, command: Document) -> Result<Document> {
        let database = database.to_owned();
        self.engine
            .run(move |client| client.run_command(&database, &command))
            .await
    }

    /// Runs an already-encoded command and answers the reply exactly as the engine wrote it,
    /// with everything
    /// [`blocking::Client::run_command_bytes`](crate::blocking::Client::run_command_bytes)
    /// says about who this is for -- including that `ok: 0` comes back as an answer, not an
    /// error.
    pub async fn run_command_bytes(&self, database: &str, command: Vec<u8>) -> Result<Vec<u8>> {
        let database = database.to_owned();
        self.engine
            .run(move |client| client.run_command_bytes(&database, &command))
            .await
    }

    pub fn database(&self, name: &str) -> Database<'_> {
        Database::new(self, name)
    }

    /// The limits reached through this client that belong to the *process* rather than to it;
    /// see [`blocking::Client::process_limits`](crate::blocking::Client::process_limits).
    pub fn process_limits(&self) -> ProcessLimits<'_> {
        ProcessLimits::new(self)
    }

    /// Closes the engine and reports what the close said. Dropping the client instead closes
    /// it silently, once every outstanding cursor is gone -- the same choice the blocking
    /// client offers.
    pub async fn close(self) -> Result<()> {
        self.engine.close().await
    }

    pub(super) fn engine(&self) -> &Engine {
        &self.engine
    }
}
