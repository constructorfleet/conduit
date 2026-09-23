"""Supervisor for stdio upstream MCP servers.

Each enabled stdio upstream is driven by one long-lived supervise task. The
task connects to the child through the `mcp` SDK's own stdio client transport
— `stdio_client` spawns the subprocess and bridges its pipes, and a
`ClientSession` owns request/response correlation and a cancellation-safe
shutdown. Instrumenta does not hand-roll JSON-RPC framing, request-id
bookkeeping, or a stdin write lock; the vendored transport already provides
all of it, the same abstraction `aggregator.py` uses for HTTP upstreams.

On a connection that fails or a child that dies, the task retries with capped
exponential backoff (1s → 2s → … → 30s, reset on success). Forwarding tools
are registered the first time a connection *succeeds*, not the first time one
is *attempted*: a stdio upstream whose very first connect loses a boot race
still has its tools registered once a later retry connects, without an
Instrumenta restart.

They are removed again when the child dies, and re-registered when it comes
back (User Story 24, #274). A tool that cannot be called should not be
advertised: leaving a dead child's tools in `tools/list` invites a model to
pick one and get "upstream is not currently connected" at call time, which
is a failure it cannot do anything useful with. The live client is still
swapped in place through a shared holder, so a forwarder registered from one
connection keeps working across the reconnect it survives.
"""

from __future__ import annotations

import asyncio
import logging
import shlex
from dataclasses import dataclass, field
from typing import Any, Callable

from mcp import types
from mcp.client import Client
from mcp.client.stdio import StdioServerParameters, stdio_client

from .backend import UpstreamServer

LOG = logging.getLogger("instrumenta.stdio")

_INITIAL_BACKOFF = 1.0
_MAX_BACKOFF = 30.0
# How often a held-open connection is probed for liveness so a crashed child
# becomes visible on `/upstreams` and triggers a reconnect, rather than only
# surfacing on the next tool call.
_LIVENESS_POLL = 5.0


class ClientHolder:
    """Mutable handle to the current live client for one upstream.

    A forwarding tool closes over the holder rather than a specific `Client`
    so a reconnect can swap the client in place. `client` is `None` while the
    upstream is down; a forward attempted then fails loud instead of calling
    a dead transport.
    """

    __slots__ = ("client",)

    def __init__(self) -> None:
        self.client: Client | None = None


# Callback that registers forwarding tools for a freshly connected upstream.
# Invoked on each connection that succeeds while the upstream is unregistered,
# with the holder the forwarders should read their live client from.
RegisterTools = Callable[[UpstreamServer, list[types.Tool], ClientHolder], None]

# Callback that removes those tools again when the child goes away. Receives
# the tools as listed at registration time, so an implementation can derive
# the same prefixed names it added.
UnregisterTools = Callable[[UpstreamServer, list[types.Tool]], None]


@dataclass
class _Child:
    """Supervised state for one stdio upstream."""

    server: UpstreamServer
    holder: ClientHolder = field(default_factory=ClientHolder)
    reachable: bool = False
    registered: bool = False
    #: Tools as listed when they were registered, kept so they can be removed
    #: again without asking a child that is no longer answering.
    tools: list[types.Tool] = field(default_factory=list)
    tool_count: int = 0
    last_error: str | None = None
    backoff: float = _INITIAL_BACKOFF
    stop_event: asyncio.Event = field(default_factory=asyncio.Event)
    ready: asyncio.Event = field(default_factory=asyncio.Event)
    task: asyncio.Task[None] | None = None


