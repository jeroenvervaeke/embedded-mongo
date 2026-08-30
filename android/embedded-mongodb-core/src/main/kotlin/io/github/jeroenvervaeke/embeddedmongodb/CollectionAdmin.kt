@file:JvmName("MongoCollections")
@file:JvmMultifileClass

package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document

/**
 * Deletes this collection and every index on it.
 *
 * Dropping one that is not there is a no-op: the state asked for is the state there is. A
 * collection nothing has ever written to does not exist, so this is the ordinary way a first run
 * clears whatever a previous one left.
 *
 * @throws EmbeddedMongoException if the engine refused for any other reason.
 */
suspend fun MongoCollection.drop() =
    ignoring(MongoErrorCode.NAMESPACE_NOT_FOUND) { runCommand(Document("drop", name)) }
