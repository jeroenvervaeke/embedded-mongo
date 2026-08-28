package io.github.jeroenvervaeke.embeddedmongodb;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Comparator;
import java.util.stream.Stream;

/**
 * Drives the two ways an Android caller sizes this engine, from a real JVM.
 *
 * The point is that both are proved to *take effect*, not merely to be accepted. The three
 * limits WiredTiger reads while it is opening are read back out of {@code serverStatus}, and
 * the free-disk floor is driven the way the Kotlin side drives it -- as a {@code setParameter}
 * command over the {@code command} entry point that was already there -- and then made to
 * refuse an index build and allow the same one again.
 *
 * One process opens one engine, so everything a successful open makes unreachable happens
 * first. The partial-vector case needs an open of its own: see {@link PartialOptionsHarness}.
 */
public final class OptionsHarness {
    /** {@code EmbeddedMongoException.code} for an argument that never reached the engine. */
    private static final int INVALID_ARGUMENT = -2;

    /** {@code ErrorCodes::OutOfDiskSpace}, from src/mongo/base/error_codes.yml. */
    private static final int OUT_OF_DISK_SPACE = 14031;

    private static final long MEBIBYTE = 1024L * 1024L;

    /** What this harness asks for, in the units each slot takes. */
    private static final long CACHE_MEBIBYTES = 64;

    private static final long JOURNAL_KIBIBYTES = 512;

    /** {@code journal_preallocation}: 1 is enabled, and 0 would be "unset". */
    private static final long PREALLOCATION_ENABLED = 1;

    /** A slot this build has no name for, which it must read past rather than into. */
    private static final long FROM_A_LATER_CALLER = 99;

    /** MongoDB's own default for both floors, which this engine leaves alone. */
    private static final long DEFAULT_FLOOR_MEBIBYTES = 500;

    /**
     * Four tebibytes. Larger than the disk under any machine this runs on, so the floor cannot
     * be cleared -- and it fails for exactly the reason a nearly-full phone would.
     */
    private static final long UNREACHABLE_FLOOR_MEBIBYTES = 4L * 1024 * 1024;

    /** Small enough to be cleared on any machine that can run this suite at all. */
    private static final long REACHABLE_FLOOR_MEBIBYTES = 32;

    /** Long enough that a loaded machine cannot mistake a slow log server thread for a failure. */
    private static final long PATIENCE_NANOS = 60L * 1000 * 1000 * 1000;

    private static final long POLL_MILLIS = 50;

    private static final String DATABASE = "probe";

    private static final String COLLECTION = "places";

    public static void main(String[] args) throws Exception {
        System.load(System.getProperty("embedded.mongodb.library"));
        Path root = Files.createTempDirectory("embedded-mongodb-options");
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
        rejectsAnAbsentOptionVector(root);
        rejectsALimitTheEngineWouldNotAccept(root);
        long handle = opensWithEveryLimitNamed(root.resolve("database"));
        try {
            theLimitsReachWiredTiger(handle);
            theFloorStartsWhereMongoDbPutIt(handle);
            theFloorMovesOverTheCommandRoute(handle);
            anIndexBuildIsRefusedBelowTheFloor(handle);
        } catch (Throwable failure) {
            try {
                NativeBridge.close(handle);
            } catch (Throwable alreadyClosed) {
                failure.addSuppressed(alreadyClosed);
            }
            throw failure;
        }
        NativeBridge.close(handle);
    }

    /**
     * A null array is not "ask for nothing" -- an empty one is. Reading slots out of a null
     * reference would be a crash rather than an exception, so it is refused by name.
     */
    private static void rejectsAnAbsentOptionVector(Path root) {
        EmbeddedMongoException error = throwsFrom(
                () -> NativeBridge.openWithOptions(root.resolve("null").toString(), null),
                "openWithOptions with no array");
        check(error.getCode() == INVALID_ARGUMENT,
                "a null option vector must be an invalid argument, not " + error.getCode());
        System.out.println("PASS a null option vector is rejected: " + error.getMessage());
    }

