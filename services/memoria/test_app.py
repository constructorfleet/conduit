"""Tests for Memoria service."""

import json
import asyncio
import os
import stat
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

import pytest
from fastapi.testclient import TestClient

os.environ.setdefault("MEMORIA_METRICS_BIND", "127.0.0.1:0")

from app import app
import app as memoria_app
from conduit_link import LinkRecord, LinkState, LinkedServicePanel


class _ConduitLinkHandler(BaseHTTPRequestHandler):
    requests: list[dict[str, Any]] = []

    def do_POST(self) -> None:
        length = int(self.headers.get("content-length", "0"))
        body = json.loads(self.rfile.read(length))
        required_panel = {
            "id": "memoria",
            "label": "Memoria",
            "icon": "brain",
            "path": "/ui/",
        }
        if body.get("panel") != required_panel:
            self.send_response(422)
            self.send_header("content-type", "application/json")
            self.end_headers()
            self.wfile.write(
                json.dumps(
                    {
                        "error": "panel must use Conduit LinkedServicePanel fields",
                        "expected": required_panel,
                    }
                ).encode()
            )
            return
        self.requests.append(
            {
                "method": "POST",
                "path": self.path,
                "authorization": self.headers.get("authorization"),
                "body": body,
            }
        )
        self.send_response(201)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"sync_token": "sync-token-from-conduit"}).encode())

    def do_DELETE(self) -> None:
        self.requests.append(
            {
                "method": "DELETE",
                "path": self.path,
                "authorization": self.headers.get("authorization"),
            }
        )
        self.send_response(204)
        self.end_headers()

    def log_message(self, _format: str, *_args: Any) -> None:
        return


def test_sync_reconciles_speaker_and_conversation_rosters_without_touching_engrams():
    class Response:
        def __init__(self, payload: list[dict[str, Any]]) -> None:
            self.payload = payload

        def raise_for_status(self) -> None:
            return

        def json(self) -> list[dict[str, Any]]:
            return self.payload

    class Client:
        def __init__(self) -> None:
            self.calls: list[tuple[str, str]] = []

        async def get(self, url: str, headers: dict[str, str]) -> Response:
            self.calls.append((url, headers["authorization"]))
            if url.endswith("/speakers"):
                return Response([{"id": "speaker-1", "name": "Ada", "samples": 2}])
            return Response([{"conversation_id": "conversation-1", "turn_count": 3}])

    client = Client()
    record = LinkRecord(
        state=LinkState(
            conduit_url="http://conduit:8080",
            peer_id="memoria-home",
            peer_name="Home",
            sync_token="link-sync-secret",
            panel=LinkedServicePanel(title="Memoria", path="/ui/"),
            linked_at="2026-01-01T00:00:00Z",
        ),
        extension=None,
    )
    memoria_app.speaker_roster = [{"id": "speaker-old", "name": "Old"}]
    memoria_app.conversation_roster = [{"conversation_id": "conversation-old"}]

    asyncio.run(memoria_app.sync_rosters_once(record, client))

    assert client.calls == [
        ("http://conduit:8080/v1/linked-services/memoria-home/roster/speakers", "Bearer link-sync-secret"),
        ("http://conduit:8080/v1/linked-services/memoria-home/roster/conversations", "Bearer link-sync-secret"),
    ]
    assert memoria_app.speaker_roster == [{"id": "speaker-1", "name": "Ada", "samples": 2}]
    assert memoria_app.conversation_roster == [{"conversation_id": "conversation-1", "turn_count": 3}]


def test_failed_roster_request_keeps_the_last_complete_snapshot():
    class Response:
        def __init__(self, payload: list[dict[str, Any]], fail: bool = False) -> None:
            self.payload = payload
            self.fail = fail

        def raise_for_status(self) -> None:
            if self.fail:
                raise RuntimeError("Conduit unavailable")

        def json(self) -> list[dict[str, Any]]:
            return self.payload

    class Client:
        async def get(self, url: str, headers: dict[str, str]) -> Response:
            return Response([], fail=url.endswith("/conversations"))

    record = LinkRecord(
        state=LinkState(
            conduit_url="http://conduit:8080", peer_id="memoria-home", peer_name="Home",
            sync_token="link-sync-secret",
            panel=LinkedServicePanel(title="Memoria", path="/ui/"),
            linked_at="2026-01-01T00:00:00Z",
        ),
        extension=None,
    )
    previous_speakers = [{"id": "speaker-1", "name": "Ada"}]
    previous_conversations = [{"conversation_id": "conversation-1"}]
    memoria_app.speaker_roster = previous_speakers.copy()
    memoria_app.conversation_roster = previous_conversations.copy()

    with pytest.raises(RuntimeError, match="Conduit unavailable"):
        asyncio.run(memoria_app.sync_rosters_once(record, Client()))

    assert memoria_app.speaker_roster == previous_speakers
    assert memoria_app.conversation_roster == previous_conversations


