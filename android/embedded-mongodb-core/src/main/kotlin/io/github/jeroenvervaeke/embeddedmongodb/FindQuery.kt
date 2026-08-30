package io.github.jeroenvervaeke.embeddedmongodb

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.FlowCollector
import kotlinx.coroutines.flow.firstOrNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.toList
import org.bson.Document
import org.bson.conversions.Bson

/**
 * A `find` that has not been run yet.
 *
 * Every method returns a new query rather than changing this one, so a query can be built up in
 * pieces, handed around and reused:
 *
 * ```
 * val paid = orders.find(Document("paid", true))
 * val newest = paid.sort(Document("placed", -1)).limit(10).toList()
 * val oldest = paid.sort(Document("placed", 1)).limit(10).toList()
 * ```
 *
 * It **is** a `Flow<Document>`, exactly as the driver's `FindFlow` is, so it can be collected
 * directly and every `Flow` operator applies. Nothing reaches the engine until it is collected —
 * once per collection, since it is cold. [command] is what would be sent, which is worth having
 * when the query is being shown, logged or explained.
 *
 * "New query" is about the query, not about the documents in it: a filter, sort or projection is
 * held as the caller passed it rather than copied, so changing that [Document] afterwards changes
 * what this query sends. `Bsons.kt` says why it is not copied — build one and hand it over.
 */
class FindQuery internal constructor(
    private val collection: MongoCollection,
    private val filter: Document? = null,
    private val sort: Document? = null,
    private val projection: Document? = null,
    private val skip: Int? = null,
    private val limit: Int? = null,
    private val batchSize: Int? = null,
    private val hint: Any? = null,
) : Flow<Document> {
    /** Replaces the filter, rather than adding to it. */
    fun filter(filter: Bson): FindQuery = with(filter = filter.toDocument())

    /** The order to return matches in: `Document("placed", -1)` is newest first. */
    fun sort(sort: Bson): FindQuery = with(sort = sort.toDocument())

    /**
     * The fields to return, as `Document("name", 1)` to keep only that one or
     * `Document("addr", 0)` to keep everything else.
     *
     * Worth reaching for over a few thousand documents: the fields left out are fields that never
     * cross out of the engine.
     */
    fun projection(projection: Bson): FindQuery = with(projection = projection.toDocument())

    /** How many matches to step over before returning any. */
    fun skip(documents: Int): FindQuery {
        require(documents >= 0) { "cannot skip $documents documents" }
        return with(skip = documents)
    }

    /** How many matches to return at most. Zero is MongoDB's spelling of "no limit". */
    fun limit(documents: Int): FindQuery {
        require(documents >= 0) { "cannot limit a query to $documents documents" }
        return with(limit = documents)
    }

    /**
     * How many documents the engine puts in each batch, which is how often the paging in [asFlow]
     * costs a `getMore`. Left alone, the engine chooses.
     */
    fun batchSize(documents: Int): FindQuery {
        require(documents > 0) { "a batch of $documents documents holds nothing" }
        return with(batchSize = documents)
    }

    /** The index to use, named by its key specification — `Indexes.ascending("customer")`. */
    fun hint(keys: Bson): FindQuery = with(hint = keys.toDocument())

    /** The index to use, named by the name it was built under. */
    fun hintString(name: String): FindQuery = with(hint = name)

    /** The command this query would send. */
    fun command(): Document = Document("find", collection.name).apply {
        filter?.let { append("filter", it) }
        sort?.let { append("sort", it) }
        projection?.let { append("projection", it) }
        skip?.let { append("skip", it) }
        limit?.let { append("limit", it) }
        batchSize?.let { append("batchSize", it) }
        hint?.let { append("hint", it) }
    }

    /**
     * Every matching document, fetched a batch at a time as the collector consumes them.
     *
     * A collector that stops early kills the cursor on its way out, so `take`, `first` and a
     * cancelled scope all leave the engine holding nothing.
     */
    fun asFlow(): Flow<Document> = collection.runCursorCommand(command())


    /**
     * Every matching document, read by [read] as it arrives.
     *
     * The whole of this library's answer to typed collections: parsing is a function from a
     * [Document], and where a document becomes a domain object is a decision an application makes
     * once, at a boundary it owns. `map` on the returned flow would say the same thing; this saves
     * naming it, and puts the reading next to the query it belongs to.
     *
     * ```
     * val nearest = places.aggregate(pipeline).asFlow(Document::toPlace).toList()
     * ```
     */
    fun <T> asFlow(read: (Document) -> T): Flow<T> = asFlow().map(read)

    /** Every matching document, read into memory. */
    suspend fun toList(): List<Document> = asFlow().toList()

    /** The first matching document, or `null` when nothing matched. Asks the engine for one. */
    suspend fun firstOrNull(): Document? = limit(1).asFlow().firstOrNull()

    /**
     * The first matching document.
     *
     * @throws NoSuchElementException if nothing matched. [firstOrNull] is the one to want when
     *   nothing matching is an ordinary answer.
     */
    suspend fun first(): Document = firstOrNull() ?: throw NoSuchElementException("no document matched $this")

    /**
     * Collects this query, which is what makes it a `Flow` rather than something a `Flow` is
     * asked for. `orders.find(paid).collect(::render)` and every operator on `Flow` work directly,
     * as they do on the driver's own `FindFlow`.
     */
    override suspend fun collect(collector: FlowCollector<Document>) = asFlow().collect(collector)

    override fun toString(): String = command().toJson()

    private fun with(
        filter: Document? = this.filter,
        sort: Document? = this.sort,
        projection: Document? = this.projection,
        skip: Int? = this.skip,
        limit: Int? = this.limit,
        batchSize: Int? = this.batchSize,
        hint: Any? = this.hint,
    ) = FindQuery(collection, filter, sort, projection, skip, limit, batchSize, hint)
}
