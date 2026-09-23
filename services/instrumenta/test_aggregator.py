"""Tests for the HTTP upstream aggregator.

The aggregator connects to enabled HTTP upstreams at boot, re-registers
their tools on the local MCP server under a `<server_name>.<tool>` prefix,
and populates per-upstream reachability for `/upstreams`.

These tests substitute an in-memory MCP client factory (backed by a fake
`MCPServer`) so we can exercise the full aggregation pipeline without a
real HTTP hop — the wire is a well-tested SDK boundary; what needs testing
is the aggregation logic on top of it.
"""

from __future__ import annotations

import uuid
from pathlib import Path

import pytest
from cryptography.fernet import Fernet
from mcp.client import Client
from mcp.client._memory import InMemoryTransport
from mcp.server.mcpserver import MCPServer

import asyncio
import inspect

from instrumenta.aggregator import Aggregator, build_forwarder
from instrumenta.backend import SqliteBackend, UpstreamServer
from instrumenta.mcp_app import build_mcp_server
from instrumenta.supervisor import http_connect
from instrumenta.secret_box import SecretBox


def _make_upstream_server() -> MCPServer:
    """Fake upstream exposing one tool the aggregator should re-register."""
    server = MCPServer(name="fake-upstream", version="0.0.0")

    def echo(message: str) -> dict[str, str]:
        return {"echoed": message}

    server.add_tool(echo, name="echo", description="Echo the input message.")
    return server


def _in_memory_client_factory(upstream: MCPServer):
    def factory(server: UpstreamServer) -> Client:
        # The transport is created per call so each upstream row gets its own
        # session; here every row points at the same fake for simplicity.
        return Client(InMemoryTransport(upstream), raise_exceptions=True)

    return factory


@pytest.fixture
def secret_box() -> SecretBox:
    return SecretBox(Fernet.generate_key().decode())


@pytest.fixture
def backend(tmp_path: Path) -> SqliteBackend:
    return SqliteBackend(tmp_path / "instrumenta.db")


@pytest.mark.asyncio
async def test_start_with_no_servers_populates_no_statuses(
    backend: SqliteBackend, secret_box: SecretBox
) -> None:
    aggregator = Aggregator(backend, secret_box)
    mcp_server = build_mcp_server()
    await aggregator.start(mcp_server)
    try:
        assert aggregator.statuses() == []
    finally:
        await aggregator.close()


@pytest.mark.asyncio
async def test_start_registers_enabled_upstream_tool_under_prefix(
    backend: SqliteBackend, secret_box: SecretBox
) -> None:
    backend.insert_upstream_server(
        UpstreamServer(
            id=str(uuid.uuid4()),
            name="fake",
            transport="http",
            url="http://placeholder.invalid",  # unused: factory bypasses it
            command=None,
            secret_ciphertext=None,
            enabled=True,
            timeout_seconds=None,
        )
    )
    upstream = _make_upstream_server()
    aggregator = Aggregator(
        backend, secret_box, client_factory=_in_memory_client_factory(upstream)
    )
    mcp_server = build_mcp_server()
    await aggregator.start(mcp_server)
    try:
        # Prefixed tool is now registered on our server.
        tool_names = {tool.name for tool in await mcp_server.list_tools()}
        assert "fake.echo" in tool_names
        # And a reachable status is reported.
        [status] = aggregator.statuses()
        assert status.reachable is True
        assert status.tool_count == 1
        assert status.last_error is None
    finally:
        await aggregator.close()


@pytest.mark.asyncio
async def test_disabled_upstream_is_not_probed_but_appears_in_statuses(
    backend: SqliteBackend, secret_box: SecretBox
) -> None:
    backend.insert_upstream_server(
        UpstreamServer(
            id=str(uuid.uuid4()),
            name="off",
            transport="http",
            url="http://placeholder.invalid",
            command=None,
            secret_ciphertext=None,
            enabled=False,
            timeout_seconds=None,
        )
    )

    def factory(_server: UpstreamServer) -> Client:
        raise AssertionError("disabled upstream must not be probed")

    aggregator = Aggregator(backend, secret_box, client_factory=factory)
    await aggregator.start(build_mcp_server())
    try:
        [status] = aggregator.statuses()
        assert status.enabled is False
        assert status.reachable is False
    finally:
        await aggregator.close()


