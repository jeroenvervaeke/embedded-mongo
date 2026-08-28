package io.github.jeroenvervaeke.embeddedmongodb;

/**
 * Loads the test-only probe library and calls the one native method that panics, in its own
 * JVM so no other library can answer for the symbol.
 */
public final class PanicHarness {
    /** {@code EmbeddedMongoException.code} for a Rust panic caught at the boundary. */
    private static final int PANIC = -3;

    public static void main(String[] args) {
        System.load(System.getProperty("embedded.mongodb.library"));
        long value;
        try {
            value = PanicProbe.boom();
        } catch (Throwable thrown) {
            // Deliberately catches everything: proving that the panic arrives as this exact
            // type is the point, so anything else has to fail rather than be caught by
            // pattern.
            if (!(thrown instanceof EmbeddedMongoException error)) {
                throw new AssertionError("a panic must arrive as EmbeddedMongoException", thrown);
            }
            if (error.getCode() != PANIC) {
                throw new AssertionError("a panic must arrive as code " + PANIC
                        + ", not " + error.getCode());
            }
            if (!error.getMessage().contains("deliberate panic")) {
                throw new AssertionError("the panic message was lost: " + error.getMessage());
            }
            System.out.println("PASS a panic crosses as an exception: " + error.getMessage());
            return;
        }
        throw new AssertionError("the probe returned " + value + " instead of panicking");
    }

    private PanicHarness() {}
}
