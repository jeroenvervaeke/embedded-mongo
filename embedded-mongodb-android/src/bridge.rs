use embedded_mongodb_sys::Client;
use jni::objects::{JByteArray, JClass, JString};
use jni::sys::jlong;
use jni::{Env, EnvUnowned};

use crate::error::{BridgeError, Result};
use crate::handle::HandleId;
use crate::jvm::ThrowEmbeddedMongoException;
use crate::registry::registry;

/// `static native long open(String path)`.
///
/// Returns the handle, or throws and returns `0`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_jeroenvervaeke_embeddedmongodb_NativeBridge_open<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
) -> jlong {
    // `with_env` runs the body inside a `catch_unwind`; the policy turns whatever comes back
    // -- error or panic -- into an EmbeddedMongoException. Unwinding into the JVM would be
    // undefined behaviour, so no code path here may skip it.
    unowned_env
        .with_env(|env| open(env, &path))
        .resolve::<ThrowEmbeddedMongoException>()
}

/// `static native byte[] command(long handle, String database, byte[] command)`.
///
/// Returns the BSON reply, or throws and returns `null`. A command that the server rejects is
/// still a reply: `ok: 0` comes back as bytes, not as an exception.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_jeroenvervaeke_embeddedmongodb_NativeBridge_command<
    'local,
>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
    database: JString<'local>,
    command: JByteArray<'local>,
) -> JByteArray<'local> {
    unowned_env
        .with_env(|env| run_command(env, handle, &database, &command))
        .resolve::<ThrowEmbeddedMongoException>()
}

/// `static native void close(long handle)`.
///
/// Throws if the handle is unknown or was already closed, so a double close is reported
/// rather than ignored.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_jeroenvervaeke_embeddedmongodb_NativeBridge_close<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    unowned_env
        .with_env(|_env| close(handle))
        .resolve::<ThrowEmbeddedMongoException>()
}

fn open(env: &mut Env<'_>, path: &JString<'_>) -> Result<jlong> {
    let path = read_string(env, path, "path")?;
    let client = Client::open(&path)?;
    Ok(registry().insert(client)?.get())
}

fn run_command<'local>(
    env: &mut Env<'local>,
    handle: jlong,
    database: &JString<'_>,
    command: &JByteArray<'_>,
) -> Result<JByteArray<'local>> {
    let id = handle_id(handle)?;
    let database = read_string(env, database, "database")?;
    if command.is_null() {
        return Err(BridgeError::invalid_argument("command must not be null"));
    }
    // `GetByteArrayRegion` into a `Vec`; no JNI reference is created for either direction, so
    // a multi-megabyte command costs one copy and nothing in the local reference table.
    let request = env.convert_byte_array(command)?;
    let response = registry().run_command(id, &database, &request)?;
    // Both buffers can be megabytes. Releasing the request before the JVM allocates the reply
    // keeps only one of them alive at a time.
    drop(request);
    let response = env.byte_array_from_slice(&response)?;
    Ok(response)
}

fn close(handle: jlong) -> Result<()> {
    registry().close(handle_id(handle)?)
}

fn handle_id(handle: jlong) -> Result<HandleId> {
    let Some(id) = HandleId::new(handle) else {
        return Err(BridgeError::closed_handle(format!(
            "embedded MongoDB handle {handle} is not a handle this process issued"
        )));
    };
    Ok(id)
}

fn read_string(env: &mut Env<'_>, value: &JString<'_>, name: &str) -> Result<String> {
    if value.is_null() {
        return Err(BridgeError::invalid_argument(format!(
            "{name} must not be null"
        )));
    }
    Ok(value.try_to_string(env)?)
}
