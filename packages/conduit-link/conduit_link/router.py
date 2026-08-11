"""FastAPI router factory for the peer-side `/link` surface (spec 0005)."""

from __future__ import annotations

from typing import Callable, Generic, Mapping, TypeVar

from fastapi import APIRouter
from pydantic import BaseModel

from .client import ConduitLinkClient
from .config import LinkConfig
from .store import LinkStore

E = TypeVar("E")


class LinkRequest(BaseModel):
    conduit_url: str
    operator_token: str
    peer_name: str
    force: bool = False


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

    - `GET /link` → status view (`unlinked`, `config-managed`, or the record)
    - `POST /link` → create/replace the link, delegating to `client.create_link`
    - `DELETE /link` → best-effort unlink through `client.delete_link`
    - `GET /link/health` → 0005 §Reachability probe target

    The three callbacks let each service inject its extension shape without
    the module knowing service-specific keys. Upholds spec 0005 §Handshake.
    """
    raise NotImplementedError
