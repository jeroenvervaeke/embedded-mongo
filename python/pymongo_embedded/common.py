"""What the synchronous and asynchronous surfaces both need.

Both of them answer the same URIs and describe the same engine, so the scheme parsing and the
handshake live here rather than in two copies that could drift apart.
"""

from __future__ import annotations

from urllib.parse import unquote

import pymongo
from pymongo.common import (
    MAX_BSON_SIZE,
    MAX_MESSAGE_SIZE,
    MAX_SUPPORTED_WIRE_VERSION,
    MAX_WRITE_BATCH_SIZE,
    MIN_SUPPORTED_WIRE_VERSION,
)
from pymongo.hello import Hello

if pymongo.version_tuple[:2] != (4, 18):
    raise ImportError("pymongo-embedded 0.1 requires PyMongo 4.18.x")

_SCHEMES = ("mongodb+embedded://", "mongodb_embedded://")

_HELLO = {
    "ok": 1.0,
    "ismaster": True,
    "isWritablePrimary": True,
    "minWireVersion": MIN_SUPPORTED_WIRE_VERSION,
    "maxWireVersion": MAX_SUPPORTED_WIRE_VERSION,
    "maxBsonObjectSize": MAX_BSON_SIZE,
    "maxMessageSizeBytes": MAX_MESSAGE_SIZE,
    "maxWriteBatchSize": MAX_WRITE_BATCH_SIZE,
}


def path_from_uri(uri: object) -> str | None:
    """The database directory an embedded URI names, or `None` if this is not an embedded URI.

    Answering `None` rather than raising is what lets one client class serve both kinds of
    address: anything this does not recognise is passed to PyMongo untouched.
    """
    if not isinstance(uri, str):
        return None
    for scheme in _SCHEMES:
        if uri.startswith(scheme):
            path = uri[len(scheme) :]
            if not path or "?" in path or "#" in path:
                raise ValueError("embedded MongoDB URI must contain only a database directory")
            return unquote(path)
    return None


def describe(connection: object) -> Hello:
    """Fills in what a handshake would have told `connection`, and answers the same `Hello`.

    There is no server to ask and no round trip to make: the engine is in this process, its
    answer is the same every time, and it supports none of what a handshake would negotiate. So
    the reply is a constant, and the fields a real `_hello` would set from the wire are set from
    it here.
    """
    hello = Hello(_HELLO)
    connection.performed_handshake = True
    connection.is_writable = hello.is_writable
    connection.max_wire_version = hello.max_wire_version
    connection.max_bson_size = hello.max_bson_size
    connection.max_message_size = hello.max_message_size
    connection.max_write_batch_size = hello.max_write_batch_size
    connection.supports_sessions = False
    connection.logical_session_timeout_minutes = None
    connection.hello_ok = False
    connection.is_repl = False
    connection.is_standalone = True
    connection.is_mongos = False
    connection.server_connection_id = 0
    return hello
