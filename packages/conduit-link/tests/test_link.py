"""Behavioural tests for the shared link module."""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient

from conduit_link import (
    InMemoryConduitLinkClient,
    LinkConfig,
    LinkRequest,
    LinkStore,
    LinkStoreSecurityError,
    LinkedServiceKind,
    LinkedServicePanel,
    make_link_router,
)
from conduit_link.models import LinkState


@dataclass(frozen=True)
class FakeExtension:
    api_key: str
    provider_id: str


def _ext_from(payload: Mapping[str, object]) -> FakeExtension:
    return FakeExtension(
        api_key=str(payload["api_key"]),
        provider_id=str(payload["provider_id"]),
    )


def _ext_to(extension: FakeExtension) -> dict[str, object]:
    return {"api_key": extension.api_key, "provider_id": extension.provider_id}


def _build_create_body(
    request: LinkRequest, existing: FakeExtension | None
) -> dict[str, object]:
    return {
        "peer_name": request.peer_name,
        "peer_id": "test-peer",
        "peer_base_url": "http://peer.test",
    }


def _build_extension(
    request: LinkRequest,
    response: Mapping[str, str],
    existing: FakeExtension | None,
) -> FakeExtension:
    return FakeExtension(
        api_key=(existing.api_key if existing else "generated-key"),
        provider_id=response.get("provider_id", "auto-provisioned"),
    )


def _public(extension: FakeExtension) -> dict[str, object]:
    return {"provider_id": extension.provider_id}


def _make_config(tmp_path: Path) -> LinkConfig:
    return LinkConfig(
        service_kind=LinkedServiceKind.VOX,
        peer_name="test",
        peer_base_url="http://peer.test",
        panel=LinkedServicePanel(title="Test", path="/ui/", icon="test"),
        storage_dir=tmp_path,
    )


def _make_store(tmp_path: Path) -> LinkStore[FakeExtension]:
    return LinkStore(
        tmp_path,
        extension_from_dict=_ext_from,
        extension_to_dict=_ext_to,
    )


def _make_client(tmp_path: Path) -> TestClient:
    config = _make_config(tmp_path)
    store = _make_store(tmp_path)
    fake = InMemoryConduitLinkClient(extra_response_fields={"provider_id": "auto-42"})
    app = FastAPI()
    app.include_router(
        make_link_router(
            config=config,
            store=store,
            client=fake,
            build_create_body=_build_create_body,
            build_extension=_build_extension,
            public_response=_public,
        )
    )
    app.state.fake_client = fake
    app.state.store = store
    return TestClient(app)


def test_health_endpoint(tmp_path: Path) -> None:
    client = _make_client(tmp_path)
    response = client.get("/link/health")
    assert response.status_code == 200
    assert response.json() == {"status": "ok"}


def test_status_unlinked(tmp_path: Path) -> None:
    client = _make_client(tmp_path)
    response = client.get("/link")
    assert response.status_code == 200
    assert response.json() == {"status": "unlinked"}


def test_link_creates_and_persists(tmp_path: Path) -> None:
    client = _make_client(tmp_path)
    response = client.post(
        "/link",
        json={
            "conduit_url": "http://conduit.test",
            "operator_token": "op-token",
            "peer_name": "My Peer",
        },
    )
    assert response.status_code == 200, response.text
    body = response.json()
    assert body["status"] == "linked"
    assert body["peer_id"] == "test-peer"
    assert body["peer_name"] == "My Peer"
    assert body["provider_id"] == "auto-42"

    fake = client.app.state.fake_client
    assert fake.last_create_body == {
        "peer_name": "My Peer",
        "peer_id": "test-peer",
        "peer_base_url": "http://peer.test",
    }

    persisted = tmp_path / "link.json"
    assert persisted.exists()
    assert persisted.stat().st_mode & 0o777 == 0o600, "link.json must be 0600"


def test_link_conflict_without_force(tmp_path: Path) -> None:
    client = _make_client(tmp_path)
    payload = {
        "conduit_url": "http://conduit.test",
        "operator_token": "op-token",
        "peer_name": "P",
    }
    client.post("/link", json=payload)
    response = client.post("/link", json=payload)
    assert response.status_code == 409


def test_link_force_replaces(tmp_path: Path) -> None:
    client = _make_client(tmp_path)
    payload = {
        "conduit_url": "http://conduit.test",
        "operator_token": "op-token",
        "peer_name": "P",
    }
    client.post("/link", json=payload)
    response = client.post("/link", json={**payload, "force": True, "peer_name": "P2"})
    assert response.status_code == 200
    assert response.json()["peer_name"] == "P2"


def test_delete_unlinks(tmp_path: Path) -> None:
    client = _make_client(tmp_path)
    client.post(
        "/link",
        json={
            "conduit_url": "http://conduit.test",
            "operator_token": "op-token",
            "peer_name": "P",
        },
    )
    response = client.delete("/link")
    assert response.status_code == 204
    assert not (tmp_path / "link.json").exists()

    fake = client.app.state.fake_client
    assert fake.last_delete_peer_id == "test-peer"


def test_delete_on_unlinked_is_no_op(tmp_path: Path) -> None:
    client = _make_client(tmp_path)
    response = client.delete("/link")
    assert response.status_code == 204


def test_loose_permissions_refused(tmp_path: Path) -> None:
    store = _make_store(tmp_path)
    state = LinkState(
        conduit_url="http://conduit.test",
        peer_id="p",
        peer_name="P",
        sync_token="tok",
        panel=LinkedServicePanel(title="T", path="/", icon=None),
        linked_at="2026-01-01T00:00:00+00:00",
    )
    store.save(state, FakeExtension(api_key="k", provider_id="pv"))
    os.chmod(tmp_path / "link.json", 0o644)
    with pytest.raises(LinkStoreSecurityError):
        store.load()


def test_extension_round_trips(tmp_path: Path) -> None:
    store = _make_store(tmp_path)
    state = LinkState(
        conduit_url="http://conduit.test",
        peer_id="p",
        peer_name="P",
        sync_token="tok",
        panel=LinkedServicePanel(title="T", path="/", icon="i"),
        linked_at="2026-01-01T00:00:00+00:00",
    )
    ext = FakeExtension(api_key="secret", provider_id="pv-123")
    store.save(state, ext)

    loaded = store.load()
    assert loaded is not None
    assert loaded.state == state
    assert loaded.extension == ext