@pytest.mark.asyncio
async def test_unreachable_upstream_records_last_error(
    backend: SqliteBackend, secret_box: SecretBox
) -> None:
    backend.insert_upstream_server(
        UpstreamServer(
            id=str(uuid.uuid4()),
            name="broken",
            transport="http",
            url="http://placeholder.invalid",
            command=None,
            secret_ciphertext=None,
            enabled=True,
            timeout_seconds=None,
        )
    )

    def failing_factory(_server: UpstreamServer) -> Client:
        raise RuntimeError("simulated unreachable")

    aggregator = Aggregator(backend, secret_box, client_factory=failing_factory)
    await aggregator.start(build_mcp_server())
    try:
        [status] = aggregator.statuses()
        assert status.reachable is False
        assert status.enabled is True
        assert "simulated unreachable" in (status.last_error or "")
    finally:
        await aggregator.close()


@pytest.mark.asyncio
async def test_forwarded_tool_call_passes_arguments_through(
    backend: SqliteBackend, secret_box: SecretBox
) -> None:
    """A forwarded call with real arguments reaches the upstream.

    Guards the schema-driven forwarder: a `forward(**kwargs)` whose signature
    was left generic advertises a single opaque `kwargs` field and rejects any
    real call at validation time. The forwarder must instead mirror the
    upstream tool's parameters.
    """
    backend.insert_upstream_server(
        UpstreamServer(
            id=str(uuid.uuid4()),
            name="fake",
            transport="http",
            url="http://placeholder.invalid",
            command=None,
            secret_ciphertext=None,
            enabled=True,
            timeout_seconds=None,
        )
    )
    upstream = _make_upstream_server()
    aggregator = Aggregator(
        backend, secret_box, client_factory=_in_memory_client_factory(upstream)
    )
    mcp_server = build_mcp_server()
    await aggregator.start(mcp_server)
    try:
        # The forwarded tool advertises the upstream's real parameter, not a
        # `kwargs` bag.
        [tool] = [t for t in await mcp_server.list_tools() if t.name == "fake.echo"]
        assert "message" in (tool.input_schema.get("properties") or {})

        client = Client(InMemoryTransport(mcp_server), raise_exceptions=True)
        async with client:
            result = await client.call_tool("fake.echo", {"message": "hi"})
        assert "hi" in result.content[0].text
    finally:
        await aggregator.close()


def test_forwarder_maps_non_identifier_names_collision_safely() -> None:
    """Finding 5: a generated alias for a non-identifier property must never
    collide with a different property (real or aliased) and silently drop it.
    """
    calls: dict[str, object] = {}

    class _FakeClient:
        async def call_tool(self, name, args):
            calls["name"] = name
            calls["args"] = args

            class _R:
                content: list = []

            return _R()

    # `weird_name` is a valid identifier and is kept; `weird-name` is not and
    # would derive the same alias — the collision path must bump it.
    schema = {
        "type": "object",
        "properties": {
            "weird_name": {"type": "string"},
            "weird-name": {"type": "string"},
        },
        "required": ["weird_name"],
    }
    forward = build_forwarder(lambda: _FakeClient(), "t", "srv", schema)
    param_names = list(inspect.signature(forward).parameters)

    # Two distinct parameters survive; neither was dropped by a collision.
    assert len(param_names) == 2
    assert "weird_name" in param_names

    # Every parameter maps back to its real upstream name on forward.
    asyncio.run(forward(**{p: "v" for p in param_names}))
    assert set(calls["args"].keys()) == {"weird_name", "weird-name"}


# ── Upstream timeouts (#276) ────────────────────────────────────────────


def test_http_connect_passes_the_configured_timeout() -> None:
    """A configured `timeout_seconds` must reach the MCP client.

    The row carried the value and the factory dropped it, so a hanging (as
    opposed to refusing) upstream stalled every read with no bound at all.
    """
    server = UpstreamServer(
        id=str(uuid.uuid4()),
        name="slow",
        transport="http",
        url="http://upstream.invalid/mcp",
        command=None,
        secret_ciphertext=None,
        enabled=True,
        timeout_seconds=7,
    )

    client = http_connect(server)

    assert client.read_timeout_seconds == 7.0


def test_http_connect_without_a_timeout_sets_none() -> None:
    """No configured timeout stays no timeout; the default is not invented."""
    server = UpstreamServer(
        id=str(uuid.uuid4()),
        name="unbounded",
        transport="http",
        url="http://upstream.invalid/mcp",
        command=None,
        secret_ciphertext=None,
        enabled=True,
        timeout_seconds=None,
    )

    client = http_connect(server)

    assert client.read_timeout_seconds is None


