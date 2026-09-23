"""Supervision for upstream MCP servers, stdio and HTTP alike.

Each enabled upstream is driven by one long-lived supervise task. The task
opens a client through the `mcp` SDK — the stdio client transport spawns a
child and bridges its pipes, the streamable-HTTP client talks to a URL — then
lists the upstream's tools, registers forwarders for them, and holds the
connection open. Instrumenta does not hand-roll JSON-RPC framing, request-id
bookkeeping, or a stdin write lock; the vendored transports already provide
all of it.

The transport is the only difference between the two, so it is the only thing
injected: `connect` builds the client for one upstream and everything else —
backoff, liveness, registration, status — is shared. Two supervisors driving
the same loop is how the stdio path and the HTTP path drift apart.

On a connection that fails or an upstream that dies, the task retries with
capped exponential backoff (1s → 2s → … → 30s, reset on success). Forwarding
tools are registered the first time a connection *succeeds*, not the first
time one is *attempted*: an upstream whose very first connect loses a boot
race still has its tools registered once a later retry connects, without an
Instrumenta restart.

They are removed again when the upstream goes away, and re-registered when it
comes back (User Story 24, #274 for stdio and #252 for HTTP). A tool that
cannot be called should not be advertised: leaving a dead upstream's tools in
`tools/list` invites a model to pick one and get "upstream is not currently
connected" at call time, which is a failure it cannot do anything useful
with.

The liveness poll doubles as a refresh (#282). Its `list_tools` result was
previously discarded, so an upstream that added, removed or renamed a tool
while staying reachable kept advertising its attach-time set until it
detached or Instrumenta restarted. When the set changes, the forwarders are
replaced wholesale rather than diffed: the surface is small, replacement is
idempotent, and a partial diff is a second way to be wrong about what is
registered.
"""

from __future__ import annotations

import asyncio
import logging
import shlex
from contextlib import AsyncExitStack
from dataclasses import dataclass, field
from typing import Any, Awaitable, Callable

from mcp import types
from mcp.client import Client
from mcp.client.stdio import StdioServerParameters, stdio_client

from .backend import UpstreamServer

LOG = logging.getLogger("instrumenta.supervisor")

_INITIAL_BACKOFF = 1.0
_MAX_BACKOFF = 30.0
# How often a held-open connection is probed for liveness so a crashed child
# becomes visible on `/upstreams` and triggers a reconnect, rather than only
# surfacing on the next tool call.
_LIVENESS_POLL = 5.0
# Bound on connecting and first-listing one upstream, when the row carries
# no `timeout_seconds` of its own.
_CONNECT_TIMEOUT = 30.0
# Bound on closing a client that has already failed.
_SHUTDOWN_TIMEOUT = 5.0


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

# Builds an unentered `Client` for one upstream. The supervise loop owns the
# `async with`, so a factory that raises is an ordinary connection failure and
# retries like any other.
Connect = Callable[[UpstreamServer], Client]

# Called after each successful connect, once the tool surface is in step, with
# the live client. Prompts and resources ride on this: they are not part of
# the tool surface the supervisor owns, but they come from the same session
# and want refreshing on the same edge.
OnConnected = Callable[[UpstreamServer, Client], Awaitable[None]]


def stdio_connect(server: UpstreamServer) -> Client:
    """Build a client that spawns `server.command` and speaks stdio to it.

    Raises:
        ValueError: if the row carries no command. The loop records it as the
            connection error, which is what an operator needs to see; there is
            nothing to retry against, but a row that gains a command later
            recovers without a restart.
    """
    parts = shlex.split(server.command or "")
    if not parts:
        raise ValueError("empty command")
    return Client(
        stdio_client(StdioServerParameters(command=parts[0], args=parts[1:])),
        raise_exceptions=True,
    )


def http_connect(server: UpstreamServer) -> Client:
    """Build a streamable-HTTP client for `server.url`.

    The row's `timeout_seconds` becomes the client's read timeout, so a
    hanging upstream is bounded the same way it is at first attach (#276).
    """
    if not server.url:
        raise ValueError("no url")
    return Client(
        server.url,
        raise_exceptions=True,
        read_timeout_seconds=(
            None if server.timeout_seconds is None else float(server.timeout_seconds)
        ),
    )


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


def _same_tools(before: list[types.Tool], after: list[types.Tool]) -> bool:
    """Whether two listings describe the same surface.

    Name, description and input schema, because all three reach the model: a
    tool whose description or arguments changed is a different tool to
    whatever is choosing it, even under the same name.
    """

    def shape(tool: types.Tool) -> tuple[str, str | None, str]:
        schema = getattr(tool, "input_schema", None)
        return (tool.name, tool.description, repr(schema))

    return [shape(t) for t in before] == [shape(t) for t in after]


