package io.github.jeroenvervaeke.embeddedmongodb

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.toList
import org.bson.Document
import org.bson.conversions.Bson

/**
 * One database inside an embedded MongoDB, and the way to reach the collections in it.
 *
 * ```
 * val shop = mongo.getDatabase("shop")
 * val orders = shop.getCollection("orders")
 * orders.insertOne(Document("total", 12).append("paid", false))
 * val unpaid = orders.find(Document("paid", false)).sort(Document("total", -1)).toList()
 * ```
 *
 * A MongoDB server holds many databases and so does this engine, so the name is chosen here once
 * rather than repeated at every call. Instances are cheap and hold no state of their own: make
 * one and keep it, or make another wherever it is convenient.
 *
 * The constructor is public so that anything able to run a command can be given the whole query
 * API: a test with a scripted [CommandRunner], a wrapper that logs or traces, a module that must
 * not depend on the Android library. `EmbeddedMongo.database` is the ordinary way in.
 */
class MongoDatabase(internal val commands: CommandRunner, val name: String) {
    /**
     * The collection called [name] in this database. Nothing is created until something writes.
     *
     * `getCollection` rather than `collection`, and `getDatabase` rather than `database` on the
     * engine, because those are the names the official Java and Kotlin drivers use: code pasted
     * from either compiles here.
     */
    fun getCollection(name: String): MongoCollection = MongoCollection(commands, this.name, name)

    /** The names of every collection in this database, the system ones included. */
    suspend fun listCollectionNames(): List<String> =
        runCursorCommand(Document("listCollections", 1).append("nameOnly", true))
            .map { entry ->
                entry["name"] as? String
                    ?: throw EmbeddedMongoException("listCollections named no collection", NO_ERROR_CODE)
            }
            .toList()

    /**
     * Creates [name] explicitly, with [options] such as a `capped` size or a `validator`.
     *
     * Writing to a collection creates it, so this is for the cases where the shape matters before
     * the first document does — a `capped` size, a `validator`, a collation.
     *
     * A collection that already exists is a failure, as it is in the driver: MongoDB reports
     * [MongoErrorCode.NAMESPACE_EXISTS] and it is reported on. An application using this as a
     * race guard needs that; one that only wants the collection to exist can catch the code, or
     * write to it and let the write create it.
     *
     * `create` cannot change a collection that is already there either way. `collMod` through
     * [runCommand] is what does that.
     *
     * @throws EmbeddedMongoException if the collection exists, or the engine refused the options.
     */
    suspend fun createCollection(name: String, options: Bson? = null) {
        val create = Document("create", name)
        options?.let { create.putAll(it.toDocument()) }
        runCommand(create)
    }

    /** Deletes this database and everything in it. Dropping one that is not there is a no-op. */
    suspend fun drop() =
        ignoring(MongoErrorCode.NAMESPACE_NOT_FOUND) { runCommand(Document("dropDatabase", 1)) }

    /**
     * Runs [command] against this database and returns its reply, unread.
     *
     * The last resort, and worth reaching for only when nothing above it fits: `getParameter`,
     * `collStats`, `explain`, a command MongoDB grew after this library did. Everything else in
     * this API ends up here too, so a command written by hand is not a lesser citizen — only an
     * unchecked one.
     *
     * @throws EmbeddedMongoException if the engine reports the command as failed.
     */
    suspend fun runCommand(command: Bson): Document =
        commands.runCommand(name, command.toDocument())

    /**
     * Runs a cursor-returning [command] and emits every document it produces, issuing `getMore` as
     * the collector consumes them and killing the cursor if the collector stops early.
     *
     * The escape hatch for a command that answers with a cursor and has no builder here —
     * `listCollections`, `currentOp`, `$listLocalSessions`. [MongoCollection.find] and
     * [MongoCollection.aggregate] are the ones that do.
     */
    fun runCursorCommand(command: Bson): Flow<Document> =
        commands.documentFlow(name, command.toDocument())

    override fun toString(): String = name
}

/**
 * Runs [command], treating any of [codes] as success.
 *
 * MongoDB reports "there was nothing to do" as a failure — dropping a collection that is not
 * there, creating one that is — and an application made to catch those is being made to handle an
 * error that is not one.
 */
internal suspend inline fun ignoring(vararg codes: Int, command: () -> Unit) {
    try {
        command()
    } catch (failure: EmbeddedMongoException) {
        if (failure.code !in codes) throw failure
    }
}