def test_synced_rosters_are_available_through_the_memoria_api(client):
    memoria_app.speaker_roster = [{"id": "speaker-1", "name": "Ada"}]
    memoria_app.conversation_roster = [{"conversation_id": "conversation-1", "turn_count": 2}]
    memoria_app.roster_sync_state = {"last_synced_at": "2026-09-22T12:00:00+00:00", "error": None}

    assert client.get("/roster/speakers").json() == [{"id": "speaker-1", "name": "Ada"}]
    assert client.get("/roster/conversations").json() == [
        {"conversation_id": "conversation-1", "turn_count": 2}
    ]
    assert client.get("/roster/sync").json()["error"] is None


@pytest.fixture
def conduit_server():
    """Run a tiny Conduit-compatible link endpoint for Memoria link tests."""
    _ConduitLinkHandler.requests = []
    server = ThreadingHTTPServer(("127.0.0.1", 0), _ConduitLinkHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}", _ConduitLinkHandler.requests
    finally:
        server.shutdown()
        thread.join()
        server.server_close()


@pytest.fixture
def client(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    """Create an in-process test HTTP client with isolated storage."""
    monkeypatch.setenv("MEMORIA_BACKEND", "builtin")
    monkeypatch.setenv("MEMORIA_DATA_DIR", str(tmp_path))
    monkeypatch.delenv("MEMORIA_API_KEY", raising=False)
    monkeypatch.setenv("MEMORIA_METRICS_BIND", "127.0.0.1:0")

    with TestClient(app) as test_client:
        yield test_client


@pytest.fixture
def linked_client(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    """Create a test client whose isolated storage already contains a link."""
    monkeypatch.setenv("MEMORIA_BACKEND", "builtin")
    monkeypatch.setenv("MEMORIA_DATA_DIR", str(tmp_path))
    monkeypatch.delenv("MEMORIA_API_KEY", raising=False)
    monkeypatch.setenv("MEMORIA_METRICS_BIND", "127.0.0.1:0")

    link_file = tmp_path / "link.json"
    link_file.write_text(
        json.dumps(
            {
                "conduit_url": "http://conduit.example.test",
                "peer_id": "memoria-test-peer",
                "peer_name": "fixture-memoria",
                "sync_token": "fixture-sync-token",
                "panel": {
                    "title": "Memoria",
                    "path": "/ui/",
                    "icon": "brain",
                },
                "linked_at": "2026-09-14T12:00:00+00:00",
                "extension": {},
            }
        )
    )
    link_file.chmod(stat.S_IRUSR | stat.S_IWUSR)

    with TestClient(app) as test_client:
        yield test_client


@pytest.fixture
def api_key():
    """Get test API key from environment."""
    import os
    return os.getenv("MEMORIA_API_KEY")


@pytest.fixture
def headers(api_key):
    """Create request headers with authentication."""
    headers = {}
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    return headers


class TestHealth:
    """Health check endpoint tests."""

    def test_health_check(self, client):
        """Test health check returns status ok."""
        response = client.get("/health")
        assert response.status_code == 200

        data = response.json()
        assert data["status"] == "ok"
        assert "backend" in data
        assert "engram_count" in data
        assert "linked" in data


class TestEngrams:
    """Engram CRUD operation tests."""

    def test_store_engram(self, client, headers):
        """Test storing a new engram."""
        engram_data = {
            "content": "The user prefers tea over coffee in the morning",
            "scope": "global",
            "metadata": {"preference": "beverage"},
        }

        response = client.post("/engrams", json=engram_data, headers=headers)
        assert response.status_code == 200

        data = response.json()
        assert "id" in data
        assert data["content"] == engram_data["content"]
        assert data["scope"] == engram_data["scope"]
        assert data["metadata"] == engram_data["metadata"]
        assert "created_at" in data
        assert "updated_at" in data

    def test_store_speaker_engram(self, client, headers):
        """Test storing a speaker-specific engram."""
        engram_data = {
            "content": "Alice mentioned she has a cat named Whiskers",
            "speaker_id": "speaker-123",
            "scope": "speaker",
            "conversation_id": "conv-456",
        }

        response = client.post("/engrams", json=engram_data, headers=headers)
        assert response.status_code == 200

        data = response.json()
        assert data["speaker_id"] == engram_data["speaker_id"]
        assert data["scope"] == engram_data["scope"]
        assert data["conversation_id"] == engram_data["conversation_id"]

    def test_get_engram(self, client, headers):
        """Test retrieving a specific engram."""
        # First create an engram
        create_data = {
            "content": "Test content for retrieval",
            "scope": "global",
        }
        create_response = client.post("/engrams", json=create_data, headers=headers)
        engram_id = create_response.json()["id"]

        # Then retrieve it
        response = client.get(f"/engrams/{engram_id}", headers=headers)
        assert response.status_code == 200

        data = response.json()
        assert data["id"] == engram_id
        assert data["content"] == create_data["content"]

    def test_get_nonexistent_engram(self, client, headers):
        """Test retrieving a non-existent engram."""
        response = client.get("/engrams/nonexistent-id", headers=headers)
        assert response.status_code == 404

    def test_update_engram(self, client, headers):
        """Test updating an engram."""
        # First create an engram
        create_data = {
            "content": "Original content",
            "scope": "global",
        }
        create_response = client.post("/engrams", json=create_data, headers=headers)
        engram_id = create_response.json()["id"]

        # Update it
        update_data = {
            "content": "Updated content",
            "metadata": {"updated": True},
        }
        response = client.patch(f"/engrams/{engram_id}", json=update_data, headers=headers)
        assert response.status_code == 200

        data = response.json()
        assert data["content"] == update_data["content"]
        assert data["metadata"] == update_data["metadata"]
        assert data["updated_at"] > data["created_at"]

    def test_delete_engram(self, client, headers):
        """Test deleting an engram."""
        # First create an engram
        create_data = {
            "content": "Content to delete",
            "scope": "global",
        }
        create_response = client.post("/engrams", json=create_data, headers=headers)
        engram_id = create_response.json()["id"]

        # Delete it
        response = client.delete(f"/engrams/{engram_id}", headers=headers)
        assert response.status_code == 204

        # Verify it's gone
        get_response = client.get(f"/engrams/{engram_id}", headers=headers)
        assert get_response.status_code == 404

    def test_delete_nonexistent_engram(self, client, headers):
        """Test deleting a non-existent engram."""
        response = client.delete("/engrams/nonexistent-id", headers=headers)
        assert response.status_code == 404


class TestEngramListing:
    """Engram listing and filtering tests."""

    def test_list_all_engrams(self, client, headers):
        """Test listing all engrams."""
        # Create a few engrams
        for i in range(3):
            client.post(
                "/engrams",
                json={"content": f"Test content {i}", "scope": "global"},
                headers=headers,
            )

        response = client.get("/engrams", headers=headers)
        assert response.status_code == 200

        data = response.json()
        assert isinstance(data, list)
        assert len(data) >= 3

    def test_list_by_scope(self, client, headers):
        """Test listing engrams filtered by scope."""
        # Create engrams with different scopes
        client.post("/engrams", json={"content": "Global memory", "scope": "global"}, headers=headers)
        client.post(
            "/engrams",
            json={"content": "Speaker memory", "scope": "speaker", "speaker_id": "speaker-1"},
            headers=headers,
        )

        # List only global engrams
        response = client.get("/engrams?scope=global", headers=headers)
        assert response.status_code == 200

        data = response.json()
        for engram in data:
            assert engram["scope"] == "global"

    def test_list_by_speaker(self, client, headers):
        """Test listing engrams for a specific speaker."""
        speaker_id = "test-speaker-123"

        # Create engrams for different speakers
        client.post(
            "/engrams",
            json={"content": "Memory for speaker 1", "scope": "speaker", "speaker_id": speaker_id},
            headers=headers,
        )
        client.post(
            "/engrams",
            json={"content": "Memory for speaker 2", "scope": "speaker", "speaker_id": "other-speaker"},
            headers=headers,
        )

        # List engrams for specific speaker
        response = client.get(f"/engrams/speakers/{speaker_id}", headers=headers)
        assert response.status_code == 200

        data = response.json()
        for engram in data:
            assert engram["speaker_id"] == speaker_id

    def test_list_by_conversation(self, client, headers):
        """Test listing engrams for a specific conversation."""
        conversation_id = "test-conv-456"

        # Create engrams for different conversations
        client.post(
            "/engrams",
            json={
                "content": "Memory for conversation 1",
                "scope": "conversation",
                "conversation_id": conversation_id,
            },
            headers=headers,
        )
        client.post(
            "/engrams",
            json={
                "content": "Memory for conversation 2",
                "scope": "conversation",
                "conversation_id": "other-conv",
            },
            headers=headers,
        )

        # List engrams for specific conversation
        response = client.get(f"/engrams/conversations/{conversation_id}", headers=headers)
        assert response.status_code == 200

        data = response.json()
        for engram in data:
            assert engram["conversation_id"] == conversation_id

    def test_list_with_limit(self, client, headers):
        """Test listing engrams with limit."""
        response = client.get("/engrams?limit=5", headers=headers)
        assert response.status_code == 200

        data = response.json()
        assert len(data) <= 5


class TestEngramSearch:
    """Engram search functionality tests."""

    def test_search_engrams(self, client, headers):
        """Test searching engrams by content."""
        # Create some engrams
        client.post("/engrams", json={"content": "The user likes coffee", "scope": "global"}, headers=headers)
        client.post("/engrams", json={"content": "The user prefers tea", "scope": "global"}, headers=headers)
        client.post(
            "/engrams",
            json={"content": "Unrelated content about weather", "scope": "global"},
            headers=headers,
        )

        # Search for coffee-related content
        search_data = {"query": "coffee beverage drink", "limit": 10}
        response = client.post("/engrams/search", json=search_data, headers=headers)
        assert response.status_code == 200

        data = response.json()
        assert isinstance(data, list)
        assert len(data) > 0

        # Check that results contain expected fields
        for result in data:
            assert "engram" in result
            assert "score" in result
            assert 0 <= result["score"] <= 1

    def test_search_with_filters(self, client, headers):
        """Test searching engrams with filters."""
        speaker_id = "search-test-speaker"

        # Create engrams with different scopes and speakers
        client.post(
            "/engrams",
            json={"content": "User likes coffee", "scope": "global"},
            headers=headers,
        )
        client.post(
            "/engrams",
            json={"content": "User likes coffee", "scope": "speaker", "speaker_id": speaker_id},
            headers=headers,
        )

        # Search only speaker-scoped engrams
        search_data = {"query": "coffee", "scope": "speaker", "speaker_id": speaker_id}
        response = client.post("/engrams/search", json=search_data, headers=headers)
        assert response.status_code == 200

        data = response.json()
        for result in data:
            assert result["engram"]["scope"] == "speaker"
            assert result["engram"]["speaker_id"] == speaker_id

    def test_search_empty_query(self, client, headers):
        """Test searching with empty query."""
        search_data = {"query": "", "limit": 10}
        response = client.post("/engrams/search", json=search_data, headers=headers)
        assert response.status_code == 422  # Validation error


class TestLinking:
    """Conduit linking functionality tests."""

    def test_get_link_status(self, client):
        """Test getting unlinked status in the shared router shape."""
        response = client.get("/link")
        assert response.status_code == 200

        assert response.json() == {"status": "unlinked"}

    def test_get_link_health(self, client):
        """Test shared link reachability probe endpoint."""
        response = client.get("/link/health")
        assert response.status_code == 200

        assert response.json() == {"status": "ok"}

    def test_get_link_status_uses_isolated_test_storage(self, linked_client):
        """Test linked status reads the shared link store."""
        response = linked_client.get("/link")
        assert response.status_code == 200

        assert response.json() == {
            "status": "linked",
            "conduit_url": "http://conduit.example.test",
            "peer_id": "memoria-test-peer",
            "peer_name": "fixture-memoria",
            "linked_at": "2026-09-14T12:00:00+00:00",
        }

    def test_api_key_managed_service_keeps_spec_status_shape(self, tmp_path, monkeypatch):
        """Test config-managed is a field instead of an alternate status value."""
        monkeypatch.setenv("MEMORIA_BACKEND", "builtin")
        monkeypatch.setenv("MEMORIA_DATA_DIR", str(tmp_path))
        monkeypatch.setenv("MEMORIA_API_KEY", "configured-key")
        monkeypatch.setenv("MEMORIA_METRICS_BIND", "127.0.0.1:0")

        with TestClient(app) as client:
            response = client.get("/link")

        assert response.status_code == 200
        assert response.json() == {"status": "unlinked", "config_managed": True}

    def test_create_link_posts_spec_payload_and_persists_status(
        self, client, conduit_server
    ):
        """Test Memoria links through the shared router and generic Conduit API."""
        conduit_url, requests = conduit_server

        response = client.post(
            "/link",
            json={
                "conduit_url": f"{conduit_url}/",
                "operator_token": "operator-token",
                "peer_name": "Household Memory",
            },
        )

        assert response.status_code == 200
        body = response.json()
        assert body == {
            "status": "linked",
            "conduit_url": conduit_url,
            "peer_id": "household-memory",
            "peer_name": "Household Memory",
            "linked_at": body["linked_at"],
        }
        assert requests == [
            {
                "method": "POST",
                "path": "/v1/linked-services",
                "authorization": "Bearer operator-token",
                "body": {
                    "service_kind": "memoria",
                    "peer_name": "Household Memory",
                    "peer_id": "household-memory",
                    "peer_base_url": "http://localhost:8080",
                    "panel": {
                        "id": "memoria",
                        "label": "Memoria",
                        "icon": "brain",
                        "path": "/ui/",
                    },
                },
            }
        ]

        persisted = client.get("/link")
        assert persisted.status_code == 200
        assert persisted.json() == body
        assert "sync-token-from-conduit" not in persisted.text
        assert "operator-token" not in persisted.text

    def test_delete_link_revokes_conduit_and_returns_to_unlinked(
        self, client, conduit_server
    ):
        """Test unlinking delegates revoke to the shared router."""
        conduit_url, requests = conduit_server
        linked = client.post(
            "/link",
            json={
                "conduit_url": conduit_url,
                "operator_token": "operator-token",
                "peer_name": "Household Memory",
            },
        ).json()

        response = client.delete("/link")

        assert response.status_code == 204
        assert requests[-1] == {
            "method": "DELETE",
            "path": f"/v1/linked-services/{linked['peer_id']}",
            "authorization": "Bearer sync-token-from-conduit",
        }
        assert client.get("/link").json() == {"status": "unlinked"}

    def test_create_link_reports_unreachable_conduit(self, client, headers):
        """Test network-dependent link creation reports unreachable Conduit."""
        if os.getenv("MEMORIA_RUN_INTEGRATION_TESTS") != "1":
            pytest.skip("requires opt-in network-dependent link failure check")

        link_data = {
            "conduit_url": "http://localhost:8081",
            "operator_token": "test-token",
            "peer_name": "test-memoria",
        }

        response = client.post("/link", json=link_data, headers=headers)
        assert response.status_code == 502


class TestAuthentication:
    """Authentication tests."""

    def test_unauthenticated_request(self, tmp_path, monkeypatch):
        """Test that requests fail without authentication when API key is set."""
        monkeypatch.setenv("MEMORIA_BACKEND", "builtin")
        monkeypatch.setenv("MEMORIA_DATA_DIR", str(tmp_path))
        monkeypatch.setenv("MEMORIA_API_KEY", "test-key")
        monkeypatch.setenv("MEMORIA_METRICS_BIND", "127.0.0.1:0")

        with TestClient(app) as client:
            response = client.get("/engrams")

        assert response.status_code == 401

    def test_health_without_auth(self, client):
        """Test that health check works without authentication."""
        response = client.get("/health")
        assert response.status_code == 200


class TestValidation:
    """Input validation tests."""

    def test_store_empty_content(self, client, headers):
        """Test that storing engram with empty content fails."""
        engram_data = {"content": "", "scope": "global"}
        response = client.post("/engrams", json=engram_data, headers=headers)
        assert response.status_code == 422

    def test_store_invalid_scope(self, client, headers):
        """Test that storing engram with invalid scope fails."""
        engram_data = {"content": "Test", "scope": "invalid"}
        response = client.post("/engrams", json=engram_data, headers=headers)
        assert response.status_code == 422

    def test_store_too_long_content(self, client, headers):
        """Test that storing engram with too long content fails."""
        long_content = "x" * 10001  # Over 10000 character limit
        engram_data = {"content": long_content, "scope": "global"}
        response = client.post("/engrams", json=engram_data, headers=headers)
        assert response.status_code == 422

    def test_search_limit_bounds(self, client, headers):
        """Test that search limit respects bounds."""
        # Test minimum limit
        search_data = {"query": "test", "limit": 0}
        response = client.post("/engrams/search", json=search_data, headers=headers)
        assert response.status_code == 422

        # Test maximum limit
        search_data = {"query": "test", "limit": 101}
        response = client.post("/engrams/search", json=search_data, headers=headers)
        assert response.status_code == 422


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
