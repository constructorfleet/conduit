"""Client-side of the link handshake (peer → Conduit).

Services depend on the `ConduitLinkClient` Protocol; production code injects
`HttpConduitLinkClient`, tests inject `InMemoryConduitLinkClient`.
"""

from __future__ import annotations

import httpx
from typing import Mapping, Protocol


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
        raise NotImplementedError

    def create_link(
        self,
        conduit_url: str,
        operator_token: str,
        body: Mapping[str, object],
    ) -> Mapping[str, str]:
        raise NotImplementedError

    def delete_link(self, conduit_url: str, peer_id: str, sync_token: str) -> None:
        raise NotImplementedError


class InMemoryConduitLinkClient:
    """Test fake — no HTTP. Services use this in unit tests via the Protocol."""

    def __init__(self) -> None:
        raise NotImplementedError

    def create_link(
        self,
        conduit_url: str,
        operator_token: str,
        body: Mapping[str, object],
    ) -> Mapping[str, str]:
        raise NotImplementedError

    def delete_link(self, conduit_url: str, peer_id: str, sync_token: str) -> None:
        raise NotImplementedError
