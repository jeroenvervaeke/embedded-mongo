package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document
import org.bson.conversions.Bson

/**
 * Index key specifications, which are the one part of MongoDB's document language that is easy to
 * get quietly wrong.
 *
 * `Document("loc", "2dsphere")` and `Document("loc", 1)` differ by a value rather than by a
 * keyword, and the second one builds an index `$geoNear` will not use. Naming them stops that
 * being a typo:
 *
 * ```
 * places.createIndex(Indexes.geo2dsphere("loc"))
 * places.createIndex(Indexes.compound(Indexes.ascending("brand"), Indexes.descending("rating")))
 * ```
 *
 * Filters, sorts, projections and updates deliberately have no builders here: [Document] already
 * spells them clearly, and an application that wants the official ones adds
 * `org.mongodb:mongodb-driver-core` and writes `Filters.eq(…)` wherever this API takes a [Bson].
 */
object Indexes {
    /** A forward index over [fields], which is also the order a `sort` of the same shape reads. */
    fun ascending(vararg fields: String): Bson = keys(fields, 1)

    fun descending(vararg fields: String): Bson = keys(fields, -1)

    /** The index `$geoNear` walks and `$geoWithin` selects from, over GeoJSON values. */
    fun geo2dsphere(vararg fields: String): Bson = keys(fields, "2dsphere")

    /**
     * The index `$text` reads. One collection may hold one text index, so a search over several
     * fields names them all here rather than building an index each.
     */
    fun text(vararg fields: String): Bson = keys(fields, "text")

    /** An index over a hash of [field], which spreads values that are near each other. */
    fun hashed(field: String): Bson = Document(field, "hashed")

    /**
     * The keys of [indexes], in order, as one index over all of them.
     *
     * Order is the whole meaning of a compound index: it can answer a query on a prefix of its
     * keys and not on a suffix.
     */
    fun compound(vararg indexes: Bson): Bson =
        Document().apply { indexes.forEach { putAll(it.toDocument()) } }

    private fun keys(fields: Array<out String>, value: Any): Bson {
        require(fields.isNotEmpty()) { "an index over no fields indexes nothing" }
        return Document().apply { fields.forEach { append(it, value) } }
    }
}
