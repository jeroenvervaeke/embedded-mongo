use embedded_mongodb::Client;
use jni::objects::{JByteArray, JClass, JLongArray, JString};
use jni::sys::jlong;
use jni::{Env, EnvUnowned};

use crate::error::{BridgeError, Result};
use crate::handle::HandleId;
use crate::jvm::ThrowEmbeddedMongoException;
use crate::options::{self, SLOTS};
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

/// `static native long openWithOptions(String path, long[] options)`.
///
/// [`crate::options`] documents what the array holds and why it is an array. A separate name
/// rather than an overload of `open`: the JVM resolves a native method by its short symbol
/// name first, and two natives sharing a name both resolve to that one symbol -- so an
/// overload would silently bind one of them to a function expecting the other's arguments,
/// unless every `open` were renamed to its signature-mangled long form. That rename would
/// break the entry point this library has already published.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_jeroenvervaeke_embeddedmongodb_NativeBridge_openWithOptions<
    'local,
>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
    options: JLongArray<'local>,
) -> jlong {
    unowned_env
        .with_env(|env| open_with_options(env, &path, &options))
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

/// Opens through the safe crate rather than the raw FFI client, which is what runs the
/// one-time index repair pass over a directory an older build damaged. An Android application
/// pointed at a directory some earlier build wrote is the likeliest holder of that damage, so
/// this is the one call site where reaching straight for the FFI would cost the most.
fn open(env: &mut Env<'_>, path: &JString<'_>) -> Result<jlong> {
    let path = read_string(env, path, "path")?;
    issue(Client::new(&path)?)
}

fn open_with_options(
    env: &mut Env<'_>,
    path: &JString<'_>,
    options: &JLongArray<'_>,
) -> Result<jlong> {
    let path = read_string(env, path, "path")?;
    let options = options::open_options(read_slots(env, options)?)?;
    issue(Client::with_options(&path, options)?)
}

fn issue(client: Client) -> Result<jlong> {
    Ok(registry().insert(client)?.get())
}

/// Copies as many slots as this build understands out of the caller's array, leaving the rest
/// unset.
///
/// The length gate is what makes the vector growable in both directions, and it has to gate
/// the copy rather than the interpretation: reading past the end of the caller's array is an
/// `ArrayIndexOutOfBoundsException` from `GetLongArrayRegion`, not a shorter answer.
fn read_slots(env: &mut Env<'_>, options: &JLongArray<'_>) -> Result<[jlong; SLOTS]> {
    if options.is_null() {
        return Err(BridgeError::invalid_argument(
            "options must not be null; an empty array asks for the engine's defaults",
        ));
    }
    let mut slots = [0; SLOTS];
    let read = options.len(env)?.min(SLOTS);
    let Some(destination) = slots.get_mut(..read) else {
        return Err(BridgeError::invalid_argument(format!(
            "{read} option slots cannot be read into {SLOTS}"
        )));
    };
    options.get_region(env, 0, destination)?;
    Ok(slots)
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
