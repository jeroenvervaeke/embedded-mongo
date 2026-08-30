@file:JvmName("MongoCollections")
@file:JvmMultifileClass

package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document
import org.bson.conversions.Bson

/** How many documents a delete removed. */
data class DeleteResult(val deletedCount: Long)

/**
 * Removes the first document matching [filter].
 *
 * @throws EmbeddedMongoException if the engine rejected the write.
 */
suspend fun MongoCollection.deleteOne(filter: Bson): DeleteResult = delete(filter, limit = 1)

/**
 * Removes every document matching [filter]. `Document()` matches all of them, which is a slower
 * way of emptying a collection than [MongoCollection.drop] and the one that keeps the indexes.
 *
 * @throws EmbeddedMongoException if the engine rejected the write.
 */
suspend fun MongoCollection.deleteMany(filter: Bson): DeleteResult = delete(filter, limit = 0)

/** `limit` here is MongoDB's own spelling: 1 deletes one match, 0 deletes every match. */
private suspend fun MongoCollection.delete(filter: Bson, limit: Int): DeleteResult {
    val reply = runCommand(
        Document("delete", name).append(
            "deletes",
            listOf(Document("q", filter.toDocument()).append("limit", limit)),
        ),
    )
    return DeleteResult(reply.requiredLong("n"))
}
