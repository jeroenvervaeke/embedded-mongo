package io.github.jeroenvervaeke.embeddedmongodb;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;

/**
 * Opens a directory an older build damaged and proves the one-time index repair pass ran on the
 * way in.
 *
 * The bridge used to open the raw FFI client, which skips the pass entirely, so every reading
 * below is one that changes when it is skipped: without the pass the two documents written
 * after the reopen are missing from `customer_1` and `_id_`, the collection still holds the
 * duplicate `_id` an unmaintained index accepted, nothing was moved to the lost and found, and
 * no marker is written.
 *
 * The directory is unpacked by the Rust test that starts this JVM and named in
 * {@code embedded.mongodb.database}.
 */
public final class RepairHarness {
    /**
     * The file the pass writes once it has visited every collection. Spelled out here because
     * Java cannot reach the Rust constant; {@code src/repair/marker.rs} owns the name.
     */
    private static final String MARKER = ".embedded-mongodb-index-repair";

    /** The switch that suppresses the pass, named only so a failure can blame it. */
    private static final String SKIP = "EMBEDDED_MONGODB_SKIP_INDEX_REPAIR";

    /** Where {@code validate {repair: true}} puts a document it had to evict. */
    private static final String LOST_AND_FOUND = "lost_and_found.";

    public static void main(String[] args) throws Exception {
        System.load(System.getProperty("embedded.mongodb.library"));
        Path damaged = Paths.get(System.getProperty("embedded.mongodb.database"));
        check(Files.isDirectory(damaged), "the damaged fixture must be unpacked at " + damaged);
        check(!Files.exists(damaged.resolve(MARKER)),
                "the fixture arrived already marked, so this run would prove nothing");

        long handle = NativeBridge.open(damaged.toString());
        try {
            marked(damaged);
            reachesTheHiddenDocuments(handle);
            keptBothCopiesOfTheDuplicate(handle);
            enforcesTheIdIndexAgain(handle);
        } finally {
            NativeBridge.close(handle);
        }
        System.out.println("PASS all");
    }

    private static void marked(Path damaged) {
        check(Files.isRegularFile(damaged.resolve(MARKER)),
                "open left no " + MARKER + ", so the pass never ran" + skipHint());
        System.out.println("PASS opening through the bridge ran the index repair pass");
    }

    private static void reachesTheHiddenDocuments(long handle) {
        // 0 without the pass: `customer` c5 was written after the reopen and never reached
        // `customer_1`, so an indexed lookup skipped it while a collection scan returned it.
        check(count(handle, "shop", "orders",
                Bson.document(Bson.string("customer", "c5"))) == 1,
                "a document the damaged secondary index hid is still unreachable through it");
        check(count(handle, "shop", "orders", Bson.document(Bson.int32("_id", 5))) == 1,
                "a document the damaged _id index hid is still unreachable through it");
        // A second database, so the pass is seen to have crossed a database boundary.
        check(count(handle, "catalog", "items", Bson.document(Bson.int32("_id", 3))) == 1,
                "the pass did not reach the second database in the directory");
        System.out.println("PASS the documents the damaged indexes hid are indexed again");
    }

    private static void keptBothCopiesOfTheDuplicate(long handle) {
        // Six, not the seven the record store held: the seventh was the second copy of `_id` 1.
        check(count(handle, "shop", "orders", Bson.document()) == 6,
                "shop.orders does not hold what a repaired collection holds");
        check(count(handle, "shop", "orders", Bson.document(Bson.int32("_id", 1))) == 1,
                "_id 1 is still duplicated");
        check(count(handle, "shop", "untouched", Bson.document()) == 1,
                "a sound collection in a damaged directory was changed");
        // The counts above cannot tell a document that was moved from one that was deleted, and
        // `validate {repair: true}` can do both. This is the difference: the evicted copy has to
        // still be readable somewhere.
        String rescued = lostAndFound(handle);
        check(count(handle, "local", rescued, Bson.document()) == 1,
                "local." + rescued + " does not hold the evicted duplicate");
        System.out.println("PASS the duplicate _id was moved to local." + rescued
                + " rather than destroyed");
    }

    private static void enforcesTheIdIndexAgain(long handle) {
        byte[] document = Bson.document(Bson.int32("_id", 1),
                Bson.string("customer", "another duplicate"));
        byte[] reply = NativeBridge.command(handle, "shop",
                Bson.document(Bson.string("insert", "orders"),
                        Bson.array("documents", document)));
        check(Bson.number(reply, "ok") == 1.0, "the insert was not answered");
        // `n` is how many documents were written. An `_id_` index that is maintained again
        // refuses this one, which is exactly what the damaged directory could not do.
        check(Bson.number(reply, "n") == 0.0, "a duplicate _id was accepted after the repair");
        System.out.println("PASS the _id index refuses a duplicate again");
    }

    /**
     * The name of the {@code lost_and_found.<uuid>} collection the repair created, found by
     * scanning the {@code listCollections} reply for it.
     *
     * A scan rather than a walk of the cursor: {@link Bson} reads top-level numbers and nothing
     * else, and the name is a NUL-terminated UTF-8 string wherever in the reply it appears, so
     * reading to the terminator recovers it whole. An absent name is the finding that matters --
     * it means the repair deleted the evicted duplicate rather than moving it.
     */
    private static String lostAndFound(long handle) {
        byte[] reply = NativeBridge.command(handle, "local",
                Bson.document(Bson.int32("listCollections", 1)));
        check(Bson.number(reply, "ok") == 1.0, "listCollections on local was refused");
        byte[] needle = LOST_AND_FOUND.getBytes(StandardCharsets.UTF_8);
        for (int start = 0; start + needle.length <= reply.length; start++) {
            if (!startsWith(reply, start, needle)) {
                continue;
            }
            int end = start;
            while (end < reply.length && reply[end] != 0) {
                end++;
            }
            return new String(reply, start, end - start, StandardCharsets.UTF_8);
        }
        throw new AssertionError("`local` holds no " + LOST_AND_FOUND + "* collection, so the "
                + "evicted copy of _id 1 was destroyed rather than moved" + skipHint());
    }

    private static long count(long handle, String database, String collection, byte[] query) {
        byte[] reply = NativeBridge.command(handle, database,
                Bson.document(Bson.string("count", collection),
                        Bson.subDocument("query", query)));
        check(Bson.number(reply, "ok") == 1.0,
                "count on " + database + "." + collection + " was refused");
        return (long) Bson.number(reply, "n");
    }

    private RepairHarness() {}

    private static boolean startsWith(byte[] haystack, int offset, byte[] needle) {
        for (int index = 0; index < needle.length; index++) {
            if (haystack[offset + index] != needle[index]) {
                return false;
            }
        }
        return true;
    }

    /**
     * Names the skip switch when the environment sets it, so a suppressed pass is not reported
     * as a broken one. Appended rather than asserted up front, so that setting the variable
     * stays a usable way to check that these assertions really do detect a pass that never ran.
     */
    private static String skipHint() {
        String set = System.getenv(SKIP);
        if (set == null || set.isEmpty()) {
            return "";
        }
        return " (" + SKIP + "=" + set + " is set in the environment, which suppresses the pass)";
    }

    private static void check(boolean held, String message) {
        if (!held) {
            throw new AssertionError(message);
        }
    }
}
