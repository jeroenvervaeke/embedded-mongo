package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.BsonDocumentReader
import org.bson.Document
import org.bson.codecs.DecoderContext
import org.bson.codecs.DocumentCodec
import org.bson.conversions.Bson

/**
 * [Bson] as the [Document] a command is assembled from.
 *
 * Every filter, sort, projection, update and index specification in this API is an
 * `org.bson.conversions.Bson` rather than a [Document]. `Document` implements it, so nothing is
 * lost by writing one; and it is what the official driver's `Filters`, `Sorts`, `Projections` and
 * `Updates` builders return, so an application that puts `org.mongodb:mongodb-driver-core` on its
 * classpath can write `Filters.eq("cat", "cafe")` at any of these call sites without this library
 * depending on the driver to make that work.
 *
 * A [Document] is handed back unchanged rather than copied. It already is what the command needs,
 * and a copy would make a command *equal* to the one the caller built but not the *same*
 * document — which an application that shows the query it ran would notice.
 *
 * Named `toDocument` rather than `asDocument` deliberately: `BsonValue.asDocument` is a member
 * answering with a `BsonDocument`, so an extension of that name would be silently shadowed for
 * every `BsonDocument` receiver and convert nothing.
 */
internal fun Bson.toDocument(): Document =
    this as? Document ?: DOCUMENT_CODEC.decode(BsonDocumentReader(toBsonDocument()), DECODING)

/** The stages of a pipeline, as the `pipeline` array of an `aggregate` takes them. */
internal fun List<Bson>.toDocuments(): List<Document> = map(Bson::toDocument)

/**
 * A number the engine reported, however wide it wrote it.
 *
 * BSON keeps the width a value was written with, and MongoDB is not consistent about which one a
 * count comes back as: `n` is an `int` from one command and a `long` from another, and a value
 * that came from an aggregation is whatever the pipeline computed. Reading through [Number] is
 * what keeps that from being a `ClassCastException` on the one reply that disagreed.
 */
internal fun Document.longOrNull(field: String): Long? = (this[field] as? Number)?.toLong()

/** @throws EmbeddedMongoException if [field] is missing or is not a number. */
internal fun Document.requiredLong(field: String): Long =
    longOrNull(field) ?: throw EmbeddedMongoException(
        "the reply carries no numeric `$field` (fields: ${keys.joinToString()})",
        NO_ERROR_CODE,
    )

// Codecs and contexts are immutable and hold no per-call state, so one of each is enough for
// however many threads end up converting at the same time.
private val DOCUMENT_CODEC = DocumentCodec()
private val DECODING = DecoderContext.builder().build()
