"""Tests for the downstream MCP transports and their per-transport toggles.

Spec #198 user stories 16/17: both `/mcp/sse` and `/mcp/http` are mounted
and enabled by default; each can be switched off through `/transports`
and the choice persists across restarts.
"""

from __future__ import annotations

import asyncio
import threading
import time
from collections.abc import Iterator
from pathlib import Path

import pytest
import uvicorn
from fastapi.testclient import TestClient
from mcp import ClientSession
from mcp.client.sse import sse_client

from instrumenta.app import Config, create_app
from instrumenta.backend import TRANSPORTS, SqliteBackend
from instrumenta.transports_router import TRANSPORT_MOUNTS


class TestTransportMountTable:
    def test_every_backend_transport_has_exactly_one_mount(self) -> None:
        assert set(TRANSPORT_MOUNTS) == set(TRANSPORTS)
        assert TRANSPORT_MOUNTS == {"http": "/mcp/http", "sse": "/mcp/sse"}


class TestBackendTransportFlags:
    def test_fresh_db_reports_both_transports_enabled(self, tmp_path: Path) -> None:
        backend = SqliteBackend(tmp_path / "instrumenta.db")
        flags = backend.list_transport_flags()
        assert {flag.transport: flag.enabled for flag in flags} == {
            "http": True,
            "sse": True,
        }

    def test_disable_persists_across_reopen(self, tmp_path: Path) -> None:
        db_path = tmp_path / "instrumenta.db"
        SqliteBackend(db_path).set_transport_enabled("sse", False)
        flags = SqliteBackend(db_path).list_transport_flags()
        assert {flag.transport: flag.enabled for flag in flags} == {
            "http": True,
            "sse": False,
        }

    def test_re_enable_overwrites_previous_value(self, tmp_path: Path) -> None:
        backend = SqliteBackend(tmp_path / "instrumenta.db")
        backend.set_transport_enabled("http", False)
        backend.set_transport_enabled("http", True)
        assert backend.is_transport_enabled("http") is True


_MCP_HEADERS = {
    "Accept": "application/json, text/event-stream",
    "MCP-Protocol-Version": "2025-06-18",
}

_INITIALIZE = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "test", "version": "0"},
    },
}


class TestStreamableHttpMount:
    def test_mcp_http_serves_initialize_handshake(self, client: TestClient) -> None:
        response = client.post("/mcp/http/", json=_INITIALIZE, headers=_MCP_HEADERS)
        assert response.status_code == 200, response.text
        assert "mcp-session-id" in response.headers

    def test_legacy_mcp_root_still_serves_streamable_http(
        self, client: TestClient
    ) -> None:
        # `/mcp/` predates the per-transport split; clients configured against
        # it keep working.
        response = client.post("/mcp/", json=_INITIALIZE, headers=_MCP_HEADERS)
        assert response.status_code == 200, response.text
        assert "mcp-session-id" in response.headers


@pytest.fixture
def live_server(config: Config) -> Iterator[str]:
    """Instrumenta on a real socket.

    SSE is a long-lived GET; `TestClient` and httpx's ASGI transport both
    buffer the whole response, so the SSE transport can only be exercised
    against a live server with the SDK's own client.
    """

    server = uvicorn.Server(
        uvicorn.Config(create_app(config), host="127.0.0.1", port=0, log_level="warning")
    )
    thread = threading.Thread(target=server.run, daemon=True)
    thread.start()
    deadline = time.monotonic() + 10
    while not server.started:
        assert time.monotonic() < deadline, "uvicorn did not start"
        time.sleep(0.01)
    port = server.servers[0].sockets[0].getsockname()[1]
    try:
        yield f"http://127.0.0.1:{port}"
    finally:
        server.should_exit = True
        thread.join(timeout=10)


async def _list_tools_over_sse(base_url: str) -> set[str]:
    async with sse_client(f"{base_url}/mcp/sse/") as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            result = await session.list_tools()
            return {tool.name for tool in result.tools}


class TestSseMount:
    def test_mcp_sse_serves_the_builtin_tools(self, live_server: str) -> None:
        tool_names = asyncio.run(_list_tools_over_sse(live_server))
        assert tool_names == {"http.fetch", "time.now", "math.eval", "text.regex"}


class TestTransportsApi:
    def test_lists_both_transports_enabled_by_default(self, client: TestClient) -> None:
        response = client.get("/transports")
        assert response.status_code == 200
        assert response.json() == [
            {"transport": "http", "path": "/mcp/http/", "enabled": True},
            {"transport": "sse", "path": "/mcp/sse/", "enabled": True},
        ]

    def test_put_disables_a_transport(self, client: TestClient) -> None:
        response = client.put("/transports/sse", json={"enabled": False})
        assert response.status_code == 200
        assert response.json() == {
            "transport": "sse",
            "path": "/mcp/sse/",
            "enabled": False,
        }
        listed = {row["transport"]: row["enabled"] for row in client.get("/transports").json()}
        assert listed == {"http": True, "sse": False}

    def test_put_unknown_transport_is_404(self, client: TestClient) -> None:
        response = client.put("/transports/grpc", json={"enabled": False})
        assert response.status_code == 404

    def test_toggle_survives_restart(self, config: Config) -> None:
        with TestClient(create_app(config)) as first:
            first.put("/transports/http", json={"enabled": False})
        with TestClient(create_app(config)) as second:
            listed = {row["transport"]: row["enabled"] for row in second.get("/transports").json()}
        assert listed == {"http": False, "sse": True}


class TestTransportGate:
    def test_disabled_sse_mount_returns_404(self, client: TestClient) -> None:
        client.put("/transports/sse", json={"enabled": False})
        response = client.get("/mcp/sse/", headers={"Accept": "text/event-stream"})
        assert response.status_code == 404
        assert response.json() == {"detail": "sse transport is disabled"}

    def test_disabled_http_gates_canonical_and_legacy_paths(
        self, client: TestClient
    ) -> None:
        client.put("/transports/http", json={"enabled": False})
        for path in ("/mcp/http/", "/mcp/"):
            response = client.post(path, json=_INITIALIZE, headers=_MCP_HEADERS)
            assert response.status_code == 404, path
            assert response.json() == {"detail": "http transport is disabled"}

    def test_disabling_one_transport_leaves_the_other_serving(
        self, client: TestClient
    ) -> None:
        client.put("/transports/sse", json={"enabled": False})
        response = client.post("/mcp/http/", json=_INITIALIZE, headers=_MCP_HEADERS)
        assert response.status_code == 200, response.text

    def test_re_enabling_takes_effect_without_restart(self, client: TestClient) -> None:
        client.put("/transports/http", json={"enabled": False})
        client.put("/transports/http", json={"enabled": True})
        response = client.post("/mcp/http/", json=_INITIALIZE, headers=_MCP_HEADERS)
        assert response.status_code == 200, response.text
