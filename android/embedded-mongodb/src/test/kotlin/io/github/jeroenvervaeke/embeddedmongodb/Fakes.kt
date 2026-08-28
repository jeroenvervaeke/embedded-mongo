package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document

/**
 * An [Engine] that answers from Kotlin rather than from the native library, so cursor paging,
 * error mapping and the threading policy can be tested on the JVM.
 */
internal class FakeEngine(private val reply: (Document) -> Document) : Engine {
    val commands = mutableListOf<Document>()
    val databases = mutableListOf<String>()
    val threads = mutableListOf<String>()
    var closes = 0
        private set

    override fun command(database: String, command: ByteArray): ByteArray {
        val decoded = BsonCodec.decode(command)
        databases += database
        commands += decoded
        threads += Thread.currentThread().name
        return BsonCodec.encode(reply(decoded))
    }

    override fun close() {
        closes++
    }
}

/** A [CommandRunner] that answers with prepared replies and records what it was asked. */
internal class FakeRunner(replies: List<Document>) : CommandRunner {
    private val replies = replies.toMutableList()
    val commands = mutableListOf<Document>()

    override fun run(database: String, command: Document): Document {
        commands += command
        check(replies.isNotEmpty()) { "the cursor issued an unexpected command: $command" }
        return replies.removeAt(0)
    }
}

internal fun guard(onMainThread: Boolean, report: (String) -> Unit = {}) =
    MainThreadGuard(onMainThread = { onMainThread }, report = report)

internal fun okReply(vararg fields: Pair<String, Any?>): Document =
    Document("ok", 1.0).also { reply -> fields.forEach { (key, value) -> reply[key] = value } }

internal fun cursorReply(
    id: Long,
    batch: String,
    documents: List<Document>,
    namespace: String = "shop.orders",
): Document = okReply(
    "cursor" to Document("id", id).append("ns", namespace).append(batch, documents),
)

internal fun documents(range: IntRange): List<Document> = range.map { Document("n", it) }
