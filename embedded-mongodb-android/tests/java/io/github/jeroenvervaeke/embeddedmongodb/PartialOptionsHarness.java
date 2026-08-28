package io.github.jeroenvervaeke.embeddedmongodb;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Comparator;
import java.util.stream.Stream;

/**
 * A caller that fills in fewer slots than this build reads, which is what a library older than
 * its caller -- or a caller that names one limit and leaves the rest alone -- looks like on the
 * wire. The Kotlin side sends exactly this: it trims the slots nobody set.
 *
 * The named slot has to take effect and every slot past the end of the array has to stay the
 * engine's default. Both halves matter: a build that read past the array would crash, and one
 * that discarded a short array would silently ignore what the caller asked for.
 *
 * A process of its own because only one engine may be open in one, and {@link OptionsHarness}
 * has the other.
 */
public final class PartialOptionsHarness {
    private static final long MEBIBYTE = 1024L * 1024L;

    /** The one slot this harness fills in. */
    private static final long CACHE_MEBIBYTES = 64;

    /** What the slots it leaves off the end must still be. */
    private static final long DEFAULT_JOURNAL_KIBIBYTES = 8 * 1024;

    private static final long DEFAULT_PREALLOCATED_FILES = 0;

    public static void main(String[] args) throws Exception {
        System.load(System.getProperty("embedded.mongodb.library"));
        Path root = Files.createTempDirectory("embedded-mongodb-partial-options");
        try {
            run(root);
        } catch (Throwable failure) {
            try {
                deleteTree(root);
            } catch (IOException cleanup) {
                failure.addSuppressed(cleanup);
            }
            throw failure;
        }
        deleteTree(root);
        System.out.println("PASS all");
    }

    private static void run(Path root) {
        long handle = NativeBridge.openWithOptions(root.resolve("database").toString(),
                new long[] {CACHE_MEBIBYTES});
        check(handle != 0, "a one-slot option vector must open");
        try {
            byte[] wiredTiger = Bson.subDocumentOf(NativeBridge.command(handle, "admin",
                    Bson.document(Bson.int32("serverStatus", 1))), "wiredTiger");
            byte[] cache = Bson.subDocumentOf(wiredTiger, "cache");
            byte[] log = Bson.subDocumentOf(wiredTiger, "log");

            equal(Bson.number(cache, "maximum bytes configured"), CACHE_MEBIBYTES * MEBIBYTE,
                    "the one limit the caller named");
            equal(Bson.number(log, "maximum log file size"), DEFAULT_JOURNAL_KIBIBYTES * 1024,
                    "the journal file size the caller left off the end");
            equal(Bson.number(log, "number of pre-allocated log files to create"),
                    DEFAULT_PREALLOCATED_FILES,
                    "the pre-allocation the caller left off the end");
        } catch (Throwable failure) {
            try {
                NativeBridge.close(handle);
            } catch (Throwable alreadyClosed) {
                failure.addSuppressed(alreadyClosed);
            }
            throw failure;
        }
        NativeBridge.close(handle);
        System.out.println("PASS a short option vector names one limit and defaults the rest");
    }

    private static void deleteTree(Path root) throws IOException {
        try (Stream<Path> paths = Files.walk(root)) {
            for (Path path : paths.sorted(Comparator.reverseOrder()).toList()) {
                Files.deleteIfExists(path);
            }
        }
    }

    private PartialOptionsHarness() {}

    private static void equal(double reported, double expected, String what) {
        check(reported == expected,
                what + " is " + (long) reported + ", not the " + (long) expected + " expected");
    }

    private static void check(boolean held, String message) {
        if (!held) {
            throw new AssertionError(message);
        }
    }
}
