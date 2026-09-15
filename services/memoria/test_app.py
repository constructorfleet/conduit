"""Tests for Memoria service."""

import json
import os
import stat
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

os.environ.setdefault("MEMORIA_METRICS_BIND", "127.0.0.1:0")

from app import app


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
        """Test getting link status."""
        response = client.get("/link")
        assert response.status_code == 200

        data = response.json()
        assert "status" in data
        assert data["status"] in ["linked", "unlinked", "config-managed"]

    def test_get_link_status_uses_isolated_test_storage(self, linked_client):
        """Test link status reads the same isolated storage as the app lifespan."""
        response = linked_client.get("/link")
        assert response.status_code == 200

        data = response.json()
        assert data["status"] == "linked"
        assert data["peer_id"] == "memoria-test-peer"

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
