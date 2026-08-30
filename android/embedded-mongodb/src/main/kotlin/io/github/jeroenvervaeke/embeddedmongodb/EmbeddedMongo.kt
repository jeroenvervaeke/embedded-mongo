package io.github.jeroenvervaeke.embeddedmongodb

import android.content.Context
import java.io.File
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.withContext
import org.bson.Document

/**
 * A MongoDB running inside the application process, stored in one directory.
 *
 * ```
 * val mongo = EmbeddedMongo.open(context, File(context.filesDir, "shop"))
 * val orders = mongo.database("shop").collection("orders")
 *
 * orders.insertOne(Document("customer", "ada").append("total", 12))
 * orders.createIndex(Indexes.ascending("customer"))
 * val hers = orders.find(Document("customer", "ada")).sort(Document("total", -1)).toList()
 * ```
 *
 * [database] is the way in, and [MongoDatabase] and [MongoCollection] are where the API is: find,
 * aggregate, insert, update, delete, count and index, with cursors paged for the caller. This
 * class is the engine and its lifecycle — opening it, closing it, and running a command that has
 * no builder above it.
 *
 * How much room the engine may take is [StorageOptions], named at [open]. The defaults are sized
 * for a phone, so an application that names nothing is not left with a server's appetite; the one
 * worth reading before leaning on it is [FreeDiskFloor].
 *
 * An instance is meant to live as long as the data it serves; [close] it from a background thread
 * when it does not. **One instance per process**: the engine refuses a second runtime, so a second
 * [open] before the first is closed throws [EmbeddedMongoException]. An application that needs
 * several logical databases uses several database names inside the one directory, which is what a
 * MongoDB server does too.
 *
 * The suspending members run on a private database thread; the members named `…Blocking` run on
 * the caller's thread and refuse to run on Android's main thread, where a query long enough to
 * matter is a query long enough to trigger an ANR.
 *
 * Instances are safe to share between threads. Commands are serialised, because the engine
 * serialises them anyway.
 */
