@file:JvmName("MongoCollections")
@file:JvmMultifileClass

package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document
import org.bson.conversions.Bson

/**
 * Every distinct value stored under [field], across the documents matching [filter].
 *
 * What a filter row on a screen is built from: the categories, brands or tags that are actually in
 * the data rather than the ones an application expects to be. The engine does the de-duplication,
 * so what crosses the bridge is the handful of values rather than every document holding them.
 *
 * The values are whatever BSON held — strings, numbers, documents, and `null` for a document that
 * stores the field as null. A dotted [field] reaches into a sub-document, and a field holding an
 * array contributes its elements rather than the array.
 *
 * @throws EmbeddedMongoException if the engine rejected the command, or answered with no `values`.
 */
suspend fun MongoCollection.distinct(field: String, filter: Bson? = null): List<Any?> {
    val command = Document("distinct", name).append("key", field).apply {
        filter?.let { append("query", it.toDocument()) }
    }
    return runCommand(command)["values"] as? List<Any?>
        ?: throw EmbeddedMongoException(
            "distinct answered with no `values` array",
            NO_ERROR_CODE,
        )
}
