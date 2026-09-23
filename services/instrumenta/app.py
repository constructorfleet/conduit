"""Conduit Instrumenta — the reference tool service.

Instrumenta is Conduit's tool linked-service: a standalone-capable FastAPI
service that aggregates upstream MCP servers, ships a small set of built-in
tools, and re-exposes the merged surface over streamable-HTTP. It hosts a
configuration UI for enabling/disabling tools, authoring local prompts and
resources, and inspecting per-upstream reachability and an audit log.

This module wires link endpoints, a pluggable SQLite/PostgreSQL configuration backend,
Fernet-encrypted secrets, and the streamable-HTTP MCP endpoint with the four
built-in tools registered (see `mcp_app.py`). The aggregator PR extends the
MCP server with upstream-forwarded tools/prompts/resources.

Streamable-HTTP only (SSE deferred): the MCP SDK's streamable-HTTP transport
handles legacy clients via the `MCP-Protocol-Version` header, so a second SSE
mount is not required for v1.
"""

from __future__ import annotations

import logging
import os
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI
from fastapi.responses import RedirectResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from conduit_link import (
    HttpConduitLinkClient,
    LinkConfig,
    LinkCreateContext,
    LinkExtensionContext,
    LinkedServiceKind,
    LinkedServicePanel,
    LinkStore,
    make_link_router,
)

from .aggregator import Aggregator, UpstreamStatus
from .audit import make_audit_router
from .backend import Backend, PostgresBackend, SqliteBackend
from .items_router import make_items_router
from .mcp_app import BUILTIN_TOOL_NAMES, build_mcp_server
from .path_probe import probe_runtimes
from .secret_box import SecretBox, SecretKeyMissingError
from .servers_router import make_servers_router

LOG = logging.getLogger("instrumenta")

DEFAULT_PORT = 8085

#: Path of the streamable-HTTP MCP endpoint, relative to the service base URL.
#:
#: One constant for two callers -- the mount below and the `mcp_url` the link
#: handshake advertises -- so moving the transport moves what Conduit is told
#: to connect to. The trailing slash is part of it: the sub-app is mounted at
#: `/mcp` and serves its transport at `/`, so `/mcp` alone redirects.
MCP_PATH = "/mcp/"


class _NoExtension:
    __slots__: tuple[()] = ()


def _ext_from(_payload: dict[str, object]) -> _NoExtension:
    return _NoExtension()


def _ext_to(_extension: _NoExtension) -> dict[str, object]:
    return {}


def _build_create_body(context: LinkCreateContext[_NoExtension]) -> dict[str, object]:
    peer_id = (
        context.existing_peer_id
        or context.request.peer_name.strip().lower().replace(" ", "-")
    )
    base_url = os.getenv("INSTRUMENTA_BASE_URL", f"http://localhost:{DEFAULT_PORT}")
    return {
        "service_kind": LinkedServiceKind.INSTRUMENTA.value,
        "peer_name": context.request.peer_name,
        "peer_id": peer_id,
        "peer_base_url": base_url,
        # User Story 30 (#198): one canonical endpoint, so Conduit connects as
        # a stock MCP client instead of reconstructing the path from transport
        # knowledge it should not need.
        "mcp_url": f"{base_url}{MCP_PATH}",
        "panel": {
            "id": "instrumenta",
            "label": "Instrumenta",
            "icon": "wrench",
            "path": "/ui/",
        },
    }


def _build_extension(_context: LinkExtensionContext[_NoExtension]) -> _NoExtension:
    return _NoExtension()


def _public(_extension: _NoExtension) -> dict[str, object]:
    return {}


class ToolSummary(BaseModel):
    """One row of the merged tool surface, as the UI's Tools tab shows it."""

    name: str
    origin: str
    server: str | None
    description: str | None
    enabled: bool


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
    database_url: str | None = None

    @classmethod
    def from_env(cls) -> "Config":
        return cls(
            data_dir=Path(os.getenv("INSTRUMENTA_DATA_DIR", "/data")),
            backend_type=os.getenv("INSTRUMENTA_BACKEND", "sqlite"),
            database_url=os.getenv("INSTRUMENTA_DATABASE_URL"),
            api_key=os.getenv("INSTRUMENTA_API_KEY"),
            base_url=os.getenv("INSTRUMENTA_BASE_URL", f"http://localhost:{DEFAULT_PORT}"),
            secret_key=os.getenv("INSTRUMENTA_SECRET_KEY"),
        )


def _csv(value: str) -> list[str]:
    return [part.strip() for part in value.split(",") if part.strip()]


