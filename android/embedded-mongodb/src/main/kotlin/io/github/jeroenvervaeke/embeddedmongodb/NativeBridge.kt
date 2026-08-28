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

        /**
         * [open] with the storage limits WiredTiger reads while it is opening.
         *
         * [options] is a vector of slots whose own length says how many of them the caller
         * filled in, which is what lets a limit be added later without this signature moving:
         * a shorter array leaves the slots past its end at the engine's default, and a longer
         * one is read only as far as the library understands it. Zero means "the engine's
         * default" in every slot. See `engineSlots`.
         *
         * A name of its own rather than an overload of [open]: the JVM binds a native method
         * by its short symbol name, and two natives sharing a name would both resolve to the
         * one symbol unless every `open` were renamed to its signature-mangled long form —
         * which would break the entry point already published.
         */
        @JvmStatic
        external fun openWithOptions(path: String, options: LongArray): Long

        /** Runs one BSON-encoded command against [database] and returns the BSON-encoded reply. */
        @JvmStatic
        external fun command(handle: Long, database: String, command: ByteArray): ByteArray

        /** Releases [handle]. Passing a handle here twice, or using it afterwards, is undefined. */
        @JvmStatic
        external fun close(handle: Long)
    }
}
