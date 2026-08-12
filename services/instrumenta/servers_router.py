"""CRUD routes for upstream MCP server configuration.

Mounted at `/servers`. Config changes are written to SQLite immediately, but
the running aggregator is NOT hot-reloaded in v1 — new/removed rows take
effect on the next Instrumenta restart. This is documented on each write
route's response body so operators can see the current picture.
"""

from __future__ import annotations

import uuid

from fastapi import APIRouter, Depends, HTTPException, Request
from pydantic import BaseModel, Field

from .backend import Backend, UpstreamServer
from .secret_box import SecretBox


class ServerCreate(BaseModel):
    name: str = Field(..., min_length=1, max_length=64, pattern=r"^[a-z0-9][a-z0-9-]*$")
    url: str = Field(..., min_length=1)
    secret: str | None = None
    enabled: bool = True
    timeout_seconds: int | None = Field(default=None, ge=1, le=600)


class ServerUpdate(BaseModel):
    url: str | None = Field(default=None, min_length=1)
    secret: str | None = None
    clear_secret: bool = False
    enabled: bool | None = None
    timeout_seconds: int | None = Field(default=None, ge=1, le=600)


class ServerRead(BaseModel):
    id: str
    name: str
    transport: str
    url: str | None
    enabled: bool
    timeout_seconds: int | None
    has_secret: bool

    @classmethod
    def from_row(cls, row: UpstreamServer) -> "ServerRead":
        return cls(
            id=row.id,
            name=row.name,
            transport=row.transport,
            url=row.url,
            enabled=row.enabled,
            timeout_seconds=row.timeout_seconds,
            has_secret=row.secret_ciphertext is not None,
        )


def _backend(request: Request) -> Backend:
    return request.app.state.backend


def _secret_box(request: Request) -> SecretBox:
    return request.app.state.secret_box


def make_servers_router() -> APIRouter:
    router = APIRouter(prefix="/servers", tags=["servers"])

    _RESTART_NOTE = (
        "Config saved. Restart Instrumenta to update the aggregated tool set."
    )

    @router.get("", response_model=list[ServerRead])
    async def list_servers(backend: Backend = Depends(_backend)) -> list[ServerRead]:
        return [ServerRead.from_row(row) for row in backend.list_upstream_servers()]

    @router.post("", response_model=ServerRead, status_code=201)
    async def create_server(
        payload: ServerCreate,
        backend: Backend = Depends(_backend),
        secret_box: SecretBox = Depends(_secret_box),
    ) -> ServerRead:
        ciphertext: bytes | None = None
        if payload.secret is not None:
            ciphertext = secret_box.encrypt(payload.secret)

        server = UpstreamServer(
            id=str(uuid.uuid4()),
            name=payload.name,
            transport="http",
            url=payload.url,
            command=None,
            secret_ciphertext=ciphertext,
            enabled=payload.enabled,
            timeout_seconds=payload.timeout_seconds,
        )
        try:
            backend.insert_upstream_server(server)
        except Exception as exc:  # sqlite unique-constraint
            raise HTTPException(status_code=409, detail=str(exc))
        return ServerRead.from_row(server)

    @router.get("/{server_id}", response_model=ServerRead)
    async def get_server(
        server_id: str, backend: Backend = Depends(_backend)
    ) -> ServerRead:
        row = backend.get_upstream_server(server_id)
        if row is None:
            raise HTTPException(status_code=404, detail="server not found")
        return ServerRead.from_row(row)

    @router.patch("/{server_id}", response_model=ServerRead)
    async def update_server(
        server_id: str,
        payload: ServerUpdate,
        backend: Backend = Depends(_backend),
        secret_box: SecretBox = Depends(_secret_box),
    ) -> ServerRead:
        current = backend.get_upstream_server(server_id)
        if current is None:
            raise HTTPException(status_code=404, detail="server not found")

        new_ciphertext = current.secret_ciphertext
        if payload.clear_secret:
            new_ciphertext = None
        if payload.secret is not None:
            new_ciphertext = secret_box.encrypt(payload.secret)

        updated = UpstreamServer(
            id=current.id,
            name=current.name,
            transport=current.transport,
            url=payload.url if payload.url is not None else current.url,
            command=current.command,
            secret_ciphertext=new_ciphertext,
            enabled=payload.enabled if payload.enabled is not None else current.enabled,
            timeout_seconds=(
                payload.timeout_seconds
                if payload.timeout_seconds is not None
                else current.timeout_seconds
            ),
        )
        backend.update_upstream_server(updated)
        return ServerRead.from_row(updated)

    @router.delete("/{server_id}", status_code=204)
    async def delete_server(
        server_id: str, backend: Backend = Depends(_backend)
    ) -> None:
        deleted = backend.delete_upstream_server(server_id)
        if not deleted:
            raise HTTPException(status_code=404, detail="server not found")

    # Expose the note so a UI can render it after a mutation; unused now.
    router.state_note = _RESTART_NOTE  # type: ignore[attr-defined]
    return router
