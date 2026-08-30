package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document

/**
 * The code carried by a failure this library raised itself, rather than one the engine reported.
 * Neither MongoDB nor the bridge ever uses it: MongoDB codes are positive and the bridge's own are
 * the negative ones in [BridgeError].
 */
const val NO_ERROR_CODE: Int = 0

/**
 * The few MongoDB error codes worth naming, out of the several hundred MongoDB has.
 *
 * These are the ones an application reacts to rather than reports: a duplicate key it expected, a
 * collection that was already there, an index that was already gone. The rest arrive as
 * [EmbeddedMongoException.code] and mean what MongoDB's error code documentation says they mean.
 *
 * ```
 * try {
 *     orders.insertOne(order)
 * } catch (failure: EmbeddedMongoException) {
 *     if (failure.code != MongoErrorCode.DUPLICATE_KEY) throw failure
 *     // …that customer already has one.
 * }
 * ```
 */
object MongoErrorCode {
    /** The collection or database is not there. Dropping one that is already gone reports this. */
    const val NAMESPACE_NOT_FOUND: Int = 26

    /** The index is not there, which is what `dropIndexes` answers for one already dropped. */
    const val INDEX_NOT_FOUND: Int = 27

    /** The collection is already there, which is what `create` answers when it has nothing to do. */
    const val NAMESPACE_EXISTS: Int = 48

    /**
     * A unique index rejected a write, `_id` included. The one every application ends up
     * catching, and the reason this object exists rather than leaving callers a magic number.
     */
    const val DUPLICATE_KEY: Int = 11000
}

/**
 * A failure reported by the engine: a command that answered `ok: 0`, a write that failed, an error
 * raised inside the native bridge, or a reply this library could not make sense of.
 *
 * [code] tells the three apart — a positive MongoDB error code, one of the negative [BridgeError]
 * values, or [NO_ERROR_CODE] when the failure was raised on this side of the bridge. Compare it
 * with [MongoErrorCode] for the handful worth catching.
 *
 * [response] is the whole reply the engine sent, when the failure came from one. A failed command
 * carries more than a message — `writeErrors` says which documents of a batch were rejected and
 * why, `n` says how many were stored anyway, and `writeConcernError` is a third thing again — and
 * an exception that kept only the first message would be throwing that away. It is `null` for a
 * failure raised before or below a reply: a bridge error, or a reply this library could not read.
 *
 * The native bridge constructs this class by name through JNI with the `(String, int)`
 * constructor, so `consumer-rules.pro` keeps R8 from renaming either, and `@JvmOverloads` keeps
 * that two-argument signature in existence now that there is a third parameter.
 */
class EmbeddedMongoException @JvmOverloads constructor(
    message: String,
    val code: Int,
    val response: Document? = null,
) : Exception(message) {
    /** The bridge failure [code] names, or `null` when the code came from MongoDB or from here. */
    val bridgeError: BridgeError? get() = BridgeError.of(code)

    /**
     * The per-document failures of a write, in the order the engine reported them, or empty when
     * the failure was not one.
     *
     * The reason an unordered [insertMany] is worth asking for: the engine carries on past a
     * document it rejects, so this holds every rejection rather than only the one that stopped it,
     * and `response["n"]` says how many of the batch were stored.
     */
    val writeErrors: List<Document>
        get() = (response?.get("writeErrors") as? List<*>).orEmpty().filterIsInstance<Document>()
}

/**
 * The failures the bridge itself reports, as opposed to the ones MongoDB reports.
 *
 * They are worth telling apart: [UNKNOWN_HANDLE] means the database was closed underneath the
 * caller and reopening is the answer, while [ENGINE_ERROR] is the engine refusing the command and
 * the message is what explains it.
 */
enum class BridgeError(val code: Int) {
    /** The handle is stale, forged, or already closed. */
    UNKNOWN_HANDLE(-1),

    /** An argument the bridge could not accept, such as a path it could not read. */
    INVALID_ARGUMENT(-2),

    /** A Rust panic, caught at the boundary rather than left to unwind into the JVM. */
    PANIC(-3),

    /** JNI itself failed, which usually means the JVM is out of memory. */
    JNI_FAILURE(-4),

    /**
     * The engine failed and named no number. Named MongoDB errors, `BadValue` and the like, arrive
     * this way with the name as the first word of the message; only `Location<n>` codes carry
     * digits the bridge can pass through as a MongoDB code.
     */
    ENGINE_ERROR(-5),
    ;

    companion object {
        private val byCode = entries.associateBy(BridgeError::code)

        fun of(code: Int): BridgeError? = byCode[code]
    }
}