class StdioSupervisor:
    """Owns the supervise tasks for stdio upstreams.

    Construct with the callback that registers forwarding tools on the local
    MCP server, `add()` each enabled stdio upstream, then `start()` to launch
    supervision. `statuses()` feeds `/upstreams`; `close()` tears every child
    down cleanly.
    """

    def __init__(
        self,
        register_tools: RegisterTools | None = None,
        *,
        unregister_tools: UnregisterTools | None = None,
        initial_backoff: float = _INITIAL_BACKOFF,
        max_backoff: float = _MAX_BACKOFF,
        liveness_poll: float = _LIVENESS_POLL,
    ) -> None:
        self._register_tools = register_tools
        self._unregister_tools = unregister_tools
        self._initial_backoff = initial_backoff
        self._max_backoff = max_backoff
        self._liveness_poll = liveness_poll
        self._children: dict[str, _Child] = {}

    def add(self, server: UpstreamServer) -> None:
        """Register a stdio upstream to be supervised once `start()` runs."""
        child = _Child(server=server, backoff=self._initial_backoff)
        self._children[server.id] = child

    async def start(self, first_attempt_timeout: float = 5.0) -> None:
        """Launch one supervise task per child.

        Waits until each child has settled its first connection attempt (up to
        `first_attempt_timeout`) so tools registered at boot are present in the
        first `tools/list`. A child still connecting after the timeout keeps
        going in the background and its tools appear when it connects.
        """
        for child in self._children.values():
            child.task = asyncio.ensure_future(self._supervise(child))

        if not self._children:
            return
        try:
            await asyncio.wait_for(
                asyncio.gather(*(c.ready.wait() for c in self._children.values())),
                timeout=first_attempt_timeout,
            )
        except asyncio.TimeoutError:
            LOG.warning("some stdio upstreams had not connected within boot window")

    async def _supervise(self, child: _Child) -> None:
        name = child.server.name
        parts = shlex.split(child.server.command or "")
        if not parts:
            child.last_error = "empty command"
            child.ready.set()
            return
        params = StdioServerParameters(command=parts[0], args=parts[1:])

        while not child.stop_event.is_set():
            try:
                client = Client(stdio_client(params), raise_exceptions=True)
                async with client:
                    listed = await client.list_tools()
                    child.holder.client = client
                    child.reachable = True
                    child.last_error = None
                    child.tool_count = len(listed.tools)
                    child.backoff = self._initial_backoff
                    if not child.registered:
                        if self._register_tools is not None:
                            self._register_tools(child.server, listed.tools, child.holder)
                        child.tools = list(listed.tools)
                        child.registered = True
                        LOG.info(
                            "stdio upstream %s connected; %d tool(s) registered",
                            name,
                            child.tool_count,
                        )
                    child.ready.set()
                    await self._hold_until_stop_or_crash(child, client)
            except asyncio.CancelledError:
                raise
            except Exception as exc:  # noqa: BLE001 — any connect/child failure retries
                child.last_error = str(exc)
                LOG.warning("stdio upstream %s connection failed: %s", name, exc)
            finally:
                child.reachable = False
                child.holder.client = None
                # The child is gone, so its tools cannot be called: take them
                # off the surface until a reconnect puts them back. Doing this
                # here rather than only on a clean stop covers the crash case,
                # which is the one that matters.
                self._drop_tools(child)
                # A failed first attempt must still release boot; recovery is
                # what re-registers the tools, not the initial attempt.
                child.ready.set()

            if child.stop_event.is_set():
                break
            await self._sleep_backoff(child)

    def _drop_tools(self, child: _Child) -> None:
        """Remove a child's forwarding tools; a no-op if none are registered.

        Tool count is left alone: `/upstreams` reports what the upstream had
        when it was last reachable, which is more useful to an operator
        reading a down row than a zero.
        """
        if not child.registered:
            return
        child.registered = False
        tools, child.tools = child.tools, []
        if self._unregister_tools is None:
            return
        try:
            self._unregister_tools(child.server, tools)
        except Exception as exc:  # noqa: BLE001 — a failed removal must not stop supervision
            LOG.warning(
                "could not unregister tools for stdio upstream %s: %s",
                child.server.name,
                exc,
            )

    async def _hold_until_stop_or_crash(self, child: _Child, client: Client) -> None:
        """Keep the connection open until stop is requested or the child dies.

        Probes liveness on an interval with an ordinary `list_tools` so a
        crashed child surfaces on `/upstreams` and triggers a reconnect
        instead of lying reachable until the next forwarded call.
        """
        while not child.stop_event.is_set():
            try:
                await asyncio.wait_for(child.stop_event.wait(), timeout=self._liveness_poll)
                return
            except asyncio.TimeoutError:
                # Raises if the child has died; propagates to the reconnect loop.
                await client.list_tools()

    async def _sleep_backoff(self, child: _Child) -> None:
        delay = child.backoff
        try:
            await asyncio.wait_for(child.stop_event.wait(), timeout=delay)
        except asyncio.TimeoutError:
            pass
        child.backoff = min(child.backoff * 2, self._max_backoff)

    def statuses(self) -> list[dict[str, Any]]:
        """Per-upstream snapshot for `/upstreams`, one row per stdio child."""
        rows = []
        for child in self._children.values():
            rows.append(
                {
                    "id": child.server.id,
                    "name": child.server.name,
                    "url": None,
                    "enabled": child.server.enabled,
                    "reachable": child.reachable,
                    "tool_count": child.tool_count,
                    "last_error": child.last_error,
                }
            )
        return rows

    async def close(self) -> None:
        """Stop every supervise task and let each exit its client context."""
        for child in self._children.values():
            child.stop_event.set()
        tasks = [c.task for c in self._children.values() if c.task is not None]
        for task in tasks:
            task.cancel()
        for task in tasks:
            try:
                await task
            except (asyncio.CancelledError, Exception):  # noqa: BLE001
                pass
