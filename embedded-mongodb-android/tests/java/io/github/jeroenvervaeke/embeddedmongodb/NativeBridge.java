package io.github.jeroenvervaeke.embeddedmongodb;

/** The four native methods the library exports. */
final class NativeBridge {
    static native long open(String path);

    /**
     * The storage limits WiredTiger reads while it is being opened, as a vector of slots whose
     * length says how many of them the caller filled in. The Rust crate's {@code options}
     * module documents the slots; zero means "the engine's default" in every one of them.
     */
    static native long openWithOptions(String path, long[] options);

    static native byte[] command(long handle, String database, byte[] command);

    static native void close(long handle);

    private NativeBridge() {}
}
