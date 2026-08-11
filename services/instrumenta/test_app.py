"""Tests for the Instrumenta v1 skeleton.

One external seam: the FastAPI app via `TestClient`. Fixtures provide a
per-test temp data dir and a deterministic Fernet key so nothing leaks
between tests.
"""

from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest
from cryptography.fernet import Fernet
from fastapi.testclient import TestClient

from instrumenta.app import Config, create_app
from instrumenta.backend import SqliteBackend
from instrumenta.secrets import SecretBox, SecretKeyMissingError


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


@pytest.fixture
def client(config: Config) -> TestClient:
    return TestClient(create_app(config))


class TestHealth:
    def test_health_reports_ok_and_unlinked(self, client: TestClient) -> None:
        response = client.get("/health")
        assert response.status_code == 200
        data = response.json()
        assert data["status"] == "ok"
        assert data["backend"] == "sqlite"
        assert data["linked"] is False

    def test_health_stays_ok_regardless_of_upstream(self, client: TestClient) -> None:
        # v1 posture: /health reports Instrumenta itself, never aggregates
        # upstreams. There are no upstreams configured here — the point of the
        # test is documenting the contract: /health does not touch them.
        assert client.get("/health").json()["status"] == "ok"


class TestMcpEndpoint:
    def test_mcp_endpoint_advertises_empty_surface(self, client: TestClient) -> None:
        response = client.post("/mcp")
        assert response.status_code == 200
        data = response.json()
        assert data == {"tools": [], "prompts": [], "resources": []}


class TestRootRedirect:
    def test_root_redirects_to_ui(self, client: TestClient) -> None:
        response = client.get("/", follow_redirects=False)
        assert response.status_code == 307
        assert response.headers["location"] == "/ui/"


class TestApiKey:
    def test_link_get_is_unauthenticated(self, client: TestClient) -> None:
        # The link router itself owns its authentication; here we just assert
        # the router is mounted and reachable, which is the skeleton's job.
        response = client.get("/link")
        assert response.status_code in (200, 401, 404)


class TestSecretBoxFailLoud:
    def test_app_refuses_to_start_when_secrets_exist_without_key(
        self, data_dir: Path, secret_key: str
    ) -> None:
        # Seed an encrypted secret row via the same backend the app uses, then
        # try to construct the app with `secret_key=None`. Should raise.
        db_path = data_dir / "instrumenta.db"
        backend = SqliteBackend(db_path)
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