    /**
     * The bounds are checked before the engine is opened, so an unusable number is a named
     * error rather than an opaque failure from inside {@code wiredtiger_open}.
     */
    private static void rejectsALimitTheEngineWouldNotAccept(Path root) {
        long[][] impossible = {
            {20_000_000L, 0, 0},   // above WiredTiger's 10 TB cache maximum
            {0, 1, 0},             // below WiredTiger's 100 KB journal minimum
            {0, 0, 3},             // not one of the journal pre-allocation policies
            {-1, 0, 0},            // Java's long is signed; the slot behind it is not
        };
        for (long[] options : impossible) {
            EmbeddedMongoException error = throwsFrom(
                    () -> NativeBridge.openWithOptions(root.resolve("refused").toString(), options),
                    "openWithOptions with an out-of-range limit");
            check(error.getCode() == INVALID_ARGUMENT,
                    "an out-of-range limit must be an invalid argument, not " + error.getCode());
            check(!error.getMessage().isEmpty(), "the exception must say which limit and why");
        }
        System.out.println("PASS limits outside the engine's range are refused before it opens");
    }

    /**
     * One slot more than this build reads, which is what a caller built against a later library
     * looks like on the wire. The extra one has to be ignored rather than shift the three that
     * are understood; {@link #theLimitsReachWiredTiger} is what checks that it did not.
     *
     * If a slot is ever added, this is the test that will notice: FROM_A_LATER_CALLER lands in
     * it, and a value that means nothing to the new slot fails here rather than silently.
     */
    private static long opensWithEveryLimitNamed(Path path) {
        long[] options = {
            CACHE_MEBIBYTES, JOURNAL_KIBIBYTES, PREALLOCATION_ENABLED, FROM_A_LATER_CALLER,
        };
        long handle = NativeBridge.openWithOptions(path.toString(), options);
        check(handle != 0, "openWithOptions must not return the null handle");
        System.out.println("PASS openWithOptions reads what it knows and ignores the rest");
        return handle;
    }

    /**
     * Read back out of the engine rather than trusted: an option that crossed the boundary and
     * was then dropped would look exactly like one that was applied.
     */
    private static void theLimitsReachWiredTiger(long handle) {
        byte[] wiredTiger = Bson.subDocumentOf(
                run(handle, "admin", Bson.document(Bson.int32("serverStatus", 1))), "wiredTiger");
        byte[] cache = Bson.subDocumentOf(wiredTiger, "cache");
        byte[] log = Bson.subDocumentOf(wiredTiger, "log");

        // Both of these are published by __logmgr_config and __wt_cache_config while the
        // connection is opening, so they are settled by the time any command can run.
        equal(Bson.number(cache, "maximum bytes configured"), CACHE_MEBIBYTES * MEBIBYTE,
                "the cache ceiling");
        equal(Bson.number(log, "maximum log file size"), JOURNAL_KIBIBYTES * 1024,
                "the journal file size");
        System.out.println("PASS the cache and journal limits reached WiredTiger");
        preallocationReachedWiredTiger(handle);
    }

    /**
     * Pre-allocation is the one limit that is not settled when the engine comes up. WiredTiger
     * publishes this count from __log_prealloc_once, which only the log server thread runs, so it
     * reads 0 until that thread's first pass -- waited for rather than asserted outright. And it
     * is a lower bound rather than an equality because the same function raises the count when the
     * writing thread had to allocate a file for itself.
     */
    private static void preallocationReachedWiredTiger(long handle) {
        long deadline = System.nanoTime() + PATIENCE_NANOS;
        double reported;
        do {
            reported = Bson.number(logStatistics(handle),
                    "number of pre-allocated log files to create");
            if (reported >= 1) {
                System.out.println("PASS journal pre-allocation reached WiredTiger: "
                        + (long) reported);
                return;
            }
            pause();
        } while (System.nanoTime() < deadline);
        throw new AssertionError("journal pre-allocation never reached WiredTiger; the count "
                + "WiredTiger reports is still " + (long) reported);
    }

    private static byte[] logStatistics(long handle) {
        return Bson.subDocumentOf(Bson.subDocumentOf(
                run(handle, "admin", Bson.document(Bson.int32("serverStatus", 1))), "wiredTiger"),
                "log");
    }

    private static void pause() {
        try {
            Thread.sleep(POLL_MILLIS);
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            throw new AssertionError("interrupted while waiting for the log server thread",
                    interrupted);
        }
    }

    /**
     * The floor an application inherits if it never asks. It is MongoDB's, sized for a server,
     * and it is the whole reason the knob below has to be reachable from a phone.
     */
    private static void theFloorStartsWhereMongoDbPutIt(long handle) {
        floorsAre(handle, DEFAULT_FLOOR_MEBIBYTES, "the engine's own default");
        System.out.println("PASS the engine starts on MongoDB's 500 MB floor");
    }

