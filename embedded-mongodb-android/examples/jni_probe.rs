//! A test-only copy of the shared library, plus one native method that panics on purpose.
//!
//! `cargo test` builds a package's `rlib` but not its `cdylib`, so the JVM tests would have
//! no library to load. This example is a `cdylib` that links the very same `rlib`, which
//! means it exports the same three `Java_..._NativeBridge_*` entry points -- the JVM
//! resolving and calling them is what proves it -- and adds `PanicProbe.boom`.
//!
//! The panic entry point lives here rather than in the library so that what Android loads has
//! no way to reach a panic deliberately. `tests/jvm.rs` also runs the same harness against
//! the real `libembedded_mongodb_android.so` whenever a `cargo build` has produced one.

use embedded_mongodb_android::ThrowEmbeddedMongoException;
use jni::objects::JClass;
use jni::sys::jlong;
use jni::{Env, EnvUnowned};

/// `static native long boom()` on `io.github.jeroenvervaeke.embeddedmongodb.PanicProbe`.
///
/// Goes through the same error policy the three real entry points use, so a green test here
/// is evidence about the shipped mapping from a Rust panic to a Java exception.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_jeroenvervaeke_embeddedmongodb_PanicProbe_boom<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jlong {
    unowned_env
        .with_env(
            |_env: &mut Env<'local>| -> embedded_mongodb_android::Result<jlong> {
                panic!("deliberate panic from the JNI boundary probe")
            },
        )
        .resolve::<ThrowEmbeddedMongoException>()
}
