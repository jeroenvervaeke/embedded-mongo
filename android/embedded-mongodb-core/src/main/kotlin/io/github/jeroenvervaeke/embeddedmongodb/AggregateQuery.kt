package io.github.jeroenvervaeke.embeddedmongodb

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.FlowCollector
import kotlinx.coroutines.flow.firstOrNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.toList
import org.bson.Document
import org.bson.conversions.Bson

/**
 * An `aggregate` that has not been run yet.
 *
 * ```
 * val nearest = places.aggregate(
 *     Document("\$geoNear", Document("near", point).append("distanceField", "distance")),
 *     Document("\$limit", 20),
 * ).toList()
 * ```
 *
 * Like [FindQuery] it **is** a `Flow<Document>` and every method returns a new query, and
 * nothing reaches the engine until it is collected. [command] is what would be sent, which is what
 * an application shows when it wants to prove that the pipeline on the screen is the one that ran.
 *
 * "New query" is about the query, not about the documents in it: a filter, sort or projection is
 * held as the caller passed it rather than copied, so changing that [Document] afterwards changes
 * what this query sends. `Bsons.kt` says why it is not copied — build one and hand it over.
 */
class AggregateQuery internal constructor(
    private val collection: MongoCollection,
    private val pipeline: List<Document>,
    private val batchSize: Int? = null,
    private val allowDiskUse: Boolean? = null,
    private val hint: Any? = null,
) : Flow<Document> {
    /** The stages, replacing the ones this query already has. */
    fun pipeline(pipeline: List<Bson>): AggregateQuery = with(pipeline = pipeline.toDocuments())

    /** This pipeline with [stages] added to the end, which is how a caller adds a limit. */
    fun append(vararg stages: Bson): AggregateQuery =
        with(pipeline = pipeline + stages.asList().toDocuments())

    /**
     * How many documents the engine puts in each batch, which is how often the paging in [asFlow]
     * costs a `getMore`. Left alone, the engine chooses.
     */
    fun batchSize(documents: Int): AggregateQuery {
        require(documents > 0) { "a batch of $documents documents holds nothing" }
        return with(batchSize = documents)
    }

    /**
     * Whether a stage that runs out of memory may spill to disk. Off is MongoDB's default, and on
     * a phone it is worth knowing that on means writing into the database directory.
     *
     * A spill also consults the free-disk floor, so an application that lowered the floor for its
     * index builds has lowered it for this too.
     */
    fun allowDiskUse(allowed: Boolean): AggregateQuery = with(allowDiskUse = allowed)

    /** The index the first stage should use, named by its key specification. */
    fun hint(keys: Bson): AggregateQuery = with(hint = keys.toDocument())

    /** The index the first stage should use, named by the name it was built under. */
    fun hintString(name: String): AggregateQuery = with(hint = name)

    /** The command this query would send. */
    fun command(): Document = Document("aggregate", collection.name)
        .append("pipeline", pipeline)
        .append("cursor", Document().apply { batchSize?.let { append("batchSize", it) } })
        .apply {
            allowDiskUse?.let { append("allowDiskUse", it) }
            hint?.let { append("hint", it) }
        }

    /**
     * Every document the pipeline produces, fetched a batch at a time as the collector consumes
     * them, and killing the cursor if the collector stops early.
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

    /** Every document the pipeline produces, read into memory. */
    suspend fun toList(): List<Document> = asFlow().toList()

    /**
     * The first document the pipeline produces, or `null` when it produced none — which is the
     * honest answer from a `$count` over an empty collection, since that emits no row at all.
     *
     * A `$limit` is added so the engine is told that one document is all that is wanted. Without
     * it a pipeline ending in `$sort` or `$group` does the whole of its work and then has all but
     * the first row thrown away on this side, which is a cost the identical-looking
     * [FindQuery.firstOrNull] never pays.
     */
    suspend fun firstOrNull(): Document? = limitedToOne().asFlow().firstOrNull()

    /**
     * The first document the pipeline produces.
     *
     * @throws NoSuchElementException if it produced none. [firstOrNull] is the one to want when
     *   producing nothing is an ordinary answer.
     */
    suspend fun first(): Document =
        firstOrNull() ?: throw NoSuchElementException("the pipeline produced no documents")

    /**
     * Collects this query, which is what makes it a `Flow` rather than something a `Flow` is
     * asked for.
     */
    override suspend fun collect(collector: FlowCollector<Document>) = asFlow().collect(collector)

    override fun toString(): String = command().toJson()

    /**
     * This pipeline, cut to one document unless it ends in a stage that writes.
     *
     * `$out` and `$merge` have to be the last stage of a pipeline, so appending to one is an
     * error rather than an optimisation. They also produce no documents, so there is nothing to
     * limit: the answer either way is the `null` this call already gives.
     */
    private fun limitedToOne(): AggregateQuery =
        if (pipeline.lastOrNull()?.keys?.firstOrNull() in WRITING_STAGES) this
        else append(Document("\$limit", 1))

    private fun with(
        pipeline: List<Document> = this.pipeline,
        batchSize: Int? = this.batchSize,
        allowDiskUse: Boolean? = this.allowDiskUse,
        hint: Any? = this.hint,
    ) = AggregateQuery(collection, pipeline, batchSize, allowDiskUse, hint)
}

/** The stages that write, which must be last in a pipeline and emit nothing of their own. */
private val WRITING_STAGES = setOf("\$out", "\$merge")
