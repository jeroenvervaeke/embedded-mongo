package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document

/**
 * Runs one MongoDB command against one database, and is the whole of what the rest of this
 * library needs from an engine.
 *
 * [MongoDatabase], [MongoCollection] and every query on them are written against this and nothing
 * else: they turn a call into a command, send it here, and read the reply. `EmbeddedMongo` is the
 * implementation that reaches the native engine; a test writes one in five lines and exercises
 * the whole query layer on the JVM, with no device, no emulator and no compiled engine.
 *
 * It is also the extension point. An application that wants tracing, a slow-command log, a retry
 * or a fake for one collection wraps the runner it was given and builds its own [MongoDatabase]
 * around the wrapper — the constructor is public for exactly that.
 *
 * ## The contract an implementation keeps
 *
 * **A command the engine rejected is an [EmbeddedMongoException], not a reply.** MongoDB reports
 * a failed command as an ordinary document carrying `ok: 0`, and a write that stored nothing as
 * an `ok: 1` reply with a populated `writeErrors`. An implementation that returned either would
 * have every caller above it read a failure as an empty result.
 *
 * **Calls may arrive from any thread and are answered in any order.** Nothing above this
 * serialises them, and the engine serialises commands itself.
 */
fun interface CommandRunner {
    suspend fun runCommand(database: String, command: Document): Document
}