class UpstreamSupervisor:
    """Owns the supervise tasks for one transport's upstreams.

    Construct with the transport's `connect` factory and the callbacks that
    register and remove forwarding tools on the local MCP server, `add()` each
    enabled upstream, then `start()` to launch supervision. `statuses()` feeds
    `/upstreams`; `close()` tears every child down cleanly.
    """

    def __init__(
        self,
        register_tools: RegisterTools | None = None,
        *,
        connect: Connect = stdio_connect,
        unregister_tools: UnregisterTools | None = None,
        on_connected: OnConnected | None = None,
        initial_backoff: float = _INITIAL_BACKOFF,
        max_backoff: float = _MAX_BACKOFF,
        liveness_poll: float = _LIVENESS_POLL,
        connect_timeout: float = _CONNECT_TIMEOUT,
    ) -> None:
        self._register_tools = register_tools
        self._connect = connect
        self._unregister_tools = unregister_tools
        self._on_connected = on_connected
        self._initial_backoff = initial_backoff
        self._max_backoff = max_backoff
        self._liveness_poll = liveness_poll
        self._connect_timeout = connect_timeout
        self._children: dict[str, _Child] = {}

    def add(self, server: UpstreamServer) -> None:
        """Register an upstream to be supervised once `start()` runs."""
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
            LOG.warning("some upstreams had not connected within boot window")

    async def _supervise(self, child: _Child) -> None:
        name = child.server.name

        # An upstream that accepts a connection and then never answers must
        # not hold the task open forever, and `start()` waits on the first
        # attempt, so an unbounded connect would hold up boot too (#276).
        budget = float(child.server.timeout_seconds or self._connect_timeout)

        while not child.stop_event.is_set():
            # The stack, rather than `async with client`, so the bound covers
            # entering the client as well as the first listing: a transport
            # can hang in either.
            stack = AsyncExitStack()
            try:
                client = self._connect(child.server)
                async with asyncio.timeout(budget):
                    await stack.enter_async_context(client)
                    listed = await client.list_tools()
                child.holder.client = client
                child.reachable = True
                child.last_error = None
                child.backoff = self._initial_backoff
                self._sync_tools(child, listed.tools)
                if self._on_connected is not None:
                    await self._on_connected(child.server, client)
                child.ready.set()
                await self._hold_until_stop_or_crash(child, client)
            except asyncio.CancelledError:
                raise
            except TimeoutError:
                child.last_error = f"timed out after {budget:g}s"
                LOG.warning("upstream %s did not answer within %ss", name, budget)
            except Exception as exc:  # noqa: BLE001 — any connect/upstream failure retries
                child.last_error = str(exc)
                LOG.warning("upstream %s connection failed: %s", name, exc)
            finally:
                child.reachable = False
                child.holder.client = None
                # Closing is its own bound: a transport that hung on connect
                # is a fair bet to hang on close, and supervision has to keep
                # going either way.
                try:
                    async with asyncio.timeout(_SHUTDOWN_TIMEOUT):
                        await stack.aclose()
                except Exception as exc:  # noqa: BLE001 — teardown never masks the cause
                    LOG.debug("closing upstream %s raised: %s", name, exc)
                # The upstream is gone, so its tools cannot be called: take
                # them off the surface until a reconnect puts them back. Doing
                # this here rather than only on a clean stop covers the crash
                # case, which is the one that matters.
                self._drop_tools(child)
                # A failed first attempt must still release boot; recovery is
                # what re-registers the tools, not the initial attempt.
                child.ready.set()

            if child.stop_event.is_set():
                break
            await self._sleep_backoff(child)

    def _sync_tools(self, child: _Child, tools: list[types.Tool]) -> None:
        """Make the registered surface match what the upstream just listed.

        Called on every connect and on every liveness poll. Nothing happens
        while the tool set is unchanged, which is the overwhelmingly common
        case; when it does change, the forwarders are replaced rather than
        diffed. Replacement is idempotent and has one failure mode, where a
        diff has several -- and the surface is a handful of names, so there is
        nothing to gain by being clever about it.
        """
        child.tool_count = len(tools)
        if child.registered and _same_tools(child.tools, tools):
            return

        replacing = child.registered
        if replacing:
            self._drop_tools(child)

        if self._register_tools is not None:
            self._register_tools(child.server, tools, child.holder)
        child.tools = list(tools)
        child.registered = True
        LOG.info(
            "upstream %s %s; %d tool(s) registered",
            child.server.name,
            "changed its tools" if replacing else "connected",
            child.tool_count,
        )

    def _drop_tools(self, child: _Child) -> None:
        """Remove an upstream's forwarding tools; a no-op if none are registered.

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
        crashed upstream surfaces on `/upstreams` and triggers a reconnect
        instead of lying reachable until the next forwarded call, and keeps
        the registered surface in step with what the upstream now offers.
        """
        while not child.stop_event.is_set():
            try:
                await asyncio.wait_for(child.stop_event.wait(), timeout=self._liveness_poll)
                return
            except asyncio.TimeoutError:
                # Raises if the upstream has died; propagates to the reconnect
                # loop. The result is not discarded: an upstream that changed
                # its tools while staying reachable is exactly what this poll
                # is in a position to notice (#282).
                listed = await client.list_tools()
                self._sync_tools(child, listed.tools)

    async def _sleep_backoff(self, child: _Child) -> None:
        delay = child.backoff
        try:
            await asyncio.wait_for(child.stop_event.wait(), timeout=delay)
        except asyncio.TimeoutError:
            pass
        child.backoff = min(child.backoff * 2, self._max_backoff)

    def client_for(self, server_id: str) -> Client | None:
        """The live client for one upstream, or None while it is down."""
        child = self._children.get(server_id)
        return child.holder.client if child is not None else None

    def statuses(self) -> list[dict[str, Any]]:
        """Per-upstream snapshot for `/upstreams`, one row per child."""
        rows = []
        for child in self._children.values():
            rows.append(
                {
                    "id": child.server.id,
                    "name": child.server.name,
                    "url": child.server.url,
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
