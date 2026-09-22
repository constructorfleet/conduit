"""Downstream MCP transport toggles.

Mounted at `/transports`. Instrumenta exposes the merged MCP surface over
two transports (spec #198, User Stories 16/17); each can be switched off
here so an operator can narrow the surface to a single transport. The flag
is persisted by the backend and enforced by the gate in `app.py` on every
request to the transport's mount.
"""

from __future__ import annotations

import logging

from fastapi import APIRouter, HTTPException, Request
from pydantic import BaseModel

from .backend import TRANSPORTS, Backend, TransportFlag

LOG = logging.getLogger("instrumenta.transports")

# Mount prefix per transport. The SDK sub-apps are built with their own path
# set to `/`, so the client-facing endpoint is the prefix plus a trailing
# slash.
TRANSPORT_MOUNTS: dict[str, str] = {
    "http": "/mcp/http",
    "sse": "/mcp/sse",
}


def endpoint_path(transport: str) -> str:
    return TRANSPORT_MOUNTS[transport] + "/"


class TransportUpdate(BaseModel):
    enabled: bool


class TransportRead(BaseModel):
    transport: str
    path: str
    enabled: bool

    @classmethod
    def from_row(cls, row: TransportFlag) -> "TransportRead":
        return cls(
            transport=row.transport,
            path=endpoint_path(row.transport),
            enabled=row.enabled,
        )


def _backend(request: Request) -> Backend:
    return request.app.state.backend


def make_transports_router() -> APIRouter:
    router = APIRouter(prefix="/transports", tags=["transports"])

    @router.get("", response_model=list[TransportRead])
    async def list_transports(request: Request) -> list[TransportRead]:
        return [TransportRead.from_row(row) for row in _backend(request).list_transport_flags()]

    @router.put("/{transport}", response_model=TransportRead)
    async def set_transport(
        transport: str, body: TransportUpdate, request: Request
    ) -> TransportRead:
        if transport not in TRANSPORTS:
            raise HTTPException(
                status_code=404,
                detail=f"unknown transport {transport!r}; expected one of {list(TRANSPORTS)}",
            )
        backend = _backend(request)
        backend.set_transport_enabled(transport, body.enabled)
        LOG.info(
            "transport toggle updated",
            extra={"transport": transport, "enabled": body.enabled},
        )
        return TransportRead(
            transport=transport,
            path=endpoint_path(transport),
            enabled=backend.is_transport_enabled(transport),
        )

    return router