@pytest.mark.asyncio
async def test_hanging_upstream_is_detached_rather_than_stalling_boot(
    backend: SqliteBackend, secret_box: SecretBox
) -> None:
    """A hung upstream must be given up on, not waited on forever.

    `timeout_seconds` bounds reads once a session exists, but an upstream that
    accepts the connection and never answers would hang the attach itself, and
    with it the whole boot.
    """
    backend.insert_upstream_server(
        UpstreamServer(
            id=str(uuid.uuid4()),
            name="hangs",
            transport="http",
            url="http://placeholder.invalid",
            command=None,
            secret_ciphertext=None,
            enabled=True,
            timeout_seconds=1,
        )
    )

    class _HangingClient:
        async def __aenter__(self):
            await asyncio.sleep(3600)
            return self

        async def __aexit__(self, *exc):
            return False

    aggregator = Aggregator(
        backend, secret_box, client_factory=lambda _server: _HangingClient()
    )
    # Bounded by the test as well as by the code: a regression here would
    # otherwise hang the suite instead of failing it.
    await asyncio.wait_for(aggregator.start(build_mcp_server()), timeout=30)
    try:
        [status] = aggregator.statuses()
        assert status.reachable is False
        assert "timed out" in (status.last_error or "")
    finally:
        await aggregator.close()


# ── stdio upstreams leaving and rejoining the surface (#274) ────────────


@pytest.mark.asyncio
async def test_stdio_tools_follow_the_child_through_the_aggregator(
    backend: SqliteBackend, secret_box: SecretBox, tmp_path: Path
) -> None:
    """The production wiring, not the supervisor in isolation.

    `test_stdio.py` drives the supervisor with its own callbacks; this proves
    the Aggregator's own register/unregister pair keeps `tools/list` honest,
    including the prefixed-name bookkeeping that decides what gets removed.
    """
    import sys

    from instrumenta import _stdio_fixtures

    gate = tmp_path / "gate"
    gate.write_text("open")
    backend.insert_upstream_server(
        UpstreamServer(
            id=str(uuid.uuid4()),
            name="box",
            transport="stdio",
            url=None,
            command=f"{sys.executable} {_stdio_fixtures.__file__} {gate}",
            secret_ciphertext=None,
            enabled=True,
            timeout_seconds=None,
        )
    )

    aggregator = Aggregator(backend, secret_box)
    # Production timings would make this test spend ten seconds waiting for a
    # liveness poll and a backoff. The behaviour under test is what happens on
    # those edges, not how long they take.
    aggregator._stdio._liveness_poll = 0.2
    aggregator._stdio._initial_backoff = 0.05

    mcp_server = build_mcp_server()
    await aggregator.start(mcp_server)
    try:

        async def names() -> set[str]:
            return {tool.name for tool in await mcp_server.list_tools()}

        assert "box.echo" in await names()

        gate.unlink()  # the child exits
        await _until(lambda: not any(s.reachable for s in aggregator.statuses()))
        assert "box.echo" not in await names()

        gate.write_text("open")  # and comes back
        await _until(lambda: any(s.reachable for s in aggregator.statuses()))
        assert "box.echo" in await names()

        # Registered once, not once per reconnect: a duplicate would mean the
        # bookkeeping had lost track of what it already had.
        assert sorted(await names()).count("box.echo") == 1
    finally:
        await aggregator.close()


async def _until(predicate, timeout: float = 10.0) -> None:
    loop = asyncio.get_event_loop()
    deadline = loop.time() + timeout
    while loop.time() < deadline:
        if predicate():
            return
        await asyncio.sleep(0.02)
    raise AssertionError("condition not met within timeout")


# ── HTTP upstreams follow their server too (#252, #282) ────────────────


