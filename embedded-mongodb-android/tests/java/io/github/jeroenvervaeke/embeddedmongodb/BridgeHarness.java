package io.github.jeroenvervaeke.embeddedmongodb;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.stream.Stream;

/**
 * Drives the three native methods from a real JVM and fails the process on the first broken
 * expectation. The Rust test that launches it asserts on the PASS lines below.
 */
public final class BridgeHarness {
    /** {@code EmbeddedMongoException.code} for a handle that is not usable. */
    private static final int CLOSED_HANDLE = -1;

    /** {@code EmbeddedMongoException.code} for an argument that never reached the engine. */
    private static final int INVALID_ARGUMENT = -2;

    /** The engine's own code for a second runtime in one process, an anonymous uassert. */
    private static final int ONE_RUNTIME_PER_PROCESS = 13180000;

    private static final int BLOB_BYTES = 4 * 1024 * 1024;

    /** Long enough that a loaded machine cannot mistake scheduling for a deadlock. */
    private static final long PATIENCE_MILLIS = 60_000;

    public static void main(String[] args) throws Exception {
        System.load(System.getProperty("embedded.mongodb.library"));
        // The database this writes holds the megabyte document below, and /tmp is a memory
        // filesystem on many machines: leaving it behind fills the disk after a few runs, and
        // the storage engine rightly aborts the process when its checkpoint cannot be
        // written.
        Path root = Files.createTempDirectory("embedded-mongodb-jvm");
        try {
            run(root);
        } catch (Throwable failure) {
            // A `finally` that throws would replace the failure being reported with a
            // complaint about a leftover file.
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

    private static void run(Path root) throws Exception {
        rejectsAnUnusablePath();
        rejectsNullArguments(0);
        long handle = opens(root.resolve("database"));
        try {
            rejectsASecondRuntime(root.resolve("second"));
            answersAPing(handle);
            returnsARejectedCommandAsAReply(handle);
            carriesMegabytesBothWays(handle);
            rejectsNullArguments(handle);
            rejectsForgedHandles(handle);
            runsCommandsFromEightThreadsAtOnce(handle);
            closesWhileCommandsAreInFlight(handle);
        } catch (Throwable failure) {
            try {
                NativeBridge.close(handle);
            } catch (Throwable alreadyClosed) {
                failure.addSuppressed(alreadyClosed);
            }
            throw failure;
        }
        refusesEverythingAfterClose(handle);
    }

    private static void deleteTree(Path root) throws IOException {
        try (Stream<Path> paths = Files.walk(root)) {
            for (Path path : paths.sorted(Comparator.reverseOrder()).toList()) {
                Files.deleteIfExists(path);
            }
        }
    }

    private static void rejectsAnUnusablePath() {
        // procfs refuses mkdir for every user, so the engine cannot create its directory here.
        EmbeddedMongoException error =
                throwsFrom(() -> NativeBridge.open("/proc/embedded-mongodb-jvm-probe"),
                        "open on an uncreatable path");
        check(!error.getMessage().isEmpty(), "the exception must say what went wrong");
        System.out.println("PASS open rejects an unusable path: code=" + error.getCode()
                + " message=" + error.getMessage());
    }

    private static long opens(Path path) {
        long handle = NativeBridge.open(path.toString());
        check(handle != 0, "open must not return the null handle");
        System.out.println("PASS open returns handle " + handle);
        return handle;
    }

    private static void rejectsASecondRuntime(Path path) {
        EmbeddedMongoException error = throwsFrom(() -> NativeBridge.open(path.toString()),
                "a second open in one process");
        check(error.getCode() == ONE_RUNTIME_PER_PROCESS,
                "a numbered MongoDB error must reach Java as its number, not " + error.getCode());
        System.out.println("PASS a MongoDB error code survives the boundary: code="
                + error.getCode() + " message=" + error.getMessage());
    }

    private static void answersAPing(long handle) {
        byte[] reply = NativeBridge.command(handle, "admin", Bson.document(Bson.int32("ping", 1)));
        check(Bson.number(reply, "ok") == 1.0, "ping must succeed");
        System.out.println("PASS ping answers in " + reply.length + " bytes");
    }

    private static void returnsARejectedCommandAsAReply(long handle) {
        // A command the server refuses is an answer, not a failure of the bridge: it comes
        // back as BSON with `ok: 0` so the caller can read the code and message out of it.
        byte[] reply = NativeBridge.command(handle, "admin",
                Bson.document(Bson.int32("thisIsNotACommand", 1)));
        check(Bson.number(reply, "ok") == 0.0, "a refused command must answer ok: 0");
        check(Bson.number(reply, "code") != 0.0, "the reply must carry the MongoDB error code");
        System.out.println("PASS a refused command answers ok: 0 with code "
                + (int) Bson.number(reply, "code"));
    }

    private static void carriesMegabytesBothWays(long handle) {
        byte[] blob = new byte[BLOB_BYTES];
        for (int index = 0; index < blob.length; index++) {
            blob[index] = (byte) index;
        }
        byte[] document = Bson.document(Bson.int32("_id", 1), Bson.binary("blob", blob));
        byte[] insert = Bson.document(Bson.string("insert", "big"),
                Bson.array("documents", document));
        byte[] inserted = NativeBridge.command(handle, "test", insert);
        check(Bson.number(inserted, "ok") == 1.0, "the large insert must succeed");
        check(Bson.number(inserted, "n") == 1.0, "one document must be written");

        byte[] find = Bson.document(Bson.string("find", "big"),
                Bson.subDocument("filter", Bson.document()));
        byte[] found = NativeBridge.command(handle, "test", find);
        check(Bson.number(found, "ok") == 1.0, "the read back must succeed");
        check(found.length > BLOB_BYTES, "the reply must carry the blob, not a cursor id");
        System.out.println("PASS a " + insert.length + " byte command returned "
                + found.length + " bytes");
    }

    private static void rejectsNullArguments(long handle) {
        check(throwsFrom(() -> NativeBridge.open(null), "open(null)").getCode()
                == INVALID_ARGUMENT, "a null path must be an invalid argument");
        if (handle != 0) {
            check(throwsFrom(() -> NativeBridge.command(handle, null, new byte[0]),
                    "command with a null database").getCode() == INVALID_ARGUMENT,
                    "a null database must be an invalid argument");
            check(throwsFrom(() -> NativeBridge.command(handle, "admin", null),
                    "command with a null body").getCode() == INVALID_ARGUMENT,
                    "a null command must be an invalid argument");
        }
        System.out.println("PASS null arguments are rejected before the engine sees them");
    }

    private static void rejectsForgedHandles(long live) {
        for (long forged : new long[] {0L, -1L, Long.MIN_VALUE, Long.MAX_VALUE, live + 4242L}) {
            EmbeddedMongoException command = throwsFrom(
                    () -> NativeBridge.command(forged, "admin", Bson.document(Bson.int32("ping", 1))),
                    "command on handle " + forged);
            check(command.getCode() == CLOSED_HANDLE,
                    "handle " + forged + " must be reported as unusable, not " + command.getCode());
            EmbeddedMongoException closed =
                    throwsFrom(() -> NativeBridge.close(forged), "close on handle " + forged);
            check(closed.getCode() == CLOSED_HANDLE,
                    "handle " + forged + " must be unusable to close");
        }
        System.out.println("PASS forged, zero and out-of-range handles throw instead of crashing");
    }

    private static void runsCommandsFromEightThreadsAtOnce(long handle) throws Exception {
        int threads = 8;
        int each = 50;
        CountDownLatch start = new CountDownLatch(1);
        List<Thread> workers = new ArrayList<>();
        List<Throwable> failures = new ArrayList<>();
        for (int index = 0; index < threads; index++) {
            Thread worker = new Thread(() -> {
                try {
                    start.await();
                    for (int round = 0; round < each; round++) {
                        byte[] reply = NativeBridge.command(handle, "admin",
                                Bson.document(Bson.int32("ping", 1)));
                        check(Bson.number(reply, "ok") == 1.0, "a concurrent ping failed");
                    }
                } catch (Throwable failure) {
                    synchronized (failures) {
                        failures.add(failure);
                    }
                }
            });
            // Daemon: a worker that refuses to stop must not hold the JVM open after
            // main has already failed the run.
            worker.setDaemon(true);
            worker.start();
            workers.add(worker);
        }
        start.countDown();
        for (Thread worker : workers) {
            worker.join();
        }
        if (!failures.isEmpty()) {
            throw new AssertionError("concurrent commands failed", failures.get(0));
        }
        System.out.println("PASS " + (threads * each) + " commands ran across "
                + threads + " threads");
    }

    /**
     * Closes the handle with four threads still issuing commands on it. Nothing may crash, and
     * every command must either have completed or been refused as a closed handle -- never
     * reach an engine that is shutting down.
     */
    private static void closesWhileCommandsAreInFlight(long handle) throws Exception {
        int threads = 4;
        // Counted down exactly once per thread, after that thread's own first successful
        // command -- so when it reaches zero all four really are inside the loop, which is
        // the race this test is named for. A single fast thread cannot release it on behalf
        // of the others.
        CountDownLatch started = new CountDownLatch(threads);
        List<Thread> workers = new ArrayList<>();
        List<Throwable> failures = new ArrayList<>();
        for (int index = 0; index < threads; index++) {
            Thread worker = new Thread(() -> {
                boolean counted = false;
                try {
                    // Runs until the handle is closed underneath it, which is the point.
                    while (true) {
                        byte[] reply = NativeBridge.command(handle, "admin",
                                Bson.document(Bson.int32("ping", 1)));
                        check(Bson.number(reply, "ok") == 1.0, "a ping racing close failed");
                        if (!counted) {
                            counted = true;
                            started.countDown();
                        }
                    }
                } catch (Throwable thrown) {
                    // A refused handle is the expected end of the loop; anything else is a
                    // command that reached an engine on its way down. Nothing can be refused
                    // before the close below, so a thread that ends early is always a
                    // recorded failure and can never quietly shrink this test.
                    boolean refused = thrown instanceof EmbeddedMongoException error
                            && error.getCode() == CLOSED_HANDLE;
                    if (!refused) {
                        synchronized (failures) {
                            failures.add(thrown);
                        }
                    }
                } finally {
                    // A thread that died before its first success must not leave the latch
                    // waiting for a count that will never come; its failure is already
                    // recorded above and fails the test below.
                    if (!counted) {
                        started.countDown();
                    }
                }
            });
            // Daemon: a worker that refuses to stop must not hold the JVM open after
            // main has already failed the run.
            worker.setDaemon(true);
            worker.start();
            workers.add(worker);
        }
        check(started.await(PATIENCE_MILLIS, TimeUnit.MILLISECONDS),
                "every command thread must reach the loop before close is called");

        NativeBridge.close(handle);
        for (Thread worker : workers) {
            // Bounded: the loop ends only when a command throws, so a regression that stopped
            // close from retiring the handle would otherwise hang the whole test run instead
            // of failing it.
            worker.join(PATIENCE_MILLIS);
            check(!worker.isAlive(),
                    "a command thread did not stop after close; the handle was never retired");
        }
        if (!failures.isEmpty()) {
            throw new AssertionError("closing under load broke a command", failures.get(0));
        }
        System.out.println("PASS close under load left every command cleanly refused");
    }

    private static void refusesEverythingAfterClose(long handle) {
        EmbeddedMongoException again =
                throwsFrom(() -> NativeBridge.close(handle), "a second close");
        check(again.getCode() == CLOSED_HANDLE, "a double close must be reported cleanly");
        EmbeddedMongoException after = throwsFrom(
                () -> NativeBridge.command(handle, "admin", Bson.document(Bson.int32("ping", 1))),
                "a command after close");
        check(after.getCode() == CLOSED_HANDLE, "a command after close must be reported cleanly");
        System.out.println("PASS close is final: " + again.getMessage());
    }

    private BridgeHarness() {}

    /** A body that may throw, which `Runnable` cannot: the exception is a checked one. */
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

    private static void check(boolean held, String message) {
        if (!held) {
            throw new AssertionError(message);
        }
    }
}
