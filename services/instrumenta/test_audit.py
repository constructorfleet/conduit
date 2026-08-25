"""Tests for the audit log: backend writes + /audit query endpoint.

The audit log captures every tool invocation with hashed args, duration,
and outcome. Structured stdout logging is tested implicitly through the
endpoint (the writer fires on the same codepath).
"""

from __future__ import annotations

from pathlib import Path

import pytest
from cryptography.fernet import Fernet
from fastapi.testclient import TestClient

from instrumenta.app import Config, create_app


@pytest.fixture
def secret_key() -> str:
    return Fernet.generate_key().decode()


@pytest.fixture
def config(tmp_path: Path, secret_key: str) -> Config:
    return Config(
        data_dir=tmp_path,
        backend_type="sqlite",
        api_key=None,
        base_url="http://localhost:8085",
        secret_key=secret_key,
    )


@pytest.fixture
def client(config: Config):
    with TestClient(create_app(config)) as c:
        yield c


class TestAuditLog:
    def test_list_empty_by_default(self, client: TestClient) -> None:
        resp = client.get("/audit")
        assert resp.status_code == 200
        assert resp.json() == []

    def test_create_and_list_audit_entries(self, client: TestClient) -> None:
        payload = {
            "peer_id": "conduit",
            "tool_name": "http.fetch",
            "args_hash": "abc123",
            "duration_ms": 150,
            "outcome": "ok",
        }
        resp = client.post("/audit", json=payload)
        assert resp.status_code == 201
        body = resp.json()
        assert body["peer_id"] == "conduit"
        assert body["tool_name"] == "http.fetch"
        assert body["args_hash"] == "abc123"
        assert body["duration_ms"] == 150
        assert body["outcome"] == "ok"
        assert "id" in body
        assert "called_at" in body

    def test_list_returns_most_recent_first(self, client: TestClient) -> None:
        for i in range(3):
            client.post(
                "/audit",
                json={
                    "peer_id": "c",
                    "tool_name": f"tool_{i}",
                    "args_hash": f"h{i}",
                    "duration_ms": i,
                    "outcome": "ok",
                },
            )
        resp = client.get("/audit")
        entries = resp.json()
        assert len(entries) == 3
        assert entries[0]["tool_name"] == "tool_2"
        assert entries[2]["tool_name"] == "tool_0"

    def test_list_filter_by_tool_name(self, client: TestClient) -> None:
        client.post(
            "/audit",
            json={"peer_id": "c", "tool_name": "foo", "args_hash": "h1", "duration_ms": 1, "outcome": "ok"},
        )
        client.post(
            "/audit",
            json={"peer_id": "c", "tool_name": "bar", "args_hash": "h2", "duration_ms": 2, "outcome": "ok"},
        )
        resp = client.get("/audit", params={"tool_name": "foo"})
        assert len(resp.json()) == 1
        assert resp.json()[0]["tool_name"] == "foo"

    def test_list_filter_by_outcome(self, client: TestClient) -> None:
        client.post(
            "/audit",
            json={"peer_id": "c", "tool_name": "a", "args_hash": "h", "duration_ms": 1, "outcome": "ok"},
        )
        client.post(
            "/audit",
            json={"peer_id": "c", "tool_name": "b", "args_hash": "h", "duration_ms": 1, "outcome": "error"},
        )
        resp = client.get("/audit", params={"outcome": "error"})
        assert len(resp.json()) == 1
        assert resp.json()[0]["outcome"] == "error"

    def test_list_limit(self, client: TestClient) -> None:
        for i in range(5):
            client.post(
                "/audit",
                json={"peer_id": "c", "tool_name": f"t{i}", "args_hash": "h", "duration_ms": 1, "outcome": "ok"},
            )
        resp = client.get("/audit", params={"limit": 2})
        assert len(resp.json()) == 2

    def test_create_rejects_invalid_outcome(self, client: TestClient) -> None:
        resp = client.post(
            "/audit",
            json={"peer_id": "c", "tool_name": "x", "args_hash": "h", "duration_ms": 1, "outcome": "bad"},
        )
        assert resp.status_code == 422

    def test_create_rejects_missing_tool_name(self, client: TestClient) -> None:
        resp = client.post(
            "/audit",
            json={"peer_id": "c", "args_hash": "h", "duration_ms": 1, "outcome": "ok"},
        )
        assert resp.status_code == 422

    def test_create_allows_null_peer_id(self, client: TestClient) -> None:
        resp = client.post(
            "/audit",
            json={"tool_name": "t", "args_hash": "h", "duration_ms": 1, "outcome": "ok"},
        )
        assert resp.status_code == 201
        assert resp.json()["peer_id"] is None
