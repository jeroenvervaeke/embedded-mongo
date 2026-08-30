@file:JvmName("MongoCollections")
@file:JvmMultifileClass

package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document
import org.bson.conversions.Bson

/**
 * What an update did.
 *
 * [matchedCount] and [modifiedCount] differ when a document already held the values the update
 * asked for: MongoDB matched it and wrote nothing. Both are reported because "nothing matched" and
 * "nothing needed changing" are different answers to the same call.
 */
data class UpdateResult(
    val matchedCount: Long,
    val modifiedCount: Long,
    /** The `_id` of the document an [upsert] created, or `null` when none was. */
    val upsertedId: Any? = null,
)

/**
 * Applies [update] to the first document matching [filter].
 *
 * [update] is an update document: `Document("\$set", …)`, `Document("\$inc", …)`. A plain
 * document with no operator is what [replaceOne] is for, and the engine rejects one here.
 *
 * With [upsert] the update creates a document when nothing matched, and [UpdateResult.upsertedId]
 * is what it was stored under.
 *
 * @throws EmbeddedMongoException if the engine rejected the write.
 */
suspend fun MongoCollection.updateOne(
    filter: Bson,
    update: Bson,
    upsert: Boolean = false,
): UpdateResult = update(filter, update, multi = false, upsert = upsert)

/** [updateOne] for every document matching [filter] rather than the first. */
suspend fun MongoCollection.updateMany(
    filter: Bson,
    update: Bson,
    upsert: Boolean = false,
): UpdateResult = update(filter, update, multi = true, upsert = upsert)

/**
 * Replaces the first document matching [filter] with [replacement], keeping its `_id`.
 *
 * The whole document goes, which is what makes this different from [updateOne]: a field the
 * replacement does not carry is a field the stored document no longer has.
 *
 * @throws IllegalArgumentException if [replacement] holds update operators, which would silently
 *   be stored as field names beginning with a dollar rather than applied.
 * @throws EmbeddedMongoException if the engine rejected the write.
 */
suspend fun MongoCollection.replaceOne(
    filter: Bson,
    replacement: Bson,
    upsert: Boolean = false,
): UpdateResult {
    val document = replacement.toDocument()
    require(document.keys.none { it.startsWith('$') }) {
        "a replacement is a whole document, not update operators: use updateOne for $document"
    }
    return update(filter, document, multi = false, upsert = upsert)
}

private suspend fun MongoCollection.update(
    filter: Bson,
    update: Bson,
    multi: Boolean,
    upsert: Boolean,
): UpdateResult {
    val reply = runCommand(
        Document("update", name).append(
            "updates",
            listOf(
                Document("q", filter.toDocument())
                    .append("u", update.toDocument())
                    .append("multi", multi)
                    .append("upsert", upsert),
            ),
        ),
    )
    return UpdateResult(
        matchedCount = reply.requiredLong("n"),
        // Absent from a reply that matched nothing at all, which is a modified count of zero
        // rather than a reply this library cannot read.
        modifiedCount = reply.longOrNull("nModified") ?: 0,
        upsertedId = upsertedId(reply),
    )
}

/**
 * The `_id` of the document an upsert created.
 *
 * `upserted` is an array because one `update` command can carry several updates; this API sends
 * one, so the array holds at most one entry.
 */
private fun upsertedId(reply: Document): Any? =
    (reply["upserted"] as? List<*>)?.filterIsInstance<Document>()?.firstOrNull()?.get("_id")
