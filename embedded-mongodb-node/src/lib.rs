//! The engine behind `@0q/embedded-mongodb`.
//!
//! Node's driver speaks OP_MSG to a socket, and the JavaScript half of this package gives it
//! one: an in-process listener that hands every message it reads to [`Engine::round_trip`] and
//! writes the answer back. This half is the translation -- OP_MSG in, the engine's reply out --
//! over the crate's async API, so a command parks a task on napi's runtime while the engine's
//! own workers do the blocking, and the event loop is never the thread that waits.

use std::future::Future;

use bson::Document;
use embedded_mongodb::Client;
use embedded_mongodb_wire as wire;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use tokio::sync::RwLock;

/// One reply, alongside what the listener needs to frame it.
#[napi(object)]
pub struct RoundTrip {
    pub request_id: i32,
    /// The request's flag: a driver that set it wants no reply written.
    pub more_to_come: bool,
    /// The request was an OP_QUERY, so the reply has to be framed as an OP_REPLY.
    pub legacy: bool,
    pub response: Buffer,
}

/// An open data directory. One per process: the engine keeps a single runtime, and opening a
/// second directory before the first is closed is refused with a message saying so.
#[napi]
pub struct Engine {
    inner: RwLock<Option<Client>>,
    /// The handshake reply, answered here rather than by the engine. See [`hello`].
    hello: Vec<u8>,
}

#[napi]
impl Engine {
    /// Opens the directory, creating it if it does not exist. Also runs the one-time index
    /// repair pass over a directory an older build damaged, which is a scan of every
    /// collection in it -- the longest thing this package ever does.
    #[napi(factory)]
    pub async fn open(path: String) -> Result<Engine> {
        let client = Client::new(&path).await.map_err(to_error)?;
        Self::wrap(client).await
    }

    /// [`Engine::open`] for a caller with no loop to await on: `new MongoClient(...)` is a
    /// constructor in the driver's API, and the engine has to be up before it returns.
    #[napi(factory)]
    pub fn open_sync(path: String) -> Result<Engine> {
        block_on(Self::open(path))
    }

    /// Runs one OP_MSG and answers the reply. Commands from other connections run alongside
    /// this one -- the read guard is shared -- and only [`Engine::close`] excludes them.
    #[napi]
    pub async fn round_trip(&self, message: Buffer) -> Result<RoundTrip> {
        let request = wire::parse(&message).map_err(Error::from_reason)?;
        let response = if is_hello(&request.command_name) {
            self.hello.clone()
        } else {
            let guard = self.inner.read().await;
            let client = guard
                .as_ref()
                .ok_or_else(|| Error::from_reason("embedded MongoDB engine is closed"))?;
            client
                .run_command_bytes(&request.database, request.command)
                .await
                .map_err(to_error)?
        };
        Ok(RoundTrip {
            request_id: request.request_id,
            more_to_come: request.more_to_come,
            legacy: request.legacy,
            response: response.into(),
        })
    }

    /// Closes the engine, waiting for the commands other connections are still running. The
    /// write guard is a temporary of the first statement, so the engine's own shutdown runs
    /// with the lock released, and a command arriving meanwhile is told the engine is closed.
    #[napi]
    pub async fn close(&self) -> Result<()> {
        let closing = self.inner.write().await.take();
        let Some(client) = closing else { return Ok(()) };
        client.close().await.map_err(to_error)
    }

    /// [`Engine::close`] for the one caller with no loop to await on: `new MongoClient(...)`
    /// undoing its own open when the rest of the constructor fails. Leaving the engine open
    /// there would cost the process its one runtime, and every later open would be refused.
    #[napi]
    pub fn close_sync(&self) -> Result<()> {
        block_on(self.close())
    }
}

impl Engine {
    async fn wrap(client: Client) -> Result<Engine> {
        let hello = hello(&client).await?;
        Ok(Engine {
            inner: RwLock::new(Some(client)),
            hello,
        })
    }
}

/// `hello` and `isMaster` are answered from this one reply rather than by the engine, for
/// the same reason the Python binding answers them itself: the engine's own reply advertises
/// two things a direct client cannot have. `logicalSessionTimeoutMinutes` makes the driver
/// attach an `lsid` to every command, which the engine then refuses ("Invalid to set
/// operation session info in a direct client"), and `topologyVersion` makes its monitor
/// switch to awaitable hellos, each of which would park an engine strand for ten seconds
/// waiting for a topology change that cannot happen.
///
/// Asked of the engine once at open and then edited, rather than written down here, so the
/// wire versions and the size limits are the engine's own and follow it when it moves.
async fn hello(client: &Client) -> Result<Vec<u8>> {
    let mut reply = client
        .database("admin")
        .run_command(bson::doc! { "hello": 1 })
        .await
        .map_err(to_error)?;
    for field in [
        "logicalSessionTimeoutMinutes",
        "topologyVersion",
        "localTime",
        "connectionId",
    ] {
        reply.remove(field);
    }
    // The legacy name too, because the driver's first message is still `isMaster` unless it
    // was told to use the Stable API, and a reply to that without `helloOk` keeps it legacy.
    reply.insert("helloOk", true);
    reply.insert("ismaster", true);
    encode(&reply)
}

/// Drives one future on the calling thread. Only for the two synchronous entry points, which
/// are called from JavaScript's main thread with no runtime active on it: the engine's own
/// worker threads make the progress, so a single-threaded runtime is all the polling needs.
fn block_on<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|error| Error::from_reason(error.to_string()))?;
    runtime.block_on(future)
}

fn is_hello(command_name: &str) -> bool {
    command_name == "hello" || command_name.eq_ignore_ascii_case("ismaster")
}

fn encode(document: &Document) -> Result<Vec<u8>> {
    document
        .to_vec()
        .map_err(|error| Error::from_reason(error.to_string()))
}

fn to_error(error: embedded_mongodb::Error) -> Error {
    Error::from_reason(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::is_hello;

    #[test]
    fn hello_and_both_spellings_of_is_master_are_handshakes() {
        assert!(is_hello("hello"));
        assert!(is_hello("isMaster"));
        assert!(is_hello("ismaster"));
        assert!(!is_hello("ping"));
    }
}
