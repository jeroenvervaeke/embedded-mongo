package io.github.jeroenvervaeke.embeddedmongodb

import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.withContext
import org.bson.Document

/**
 * Every document a cursor-returning command produces, fetching further batches as the collector
 * consumes them.
 *
 * The engine answers `find` and `aggregate` with a first batch and a cursor id, and holds the rest
 * until asked. This issues the `getMore` commands that ask, so a query matching thousands of
 * documents arrives without the caller doing the paging.
 *
 * A collector that stops early — `take`, `first`, a cancelled scope, an exception thrown in the
 * collector — leaves the engine holding a cursor and the storage snapshot it reads from, so the
 * cursor is killed on every way out of the flow. That happens under [NonCancellable]: the usual
 * way out is a cancellation, and a `killCursors` that inherited it would be cancelled before it
 * reached the engine.
 *
 * A failure to kill it is reported rather than swallowed, because the engine is then still holding
 * the cursor. When the flow is already failing it is attached to that failure as a suppressed one:
 * the collector is owed the reason their query stopped before they are owed the tidying-up.
 */
internal fun CommandRunner.documentFlow(database: String, command: Document): Flow<Document> = flow {
    val cursor = CursorPaging(this@documentFlow, database, runCommand(database, command))
    try {
        while (true) {
            val batch = cursor.nextBatch() ?: break
            batch.forEach { document -> emit(document) }
        }
    } catch (failure: Throwable) {
        withContext(NonCancellable) {
            try {
                cursor.kill()
            } catch (killing: Throwable) {
                failure.addSuppressed(killing)
            }
        }
        throw failure
    }
    withContext(NonCancellable) { cursor.kill() }
}

/**
 * One open cursor: the batch it is holding, and whether the engine has more.
 *
 * The first batch arrives in the reply to the command that opened the cursor, so it is read here
 * rather than fetched; every batch after it costs a `getMore`.
 */
private class CursorPaging(
    private val commands: CommandRunner,
    private val database: String,
    opening: Document,
) {
    private val collection: String
    private var id: Long
    private var pending: List<Document>?

    init {
        val cursor = cursorOf(opening)
        collection = collectionOf(cursor)
        id = idOf(cursor)
        pending = batchOf(cursor, FIRST_BATCH)
    }

    /** The next batch, or `null` once the engine has no more to give. */
    suspend fun nextBatch(): List<Document>? {
        pending?.let { batch ->
            pending = null
            return batch
        }
        if (id == EXHAUSTED) return null
        val cursor = cursorOf(
            commands.runCommand(database, Document(GET_MORE, id).append("collection", collection)),
        )
        id = idOf(cursor)
        return batchOf(cursor, NEXT_BATCH)
    }

    /** Tells the engine to drop the cursor if it still holds one. A no-op once it does not. */
    suspend fun kill() {
        val abandoned = id
        id = EXHAUSTED
        if (abandoned == EXHAUSTED) return
        commands.runCommand(
            database,
            Document(KILL_CURSORS, collection).append("cursors", listOf(abandoned)),
        )
    }
}

private const val FIRST_BATCH = "firstBatch"
private const val NEXT_BATCH = "nextBatch"
private const val GET_MORE = "getMore"
private const val KILL_CURSORS = "killCursors"

/** The id the engine reports once a cursor holds nothing more; asking for more of it is an error. */
private const val EXHAUSTED = 0L

private fun cursorOf(reply: Document): Document =
    reply["cursor"] as? Document
        ?: throw EmbeddedMongoException(
            "the command returned no cursor (fields: ${reply.keys.joinToString()})",
            NO_ERROR_CODE,
        )

private fun idOf(cursor: Document): Long =
    cursor.longOrNull("id")
        ?: throw EmbeddedMongoException("the cursor reply carries no id", NO_ERROR_CODE)

/**
 * `getMore` and `killCursors` name the collection, while the reply names the full namespace, so
 * the database prefix is dropped here. Only the first separator is a prefix: collection names may
 * contain further dots.
 */
private fun collectionOf(cursor: Document): String {
    val namespace = cursor["ns"] as? String
        ?: throw EmbeddedMongoException("the cursor reply carries no namespace", NO_ERROR_CODE)
    val collection = namespace.substringAfter('.', missingDelimiterValue = "")
    if (collection.isEmpty()) {
        throw EmbeddedMongoException("the cursor namespace `$namespace` names no collection", NO_ERROR_CODE)
    }
    return collection
}

private fun batchOf(cursor: Document, name: String): List<Document> {
    val batch = cursor[name] as? List<*>
        ?: throw EmbeddedMongoException("the cursor reply carries no $name array", NO_ERROR_CODE)
    return batch.map {
        it as? Document
            ?: throw EmbeddedMongoException("the cursor $name holds a value that is not a document", NO_ERROR_CODE)
    }
}
