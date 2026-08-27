package io.github.jeroenvervaeke.embeddedmongodb

import java.nio.ByteBuffer
import org.bson.BSONException
import org.bson.BsonBinaryReader
import org.bson.BsonBinaryWriter
import org.bson.Document
import org.bson.codecs.DecoderContext
import org.bson.codecs.DocumentCodec
import org.bson.codecs.EncoderContext
import org.bson.io.BasicOutputBuffer

/**
 * Translates between [Document] and the BSON bytes that cross the JNI bridge.
 *
 * The BSON library's own codecs do the work: hand-rolled encoding would have to keep up with
 * every BSON type the engine can return, and get the subtypes of binary, decimal128 and the
 * legacy UUID representations right while doing so.
 */
internal object BsonCodec {
    fun encode(document: Document): ByteArray = BasicOutputBuffer().use { buffer ->
        BsonBinaryWriter(buffer).use { writer -> codec.encode(writer, document, encoderContext) }
        buffer.toByteArray()
    }

    /**
     * Reads a reply the engine produced.
     *
     * Bytes that are not a BSON document mean a broken bridge rather than a mistake the caller
     * made, so the BSON library's exception is translated into the one the rest of this API
     * throws, keeping the original as its cause.
     */
    fun decode(bytes: ByteArray): Document = try {
        BsonBinaryReader(ByteBuffer.wrap(bytes)).use { reader -> codec.decode(reader, decoderContext) }
    } catch (error: BSONException) {
        throw EmbeddedMongoException(
            "the engine returned ${bytes.size} bytes that are not a BSON document",
            NO_ERROR_CODE,
        ).apply { initCause(error) }
    }

    // Codecs and contexts are immutable and hold no per-call state, so one of each is enough for
    // however many threads end up encoding at the same time.
    private val codec = DocumentCodec()
    private val encoderContext = EncoderContext.builder().build()
    private val decoderContext = DecoderContext.builder().build()
}
