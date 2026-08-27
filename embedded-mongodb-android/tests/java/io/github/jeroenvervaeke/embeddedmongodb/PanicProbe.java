package io.github.jeroenvervaeke.embeddedmongodb;

/** Backed by the test-only `panic_probe` library, which panics on purpose. */
final class PanicProbe {
    static native long boom();

    private PanicProbe() {}
}
