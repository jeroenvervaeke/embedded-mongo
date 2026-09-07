from __future__ import annotations

from typing import Any

from pymongo.errors import DocumentTooLarge, ProtocolError
from pymongo.hello import Hello
from pymongo.message import _OpMsg
from pymongo.monitoring import ConnectionClosedReason
from pymongo.pool_shared import _CancellationContext
from pymongo.synchronous.pool import Connection, Pool

from ._native import NativeClient
from .common import (
    PENDING_ALREADY,
    PENDING_MISSING,
    Socket,
    describe,
    mismatched,
    too_large,
)


class _Interface:
    def __init__(self) -> None:
        self.get_conn = Socket()

    def close(self) -> None:
        pass


class EmbeddedConnection(Connection):
    def __init__(
        self,
        runtime: NativeClient,
        pool: Pool,
        address: tuple[str, int],
        connection_id: int,
        is_sdam: bool,
    ) -> None:
        super().__init__(_Interface(), pool, address, connection_id, is_sdam)
        self._runtime = runtime
        self._pending: tuple[int, _OpMsg] | None = None

    def _hello(self, topology_version: Any, heartbeat_frequency: Any) -> Hello:
        return describe(self)

    def send_message(self, message: bytes, max_doc_size: int) -> None:
        if max_doc_size > self.max_bson_size:
            raise DocumentTooLarge(too_large(max_doc_size, self.max_bson_size))
        if self._pending is not None:
            raise ProtocolError(PENDING_ALREADY)
        try:
            request_id, more_to_come, response = self._runtime.round_trip(message)
            if not more_to_come:
                self._pending = request_id, _OpMsg(0, response)
        except BaseException as error:
            self._raise_connection_failure(error)

    def receive_message(self, request_id: int | None) -> _OpMsg:
        try:
            pending, self._pending = self._pending, None
            if pending is None:
                raise ProtocolError(PENDING_MISSING)
            response_to, response = pending
            if request_id is not None and request_id != response_to:
                raise ProtocolError(mismatched(response_to, request_id))
            return response
        except BaseException as error:
            self._raise_connection_failure(error)

    def conn_closed(self) -> bool:
        return self.closed


class EmbeddedPool(Pool):
    def __init__(self, *args: Any, runtime: NativeClient, **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        self._runtime = runtime
        self._check_interval_seconds = None

    def connect(self, handler: Any = None) -> EmbeddedConnection:
        with self.lock:
            connection_id = self.next_connection_id
            self.next_connection_id += 1
            temporary_context = _CancellationContext()
            self.active_contexts.add(temporary_context)
        self._telemetry.connection_created(connection_id)

        connection = None
        # Whether the handshake got through, which is what decides below whether the failure
        # is one `_handle_connection_error` should be labelling. Tracked rather than inferred
        # for the same reason the base class tracks it: an `is_sdam` pool performs no
        # handshake, so "did it succeed" and "was there one" are different questions.
        completed_hello = False
        try:
            connection = EmbeddedConnection(
                self._runtime, self, self.address, connection_id, self.is_sdam
            )
            with self.lock:
                self.active_contexts.add(connection.cancel_context)
                self.active_contexts.discard(temporary_context)
            if temporary_context.cancelled:
                connection.cancel_context.cancel()
            if not self.is_sdam:
                connection.hello()
                completed_hello = True
                self.is_writable = connection.is_writable
            if handler:
                handler.contribute_socket(connection, completed_handshake=False)
            connection.authenticate()
        except BaseException as error:
            with self.lock:
                self.active_contexts.discard(temporary_context)
                if connection is not None:
                    self.active_contexts.discard(connection.cancel_context)
            if not completed_hello:
                self._handle_connection_error(error)
            if connection is None:
                self._telemetry.connection_closed(
                    connection_id, ConnectionClosedReason.ERROR
                )
            else:
                connection.close_conn(ConnectionClosedReason.ERROR)
            raise

        # Outside the `try`, as in the base class: a cluster time that failed to be gossiped
        # is not a connection that failed to be made, and treating it as one would close a
        # working connection and report it as a connection error.
        if handler:
            handler.client._topology.receive_cluster_time(connection._cluster_time)
        return connection
