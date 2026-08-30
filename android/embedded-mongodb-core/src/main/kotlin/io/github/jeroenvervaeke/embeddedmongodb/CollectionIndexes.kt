@file:JvmName("MongoCollections")
@file:JvmMultifileClass

package io.github.jeroenvervaeke.embeddedmongodb

import kotlinx.coroutines.flow.toList
import org.bson.Document
import org.bson.conversions.Bson

/** One index to build: its keys, and how it should behave. */
data class IndexModel(val keys: Bson, val options: IndexOptions = IndexOptions())

/**
 * What an index does beyond covering its keys.
 *
 * Every field is optional and an unset one is left to MongoDB, so an [IndexOptions] that names
 * nothing builds exactly the index [Indexes] described.
 */
data class IndexOptions(
    /** What to call it. Unset, MongoDB's own naming rule applies: `field_direction`, joined by `_`. */
    val name: String? = null,
    /** Whether a second document with the same key is rejected rather than stored. */
    val unique: Boolean = false,
    /** Whether documents that do not have the key are left out of the index entirely. */
    val sparse: Boolean = false,
    /** Indexes only the documents matching this, which keeps a rarely-queried subset cheap. */
    val partialFilter: Bson? = null,
    /** Deletes documents this many seconds after the indexed date. MongoDB's TTL index. */
    val expireAfterSeconds: Long? = null,
    /**
     * How much a match in each field counts, for a text index only: `Document("name", 10)` ranks a
     * hit on the name above one anywhere else.
     */
    val weights: Bson? = null,
    /** The language whose stop words and stemming a text index uses. */
    val defaultLanguage: String? = null,
)

/**
 * Builds one index and returns the name it was built under.
 *
 * Idempotent: an index that already exists with the same specification is reported rather than
 * rebuilt, which is what makes this safe to run on every start rather than only after seeding —
 * and an index that has silently gone missing is otherwise a collection scan nobody notices.
 *
 * An index build reads the free-disk floor, so on a device short of space this is the call that
 * fails first; `FreeDiskFloor` is where that is explained and lowered.
 *
 * @throws EmbeddedMongoException if the engine refused to build it — including when an index of
 *   this name already exists with *different* keys, which is a mistake rather than a repeat.
 */
suspend fun MongoCollection.createIndex(keys: Bson, options: IndexOptions = IndexOptions()): String =
    createIndexes(listOf(IndexModel(keys, options))).single()

/**
 * Builds every index in [indexes] in one command, and returns the names they were built under.
 *
 * One command rather than one each: `createIndexes` takes a list, and a half-indexed collection is
 * a state no screen has a sensible thing to show for.
 *
 * @throws IllegalArgumentException if [indexes] is empty.
 * @throws EmbeddedMongoException if the engine refused to build any of them.
 */
suspend fun MongoCollection.createIndexes(indexes: List<IndexModel>): List<String> {
    require(indexes.isNotEmpty()) { "creating no indexes is a command the engine rejects" }
    val specifications = indexes.map { it.specification() }
    runCommand(Document("createIndexes", name).append("indexes", specifications))
    return specifications.map { it.getString("name") }
}

/** Every index on this collection, as the engine describes it. */
suspend fun MongoCollection.listIndexes(): List<Document> =
    runCursorCommand(Document("listIndexes", name)).toList()

/**
 * Removes the index called [name]. Removing one that is not there is a no-op.
 *
 * `_id_` cannot be removed, and asking to is a failure rather than a no-op: MongoDB reports it
 * with a code of its own, and an application that meant to drop it meant something impossible.
 */
suspend fun MongoCollection.dropIndex(name: String) =
    ignoring(MongoErrorCode.NAMESPACE_NOT_FOUND, MongoErrorCode.INDEX_NOT_FOUND) {
        runCommand(Document("dropIndexes", this.name).append("index", name))
    }

/**
 * The index as `createIndexes` takes it.
 *
 * `name` is filled in here rather than left out, because the reply reports what was built by name
 * and a caller who named none is still owed the name to drop it by later. The rule is MongoDB's
 * own — every key and its value, joined by underscores — so a specification built here and one
 * built by a driver describe the same index.
 */
private fun IndexModel.specification(): Document {
    val keys = keys.toDocument()
    return Document("key", keys)
        .append("name", options.name ?: keys.entries.joinToString("_") { "${it.key}_${it.value}" })
        .apply {
            if (options.unique) append("unique", true)
            if (options.sparse) append("sparse", true)
            options.partialFilter?.let { append("partialFilterExpression", it.toDocument()) }
            options.expireAfterSeconds?.let { append("expireAfterSeconds", it) }
            options.weights?.let { append("weights", it.toDocument()) }
            options.defaultLanguage?.let { append("default_language", it) }
        }
}