def _make_backend(config: Config) -> Backend:
    if config.backend_type == "sqlite":
        return SqliteBackend(config.data_dir / "instrumenta.db")
    if config.backend_type == "postgres":
        database_url = config.database_url or os.getenv(
            "INSTRUMENTA_DATABASE_URL",
            "postgresql://postgres:postgres@postgres:5432/postgres",
        )
        return PostgresBackend(database_url)
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

    mcp_server = build_mcp_server()
    aggregator = Aggregator(backend, secret_box)

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        # Register upstream tools BEFORE the session manager starts so
        # `tools/list` sees them from the first request.
        await aggregator.start(mcp_server)
        # `session_manager` only exists after `streamable_http_app()` is called
        # and must be entered from the host lifespan or the first request
        # raises `RuntimeError: Task group is not initialized` — the SDK's
        # ASGI sub-app lifespan is not run for mounted apps in Starlette.
        async with mcp_server.session_manager.run():
            app.state.backend = backend
            app.state.link_store = link_store
            app.state.secret_box = secret_box
            app.state.config = config
            app.state.mcp_server = mcp_server
            app.state.aggregator = aggregator
            yield
        await aggregator.close()
        await backend.close()

    app = FastAPI(
        title="Conduit Instrumenta",
        description="Reference tool service — MCP aggregator with a small built-in set",
        version="0.1.0",
        lifespan=lifespan,
    )

    from .metrics import PrometheusMiddleware, start_metrics_server

    app.add_middleware(PrometheusMiddleware)
    start_metrics_server()

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

    @app.get("/upstreams")
    async def list_upstreams() -> list[UpstreamStatus]:
        """Per-upstream reachability snapshot.

        Kept separate from `/health` so a flaky upstream never flips
        Conduit's reachability probe on Instrumenta itself.
        """
        return aggregator.statuses()

    @app.get("/tools")
    async def list_tools() -> list[ToolSummary]:
        """The merged tool surface: built-ins plus every aggregated upstream.

        Names come from the MCP server itself, so what the UI lists is exactly
        what a client gets from `tools/list` -- the tab used to count
        `tool_count` from `/upstreams` and invent `<server>.tool_0`, names no
        upstream has.

        `enabled` reads the per-item flags (User Story 3). Absent a flag a tool
        is enabled, so the surface is on by default and the operator turns
        things off.
        """
        disabled = {
            flag.item_name
            for flag in backend.list_item_flags(item_kind="tool")
            if not flag.enabled
        }
        rows = []
        for tool in await mcp_server.list_tools():
            is_builtin = tool.name in BUILTIN_TOOL_NAMES
            rows.append(
                ToolSummary(
                    name=tool.name,
                    origin="builtin" if is_builtin else "upstream",
                    # Aggregated tools are registered as `<server>.<tool>`; a
                    # built-in's dot is part of its own name, not a prefix.
                    server=None if is_builtin else tool.name.split(".", 1)[0],
                    description=tool.description,
                    enabled=tool.name not in disabled,
                )
            )
        return rows

    @app.get("/runtimes")
    async def list_runtimes() -> dict[str, bool]:
        """Boot-time PATH probe for stdio runtimes.

        Read-only; reflects the system PATH at startup.
        """
        return probe_runtimes()

    app.include_router(make_servers_router())
    app.include_router(make_items_router())
    app.include_router(make_audit_router())

    # Mount the streamable-HTTP MCP transport at `/mcp`. The SDK's default
    # `streamable_http_path='/mcp'` combined with a mount would become
    # `/mcp/mcp`, so we set the sub-app path to `/` and let the mount prefix
    # do the routing.
    #
    # DNS-rebinding protection defaults to loopback-only; that trips
    # TestClient (Host: testserver) and any non-loopback deployment. The
    # allowlist is configurable via `INSTRUMENTA_ALLOWED_HOSTS` /
    # `INSTRUMENTA_ALLOWED_ORIGINS` (comma-separated) so real deployments
    # can widen it, and tests get "testserver" by default.
    from mcp.server.transport_security import TransportSecuritySettings

    allowed_hosts = _csv(os.getenv("INSTRUMENTA_ALLOWED_HOSTS", "testserver,localhost,127.0.0.1"))
    allowed_origins = _csv(os.getenv("INSTRUMENTA_ALLOWED_ORIGINS", ""))
    transport_security = TransportSecuritySettings(
        allowed_hosts=allowed_hosts,
        allowed_origins=allowed_origins,
    )
    app.mount(
        MCP_PATH.rstrip("/"),
        mcp_server.streamable_http_app(
            streamable_http_path="/",
            transport_security=transport_security,
        ),
    )

    static_dir = Path(__file__).parent / "static"
    if static_dir.exists():
        app.mount("/ui", StaticFiles(directory=str(static_dir), html=True), name="ui")

    @app.get("/")
    async def root() -> RedirectResponse:
        return RedirectResponse(url="/ui/")

    return app


if __name__ == "__main__":
    import uvicorn

    logging.basicConfig(level=logging.INFO)
    uvicorn.run(create_app(), host="0.0.0.0", port=DEFAULT_PORT)
