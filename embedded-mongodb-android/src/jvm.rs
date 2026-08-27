use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};

use jni::errors::ErrorPolicy;
use jni::objects::{JObject, JString, JThrowable};
use jni::strings::JNIString;
use jni::{Env, JValue, jni_sig, jni_str};

use crate::error::BridgeError;

/// Turns every error and every caught panic into
/// `io.github.jeroenvervaeke.embeddedmongodb.EmbeddedMongoException(message, code)`, and
/// returns the Java default (`0`, `null`, nothing) from the native method.
///
/// Nothing here may unwind: it runs while resolving a `catch_unwind`, on its way back into
/// the JVM.
pub struct ThrowEmbeddedMongoException;

/// The exception the Kotlin side declares, in JNI's slash-separated spelling.
const EXCEPTION_CLASS: &str = "io/github/jeroenvervaeke/embeddedmongodb/EmbeddedMongoException";

impl<T: Default> ErrorPolicy<T, BridgeError> for ThrowEmbeddedMongoException {
    type Captures<'local: 'method, 'method> = ();

    fn on_error<'local: 'method, 'method>(
        env: &mut Env<'local>,
        _captures: &mut Self::Captures<'local, 'method>,
        error: BridgeError,
    ) -> jni::errors::Result<T> {
        throw(env, &error);
        Ok(T::default())
    }

    fn on_panic<'local: 'method, 'method>(
        env: &mut Env<'local>,
        _captures: &mut Self::Captures<'local, 'method>,
        payload: Box<dyn Any + Send + 'static>,
    ) -> jni::errors::Result<T> {
        // A panic that crossed this boundary unwinding would be undefined behaviour, so it
        // becomes an ordinary exception instead, with its message intact.
        let error = BridgeError::from_panic(payload.as_ref());
        // Dropping a payload can itself panic. That second panic would escape through the
        // policy's own last-resort handler, which nothing wraps, and unwind into the JVM --
        // so the payload is leaked instead, exactly as jni's built-in policies do.
        if let Err(while_dropping) = catch_unwind(AssertUnwindSafe(move || drop(payload))) {
            std::mem::forget(while_dropping);
        }
        throw(env, &error);
        Ok(T::default())
    }
}

fn throw(env: &mut Env<'_>, error: &BridgeError) {
    // JNI forbids most calls while an exception is pending, and whatever is already in flight
    // describes the failure more precisely than we can.
    if env.exception_check() {
        return;
    }
    let Err(failure) = raise(env, error) else {
        return;
    };
    // `raise` failed part-way and left an exception of its own pending -- a
    // NoClassDefFoundError when the library was loaded without its Kotlin side, typically.
    // Java must still learn what actually went wrong, so that one is replaced.
    env.exception_clear();
    let message = JNIString::new(format!("{error}; and then {failure} while reporting it"));
    let _ = env.throw_new(jni_str!("java/lang/RuntimeException"), &message);
}

fn raise(env: &mut Env<'_>, error: &BridgeError) -> jni::errors::Result<()> {
    let class = env.find_class(JNIString::new(EXCEPTION_CLASS))?;
    let message = JObject::from(JString::from_str(env, error.message())?);
    let exception = env.new_object(
        &class,
        jni_sig!("(Ljava/lang/String;I)V"),
        &[
            JValue::Object(&message),
            JValue::Int(error.code().as_java()),
        ],
    )?;
    // Proves the class really is a Throwable before `Throw` is handed it.
    let exception = env.cast_local::<JThrowable>(exception)?;
    match env.throw(&exception) {
        // `Env::throw` reports the exception it has just raised as an error, by design.
        Ok(()) | Err(jni::errors::Error::JavaException) => Ok(()),
        Err(failure) => Err(failure),
    }
}