    /**
     * Two commands rather than one: {@code setParameter} reports the previous value in a field
     * named {@code was}, so a combined command answers with two fields of that name and a
     * parameter that was quietly rejected cannot be told from one that was applied. This is
     * exactly what {@code FreeDiskFloor.kt} sends.
     */
    private static void theFloorMovesOverTheCommandRoute(long handle) {
        setFloor(handle, UNREACHABLE_FLOOR_MEBIBYTES);
        floorsAre(handle, UNREACHABLE_FLOOR_MEBIBYTES, "the floor that was just set");
        System.out.println("PASS the floor moved without a native entry point of its own");
    }

    /** The whole point of the floor, driven from both sides on one engine. */
    private static void anIndexBuildIsRefusedBelowTheFloor(long handle) {
        // The build is only reached once the collection resolves, so it has to exist first.
        byte[] inserted = run(handle, DATABASE, Bson.document(Bson.string("insert", COLLECTION),
                Bson.array("documents",
                        Bson.document(Bson.int32("_id", 1), Bson.string("name", "a")))));
        equal(Bson.number(inserted, "ok"), 1, "the insert");

        byte[] refused = run(handle, DATABASE, createIndex());
        equal(Bson.number(refused, "ok"), 0, "an index build below a 4 TiB floor");
        equal(Bson.number(refused, "code"), OUT_OF_DISK_SPACE,
                "the reason the index build was refused");
        System.out.println("PASS an index build is refused below the floor");

        setFloor(handle, REACHABLE_FLOOR_MEBIBYTES);
        byte[] built = run(handle, DATABASE, createIndex());
        equal(Bson.number(built, "ok"), 1, "the same index build below a floor this device clears");
        System.out.println("PASS lowering the floor is what lets the index build run");
    }

    private static byte[] createIndex() {
        return Bson.document(Bson.string("createIndexes", COLLECTION),
                Bson.array("indexes", Bson.document(
                        Bson.subDocument("key", Bson.document(Bson.int32("name", 1))),
                        Bson.string("name", "name_1"))));
    }

    private static void setFloor(long handle, long mebibytes) {
        byte[] indexBuilds = run(handle, "admin", Bson.document(Bson.int32("setParameter", 1),
                Bson.int64("indexBuildMinAvailableDiskSpaceMB", mebibytes)));
        equal(Bson.number(indexBuilds, "ok"), 1, "setting the index build floor");
        byte[] spilling = run(handle, "admin", Bson.document(Bson.int32("setParameter", 1),
                Bson.int64("internalQuerySpillingMinAvailableDiskSpaceBytes",
                        mebibytes * MEBIBYTE)));
        equal(Bson.number(spilling, "ok"), 1, "setting the query spilling floor");
    }

    private static void floorsAre(long handle, long mebibytes, String what) {
        byte[] reply = run(handle, "admin", Bson.document(Bson.int32("getParameter", 1),
                Bson.int32("indexBuildMinAvailableDiskSpaceMB", 1),
                Bson.int32("internalQuerySpillingMinAvailableDiskSpaceBytes", 1)));
        equal(Bson.number(reply, "indexBuildMinAvailableDiskSpaceMB"), mebibytes,
                what + ", for index builds");
        equal(Bson.number(reply, "internalQuerySpillingMinAvailableDiskSpaceBytes"),
                mebibytes * MEBIBYTE, what + ", for query spilling");
    }

    private static byte[] run(long handle, String database, byte[] command) {
        return NativeBridge.command(handle, database, command);
    }

    private static void deleteTree(Path root) throws IOException {
        try (Stream<Path> paths = Files.walk(root)) {
            for (Path path : paths.sorted(Comparator.reverseOrder()).toList()) {
                Files.deleteIfExists(path);
            }
        }
    }

    private OptionsHarness() {}

    /** A body that may throw, which {@code Runnable} cannot: the exception is a checked one. */
    @FunctionalInterface
    private interface Body {
        void run() throws Exception;
    }

    private static EmbeddedMongoException throwsFrom(Body body, String what) {
        try {
            body.run();
        } catch (EmbeddedMongoException error) {
            return error;
        } catch (Exception other) {
            throw new AssertionError(what + " threw " + other, other);
        }
        throw new AssertionError(what + " was expected to throw EmbeddedMongoException");
    }

    private static void equal(double reported, double expected, String what) {
        check(reported == expected,
                what + " is " + (long) reported + ", not the " + (long) expected + " asked for");
    }

    private static void check(boolean held, String message) {
        if (!held) {
            throw new AssertionError(message);
        }
    }
}
