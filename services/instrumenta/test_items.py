"""Tests for the Items API: flags, local prompts, local resources.

Uses the same FastAPI TestClient seam as the other test modules.
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


# ── Item Flags ──────────────────────────────────────────────────────────


class TestItemFlags:
    def test_list_empty_by_default(self, client: TestClient) -> None:
        assert client.get("/items/flags").json() == []

    def test_upsert_flag(self, client: TestClient) -> None:
        resp = client.put(
            "/items/flags",
            json={
                "origin": "upstream-github",
                "item_kind": "tool",
                "item_name": "upstream-github.list_issues",
                "enabled": False,
            },
        )
        assert resp.status_code == 200
        body = resp.json()
        assert body["origin"] == "upstream-github"
        assert body["item_kind"] == "tool"
        assert body["item_name"] == "upstream-github.list_issues"
        assert body["enabled"] is False

    def test_upsert_flag_default_enabled(self, client: TestClient) -> None:
        resp = client.put(
            "/items/flags",
            json={
                "origin": "built-in",
                "item_kind": "tool",
                "item_name": "http.fetch",
            },
        )
        assert resp.status_code == 200
        assert resp.json()["enabled"] is True

    def test_upsert_updates_existing_flag(self, client: TestClient) -> None:
        payload = {
            "origin": "built-in",
            "item_kind": "tool",
            "item_name": "time.now",
            "enabled": False,
        }
        client.put("/items/flags", json=payload)
        payload["enabled"] = True
        resp = client.put("/items/flags", json=payload)
        assert resp.status_code == 200
        assert resp.json()["enabled"] is True

    def test_list_flags_filter_by_origin(self, client: TestClient) -> None:
        client.put(
            "/items/flags",
            json={"origin": "a", "item_kind": "tool", "item_name": "a.foo", "enabled": True},
        )
        client.put(
            "/items/flags",
            json={"origin": "b", "item_kind": "tool", "item_name": "b.bar", "enabled": True},
        )
        resp = client.get("/items/flags", params={"origin": "a"})
        assert len(resp.json()) == 1
        assert resp.json()[0]["origin"] == "a"

    def test_list_flags_filter_by_item_kind(self, client: TestClient) -> None:
        client.put(
            "/items/flags",
            json={"origin": "x", "item_kind": "tool", "item_name": "x.t", "enabled": True},
        )
        client.put(
            "/items/flags",
            json={"origin": "x", "item_kind": "prompt", "item_name": "x.p", "enabled": True},
        )
        resp = client.get("/items/flags", params={"item_kind": "prompt"})
        assert len(resp.json()) == 1
        assert resp.json()[0]["item_kind"] == "prompt"

    def test_delete_flag(self, client: TestClient) -> None:
        client.put(
            "/items/flags",
            json={"origin": "o", "item_kind": "tool", "item_name": "o.t", "enabled": False},
        )
        resp = client.delete("/items/flags/o/tool/o.t")
        assert resp.status_code == 204
        assert client.get("/items/flags").json() == []

    def test_delete_missing_flag_is_404(self, client: TestClient) -> None:
        resp = client.delete("/items/flags/nope/tool/nope")
        assert resp.status_code == 404

    def test_upsert_rejects_invalid_item_kind(self, client: TestClient) -> None:
        resp = client.put(
            "/items/flags",
            json={"origin": "x", "item_kind": "bad", "item_name": "x.y"},
        )
        assert resp.status_code == 422


# ── Local Prompts ───────────────────────────────────────────────────────


class TestLocalPrompts:
    def test_list_empty_by_default(self, client: TestClient) -> None:
        assert client.get("/items/prompts").json() == []

    def test_create_prompt(self, client: TestClient) -> None:
        resp = client.post(
            "/items/prompts",
            json={"name": "summarize", "template": "Summarize: {text}", "description": "A summarizer"},
        )
        assert resp.status_code == 201
        body = resp.json()
        assert body["name"] == "summarize"
        assert body["template"] == "Summarize: {text}"
        assert body["description"] == "A summarizer"
        assert "id" in body

    def test_create_prompt_without_description(self, client: TestClient) -> None:
        resp = client.post(
            "/items/prompts",
            json={"name": "minimal", "template": "Hello"},
        )
        assert resp.status_code == 201
        assert resp.json()["description"] is None

    def test_get_prompt(self, client: TestClient) -> None:
        created = client.post(
            "/items/prompts",
            json={"name": "fetch", "template": "Fetch {url}"},
        ).json()
        resp = client.get(f"/items/prompts/{created['id']}")
        assert resp.status_code == 200
        assert resp.json()["name"] == "fetch"

    def test_get_missing_prompt_is_404(self, client: TestClient) -> None:
        resp = client.get("/items/prompts/nonexistent")
        assert resp.status_code == 404

    def test_update_prompt(self, client: TestClient) -> None:
        created = client.post(
            "/items/prompts",
            json={"name": "old", "template": "Old template"},
        ).json()
        resp = client.patch(
            f"/items/prompts/{created['id']}",
            json={"name": "new", "template": "New template", "description": "Updated"},
        )
        assert resp.status_code == 200
        assert resp.json()["name"] == "new"
        assert resp.json()["template"] == "New template"
        assert resp.json()["description"] == "Updated"

    def test_partial_update_prompt(self, client: TestClient) -> None:
        created = client.post(
            "/items/prompts",
            json={"name": "keep", "template": "Keep this", "description": "Original"},
        ).json()
        resp = client.patch(
            f"/items/prompts/{created['id']}",
            json={"template": "Changed"},
        )
        assert resp.status_code == 200
        assert resp.json()["name"] == "keep"
        assert resp.json()["template"] == "Changed"
        assert resp.json()["description"] == "Original"

    def test_delete_prompt(self, client: TestClient) -> None:
        created = client.post(
            "/items/prompts",
            json={"name": "gone", "template": "Bye"},
        ).json()
        resp = client.delete(f"/items/prompts/{created['id']}")
        assert resp.status_code == 204
        assert client.get(f"/items/prompts/{created['id']}").status_code == 404

    def test_delete_missing_prompt_is_404(self, client: TestClient) -> None:
        resp = client.delete("/items/prompts/nonexistent")
        assert resp.status_code == 404

    def test_create_rejects_duplicate_name(self, client: TestClient) -> None:
        client.post("/items/prompts", json={"name": "dup", "template": "A"})
        resp = client.post("/items/prompts", json={"name": "dup", "template": "B"})
        assert resp.status_code == 409


# ── Local Resources ─────────────────────────────────────────────────────


class TestLocalResources:
    def test_list_empty_by_default(self, client: TestClient) -> None:
        assert client.get("/items/resources").json() == []

    def test_create_resource(self, client: TestClient) -> None:
        resp = client.post(
            "/items/resources",
            json={
                "uri": "file:///docs/readme.md",
                "name": "Readme",
                "mime_type": "text/markdown",
                "content": "# Hello",
            },
        )
        assert resp.status_code == 201
        body = resp.json()
        assert body["uri"] == "file:///docs/readme.md"
        assert body["name"] == "Readme"
        assert body["mime_type"] == "text/markdown"
        assert body["content"] == "# Hello"
        assert "id" in body

    def test_create_resource_minimal(self, client: TestClient) -> None:
        resp = client.post(
            "/items/resources",
            json={"uri": "https://example.com/data", "name": "Data"},
        )
        assert resp.status_code == 201
        assert resp.json()["mime_type"] is None
        assert resp.json()["content"] is None

    def test_get_resource(self, client: TestClient) -> None:
        created = client.post(
            "/items/resources",
            json={"uri": "file:///a.txt", "name": "A", "content": "aaa"},
        ).json()
        resp = client.get(f"/items/resources/{created['id']}")
        assert resp.status_code == 200
        assert resp.json()["content"] == "aaa"

    def test_get_missing_resource_is_404(self, client: TestClient) -> None:
        resp = client.get("/items/resources/nonexistent")
        assert resp.status_code == 404

    def test_update_resource(self, client: TestClient) -> None:
        created = client.post(
            "/items/resources",
            json={"uri": "file:///old", "name": "Old", "content": "old"},
        ).json()
        resp = client.patch(
            f"/items/resources/{created['id']}",
            json={"uri": "file:///new", "name": "New", "content": "new"},
        )
        assert resp.status_code == 200
        assert resp.json()["uri"] == "file:///new"
        assert resp.json()["content"] == "new"

    def test_partial_update_resource(self, client: TestClient) -> None:
        created = client.post(
            "/items/resources",
            json={"uri": "file:///keep", "name": "Keep", "content": "original"},
        ).json()
        resp = client.patch(
            f"/items/resources/{created['id']}",
            json={"content": "updated"},
        )
        assert resp.status_code == 200
        assert resp.json()["uri"] == "file:///keep"
        assert resp.json()["content"] == "updated"

    def test_delete_resource(self, client: TestClient) -> None:
        created = client.post(
            "/items/resources",
            json={"uri": "file:///gone", "name": "Gone"},
        ).json()
        resp = client.delete(f"/items/resources/{created['id']}")
        assert resp.status_code == 204
        assert client.get(f"/items/resources/{created['id']}").status_code == 404

    def test_delete_missing_resource_is_404(self, client: TestClient) -> None:
        resp = client.delete("/items/resources/nonexistent")
        assert resp.status_code == 404

    def test_create_rejects_duplicate_uri(self, client: TestClient) -> None:
        client.post("/items/resources", json={"uri": "file:///dup", "name": "A"})
        resp = client.post("/items/resources", json={"uri": "file:///dup", "name": "B"})
        assert resp.status_code == 409
