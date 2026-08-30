package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document

/**
 * A [CommandRunner] that answers from Kotlin and records what it was asked.
 *
 * The whole of what this module needs to be tested with: every database, collection and query is
 * written against this interface, so a lambda here stands in for the engine.
 */
internal class FakeCommands(private val reply: (Document) -> Document) : CommandRunner {
    val commands = mutableListOf<Document>()
    val databases = mutableListOf<String>()

    val lastCommand: Document get() = commands.last()

    override suspend fun runCommand(database: String, command: Document): Document {
        databases += database
        commands += command
        return reply(command)
    }
}

/** A database and a collection on one [FakeCommands], which is what most of these tests need. */
internal class FakeMongo(reply: (Document) -> Document = { okReply() }) {
    val commands = FakeCommands(reply)
    val database = MongoDatabase(commands, DATABASE)
    val orders = database.getCollection(COLLECTION)

    val sent: List<Document> get() = commands.commands
    val lastCommand: Document get() = commands.lastCommand

    companion object {
        const val DATABASE = "shop"
        const val COLLECTION = "orders"
    }
}

internal fun okReply(vararg fields: Pair<String, Any?>): Document =
    Document("ok", 1.0).also { reply -> fields.forEach { (key, value) -> reply[key] = value } }

/** A reply from a command that opened a cursor, or from the `getMore` that continued one. */
internal fun cursorReply(
    id: Long,
    batch: String,
    documents: List<Document>,
    namespace: String = "${FakeMongo.DATABASE}.${FakeMongo.COLLECTION}",
): Document = okReply(
    "cursor" to Document("id", id).append("ns", namespace).append(batch, documents),
)

/** One batch of documents, and the last one: the engine reports an exhausted cursor as id 0. */
internal fun singleBatch(documents: List<Document>): Document =
    cursorReply(0, "firstBatch", documents)

internal fun documents(range: IntRange): List<Document> = range.map { Document("n", it) }

/** A failure shaped the way the engine's own reach this module: a message and a MongoDB code. */
internal fun mongoError(code: Int, message: String = "the engine refused"): Nothing =
    throw EmbeddedMongoException(message, code)
