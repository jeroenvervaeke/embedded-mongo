@file:JvmName("MongoCollections")
@file:JvmMultifileClass

package io.github.jeroenvervaeke.embeddedmongodb

import kotlinx.coroutines.flow.firstOrNull
import org.bson.Document
import org.bson.conversions.Bson

/**
 * How many documents match [filter], counted by reading them.
 *
 * The slower of the two counts and the one to reach for by default, because it is the one that is
 * true. This engine runs inside a process Android ends without warning, and after an unclean
 * shutdown the metadata count [estimatedDocumentCount] reads has been measured reporting 0 against
 * a collection holding about 90,000 documents. An application that decides whether to seed by
 * counting what is there cannot afford that answer.
 *
 * It costs a scan of whatever the filter selects, which for an unfiltered count is the whole
 * collection.
 */
suspend fun MongoCollection.countDocuments(filter: Bson? = null): Long {
    val stages = buildList {
        filter?.let { add(Document("\$match", it.toDocument())) }
        add(Document("\$count", COUNT))
    }
    // Collected rather than asked for through AggregateQuery.firstOrNull, which would append a
    // `$limit` to a pipeline that already emits at most one row.
    //
    // A `$count` over an empty selection produces no row at all, which is the honest shape of the
    // answer and is why an absent row is zero rather than a reply this library cannot read.
    val counted = aggregate(stages).asFlow().firstOrNull() ?: return 0
    return counted.requiredLong(COUNT)
}

/**
 * How many documents the collection holds, according to its metadata.
 *
 * Cheap — it reads a number rather than the collection — and worth it for a figure nothing depends
 * on: a "roughly this many" on a screen. Anything that decides something wants [countDocuments];
 * see there for what this number has been seen doing after an unclean shutdown.
 */
suspend fun MongoCollection.estimatedDocumentCount(): Long =
    runCommand(Document("count", name)).requiredLong("n")

private const val COUNT = "count"
