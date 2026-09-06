from __future__ import annotations

from typing import Any

from pymongo.errors import DocumentTooLarge, ProtocolError
from pymongo.hello import Hello
from pymongo.message import _OpMsg
from pymongo.monitoring import ConnectionClosedReason
from pymongo.pool_shared import _CancellationContext
from pymongo.synchronous.pool import Connection, Pool

from ._native import NativeClient
from .common import describe


class _Socket:
    def settimeout(self, timeout: float | None) -> None:
        pass


class _Interface:
    def __init__(self) -> None:
        self.get_conn = _Socket()

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
            raise DocumentTooLarge(
                f"BSON document too large ({max_doc_size} bytes); maximum is "
                f"{self.max_bson_size} bytes"
            )
        if self._pending is not None:
            raise ProtocolError("embedded connection already has a pending response")
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
                raise ProtocolError("embedded connection has no pending response")
            response_to, response = pending
            if request_id is not None and request_id != response_to:
                raise ProtocolError(
                    f"response id {response_to} does not match request id {request_id}"
                )
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
                self.is_writable = connection.is_writable
            if handler:
                handler.contribute_socket(connection, completed_handshake=False)
            connection.authenticate()
            if handler:
                handler.client._topology.receive_cluster_time(connection._cluster_time)
            return connection
        except BaseException:
            with self.lock:
                self.active_contexts.discard(temporary_context)
                if connection is not None:
                    self.active_contexts.discard(connection.cancel_context)
            if connection is None:
                self._telemetry.connection_closed(
                    connection_id, ConnectionClosedReason.ERROR
                )
            else:
                connection.close_conn(ConnectionClosedReason.ERROR)
            raise
