package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document

/**
 * Returns [command] with the write concern that survives the process dying — unless it is not a
 * write, or the caller already named one.
 *
 * MongoDB acknowledges a write as soon as it is in memory, and Android ends processes without
 * warning. Measured against this engine, that combination loses the last few hundred acknowledged
 * writes, and an insert that implicitly created a collection can lose the collection along with
 * them. Journalling costs a few milliseconds per write and loses nothing, which is the right
 * default for a database whose process can disappear between two statements. A caller who wants
 * the faster, lossy behaviour asks for it by putting their own `writeConcern` in the command.
 */
internal fun durable(command: Document): Document {
    if (command.keys.firstOrNull() !in WRITE_COMMANDS || command.containsKey(WRITE_CONCERN)) {
        return command
    }
    // Copied rather than appended to: the caller's document belongs to the caller, and running the
    // same command twice should not mean two different things.
    return Document(command).append(WRITE_CONCERN, Document("w", 1).append("j", true))
}

private const val WRITE_CONCERN = "writeConcern"

/**
 * The commands that write. Named rather than derived, because `writeConcern` is not something
 * every command accepts: a read rejects it.
 */
private val WRITE_COMMANDS = setOf(
    "insert",
    "update",
    "delete",
    "findAndModify",
    "create",
    "createIndexes",
    "collMod",
    "drop",
    "dropDatabase",
    "dropIndexes",
    "renameCollection",
)
