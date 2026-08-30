@file:JvmName("MongoCollections")
@file:JvmMultifileClass

package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document
import org.bson.conversions.Bson

/** Which version of a changed document a `findOneAnd…` hands back. */
enum class ReturnDocument {
    /** The document as it was before the change, which is MongoDB's own default. */
    BEFORE,

    /** The document as it is after the change. */
    AFTER,
}

/**
 * Applies [update] to the first document matching [filter] and returns the document, or `null`
 * when nothing matched.
 *
 * The only atomic read-modify-write here, and the reason it exists rather than being left to
 * [runCommand]: an [updateOne] followed by a [find] is two commands, and two coroutines really do
 * interleave between them. "Claim the next unsent row" and "increment this and tell me the new
 * value" are the shapes that need it.
 *
 * [returning] chooses which version comes back. `BEFORE` is MongoDB's default and is what makes
 * this a claim: the caller sees the document as it was when they won it.
 *
 * [sort] decides which document is first when several match, which is what makes a queue a queue.
 * [projection] cuts the returned document down; it does not affect what is written.
 *
 * With [upsert] a document is created when nothing matched. Note that `BEFORE` then still answers
 * `null` — there was no earlier version — so a caller who wants what was created asks for `AFTER`.
 *
 * @throws EmbeddedMongoException if the engine rejected the write.
 */
suspend fun MongoCollection.findOneAndUpdate(
    filter: Bson,
    update: Bson,
    sort: Bson? = null,
    projection: Bson? = null,
    upsert: Boolean = false,
    returning: ReturnDocument = ReturnDocument.BEFORE,
): Document? = findAndModify(filter, sort, projection) {
    append("update", update.toDocument())
    append("upsert", upsert)
    append("new", returning == ReturnDocument.AFTER)
}

/**
 * Replaces the first document matching [filter] with [replacement] and returns the document, or
 * `null` when nothing matched.
 *
 * [findOneAndUpdate] applies operators; this puts a whole document in place of the stored one,
 * keeping its `_id`. A field the replacement does not carry is a field the stored document no
 * longer has.
 *
 * @throws IllegalArgumentException if [replacement] holds update operators, which would be stored
 *   as field names beginning with a dollar rather than applied.
 * @throws EmbeddedMongoException if the engine rejected the write.
 */
suspend fun MongoCollection.findOneAndReplace(
    filter: Bson,
    replacement: Bson,
    sort: Bson? = null,
    projection: Bson? = null,
    upsert: Boolean = false,
    returning: ReturnDocument = ReturnDocument.BEFORE,
): Document? {
    val document = replacement.toDocument()
    require(document.keys.none { it.startsWith('$') }) {
        "a replacement is a whole document, not update operators: use findOneAndUpdate for $document"
    }
    return findAndModify(filter, sort, projection) {
        append("update", document)
        append("upsert", upsert)
        append("new", returning == ReturnDocument.AFTER)
    }
}

/**
 * Removes the first document matching [filter] and returns it, or `null` when nothing matched.
 *
 * Atomic in the way [findOneAndUpdate] is: nothing can take the document between the read and the
 * delete, which a [find] followed by a [deleteOne] cannot promise.
 *
 * @throws EmbeddedMongoException if the engine rejected the write.
 */
suspend fun MongoCollection.findOneAndDelete(
    filter: Bson,
    sort: Bson? = null,
    projection: Bson? = null,
): Document? = findAndModify(filter, sort, projection) { append("remove", true) }

/**
 * The document `findAndModify` answered with.
 *
 * `value` is `null` rather than absent when nothing matched, so the two cannot be told apart and
 * both mean the same thing here. Anything else under that key is a reply this library cannot read
 * rather than a document, and saying so beats handing back a silent `null`.
 */
private suspend fun MongoCollection.findAndModify(
    filter: Bson,
    sort: Bson?,
    projection: Bson?,
    action: Document.() -> Unit,
): Document? {
    val command = Document("findAndModify", name).append("query", filter.toDocument()).apply {
        sort?.let { append("sort", it.toDocument()) }
        projection?.let { append("fields", it.toDocument()) }
        action()
    }
    val reply = runCommand(command)
    val value = reply["value"] ?: return null
    return value as? Document
        ?: throw EmbeddedMongoException(
            "findAndModify answered with a `value` that is not a document",
            NO_ERROR_CODE,
        )
}
