@file:JvmName("MongoCollections")
@file:JvmMultifileClass

package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document
import org.bson.conversions.Bson
import org.bson.types.ObjectId

/**
 * What [insertOne] stored the document under, whether the caller chose it or this library did.
 *
 * Nullable because MongoDB permits `_id: null`: an application that stored one gets it back, and
 * one that let this library generate the id never sees null.
 */
data class InsertOneResult(val insertedId: Any?)

/** What [insertMany] stored each document under, in the order they were given. */
data class InsertManyResult(val insertedIds: List<Any?>)

/**
 * Stores [document], giving it an `ObjectId` `_id` if it does not already have one.
 *
 * The id is generated here rather than left to the engine so that the caller is told what it is —
 * which is the difference between being able to read the document back and having to search for
 * it. That is what every official driver does, and the Rust API of this engine too.
 *
 * The caller's document is not modified: a generated id goes into a copy, so running the same
 * insert twice stores two documents rather than failing on a duplicate key the second time.
 *
 * Written durably unless the command names its own `writeConcern` — see `EmbeddedMongo`, where
 * that default is explained.
 *
 * @throws EmbeddedMongoException if the engine rejected the write.
 */
suspend fun MongoCollection.insertOne(document: Bson): InsertOneResult {
    val stored = document.toDocument().withId()
    insert(listOf(stored), ordered = true)
    return InsertOneResult(stored[ID])
}

/**
 * Stores every document in [documents], giving each an `ObjectId` `_id` if it does not have one.
 *
 * [ordered] is MongoDB's own: ordered stops at the first document the engine rejects, unordered
 * carries on and reports every rejection at the end. Unordered is the one to want for independent
 * documents — a batch of seed data, say, where one bad row should cost that row and not the
 * hundreds behind it.
 *
 * @throws IllegalArgumentException if [documents] is empty, which the engine rejects outright.
 * @throws EmbeddedMongoException if the engine rejected the write.
 */
suspend fun MongoCollection.insertMany(
    documents: List<Bson>,
    ordered: Boolean = true,
): InsertManyResult {
    require(documents.isNotEmpty()) { "an insert of no documents is a command the engine rejects" }
    val stored = documents.map { it.toDocument().withId() }
    insert(stored, ordered)
    return InsertManyResult(stored.map { it[ID] })
}

private const val ID = "_id"

/**
 * Sends the insert and checks that the engine stored as many documents as it was given.
 *
 * A failed write is already an exception by the time it gets here — the [CommandRunner] contract
 * makes `writeErrors` a failure rather than a field — so a count that still disagrees means a
 * reply this library cannot account for, and saying so beats returning ids for documents that
 * are not there.
 */
private suspend fun MongoCollection.insert(documents: List<Document>, ordered: Boolean) {
    val reply = runCommand(
        Document("insert", name).append("documents", documents).append("ordered", ordered),
    )
    val inserted = reply.requiredLong("n")
    if (inserted != documents.size.toLong()) {
        throw EmbeddedMongoException(
            "the engine stored $inserted of ${documents.size} documents without reporting an error",
            NO_ERROR_CODE,
        )
    }
}

/** This document if it names an `_id`, or a copy that does. */
private fun Document.withId(): Document =
    if (containsKey(ID)) this else Document(this).append(ID, ObjectId())
