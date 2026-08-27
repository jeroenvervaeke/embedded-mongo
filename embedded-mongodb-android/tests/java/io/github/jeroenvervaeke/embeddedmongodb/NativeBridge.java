package io.github.jeroenvervaeke.embeddedmongodb;

/** The three native methods the library exports. */
final class NativeBridge {
    static native long open(String path);

    static native byte[] command(long handle, String database, byte[] command);

    static native void close(long handle);

    private NativeBridge() {}
}
