package io.github.jeroenvervaeke.embeddedmongodb;

/**
 * The exception the native library throws, reproduced here exactly as the JNI contract
 * declares it. The Kotlin module owns the shipped copy; this one exists so the Rust test
 * suite can prove the native side against the contract without the Gradle build.
 */
public class EmbeddedMongoException extends Exception {
    private static final long serialVersionUID = 1L;

    private final int code;

    public EmbeddedMongoException(String message, int code) {
        super(message);
        this.code = code;
    }

    public int getCode() {
        return code;
    }
}
