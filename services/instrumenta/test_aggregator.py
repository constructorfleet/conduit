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


def test_default_client_factory_passes_the_configured_timeout() -> None:
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

    client = Aggregator._default_client_factory(server)

    assert client.read_timeout_seconds == 7.0


def test_default_client_factory_without_a_timeout_sets_none() -> None:
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

    client = Aggregator._default_client_factory(server)

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
