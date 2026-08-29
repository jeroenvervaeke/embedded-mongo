use crate::{Database, Error, OpenOptions, Result, error::validate_response, limits, repair};
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
    ///
    /// Opens on MongoDB's own free-disk floors however low an earlier client in this process
    /// left them: they are process-wide server parameters rather than a setting of any one
    /// client, and this open puts them back rather than inheriting them. See
    /// [`FreeDiskFloor`](crate::FreeDiskFloor).
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        Self::open(path, None)
    }

    /// [`Client::new`] with the engine's storage limits overridden -- how much memory its
    /// cache may reach, how large its journal files are and how little free disk space it
    /// will still start an index build on. Anything left unset in `options` keeps the
    /// engine's own default, which is what `new` opens with.
    ///
    /// That holds for the free-disk floor as well, and it takes work rather than nothing: the
    /// floors are process-wide server parameters that outlive the client which named them, so
    /// an open that names none puts MongoDB's own back rather than inheriting whatever an
    /// earlier client in this process left behind. [`FreeDiskFloor`](crate::FreeDiskFloor) has
    /// the whole of it.
    pub fn with_options(path: impl AsRef<Path>, options: OpenOptions) -> Result<Self> {
        Self::open(path, Some(options))
    }

    /// Instrumented here rather than on the two public constructors, so that both report the
    /// same span whichever one the caller reached for.
    #[tracing::instrument(
        name = "embedded_mongodb.open",
        level = "debug",
        skip_all,
        fields(path = %path.as_ref().display()),
        err
    )]
    fn open(path: impl AsRef<Path>, options: Option<OpenOptions>) -> Result<Self> {
        let path = path.as_ref();
        let Some(text) = path.to_str() else {
            return Err(Error::NonUtf8Path);
        };
        // Asked before the engine starts: afterwards every directory holds a database, and
        // the one this process just created would be indistinguishable from one that predates
        // the fix and has to be scanned.
        let origin = repair::origin(path);

        let inner = match options {
            Some(options) => NativeClient::open_with_options(text, options.engine)?,
            None => NativeClient::open(text)?,
        };
        let client = Self { inner };
        // Before the repair pass, which creates indexes: a floor the caller lowered so that
        // index builds work on a full device has to be in force by the time this engine
        // builds one of its own.
        //
        // Run for every open, including one that named no floor at all. The floors are
        // server parameters of the process rather than settings of a client, so a caller who
        // named none has to be put back on MongoDB's own instead of being left on whatever an
        // earlier client set and closed; `limits::at_open` is where that is spelled out.
        limits::at_open::establish_free_disk_floor(
            &client,
            options.and_then(|options| options.free_disk_floor),
        )?;
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
        let response = self.send(database, &command.to_vec()?)?;
        validate_response(Document::from_reader(response.as_slice())?)
    }

    /// Runs an already-encoded command and answers the reply exactly as the engine wrote it.
    ///
    /// For callers that speak BSON themselves and must pass a refusal on rather than raise it:
    /// the bindings hand a `byte[]` or an OP_MSG section straight through, and a command the
    /// server rejects is an answer they owe their caller, not an error of their own. So this
    /// is the one route that does not read `ok` and does not turn `ok: 0` into
    /// [`Error::Server`] -- only a failure of the engine itself comes back as an error here.
    ///
    /// It exists so those bindings can open through [`Client::new`], and with it the one-time
    /// index repair pass, instead of reaching past this crate to the raw FFI client. Anything
    /// that works in documents should use [`Client::run_command`], which checks the reply.
    #[tracing::instrument(
        name = "embedded_mongodb.command",
        level = "debug",
        skip_all,
        // The command's name would cost a BSON decode of a buffer this call exists to pass
        // through untouched, so the span reports only what is already known about it.
        fields(database = database, request_bytes = command.len()),
        err
    )]
    pub fn run_command_bytes(&self, database: &str, command: &[u8]) -> Result<Vec<u8>> {
        self.send(database, command)
    }

    pub fn database(&self, name: &str) -> Database<'_> {
        Database::new(self, name)
    }

    #[tracing::instrument(name = "embedded_mongodb.close", level = "debug", skip_all, err)]
    pub fn close(self) -> Result<()> {
        self.inner.close().map_err(Error::from)
    }

    fn send(&self, database: &str, command: &[u8]) -> Result<Vec<u8>> {
        self.inner
            .run_command(database, command)
            .map_err(Error::from)
    }
}