class _ControllableUpstream:
    """A fake HTTP upstream whose tool set and reachability a test can change.

    Wraps a real in-memory MCP session, so the aggregator exercises the same
    client API it uses in production; only the failure and the tool set are
    under the test's hand.
    """

    def __init__(self) -> None:
        self.server = MCPServer(name="fake-upstream", version="0.0.0")
        self.add_tool("echo")
        self.reachable = True

    def add_tool(self, name: str) -> None:
        def handler(message: str = "") -> dict[str, str]:
            return {"echoed": message}

        self.server.add_tool(handler, name=name, description=f"{name} tool")

    def remove_tool(self, name: str) -> None:
        self.server.remove_tool(name)

    def connect(self, _server: UpstreamServer):
        upstream = self

        class _Client:
            def __init__(self) -> None:
                self._inner = Client(InMemoryTransport(upstream.server), raise_exceptions=True)

            async def __aenter__(self):
                upstream._check()
                await self._inner.__aenter__()
                return self

            async def __aexit__(self, *exc):
                return await self._inner.__aexit__(*exc)

            async def list_tools(self):
                upstream._check()
                return await self._inner.list_tools()

            async def list_prompts(self):
                return await self._inner.list_prompts()

            async def list_resources(self):
                return await self._inner.list_resources()

            async def call_tool(self, name, args):
                upstream._check()
                return await self._inner.call_tool(name, args)

        return _Client()

    def _check(self) -> None:
        if not self.reachable:
            raise RuntimeError("upstream is gone")


def _http_row(name: str) -> UpstreamServer:
    return UpstreamServer(
        id=str(uuid.uuid4()),
        name=name,
        transport="http",
        url="http://placeholder.invalid",
        command=None,
        secret_ciphertext=None,
        enabled=True,
        timeout_seconds=None,
    )


async def _http_aggregator(backend, secret_box, upstream: _ControllableUpstream):
    aggregator = Aggregator(backend, secret_box, client_factory=upstream.connect)
    # Production polls a third party every 30s; the behaviour under test is
    # what happens on the poll, not how long it waits for one.
    aggregator._http._liveness_poll = 0.05
    aggregator._http._initial_backoff = 0.05
    return aggregator


@pytest.mark.asyncio
async def test_unreachable_http_upstream_loses_its_tools(
    backend: SqliteBackend, secret_box: SecretBox
) -> None:
    """#252: an HTTP upstream that goes away stops being advertised.

    It used to be attached once and never contacted again, so its tools stayed
    in `tools/list` and a call failed at call time instead.
    """
    backend.insert_upstream_server(_http_row("fake"))
    upstream = _ControllableUpstream()
    aggregator = await _http_aggregator(backend, secret_box, upstream)
    mcp_server = build_mcp_server()
    await aggregator.start(mcp_server)
    try:

        async def names() -> set[str]:
            return {tool.name for tool in await mcp_server.list_tools()}

        assert "fake.echo" in await names()

        upstream.reachable = False
        await _until(lambda: not any(s.reachable for s in aggregator.statuses()))
        assert "fake.echo" not in await names()

        upstream.reachable = True
        await _until(lambda: any(s.reachable for s in aggregator.statuses()))
        assert "fake.echo" in await names()
    finally:
        await aggregator.close()


@pytest.mark.asyncio
async def test_reachable_http_upstream_that_changes_tools_is_followed(
    backend: SqliteBackend, secret_box: SecretBox
) -> None:
    """#282: the liveness poll's listing is used, not discarded.

    An upstream that stays up while adding, removing or renaming a tool used
    to keep advertising the set it had at attach time until it detached or
    Instrumenta restarted.
    """
    backend.insert_upstream_server(_http_row("fake"))
    upstream = _ControllableUpstream()
    aggregator = await _http_aggregator(backend, secret_box, upstream)
    mcp_server = build_mcp_server()
    await aggregator.start(mcp_server)
    try:

        async def names() -> set[str]:
            return {tool.name for tool in await mcp_server.list_tools()}

        assert await names() >= {"fake.echo"}
        assert "fake.shout" not in await names()

        # Added while reachable — no disconnect, no restart.
        upstream.add_tool("shout")
        await _until_async(lambda n: "fake.shout" in n, names)
        assert "fake.echo" in await names()

        # And removed the same way.
        upstream.remove_tool("echo")
        await _until_async(lambda n: "fake.echo" not in n, names)
        assert "fake.shout" in await names()

        # The status count follows too, rather than reporting the old set.
        [status] = [s for s in aggregator.statuses() if s.name == "fake"]
        assert status.tool_count == 1
        assert status.reachable is True
    finally:
        await aggregator.close()


async def _until_async(predicate, produce, timeout: float = 10.0) -> None:
    loop = asyncio.get_event_loop()
    deadline = loop.time() + timeout
    while loop.time() < deadline:
        if predicate(await produce()):
            return
        await asyncio.sleep(0.02)
    raise AssertionError("condition not met within timeout")
