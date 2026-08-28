package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document

/**
 * The documents produced by a cursor-returning command — `find`, `aggregate` — fetched one
 * batch at a time.
 *
 * The engine answers with a first batch and a cursor id, and holds the rest until asked. Iterating
 * this sequence issues the `getMore` commands that ask, so a query returning thousands of documents
 * arrives without the caller doing the paging.
 *
 * The sequence can be iterated once, because iterating it consumes the cursor rather than a
 * buffered list. Callers that stop early must [close] it: a cursor left open holds the storage
 * snapshot it reads from until the engine times it out. [close] is a no-op on an exhausted cursor,
 * so `use { }` costs nothing in the common case:
 *
 * ```
 * database.cursor("shop", Document("find", "orders")).use { orders ->
 *     orders.take(10).forEach(::render)
 * }
 * ```
 */
class DocumentCursor internal constructor(
    private val runner: CommandRunner,
    private val database: String,
    reply: Document,
) : Sequence<Document>, AutoCloseable {
    private val collection: String
    private var cursorId: Long
    private var batch: Iterator<Document>
    private var iterated = false

    init {
        val cursor = cursorOf(reply)
        collection = collectionOf(cursor)
        cursorId = idOf(cursor)
        batch = batchOf(cursor, FIRST_BATCH).iterator()
    }

    override fun iterator(): Iterator<Document> {
        check(!iterated) { "a DocumentCursor is consumed by iterating it and cannot be iterated twice" }
        iterated = true
        return generateSequence(::nextOrNull).iterator()
    }

    /**
     * Tells the engine to drop the cursor if it still holds one.
     *
     * A failure here is reported rather than swallowed: it means the engine is still holding the
     * cursor. Inside `use { }` it surfaces as a suppressed exception when the body failed too.
     */
    override fun close() {
        val abandoned = cursorId
        cursorId = EXHAUSTED
        if (abandoned == EXHAUSTED) return
        runner.run(
            database,
            Document("killCursors", collection).append("cursors", listOf(abandoned)),
        )
    }

    private fun nextOrNull(): Document? {
        while (!batch.hasNext()) {
            if (cursorId == EXHAUSTED) return null
            fetchNextBatch()
        }
        return batch.next()
    }

    private fun fetchNextBatch() {
        val cursor = cursorOf(
            runner.run(database, Document("getMore", cursorId).append("collection", collection)),
        )
        cursorId = idOf(cursor)
        batch = batchOf(cursor, NEXT_BATCH).iterator()
    }
}

/** Runs one command and returns its reply, having already turned a failed reply into an exception. */
internal fun interface CommandRunner {
    fun run(database: String, command: Document): Document
}

private const val FIRST_BATCH = "firstBatch"
private const val NEXT_BATCH = "nextBatch"

/** The id the engine reports once a cursor holds nothing more; asking for more of it is an error. */
private const val EXHAUSTED = 0L

private fun cursorOf(reply: Document): Document =
    reply["cursor"] as? Document
        ?: throw EmbeddedMongoException(
            "the command returned no cursor (fields: ${reply.keys.joinToString()})",
            NO_ERROR_CODE,
        )

private fun idOf(cursor: Document): Long =
    (cursor["id"] as? Number)?.toLong()
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
