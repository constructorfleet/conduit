"""Tests for the Instrumenta v1 skeleton.

One external seam: the FastAPI app via `TestClient`. Fixtures provide a
per-test temp data dir and a deterministic Fernet key so nothing leaks
between tests.
"""

from __future__ import annotations

import sqlite3
import os
from pathlib import Path

import pytest
from cryptography.fernet import Fernet
from fastapi.testclient import TestClient

from instrumenta.app import Config, _make_backend, create_app
from instrumenta.backend import SqliteBackend
from instrumenta.secret_box import SecretBox, SecretKeyMissingError


@pytest.fixture
def data_dir(tmp_path: Path) -> Path:
    return tmp_path


@pytest.fixture
def secret_key() -> str:
    return Fernet.generate_key().decode()


@pytest.fixture
def config(data_dir: Path, secret_key: str) -> Config:
    return Config(
        data_dir=data_dir,
        backend_type="sqlite",
        api_key=None,
        base_url="http://localhost:8085",
        secret_key=secret_key,
    )


def test_postgres_backend_is_selected(monkeypatch: pytest.MonkeyPatch) -> None:
    config = Config(
        data_dir=Path("/tmp/instrumenta-test"),
        backend_type="postgres",
        database_url="postgresql://example.invalid/instrumenta",
        api_key=None,
        base_url="http://localhost:8085",
        secret_key=None,
    )
    selected: list[str] = []

    class FakePostgresBackend:
        def __init__(self, database_url: str) -> None:
            selected.append(database_url)

    monkeypatch.setattr("instrumenta.app.PostgresBackend", FakePostgresBackend)
    backend = _make_backend(config)
    assert isinstance(backend, FakePostgresBackend)
    assert selected == ["postgresql://example.invalid/instrumenta"]


def test_postgres_app_uses_real_backend() -> None:
    database_url = os.getenv("INSTRUMENTA_TEST_POSTGRES_URL")
    if not database_url:
        pytest.skip("set INSTRUMENTA_TEST_POSTGRES_URL to run PostgreSQL tests")
    config = Config(
        data_dir=Path("/tmp/instrumenta-postgres-test"),
        backend_type="postgres",
        database_url=database_url,
        api_key=None,
        base_url="http://localhost:8085",
        secret_key=None,
    )
    with TestClient(create_app(config)) as client:
        response = client.get("/health")
    assert response.status_code == 200
    assert response.json()["backend"] == "postgres"


@pytest.fixture
def client(config: Config):
    # `with TestClient(...)` triggers the FastAPI lifespan, which the MCP
    # streamable-HTTP session manager needs before it can serve requests.
    with TestClient(create_app(config)) as c:
        yield c


class TestHealth:
    def test_health_reports_ok_and_unlinked(self, client: TestClient) -> None:
        response = client.get("/health")
        assert response.status_code == 200
        data = response.json()
        assert data["status"] == "ok"
        assert data["backend"] == "sqlite"
        assert data["linked"] is False

def _parse_sse(text: str) -> dict:
    """Pull the JSON payload out of a streamable-HTTP SSE response."""
    import json

    for line in text.splitlines():
        if line.startswith("data: "):
            return json.loads(line[len("data: ") :])
    raise AssertionError(f"no data frame in response: {text!r}")


