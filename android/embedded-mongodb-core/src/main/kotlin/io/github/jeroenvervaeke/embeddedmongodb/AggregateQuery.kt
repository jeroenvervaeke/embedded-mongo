package io.github.jeroenvervaeke.embeddedmongodb

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.firstOrNull
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
 * Like [FindQuery] every method returns a new query, and nothing reaches the engine until the
 * query is collected. [command] is what would be sent, which is what an application shows when it
 * wants to prove that the pipeline on the screen is the pipeline that ran.
 */
class AggregateQuery internal constructor(
    private val collection: MongoCollection,
    private val pipeline: List<Document>,
    private val batchSize: Int? = null,
    private val allowDiskUse: Boolean? = null,
) {
    /** The stages, replacing the ones this query already has. */
    fun pipeline(pipeline: List<Bson>): AggregateQuery = with(pipeline = pipeline.toDocuments())

    /** This pipeline with [stages] appended, which is how a caller adds a filter or a limit. */
    fun then(vararg stages: Bson): AggregateQuery =
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

    /** The command this query would send. */
    fun command(): Document = Document("aggregate", collection.name)
        .append("pipeline", pipeline)
        .append("cursor", Document().apply { batchSize?.let { append("batchSize", it) } })
        .apply { allowDiskUse?.let { append("allowDiskUse", it) } }

    /**
     * Every document the pipeline produces, fetched a batch at a time as the collector consumes
     * them, and killing the cursor if the collector stops early.
     */
    fun asFlow(): Flow<Document> = collection.runCursorCommand(command())

    /** Every document the pipeline produces, read into memory. */
    suspend fun toList(): List<Document> = asFlow().toList()

    /**
     * The first document the pipeline produces, or `null` when it produced none — which is the
     * honest answer from a `$count` over an empty collection, since that emits no row at all.
     */
    suspend fun firstOrNull(): Document? = asFlow().firstOrNull()

    private fun with(
        pipeline: List<Document> = this.pipeline,
        batchSize: Int? = this.batchSize,
        allowDiskUse: Boolean? = this.allowDiskUse,
    ) = AggregateQuery(collection, pipeline, batchSize, allowDiskUse)
}
