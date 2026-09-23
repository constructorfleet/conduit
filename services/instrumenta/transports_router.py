"""Downstream MCP transport toggles.

Mounted at `/transports`. Instrumenta exposes the merged MCP surface over
two transports (spec #198, User Stories 16/17); each can be switched off
here so an operator can narrow the surface to a single transport. The flag
is persisted by the backend and enforced by the gate in `app.py` on every
request to the transport's mount.
"""

from __future__ import annotations

import logging

from fastapi import APIRouter, Depends, HTTPException, Request
from pydantic import BaseModel

from .backend import TRANSPORTS, Backend, TransportFlag

LOG = logging.getLogger("instrumenta.transports")

# Mount prefix per transport, derived from the backend's `TRANSPORTS` so the
# two lists cannot drift. The SDK sub-apps are built with their own path
# set to `/`, so the client-facing endpoint is the prefix plus a trailing
# slash.
TRANSPORT_MOUNTS: dict[str, str] = {name: f"/mcp/{name}" for name in TRANSPORTS}


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
    async def list_transports(backend: Backend = Depends(_backend)) -> list[TransportRead]:
        return [TransportRead.from_row(row) for row in backend.list_transport_flags()]

    @router.put("/{transport}", response_model=TransportRead)
    async def set_transport(
        transport: str, body: TransportUpdate, backend: Backend = Depends(_backend)
    ) -> TransportRead:
        if transport not in TRANSPORTS:
            raise HTTPException(
                status_code=404,
                detail=f"unknown transport {transport!r}; expected one of {list(TRANSPORTS)}",
            )
        backend.set_transport_enabled(transport, body.enabled)
        LOG.info(
            "transport toggle updated",
            extra={"transport": transport, "enabled": body.enabled},
        )
        return TransportRead.from_row(TransportFlag(transport=transport, enabled=body.enabled))

    return router
