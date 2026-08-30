package io.github.jeroenvervaeke.embeddedmongodb

import kotlinx.coroutines.flow.Flow
import org.bson.Document
import org.bson.conversions.Bson

/**
 * One collection, and the questions worth asking about it.
 *
 * ```
 * val orders = mongo.database("shop").collection("orders")
 *
 * orders.insertMany(listOf(first, second))
 * orders.createIndex(Indexes.ascending("customer"))
 *
 * val recent = orders.find(Document("paid", true))
 *     .sort(Document("placed", -1))
 *     .limit(20)
 *     .toList()
 *
 * val spend = orders.aggregate(
 *     Document("\$match", Document("paid", true)),
 *     Document("\$group", Document("_id", "\$customer").append("total", Document("\$sum", "\$amount"))),
 * ).toList()
 * ```
 *
 * Everything on a collection is an extension function rather than a member, so that reading,
 * writing, counting and indexing each live in a file of their own — and so that an application can
 * add its own operation in the same shape as the ones here, built on [runCommand].
 *
 * Filters, sorts, projections, updates and index keys are all `org.bson.conversions.Bson`.
 * [Document] implements it, so `Document("paid", true)` is a filter; and an application that adds
 * `org.mongodb:mongodb-driver-core` can write `Filters.eq("paid", true)` at the same call sites
 * without this library depending on the driver.
 */
class MongoCollection internal constructor(
    internal val commands: CommandRunner,
    val databaseName: String,
    val name: String,
) {
    /** `database.collection`, which is how MongoDB names a collection in a reply. */
    val namespace: String get() = "$databaseName.$name"

    /**
     * The documents matching [filter], as a query still to be narrowed and then collected.
     *
     * Nothing reaches the engine until the query is collected, so `sort`, `limit` and the rest can
     * be added in any order and by whoever knows about them.
     */
    fun find(filter: Bson? = null): FindQuery = FindQuery(this, filter = filter?.toDocument())

    /** The documents [pipeline] produces, as a query still to be tuned and then collected. */
    fun aggregate(pipeline: List<Bson>): AggregateQuery = AggregateQuery(this, pipeline.toDocuments())

    /** [aggregate] for a pipeline written out stage by stage. */
    fun aggregate(vararg stages: Bson): AggregateQuery = aggregate(stages.asList())

    /**
     * Deletes this collection and every index on it. Dropping one that is not there is a no-op:
     * the state asked for is the state there is.
     */
    suspend fun drop() =
        ignoring(MongoErrorCode.NAMESPACE_NOT_FOUND) { runCommand(Document("drop", name)) }

    /**
     * Runs [command] against this collection's database and returns its reply, unread.
     *
     * The last resort, and the one an application extends this API through: a `distinct`, a
     * `mapReduce`, a command MongoDB grew after this library did. The collection is named by
     * whoever builds the command, because a command that names it is the only kind there is.
     *
     * @throws EmbeddedMongoException if the engine reports the command as failed.
     */
    suspend fun runCommand(command: Bson): Document =
        commands.runCommand(databaseName, command.toDocument())

    /** [runCommand] for a command that answers with a cursor rather than with one reply. */
    fun runCursorCommand(command: Bson): Flow<Document> =
        commands.documentFlow(databaseName, command.toDocument())

    override fun toString(): String = namespace
}
