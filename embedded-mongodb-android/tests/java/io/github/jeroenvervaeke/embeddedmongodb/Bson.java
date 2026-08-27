package io.github.jeroenvervaeke.embeddedmongodb;

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;

/** Just enough BSON to build a command and read a number out of the reply. */
final class Bson {
    static byte[] document(byte[]... elements) {
        ByteArrayOutputStream body = new ByteArrayOutputStream();
        for (byte[] element : elements) {
            body.writeBytes(element);
        }
        byte[] bytes = body.toByteArray();
        ByteBuffer buffer = allocate(4 + bytes.length + 1);
        buffer.putInt(4 + bytes.length + 1);
        buffer.put(bytes);
        buffer.put((byte) 0);
        return buffer.array();
    }

    static byte[] int32(String name, int value) {
        return element((byte) 0x10, name, allocate(4).putInt(value).array());
    }

    static byte[] string(String name, String value) {
        byte[] utf8 = value.getBytes(StandardCharsets.UTF_8);
        ByteBuffer buffer = allocate(4 + utf8.length + 1);
        buffer.putInt(utf8.length + 1);
        buffer.put(utf8);
        buffer.put((byte) 0);
        return element((byte) 0x02, name, buffer.array());
    }

    static byte[] binary(String name, byte[] value) {
        ByteBuffer buffer = allocate(4 + 1 + value.length);
        buffer.putInt(value.length);
        buffer.put((byte) 0);
        buffer.put(value);
        return element((byte) 0x05, name, buffer.array());
    }

    static byte[] subDocument(String name, byte[] document) {
        return element((byte) 0x03, name, document);
    }

    static byte[] array(String name, byte[]... documents) {
        byte[][] elements = new byte[documents.length][];
        for (int index = 0; index < documents.length; index++) {
            elements[index] = subDocument(Integer.toString(index), documents[index]);
        }
        return element((byte) 0x04, name, document(elements));
    }

    /** Reads a top-level number, whatever numeric type it arrived as. */
    static double number(byte[] document, String name) {
        ByteBuffer buffer = ByteBuffer.wrap(document).order(ByteOrder.LITTLE_ENDIAN);
        int declared = buffer.getInt();
        if (declared != document.length) {
            throw new IllegalArgumentException(
                    "BSON says " + declared + " bytes but the reply is " + document.length);
        }
        while (true) {
            byte type = buffer.get();
            if (type == 0) {
                throw new IllegalArgumentException("no field named " + name + " in the reply");
            }
            String field = cstring(buffer);
            switch (type) {
                case 0x01:
                    double asDouble = buffer.getDouble();
                    if (field.equals(name)) {
                        return asDouble;
                    }
                    break;
                case 0x10:
                    int asInt = buffer.getInt();
                    if (field.equals(name)) {
                        return asInt;
                    }
                    break;
                case 0x12:
                    long asLong = buffer.getLong();
                    if (field.equals(name)) {
                        return asLong;
                    }
                    break;
                default:
                    if (field.equals(name)) {
                        throw new IllegalArgumentException(name + " is not a number");
                    }
                    skip(buffer, type);
            }
        }
    }

    private Bson() {}

    private static byte[] element(byte type, String name, byte[] value) {
        byte[] utf8 = name.getBytes(StandardCharsets.UTF_8);
        ByteBuffer buffer = allocate(1 + utf8.length + 1 + value.length);
        buffer.put(type);
        buffer.put(utf8);
        buffer.put((byte) 0);
        buffer.put(value);
        return buffer.array();
    }

    private static ByteBuffer allocate(int size) {
        return ByteBuffer.allocate(size).order(ByteOrder.LITTLE_ENDIAN);
    }

    private static String cstring(ByteBuffer buffer) {
        int start = buffer.position();
        while (buffer.get() != 0) {
            // The terminator moves the position past the name.
        }
        return new String(buffer.array(), start, buffer.position() - start - 1,
                StandardCharsets.UTF_8);
    }

    private static void skip(ByteBuffer buffer, byte type) {
        switch (type) {
            case 0x02:
            case 0x0D:
            case 0x0E:
                buffer.position(buffer.position() + 4 + buffer.getInt());
                break;
            case 0x03:
            case 0x04:
            case 0x0F:
                int length = buffer.getInt();
                buffer.position(buffer.position() + length - 4);
                break;
            case 0x05:
                int size = buffer.getInt();
                buffer.position(buffer.position() + 1 + size);
                break;
            case 0x06:
            case 0x0A:
            case (byte) 0x7F:
            case (byte) 0xFF:
                break;
            case 0x07:
                buffer.position(buffer.position() + 12);
                break;
            case 0x08:
                buffer.position(buffer.position() + 1);
                break;
            case 0x09:
            case 0x11:
                buffer.position(buffer.position() + 8);
                break;
            case 0x13:
                buffer.position(buffer.position() + 16);
                break;
            default:
                throw new IllegalArgumentException("unhandled BSON type 0x"
                        + Integer.toHexString(type & 0xFF));
        }
    }
}
