"""Tests for the stdio supervisor and PATH probe.

The stdio supervisor drives each enabled stdio upstream through the `mcp`
SDK's own stdio client transport — the SDK spawns the child and owns
request/response correlation and shutdown — and autorestarts with capped
exponential backoff. Its one Instrumenta-specific guarantee is
register-on-recovery: a child that loses its first connection still has its
tools registered once a retry connects, without an Instrumenta restart.

These tests spawn a real fake MCP server subprocess (`_stdio_fixtures.py`)
rather than a hand-rolled JSON pipe, so they exercise the actual transport
the production path uses.
"""

from __future__ import annotations

import asyncio
import shutil
import sys
from pathlib import Path

import pytest
from mcp.client import Client
from mcp.client._memory import InMemoryTransport
from mcp.server.mcpserver import MCPServer

from instrumenta import _stdio_fixtures
from instrumenta.aggregator import build_forwarder
from instrumenta.backend import UpstreamServer
from instrumenta.path_probe import probe_runtimes
from instrumenta.stdio_supervisor import ClientHolder, StdioSupervisor

FIXTURE = _stdio_fixtures.__file__


class TestPathProbe:
    def test_probe_returns_dict(self) -> None:
        result = probe_runtimes()
        assert isinstance(result, dict)
        assert "python3" in result

    def test_python3_found_on_path(self) -> None:
        result = probe_runtimes()
        assert result["python3"] is True

    def test_nonexistent_binary_not_found(self) -> None:
        result = probe_runtimes(runtimes=("nonexistent_binary_xyz",))
        assert result.get("nonexistent_binary_xyz") is False

    def test_reflects_actual_PATH(self) -> None:
        result = probe_runtimes()
        assert shutil.which("python3") is not None
        assert result["python3"] is True


def _stdio_server(name: str, command: str, *, enabled: bool = True) -> UpstreamServer:
    return UpstreamServer(
        id=name,
        name=name,
        transport="stdio",
        url=None,
        command=command,
        secret_ciphertext=None,
        enabled=enabled,
        timeout_seconds=None,
    )


def _register_on(mcp_server: MCPServer):
    """A registration callback wiring the supervisor to the production forwarder."""

    def register(server: UpstreamServer, tools, holder: ClientHolder) -> None:
        for tool in tools:
            forward = build_forwarder(
                lambda h=holder: h.client,
                tool.name,
                server.name,
                getattr(tool, "input_schema", None),
            )
            mcp_server.add_tool(
                forward,
                name=f"{server.name}.{tool.name}",
                description=tool.description or "",
            )

    return register


def _unregister_on(mcp_server: MCPServer):
    """The matching removal callback.

    Derives the prefixed name from the same rule the registration helper
    uses rather than reading anything the supervisor tracks, so the test does
    not simply agree with the production bookkeeping about what was added.
    """

    def unregister(server: UpstreamServer, tools) -> None:
        for tool in tools:
            mcp_server.remove_tool(f"{server.name}.{tool.name}")

    return unregister


async def _call_tool(mcp_server: MCPServer, name: str, args: dict) -> str:
    client = Client(InMemoryTransport(mcp_server), raise_exceptions=True)
    async with client:
        result = await client.call_tool(name, args)
    return result.content[0].text


async def _wait_until(predicate, timeout: float = 5.0) -> None:
    deadline = asyncio.get_event_loop().time() + timeout
    while asyncio.get_event_loop().time() < deadline:
        if predicate():
            return
        await asyncio.sleep(0.02)
    raise AssertionError("condition not met within timeout")


