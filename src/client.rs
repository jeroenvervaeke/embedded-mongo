use crate::{
    Error, OpenOptions, Result, database::Database, error::validate_response, limits,
    limits::ProcessLimits, options, repair,
};
use bson::Document;
use embedded_mongodb_sys::{Client as NativeClient, Session as NativeSession};
use std::path::Path;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

pub struct Client {
    /// The sessions this client runs commands on, sized to
    /// [`OpenOptions::command_strands`](crate::OpenOptions::command_strands). Held in a pool
    /// because a `Client` is shared across threads and each session runs one command at a time:
    /// a caller checks one out for the length of its command and hands it back. This is the
    /// whole of the parallelism policy, and it is here, in Rust, rather than in the engine.
    ///
    /// Declared before `runtime` on purpose: a `Client` dropped without an explicit
    /// [`close`](Client::close) drops its fields in this order, and every session must be gone
    /// before the runtime that owns their engine is torn down. `close` drops the pool first for
    /// the same reason.
    pool: SessionPool,
    /// The runtime handle, closed after the pool so no session outlives the engine.
    runtime: NativeClient,
}

// SAFETY: NativeClient is Send + Sync; the pool guards its sessions behind a mutex and hands
// each to one thread at a time, which is what NativeSession (Send, not Sync) requires.
unsafe impl Send for Client {}
unsafe impl Sync for Client {}

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

        let runtime = match options {
            Some(options) => NativeClient::open_with_options(text, options.engine)?,
            None => NativeClient::open(text)?,
        };
        // The pool is opened before anything runs a command, because everything does -- the
        // floor below and the repair pass both go through the pool like any other caller.
        let strands = options::OpenOptions::strand_count(options.as_ref());
        let pool = SessionPool::open(&runtime, strands)?;
        let client = Self { pool, runtime };
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

    /// The limits reached through this client that belong to the *process* rather than to it.
    ///
    /// The free-disk floors are server parameters of the one runtime this process keeps, so
    /// moving one through this client moves it for every client in the process and for every
    /// database name they serve. [`ProcessLimits`] is where that is spelled out; the scope is
    /// on the handle so that it is in front of a reader at the call site rather than only in
    /// the documentation.
    pub fn process_limits(&self) -> ProcessLimits<'_> {
        ProcessLimits::new(self)
    }

    #[tracing::instrument(name = "embedded_mongodb.close", level = "debug", skip_all, err)]
    pub fn close(self) -> Result<()> {
        // Every session dropped before the runtime is closed: the pool is torn down here,
        // while `self` still owns it, so no session can outlive the engine it holds a client
        // of. `close` taking `self` is what guarantees no checkout is in flight.
        drop(self.pool);
        self.runtime.close().map_err(Error::from)
    }

    fn send(&self, database: &str, command: &[u8]) -> Result<Vec<u8>> {
        let session = self.pool.checkout();
        session.run_command(database, command).map_err(Error::from)
    }
}

/// The sessions a [`Client`] runs commands on, and the checkout that hands them out one to a
/// thread.
///
/// A plain mutex-guarded free list with a condition variable: a caller takes a session, runs
/// its command with the pool unlocked, and returns it. Callers past the pool wait here for one
/// to come back rather than failing -- the pool bounds how many commands run at once, not how
/// many may be asked for. This is the safe-Rust counterpart of what a lock in the engine would
/// otherwise be, and it is here so that it can be read, tested and changed without touching the
/// native library.
struct SessionPool {
    idle: Mutex<Vec<NativeSession>>,
    returned: Condvar,
}

impl SessionPool {
    fn open(runtime: &NativeClient, strands: u32) -> Result<Self> {
        let mut idle = Vec::with_capacity(strands as usize);
        for _ in 0..strands {
            idle.push(runtime.open_session()?);
        }
        Ok(Self {
            idle: Mutex::new(idle),
            returned: Condvar::new(),
        })
    }

    /// Takes a session, waiting while every one is running a command. The returned guard hands
    /// the session back on drop.
    fn checkout(&self) -> Checkout<'_> {
        let mut idle = lock(&self.idle);
        while idle.is_empty() {
            idle = self
                .returned
                .wait(idle)
                .unwrap_or_else(PoisonError::into_inner);
        }
        let session = idle.pop().expect("waited until a session was free");
        Checkout {
            pool: self,
            session: Some(session),
        }
    }
}

/// A session on loan from the pool. Runs one command -- the only thing a borrowed session is
/// for -- and returns to the pool when dropped.
struct Checkout<'pool> {
    pool: &'pool SessionPool,
    session: Option<NativeSession>,
}

impl Checkout<'_> {
    fn run_command(
        &self,
        database: &str,
        command: &[u8],
    ) -> std::result::Result<Vec<u8>, embedded_mongodb_sys::Error> {
        // The pool is unlocked for the whole command, so other threads run theirs on other
        // sessions meanwhile -- which is the parallelism the pool exists to allow.
        self.session
            .as_ref()
            .expect("a checked-out session is present until it is returned")
            .run_command(database, command)
    }
}

impl Drop for Checkout<'_> {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            lock(&self.pool.idle).push(session);
            self.pool.returned.notify_one();
        }
    }
}

/// The pool's mutex guards a plain list; a panic under it leaves that list sound, and refusing
/// it would strand every other caller waiting on the pool. So the poison is shrugged off, as
/// `limits::process` does with its own.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