class TestMcpEndpoint:
    """End-to-end MCP wire tests over `/mcp`.

    These use the real streamable-HTTP transport rather than probing the
    endpoint shape, because that is the only way to catch mount-path bugs
    (trailing-slash redirects strip POST bodies on strict clients) and
    handshake regressions.
    """

    _MCP_HEADERS = {
        "Accept": "application/json, text/event-stream",
        "MCP-Protocol-Version": "2025-06-18",
    }

    def _initialize(self, client: TestClient) -> str:
        response = client.post(
            "/mcp/",
            json={
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "0"},
                },
            },
            headers=self._MCP_HEADERS,
        )
        assert response.status_code == 200, response.text
        session_id = response.headers["mcp-session-id"]
        client.post(
            "/mcp/",
            json={
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {},
            },
            headers={**self._MCP_HEADERS, "mcp-session-id": session_id},
        )
        return session_id

    def test_mount_serves_initialize_handshake(
        self, client: TestClient
    ) -> None:
        response = client.post(
            "/mcp/",
            json={
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "0"},
                },
            },
            headers=self._MCP_HEADERS,
        )
        assert response.status_code == 200
        assert "mcp-session-id" in response.headers

    def test_tools_list_returns_all_four_builtins(self, client: TestClient) -> None:
        session_id = self._initialize(client)
        response = client.post(
            "/mcp/",
            json={"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
            headers={**self._MCP_HEADERS, "mcp-session-id": session_id},
        )
        assert response.status_code == 200
        # Streamable-HTTP wraps responses in SSE frames; the JSON is on a
        # `data: ` line. Parse it out rather than pulling in the client SDK.
        import json

        for line in response.text.splitlines():
            if line.startswith("data: "):
                payload = json.loads(line[len("data: ") :])
                break
        else:
            raise AssertionError(f"no data frame in response: {response.text!r}")
        tool_names = {tool["name"] for tool in payload["result"]["tools"]}
        assert tool_names == {"http.fetch", "time.now", "math.eval", "text.regex"}


class TestRootRedirect:
    def test_root_redirects_to_ui(self, client: TestClient) -> None:
        response = client.get("/", follow_redirects=False)
        assert response.status_code == 307
        assert response.headers["location"] == "/ui/"


class TestLinkRouter:
    def test_link_router_is_mounted(self, client: TestClient) -> None:
        # The shared conduit_link router exposes GET /link; a fresh service is
        # unlinked, so it responds 404 rather than the "route not registered"
        # 404 (which would come with an empty allow-list). We assert the route
        # exists in the OpenAPI schema — mounting is what the skeleton needs
        # to prove; the router's own tests cover the semantics.
        paths = client.get("/openapi.json").json()["paths"]
        assert "/link" in paths


class TestSecretBoxFailLoud:
    def test_app_refuses_to_start_when_secrets_exist_without_key(
        self, data_dir: Path, secret_key: str
    ) -> None:
        # Seed an encrypted secret row via the same backend the app uses, then
        # try to construct the app with `secret_key=None`. Should raise.
        db_path = data_dir / "instrumenta.db"
        # Constructed for the side effect: it creates the schema the raw
        # INSERT below writes into. The handle itself is not needed.
        SqliteBackend(db_path)
        conn = sqlite3.connect(db_path)
        conn.execute(
            "INSERT INTO upstream_servers (id, name, transport, url, secret_ciphertext) "
            "VALUES (?, ?, ?, ?, ?)",
            ("srv-1", "github", "http", "https://example.invalid/mcp", b"encrypted-blob"),
        )
        conn.commit()
        conn.close()

        no_key_config = Config(
            data_dir=data_dir,
            backend_type="sqlite",
            api_key=None,
            base_url="http://localhost:8085",
            secret_key=None,
        )
        with pytest.raises(SecretKeyMissingError):
            create_app(no_key_config)

    def test_app_starts_when_secrets_exist_with_key(
        self, data_dir: Path, secret_key: str, config: Config
    ) -> None:
        db_path = data_dir / "instrumenta.db"
        SqliteBackend(db_path)
        conn = sqlite3.connect(db_path)
        conn.execute(
            "INSERT INTO upstream_servers (id, name, transport, url, secret_ciphertext) "
            "VALUES (?, ?, ?, ?, ?)",
            ("srv-2", "gitlab", "http", "https://example.invalid/mcp", b"encrypted-blob"),
        )
        conn.commit()
        conn.close()

        # Should not raise — key is present so the fail-loud contract passes.
        app = create_app(config)
        with TestClient(app) as client:
            assert client.get("/health").status_code == 200


class TestSecretBox:
    def test_roundtrip(self, secret_key: str) -> None:
        box = SecretBox(secret_key)
        ciphertext = box.encrypt("hunter2")
        assert box.decrypt(ciphertext) == "hunter2"

    def test_encrypt_without_key_raises(self) -> None:
        with pytest.raises(SecretKeyMissingError):
            SecretBox(None).encrypt("anything")

    def test_invalid_key_raises_on_construction(self) -> None:
        with pytest.raises(SecretKeyMissingError):
            SecretBox("not-a-valid-fernet-key")


class TestBackend:
    def test_has_encrypted_secret_reports_false_on_fresh_db(
        self, data_dir: Path
    ) -> None:
        backend = SqliteBackend(data_dir / "instrumenta.db")
        assert backend.has_encrypted_secret() is False

    def test_has_encrypted_secret_reports_true_when_row_present(
        self, data_dir: Path
    ) -> None:
        db_path = data_dir / "instrumenta.db"
        backend = SqliteBackend(db_path)
        conn = sqlite3.connect(db_path)
        conn.execute(
            "INSERT INTO upstream_servers (id, name, transport, url, secret_ciphertext) "
            "VALUES (?, ?, ?, ?, ?)",
            ("srv-3", "example", "http", "https://example.invalid/mcp", b"blob"),
        )
        conn.commit()
        conn.close()
        assert backend.has_encrypted_secret() is True


class TestLinkHandshakeAdvertisesMcpUrl:
    """User Story 30 (#198): the handshake carries one canonical `mcp_url`.

    Conduit should be able to connect as a stock MCP client from the link
    body alone, without knowing how Instrumenta happens to mount its
    transport.
    """

    def test_create_body_carries_the_streamable_http_endpoint(self) -> None:
        from instrumenta.app import MCP_PATH, _build_create_body

        class _Request:
            peer_name = "Instrumenta"

        class _Context:
            existing_peer_id = None
            request = _Request()

        body = _build_create_body(_Context())

        assert body["mcp_url"] == f"{body['peer_base_url']}{MCP_PATH}"
        assert body["mcp_url"].endswith("/mcp/")

    def test_advertised_endpoint_is_the_one_the_app_actually_serves(
        self, client: TestClient
    ) -> None:
        """The advertised path must be routable, not just well-formed.

        A hand-written string here would drift the first time the mount moves,
        and the handshake would send Conduit somewhere that 404s.
        """
        from instrumenta.app import MCP_PATH

        response = client.post(
            MCP_PATH,
            json={"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}},
            headers={"Accept": "application/json, text/event-stream"},
        )

        assert response.status_code != 404


class TestToolsSurface:
    """`/tools` exposes the merged surface with real names (#278).

    The UI previously counted `tool_count` from `/upstreams` and invented
    `<server>.tool_0`, `<server>.tool_1`, ... — names no upstream has and no
    caller can act on. It also hardcoded the built-ins as permanently enabled,
    so the per-tool flags from User Story 3 were unreachable.
    """

    def test_builtins_are_listed_by_their_real_names(self, client: TestClient) -> None:
        tools = client.get("/tools").json()

        builtins = {t["name"] for t in tools if t["origin"] == "builtin"}
        assert builtins == {"http.fetch", "time.now", "math.eval", "text.regex"}

    def test_no_tool_name_is_fabricated(self, client: TestClient) -> None:
        """Every advertised name must be one the MCP surface actually serves.

        This is the regression the issue is about: names built by counting
        `tool_count` matched the count and nothing else.
        """
        tools = client.get("/tools").json()

        mcp = TestMcpEndpoint()
        session_id = mcp._initialize(client)
        response = client.post(
            "/mcp/",
            json={"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
            headers={**mcp._MCP_HEADERS, "mcp-session-id": session_id},
        )
        served = {
            tool["name"]
            for tool in _parse_sse(response.text)["result"]["tools"]
        }

        assert {t["name"] for t in tools} == served

    def test_tools_are_enabled_by_default(self, client: TestClient) -> None:
        tools = client.get("/tools").json()

        assert all(t["enabled"] for t in tools)

    def test_a_disabling_flag_is_reflected(self, client: TestClient) -> None:
        """The tab's toggle writes `/items/flags`; `/tools` must read it back."""
        client.put(
            "/items/flags",
            json={
                "origin": "builtin",
                "item_kind": "tool",
                "item_name": "time.now",
                "enabled": False,
            },
        ).raise_for_status()

        tools = {t["name"]: t for t in client.get("/tools").json()}

        assert tools["time.now"]["enabled"] is False
        assert tools["math.eval"]["enabled"] is True