class TestStdioSupervisor:
    @pytest.mark.asyncio
    async def test_tools_registered_and_forward(self) -> None:
        """A live stdio upstream's tools register and forward end to end."""
        mcp_server = MCPServer(name="host")
        command = f"{sys.executable} {FIXTURE}"
        supervisor = StdioSupervisor(
            _register_on(mcp_server), initial_backoff=0.05, liveness_poll=0.2
        )
        supervisor.add(_stdio_server("box", command))
        await supervisor.start()
        try:
            names = {t.name for t in await mcp_server.list_tools()}
            assert "box.echo" in names

            [status] = supervisor.statuses()
            assert status["reachable"] is True
            assert status["tool_count"] == 1

            # The registered forwarder reaches the child over the real transport.
            assert await _call_tool(mcp_server, "box.echo", {"message": "hi"}) == "echo: hi"
        finally:
            await supervisor.close()

    @pytest.mark.asyncio
    async def test_failed_first_attempt_recovers_and_registers(self, tmp_path: Path) -> None:
        """Register-on-recovery: a child that loses the boot race still gets
        its tools registered once a retry connects — without a restart.
        """
        gate = tmp_path / "gate"  # absent → the child exits non-zero
        mcp_server = MCPServer(name="host")
        command = f"{sys.executable} {FIXTURE} {gate}"
        supervisor = StdioSupervisor(
            _register_on(mcp_server), initial_backoff=0.05, liveness_poll=0.2
        )
        supervisor.add(_stdio_server("box", command))
        await supervisor.start()
        try:
            # First attempt failed: unreachable, an error recorded, no tools yet.
            [status] = supervisor.statuses()
            assert status["reachable"] is False
            assert status["last_error"] is not None
            assert "box.echo" not in {t.name for t in await mcp_server.list_tools()}

            # The child can now come up; a retry must connect and register.
            gate.write_text("open")
            await _wait_until(lambda: supervisor.statuses()[0]["reachable"] is True)

            assert "box.echo" in {t.name for t in await mcp_server.list_tools()}
            assert await _call_tool(mcp_server, "box.echo", {"message": "yo"}) == "echo: yo"
        finally:
            await supervisor.close()

    @pytest.mark.asyncio
    async def test_bad_command_reports_unreachable_without_blocking(self) -> None:
        """An unspawnable command settles quickly as unreachable, never hangs boot."""
        mcp_server = MCPServer(name="host")
        supervisor = StdioSupervisor(
            _register_on(mcp_server), initial_backoff=0.05, liveness_poll=0.2
        )
        supervisor.add(_stdio_server("bad", "nonexistent_binary_xyz_12345"))
        await asyncio.wait_for(supervisor.start(), timeout=3.0)
        try:
            [status] = supervisor.statuses()
            assert status["reachable"] is False
            assert status["last_error"] is not None
        finally:
            await supervisor.close()

    @pytest.mark.asyncio
    async def test_tools_disappear_while_the_child_is_down(self, tmp_path: Path) -> None:
        """User Story 24 for stdio (#274): a dead child's tools leave the surface.

        The supervisor registered once and never unregistered, so a model
        picking from `tools/list` could choose a tool whose upstream was gone
        and get "upstream is not currently connected" at call time. A tool that
        cannot be called should not be advertised.
        """
        gate = tmp_path / "gate"
        gate.write_text("open")  # present → the child comes up
        mcp_server = MCPServer(name="host")
        command = f"{sys.executable} {FIXTURE} {gate}"
        supervisor = StdioSupervisor(
            _register_on(mcp_server),
            unregister_tools=_unregister_on(mcp_server),
            initial_backoff=0.05,
            liveness_poll=0.2,
        )
        supervisor.add(_stdio_server("box", command))
        await supervisor.start()
        try:
            assert "box.echo" in {t.name for t in await mcp_server.list_tools()}

            # Kill the connected child: the fixture exits when the gate goes.
            gate.unlink()
            await _wait_until(lambda: supervisor.statuses()[0]["reachable"] is False)

            assert "box.echo" not in {t.name for t in await mcp_server.list_tools()}

            # And they come back on reconnect, still callable.
            gate.write_text("open")
            await _wait_until(lambda: supervisor.statuses()[0]["reachable"] is True)

            assert "box.echo" in {t.name for t in await mcp_server.list_tools()}
            assert await _call_tool(mcp_server, "box.echo", {"message": "back"}) == "echo: back"
        finally:
            await supervisor.close()

    @pytest.mark.asyncio
    async def test_close_leaves_no_tools_behind(self, tmp_path: Path) -> None:
        """Shutdown removes them too, so a restarted aggregator starts clean."""
        gate = tmp_path / "gate"
        gate.write_text("open")
        mcp_server = MCPServer(name="host")
        command = f"{sys.executable} {FIXTURE} {gate}"
        supervisor = StdioSupervisor(
            _register_on(mcp_server),
            unregister_tools=_unregister_on(mcp_server),
            initial_backoff=0.05,
            liveness_poll=0.2,
        )
        supervisor.add(_stdio_server("box", command))
        await supervisor.start()
        assert "box.echo" in {t.name for t in await mcp_server.list_tools()}

        await supervisor.close()

        assert "box.echo" not in {t.name for t in await mcp_server.list_tools()}
