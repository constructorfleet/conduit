"""FastAPI router factory for the peer-side `/link` surface (spec 0005)."""

from __future__ import annotations

import logging
from datetime import datetime, timezone
from typing import Callable, Generic, Mapping, TypeVar

from fastapi import APIRouter, HTTPException, Response
from pydantic import BaseModel

from .client import ConduitLinkClient
from .config import LinkConfig
from .errors import LinkStoreSecurityError
from .models import LinkState
from .store import LinkStore

LOG = logging.getLogger(__name__)

E = TypeVar("E")


class LinkRequest(BaseModel):
    conduit_url: str
    operator_token: str
    peer_name: str
    force: bool = False


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def _trimmed(name: str, value: str) -> str:
    trimmed = value.strip()
    if not trimmed:
        raise HTTPException(status_code=422, detail=f"{name} must not be blank")
    return trimmed


def make_link_router(
    *,
    config: LinkConfig,
    store: "LinkStore[E]",
    client: ConduitLinkClient,
    build_create_body: Callable[[LinkRequest, E | None], Mapping[str, object]],
    build_extension: Callable[[LinkRequest, Mapping[str, str], E | None], E],
    public_response: Callable[[E], Mapping[str, object]],
) -> APIRouter:
    """Return an APIRouter exposing:

    - `GET /link` → status view (`unlinked` or the record)
    - `POST /link` → create/replace the link, delegating to `client.create_link`
    - `DELETE /link` → best-effort unlink through `client.delete_link`
    - `GET /link/health` → 0005 §Reachability probe target

    The three callbacks let each service inject its extension shape without
    the module knowing service-specific keys. Upholds spec 0005 §Handshake.
    """

    router = APIRouter()

    @router.get("/link/health")
    def health() -> dict[str, str]:
        return {"status": "ok"}

    @router.get("/link")
    def status() -> dict[str, object]:
        try:
            record = store.load()
        except LinkStoreSecurityError as error:
            raise HTTPException(status_code=500, detail=str(error)) from error
        if record is None:
            return {"status": "unlinked"}
        return {
            "status": "linked",
            "conduit_url": record.state.conduit_url,
            "peer_id": record.state.peer_id,
            "peer_name": record.state.peer_name,
            "linked_at": record.state.linked_at,
            **public_response(record.extension),
        }

    @router.post("/link")
    def create(body: LinkRequest) -> dict[str, object]:
        try:
            existing = store.load()
        except LinkStoreSecurityError as error:
            raise HTTPException(status_code=500, detail=str(error)) from error

        if existing is not None and not body.force:
            raise HTTPException(
                status_code=409,
                detail=(
                    f"{config.service_kind.value} is already linked; "
                    "unlink first or pass force=true"
                ),
            )

        conduit_url = _trimmed("conduit_url", body.conduit_url)
        operator_token = _trimmed("operator_token", body.operator_token)
        peer_name = _trimmed("peer_name", body.peer_name)

        create_body = dict(build_create_body(body, existing.extension if existing else None))

        try:
            response = client.create_link(conduit_url, operator_token, create_body)
        except Exception as error:
            raise HTTPException(
                status_code=502, detail=f"could not reach Conduit: {error}"
            ) from error

        sync_token = response.get("sync_token")
        if not sync_token:
            raise HTTPException(
                status_code=502, detail="Conduit did not return a sync_token"
            )

        peer_id = existing.state.peer_id if existing else create_body.get("peer_id")
        if not isinstance(peer_id, str) or not peer_id:
            raise HTTPException(
                status_code=500,
                detail="peer_id missing from create body; service integration bug",
            )

        extension = build_extension(
            body, response, existing.extension if existing else None
        )
        state = LinkState(
            conduit_url=conduit_url,
            peer_id=peer_id,
            peer_name=peer_name,
            sync_token=sync_token,
            panel=config.panel,
            linked_at=_now_iso(),
        )
        record = store.save(state, extension)

        LOG.info(
            "linked %s to Conduit peer=%s", config.service_kind.value, record.state.peer_id
        )
        return {
            "status": "linked",
            "peer_id": record.state.peer_id,
            "peer_name": record.state.peer_name,
            "conduit_url": record.state.conduit_url,
            "linked_at": record.state.linked_at,
            **public_response(record.extension),
        }

    @router.delete("/link")
    def unlink() -> Response:
        try:
            record = store.load()
        except LinkStoreSecurityError as error:
            raise HTTPException(status_code=500, detail=str(error)) from error
        if record is None:
            return Response(status_code=204)
        try:
            client.delete_link(
                record.state.conduit_url, record.state.peer_id, record.state.sync_token
            )
        except Exception as error:
            LOG.warning(
                "best-effort %s unlink failed: peer=%s error=%s",
                config.service_kind.value,
                record.state.peer_id,
                error,
            )
        store.remove()
        LOG.info(
            "unlinked %s from Conduit peer=%s",
            config.service_kind.value,
            record.state.peer_id,
        )
        return Response(status_code=204)

    return router
