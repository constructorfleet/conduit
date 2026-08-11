"""Client-side of the link handshake (peer → Conduit).

Services depend on the `ConduitLinkClient` Protocol; production code injects
`HttpConduitLinkClient`, tests inject `InMemoryConduitLinkClient`.
"""

from __future__ import annotations

import secrets
from typing import Mapping, Protocol

import httpx


class ConduitLinkClient(Protocol):
    """Peer-side view of Conduit's `/v1/linked-services` surface (spec 0005)."""

    def create_link(
        self,
        conduit_url: str,
        operator_token: str,
        body: Mapping[str, object],
    ) -> Mapping[str, str]:
        """POST /v1/linked-services — returns at minimum a `sync_token`.

        Additional response fields (e.g. `provider_definition_id` for Vox) are
        service-specific and surface through the returned mapping.
        """
        ...

    def delete_link(self, conduit_url: str, peer_id: str, sync_token: str) -> None:
        """DELETE /v1/linked-services/{peer_id} — best-effort unlink."""
        ...


class HttpConduitLinkClient:
    """Real HTTP client — hits the versioned `/v1/linked-services` endpoint."""

    def __init__(
        self,
        *,
        timeout: float = 10.0,
        transport: httpx.BaseTransport | None = None,
    ) -> None:
        self._timeout = timeout
        self._transport = transport

    def create_link(
        self,
        conduit_url: str,
        operator_token: str,
        body: Mapping[str, object],
    ) -> Mapping[str, str]:
        url = f"{conduit_url.rstrip('/')}/v1/linked-services"
        with httpx.Client(timeout=self._timeout, transport=self._transport) as client:
            response = client.post(
                url,
                json=dict(body),
                headers={"authorization": f"Bearer {operator_token}"},
            )
        response.raise_for_status()
        payload = response.json()
        return {str(key): str(value) for key, value in payload.items()}

    def delete_link(self, conduit_url: str, peer_id: str, sync_token: str) -> None:
        url = f"{conduit_url.rstrip('/')}/v1/linked-services/{peer_id}"
        with httpx.Client(timeout=self._timeout, transport=self._transport) as client:
            response = client.delete(
                url,
                headers={"authorization": f"Bearer {sync_token}"},
            )
        # Best-effort: a non-2xx unlink is logged by the caller, not raised.
        _ = response.status_code


class InMemoryConduitLinkClient:
    """Test fake — no HTTP. Services use this in unit tests via the Protocol.

    Mints a deterministic `sync_token` on each `create_link` and remembers the
    request body so tests can assert what the peer sent.
    """

    def __init__(
        self,
        *,
        extra_response_fields: Mapping[str, str] | None = None,
    ) -> None:
        self._extra = dict(extra_response_fields or {})
        self.last_create_body: Mapping[str, object] | None = None
        self.last_delete_peer_id: str | None = None
        self.last_delete_token: str | None = None

    def create_link(
        self,
        conduit_url: str,
        operator_token: str,
        body: Mapping[str, object],
    ) -> Mapping[str, str]:
        self.last_create_body = dict(body)
        response = {"sync_token": secrets.token_urlsafe(32)}
        response.update(self._extra)
        return response

    def delete_link(self, conduit_url: str, peer_id: str, sync_token: str) -> None:
        self.last_delete_peer_id = peer_id
        self.last_delete_token = sync_token
