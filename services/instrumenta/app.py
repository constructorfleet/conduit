"""Conduit Instrumenta — the reference tool service.

Instrumenta is Conduit's tool linked-service: a standalone-capable FastAPI
service that aggregates upstream MCP servers, ships a small set of built-in
tools, and re-exposes the merged surface over streamable-HTTP. It hosts a
configuration UI for enabling/disabling tools, authoring local prompts and
resources, and inspecting per-upstream reachability and an audit log.

This module is the v1 skeleton: link endpoints mirroring Memoria/Vox, a SQLite
backend for configuration, Fernet-encrypted secrets, and an empty `/mcp`
streamable-HTTP endpoint that the aggregator and built-in tool PRs will fill.

Streamable-HTTP only (SSE deferred): the MCP SDK's streamable-HTTP transport
handles legacy clients via the `MCP-Protocol-Version` header, so a second SSE
mount is not required for v1.
"""

from __future__ import annotations

import logging
import os
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import Depends, FastAPI, HTTPException
from fastapi.responses import RedirectResponse
from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from conduit_link import (
    HttpConduitLinkClient,
    LinkConfig,
    LinkedServiceKind,
    LinkedServicePanel,
    LinkRequest as _SharedLinkRequest,
    LinkStore,
    make_link_router,
)

from .backend import Backend, SqliteBackend
from .secrets import SecretBox, SecretKeyMissingError

LOG = logging.getLogger("instrumenta")

DEFAULT_PORT = 8085


class _NoExtension:
    __slots__: tuple[()] = ()


def _ext_from(_payload: dict[str, object]) -> _NoExtension:
    return _NoExtension()


def _ext_to(_extension: _NoExtension) -> dict[str, object]:
    return {}


def _build_create_body(
    request: _SharedLinkRequest, _existing: _NoExtension | None
) -> dict[str, object]:
    peer_id = request.peer_name.strip().lower().replace(" ", "-")
    base_url = os.getenv("INSTRUMENTA_BASE_URL", f"http://localhost:{DEFAULT_PORT}")
    return {
        "service_kind": LinkedServiceKind.INSTRUMENTA.value,
        "peer_name": request.peer_name,
        "peer_id": peer_id,
        "peer_base_url": base_url,
        "mcp_url": f"{base_url.rstrip('/')}/mcp",
        "panel": {
            "id": "instrumenta",
            "label": "Instrumenta",
            "icon": "wrench",
            "path": "/ui/",
        },
    }


def _build_extension(_request, _response, _existing) -> _NoExtension:
    return _NoExtension()


def _public(_extension: _NoExtension) -> dict[str, object]:
    return {}


class HealthResponse(BaseModel):
    """`/health` reports Instrumenta's own liveness only.

    Upstream reachability is reported on `/upstreams` (added by the aggregator
    PR) so a flaky upstream never flips Conduit's reachability probe.
    """

    status: str
    backend: str
    linked: bool


class Config(BaseModel):
    """Runtime configuration resolved from environment at app-construction."""

    data_dir: Path
    backend_type: str
    api_key: str | None
    base_url: str
    secret_key: str | None

    @classmethod
    def from_env(cls) -> "Config":
        return cls(
            data_dir=Path(os.getenv("INSTRUMENTA_DATA_DIR", "/data")),
            backend_type=os.getenv("INSTRUMENTA_BACKEND", "sqlite"),
            api_key=os.getenv("INSTRUMENTA_API_KEY"),
            base_url=os.getenv("INSTRUMENTA_BASE_URL", f"http://localhost:{DEFAULT_PORT}"),
            secret_key=os.getenv("INSTRUMENTA_SECRET_KEY"),
        )


def _make_backend(config: Config) -> Backend:
    if config.backend_type == "sqlite":
        return SqliteBackend(config.data_dir / "instrumenta.db")
    if config.backend_type == "postgres":
        raise NotImplementedError(
            "postgres backend is planned; see wayfinder map issue #199"
        )
    raise ValueError(f"Unknown INSTRUMENTA_BACKEND: {config.backend_type}")


def create_app(config: Config | None = None) -> FastAPI:
    """Application factory.

    Accepts an optional `Config` so tests can point at a temp data dir and
    supply a deterministic secret key. Production callers use `create_app()`
    with defaults resolved from the environment.
    """

    if config is None:
        config = Config.from_env()

    config.data_dir.mkdir(parents=True, exist_ok=True)

    backend = _make_backend(config)

    # Instantiating SecretBox eagerly is the "fail loud when secrets exist but
    # no key is configured" contract from spec #198 (User Story 11).
    secret_box = SecretBox(config.secret_key)
    if backend.has_encrypted_secret() and not secret_box.can_decrypt():
        raise SecretKeyMissingError(
            "encrypted secrets exist but INSTRUMENTA_SECRET_KEY is not set"
        )

    link_store = LinkStore[_NoExtension](
        config.data_dir,
        extension_from_dict=_ext_from,
        extension_to_dict=_ext_to,
    )

    security = HTTPBearer(auto_error=False)

    async def verify_token(
        credentials: HTTPAuthorizationCredentials | None = Depends(security),
    ) -> None:
        if config.api_key and (
            credentials is None or credentials.credentials != config.api_key
        ):
            raise HTTPException(status_code=401, detail="Invalid or missing API key")

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        app.state.backend = backend
        app.state.link_store = link_store
        app.state.secret_box = secret_box
        app.state.config = config
        yield
        await backend.close()

    app = FastAPI(
        title="Conduit Instrumenta",
        description="Reference tool service — MCP aggregator with a small built-in set",
        version="0.1.0",
        lifespan=lifespan,
    )

    link_config = LinkConfig(
        service_kind=LinkedServiceKind.INSTRUMENTA,
        peer_name="instrumenta",
        peer_base_url=config.base_url,
        panel=LinkedServicePanel(title="Instrumenta", path="/ui/", icon="wrench"),
        storage_dir=config.data_dir,
    )
    app.include_router(
        make_link_router(
            config=link_config,
            store=link_store,
            client=HttpConduitLinkClient(),
            build_create_body=_build_create_body,
            build_extension=_build_extension,
            public_response=_public,
        )
    )

    @app.get("/health")
    async def health_check() -> HealthResponse:
        return HealthResponse(
            status="ok",
            backend=config.backend_type,
            linked=link_store.load() is not None,
        )

    @app.post("/mcp")
    async def mcp_endpoint() -> dict[str, object]:
        """Streamable-HTTP MCP endpoint (skeleton).

        The `mcp` SDK's `streamable_http_app()` will be mounted here once the
        aggregator PR lands; today it returns an empty tool list so integration
        tests can assert the endpoint is wired without pulling the SDK in.
        """
        return {"tools": [], "prompts": [], "resources": []}

    static_dir = Path(__file__).parent / "static"
    if static_dir.exists():
        app.mount("/ui", StaticFiles(directory=str(static_dir), html=True), name="ui")

    @app.get("/")
    async def root() -> RedirectResponse:
        return RedirectResponse(url="/ui/")

    # `verify_token` is defined but has no protected routes in the skeleton;
    # keep it referenced so a linter does not eagerly delete it.
    app.state.verify_token = verify_token

    return app


if __name__ == "__main__":
    import uvicorn

    logging.basicConfig(level=logging.INFO)
    uvicorn.run(create_app(), host="0.0.0.0", port=DEFAULT_PORT)