class EmbeddedMongo internal constructor(
    private val engine: Engine,
    private val guard: MainThreadGuard,
) : AutoCloseable, CommandRunner {
    // One thread rather than a pool: the engine runs every command on a single internal strand, so
    // extra threads would queue behind the first one while costing a stack each. A single thread
    // also gives suspending callers the ordering they would get from one connection to a server.
    private val databaseThread: ExecutorService = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "embedded-mongodb").apply { isDaemon = true }
    }
    private val dispatcher: CoroutineDispatcher = databaseThread.asCoroutineDispatcher()

    @Volatile
    private var closed = false

    /**
     * The database called [name], which is where the collections and the queries are.
     *
     * Nothing is created and no command is sent: naming a database that holds nothing is free, and
     * it starts existing when something is written into it.
     */
    fun database(name: String): MongoDatabase = MongoDatabase(this, name)

    /**
     * Runs [command] against [database] on the database thread and returns its reply.
     *
     * The primitive every collection and query is built from, and the last resort for a command
     * none of them cover. Prefer [database] and the API on [MongoDatabase]: it names the database
     * once, checks the replies, and pages the cursors.
     *
     * @throws EmbeddedMongoException if the engine reports the command as failed.
     * @throws IllegalStateException if called after [close].
     */
    override suspend fun runCommand(database: String, command: Document): Document {
        // Checked before dispatching as well as inside the command: once the database thread is
        // shut down, dispatching onto it cancels the caller's job, and a CancellationException
        // would tell them far less than this does.
        checkOpen()
        return withContext(dispatcher) { runCommandBlocking(database, command) }
    }

    /**
     * Runs [command] against [database] on the calling thread.
     *
     * A write that names no `writeConcern` is journalled before it is acknowledged; see [durable]
     * for why that is the default here and how to choose otherwise.
     *
     * @throws EmbeddedMongoException if the engine reports the command as failed.
     * @throws IllegalStateException if called on the main thread, or after [close].
     */
    fun runCommandBlocking(database: String, command: Document): Document {
        guard.reject("Running a MongoDB command")
        checkOpen()
        val encoded = BsonCodec.encode(durable(command))
        return checkedReply(BsonCodec.decode(engine.command(database, encoded)))
    }

    /**
     * Closes the database, waiting for a command already running on another thread to finish.
     *
     * Calling this on the main thread logs a warning rather than throwing: closing from a
     * lifecycle callback is normal, and an exception thrown out of `use { }` would hide whatever
     * the body failed with.
     */
    override fun close() {
        guard.warn("Closing an embedded MongoDB database")
        if (closed) return
        closed = true
        try {
            engine.close()
        } finally {
            databaseThread.shutdown()
        }
    }

    private fun checkOpen() = check(!closed) { "the embedded MongoDB database is closed" }

    // `@JvmOverloads` on all four: `options` arrived as a defaulted parameter, which changes the
    // JVM signature of the function that was there before it. Without the annotation an
    // application compiled against an earlier build of this library would fail with
    // NoSuchMethodError rather than pick up the new default, which is the same promise the
    // native bridge keeps by adding an entry point rather than editing one.
    companion object {
        /**
         * Opens, creating it if it does not exist, the database stored in [directory], sized by
         * [options] and having first checked that the volume can give the engine room to work.
         *
         * This is the overload to prefer on Android: running out of space aborts the process
         * rather than failing a command, and [context] is what makes the room measurable.
         */
        @JvmOverloads
        suspend fun open(
            context: Context,
            directory: File,
            options: StorageOptions = StorageOptions(),
        ): EmbeddedMongo = openedOrClosed { openBlocking(context, directory, options) }

        /**
         * Opens, creating it if it does not exist, the database stored in [directory], sized by
         * [options].
         *
         * Prefer the overload taking a [Context]: without one there is no way to ask the platform
         * how much room the volume can give, and the first sign of a full one is the process
         * being killed.
         */
        @JvmOverloads
        suspend fun open(directory: File, options: StorageOptions = StorageOptions()): EmbeddedMongo =
            openedOrClosed { openBlocking(directory, options) }

        /**
         * Opens the database stored in [directory] on the calling thread, having first checked
         * that the volume can give the engine room to work.
         *
         * A [StorageOptions.freeDiskFloor] lowers what that check insists on as well as what the
         * engine does, since an application that named a floor has already said how much room is
         * enough for it.
         *
         * @throws IllegalStateException if called on the main thread.
         * @throws IllegalArgumentException if [directory] exists and is not a directory.
         * @throws InsufficientStorageException if the volume cannot give the engine the space it
         *   needs.
         * @throws EmbeddedMongoException if the engine cannot open the directory or will not take
         *   [options], a second database already open in this process included.
         */
        @JvmOverloads
        fun openBlocking(
            context: Context,
            directory: File,
            options: StorageOptions = StorageOptions(),
        ): EmbeddedMongo {
            MainThreadGuard.Android.reject(OPENING)
            prepare(directory)
            checkStorage(context, directory, options)
            return opened(directory, options)
        }

        /**
         * Opens the database stored in [directory] on the calling thread, sized by [options].
         *
         * @throws IllegalStateException if called on the main thread.
         * @throws IllegalArgumentException if [directory] exists and is not a directory.
         * @throws EmbeddedMongoException if the engine cannot open the directory or will not take
         *   [options], a second database already open in this process or a volume with no room
         *   for it included.
         */
        @JvmOverloads
        fun openBlocking(directory: File, options: StorageOptions = StorageOptions()): EmbeddedMongo {
            MainThreadGuard.Android.reject(OPENING)
            prepare(directory)
            return opened(directory, options)
        }

        private const val OPENING = "Opening an embedded MongoDB database"

        private fun prepare(directory: File) {
            directory.mkdirs()
            require(directory.isDirectory) {
                "$directory cannot hold a database: it exists and is not a directory"
            }
        }

        /**
         * The free-disk floor is applied here rather than inside the engine because it is a
         * server parameter, which only exists once the engine is running. Nothing between the
         * open and this needs it: the index repair pass the open may run builds its indexes
         * through `validate`, which does not consult the floor — only `createIndexes` does, and
         * the caller cannot have run one yet.
         *
         * Applied on every open, including one that named no floor at all. The floor is a
         * setting of the process rather than of a database, so a caller who named none has to be
         * put back on MongoDB's own floors instead of being left on whatever an earlier database
         * set and closed; `establishFreeDiskFloor` is where that is spelled out.
         */
        private fun opened(directory: File, options: StorageOptions) =
            EmbeddedMongo(NativeEngine.open(directory, options), MainThreadGuard.Android)
                .establishFreeDiskFloor(options.freeDiskFloor)
    }
}
