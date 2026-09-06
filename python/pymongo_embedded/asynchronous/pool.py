from __future__ import annotations

from typing import Any

from pymongo.asynchronous.pool import AsyncConnection, Pool
from pymongo.errors import DocumentTooLarge, ProtocolError
from pymongo.hello import Hello
from pymongo.message import _OpMsg
from pymongo.monitoring import ConnectionClosedReason
from pymongo.pool_shared import _CancellationContext

from ..common import describe

from .._native import AsyncNativeClient


class _Socket:
    def settimeout(self, timeout: float | None) -> None:
        pass


class _Interface:
    """What `AsyncConnection` expects a socket to be, for a connection that has none.

    `close` is a coroutine because the base class awaits it, and `is_closing` answers False
    because there is no socket to have been closed underneath us -- the connection's own
    `closed` flag is the whole truth, which is what `conn_closed` below returns.
    """

    def __init__(self) -> None:
        self.get_conn = _Socket()

    async def close(self) -> None:
        pass

    def is_closing(self) -> bool:
        return False


class AsyncEmbeddedConnection(AsyncConnection):
    def __init__(
        self,
        runtime: AsyncNativeClient,
        pool: Pool,
        address: tuple[str, int],
        connection_id: int,
        is_sdam: bool,
    ) -> None:
        super().__init__(_Interface(), pool, address, connection_id, is_sdam)
        self._runtime = runtime
        self._pending: tuple[int, _OpMsg] | None = None

    async def _hello(self, topology_version: Any, heartbeat_frequency: Any) -> Hello:
        return describe(self)

    async def send_message(self, message: bytes, max_doc_size: int) -> None:
        """Runs the command, rather than sending it.

        There is no socket and so no send that could be separated from a receive: the reply is
        already in hand when this returns, and `receive_message` hands over what was kept here.
        Awaiting it is what makes the whole thing worth building -- the event loop is free for
        the length of the command, and cancelling the task stops the engine rather than merely
        stopping the wait.
        """
        if max_doc_size > self.max_bson_size:
            raise DocumentTooLarge(
                f"BSON document too large ({max_doc_size} bytes); maximum is "
                f"{self.max_bson_size} bytes"
            )
        if self._pending is not None:
            raise ProtocolError("embedded connection already has a pending response")
        try:
            request_id, more_to_come, response = await self._runtime.round_trip(message)
            if not more_to_come:
                self._pending = request_id, _OpMsg(0, response)
        except BaseException as error:
            await self._raise_connection_failure(error)

    async def receive_message(self, request_id: int | None) -> _OpMsg:
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
            await self._raise_connection_failure(error)

    def conn_closed(self) -> bool:
        return self.closed


class AsyncEmbeddedPool(Pool):
    def __init__(self, *args: Any, runtime: AsyncNativeClient, **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        self._runtime = runtime
        self._check_interval_seconds = None

    async def connect(self, handler: Any = None) -> AsyncEmbeddedConnection:
        async with self.lock:
            connection_id = self.next_connection_id
            self.next_connection_id += 1
            temporary_context = _CancellationContext()
            self.active_contexts.add(temporary_context)
        self._telemetry.connection_created(connection_id)

        connection = None
        try:
            connection = AsyncEmbeddedConnection(
                self._runtime, self, self.address, connection_id, self.is_sdam
            )
            async with self.lock:
                self.active_contexts.add(connection.cancel_context)
                self.active_contexts.discard(temporary_context)
            if temporary_context.cancelled:
                connection.cancel_context.cancel()
            if not self.is_sdam:
                await connection.hello()
                self.is_writable = connection.is_writable
            if handler:
                handler.contribute_socket(connection, completed_handshake=False)
            await connection.authenticate()
            if handler:
                await handler.client._topology.receive_cluster_time(connection._cluster_time)
            return connection
        except BaseException:
            async with self.lock:
                self.active_contexts.discard(temporary_context)
                if connection is not None:
                    self.active_contexts.discard(connection.cancel_context)
            if connection is None:
                self._telemetry.connection_closed(
                    connection_id, ConnectionClosedReason.ERROR
                )
            else:
                await connection.close_conn(ConnectionClosedReason.ERROR)
            raise
