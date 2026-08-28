package io.github.jeroenvervaeke.embeddedmongodb

/**
 * The code carried by a failure this library raised itself, rather than one the engine reported.
 * Neither MongoDB nor the bridge ever uses it: MongoDB codes are positive and the bridge's own are
 * the negative ones in [BridgeError].
 */
internal const val NO_ERROR_CODE = 0

/**
 * A failure reported by the engine: a command that answered `ok: 0`, a write that failed, an error
 * raised inside the native bridge, or a reply this library could not make sense of.
 *
 * [code] tells the three apart — a positive MongoDB error code, one of the negative [BridgeError]
 * values, or [NO_ERROR_CODE] when the failure was raised on this side of the bridge. The native
 * bridge constructs this class by name through JNI with the `(String, int)` constructor, so
 * `consumer-rules.pro` keeps R8 from renaming either.
 */
class EmbeddedMongoException(message: String, val code: Int) : Exception(message) {
    /** The bridge failure [code] names, or `null` when the code came from MongoDB or from here. */
    val bridgeError: BridgeError? get() = BridgeError.of(code)
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
