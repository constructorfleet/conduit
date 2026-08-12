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

from instrumenta.aggregator import Aggregator
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
