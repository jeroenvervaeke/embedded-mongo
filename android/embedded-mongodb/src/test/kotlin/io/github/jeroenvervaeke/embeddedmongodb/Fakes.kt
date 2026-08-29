package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document

/**
 * An [Engine] that answers from Kotlin rather than from the native library, so cursor paging,
 * error mapping and the threading policy can be tested on the JVM.
 */
internal class FakeEngine(
    private val closeFailure: Throwable? = null,
    private val reply: (Document) -> Document,
) : Engine {
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
        closeFailure?.let { throw it }
    }
}

/** A [BridgeOpener] that records which entry point it was sent to, and with what. */
internal class FakeOpener(private val handle: Long = 1) : BridgeOpener {
    var openedPath: String? = null
        private set
    var openedWithOptionsPath: String? = null
        private set
    var slots: LongArray? = null
        private set

    override fun open(path: String): Long {
        openedPath = path
        return handle
    }

    override fun openWithOptions(path: String, options: LongArray): Long {
        openedWithOptionsPath = path
        slots = options
        return handle
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

/**
 * An [Engine] starting on [mebibytes] for both free-disk floors, which accepts every command and
 * answers `getParameter` with whatever `setParameter` last wrote — except that a `setParameter`
 * naming [refuse] is rejected the way an engine without that knob would.
 *
 * The floors are remembered rather than fixed because the open path turns on *when* it reads
 * them: it has to record MongoDB's own before applying the caller's, or it records the caller's
 * and hands it to the next open that asked for the default. A fake whose reply ignores what was
 * set on it answers a read taken after a write exactly as one taken before, so no test written
 * against it could tell those two apart.
 */
internal fun engineReporting(
    mebibytes: Long,
    refuse: String? = null,
    closeFailure: Throwable? = null,
): FakeEngine {
    var indexBuild = mebibytes
    var querySpilling = mebibytes * 1024 * 1024
    return FakeEngine(closeFailure) { command ->
        when {
            command.keys.first() == "getParameter" ->
                okReply(INDEX_BUILD_FLOOR to indexBuild, QUERY_SPILLING_FLOOR to querySpilling)
            refuse != null && command.containsKey(refuse) ->
                Document("ok", 0.0).append("errmsg", "no such parameter $refuse")
            else -> {
                (command[INDEX_BUILD_FLOOR] as? Long)?.let { indexBuild = it }
                (command[QUERY_SPILLING_FLOOR] as? Long)?.let { querySpilling = it }
                okReply()
            }
        }
    }
}

private const val INDEX_BUILD_FLOOR = "indexBuildMinAvailableDiskSpaceMB"

private const val QUERY_SPILLING_FLOOR = "internalQuerySpillingMinAvailableDiskSpaceBytes"

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
