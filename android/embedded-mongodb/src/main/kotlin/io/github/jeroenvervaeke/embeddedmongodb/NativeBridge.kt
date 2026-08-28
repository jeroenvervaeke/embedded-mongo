package io.github.jeroenvervaeke.embeddedmongodb

/**
 * The raw JNI entry points implemented by the `embedded-mongodb-android` Rust crate.
 *
 * Every name here is part of that crate's ABI: it exports
 * `Java_io_github_jeroenvervaeke_embeddedmongodb_NativeBridge_*`, so renaming the class, the
 * package or a method breaks the link at run time rather than at compile time. `consumer-rules.pro`
 * stops R8 from doing exactly that in a minified application.
 */
internal class NativeBridge private constructor() {
    companion object {
        init {
            System.loadLibrary("embedded_mongodb_android")
        }

        /** Opens the database stored in [path], returning a handle that owns it. */
        @JvmStatic
        external fun open(path: String): Long

        /** Runs one BSON-encoded command against [database] and returns the BSON-encoded reply. */
        @JvmStatic
        external fun command(handle: Long, database: String, command: ByteArray): ByteArray

        /** Releases [handle]. Passing a handle here twice, or using it afterwards, is undefined. */
        @JvmStatic
        external fun close(handle: Long)
    }
}
