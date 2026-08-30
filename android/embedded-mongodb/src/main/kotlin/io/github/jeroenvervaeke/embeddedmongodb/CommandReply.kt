package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document

/**
 * Returns [reply] when the command succeeded and throws [EmbeddedMongoException] when it did not.
 *
 * A failed MongoDB command is an ordinary reply carrying `ok: 0`, so without this check a caller
 * would read a failure as an empty result. Write errors and write concern errors are raised the
 * same way, matching the Rust API: an insert that stored nothing still answers `ok: 1`, with the
 * reason buried in `writeErrors`.
 *
 * The whole reply travels on the exception. One `writeErrors` entry gives the message and the
 * code, but a batch has as many of them as it had bad documents, and `n` says how many went in
 * regardless — an unordered insert of five hundred that rejected one is not the same event as one
 * that rejected all five hundred, and only the reply can tell them apart.
 */
internal fun checkedReply(reply: Document): Document {
    val ok = (reply["ok"] as? Number)?.toDouble()
        ?: throw EmbeddedMongoException(
            "MongoDB reply carries no ok field (fields: ${reply.keys.joinToString()})",
            NO_ERROR_CODE,
        )
    val details = failureDetails(reply)
    if (ok != 0.0 && details == null) return reply
    val failure = details ?: reply
    throw EmbeddedMongoException(
        failure["errmsg"] as? String ?: "the MongoDB command failed without an error message",
        (failure["code"] as? Number)?.toInt() ?: NO_ERROR_CODE,
        reply,
    )
}

/**
 * The sub-document describing what went wrong, when the failure is a per-write one. An empty
 * `writeErrors` array is what a successful bulk write carries, so only a populated one counts.
 */
private fun failureDetails(reply: Document): Document? {
    val writeErrors = reply["writeErrors"] as? List<*>
    return writeErrors?.filterIsInstance<Document>()?.firstOrNull()
        ?: reply["writeConcernError"] as? Document
}
