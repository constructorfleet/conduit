"""Conduit Memoria — the reference memory service with MCP backing.

Memoria provides persistent storage and retrieval of conversation memories (engrams).
It supports both HTTP API (Vox-style) and MCP server interfaces.

## HTTP Interface
- FastAPI-based REST API
- Vox-style linking with Conduit
- Self-contained UI for memory management
- Token-based authentication

## MCP Interface
- Standard MCP server over stdio/HTTP/SSE
- Tools for memory operations
- Resources for memory access
- Seamless MCP client integration

## Storage Backends
- Builtin: JSON file storage with BM25 search
- PgVector: PostgreSQL with vector embeddings for semantic search

## What it does not do
It does not decide which memories are relevant. It returns scored matches and lets
the caller (Conduit or MCP client) apply thresholds and filtering logic.
"""

from __future__ import annotations

import asyncio
import logging
import os
import threading
import uuid
from contextlib import asynccontextmanager, suppress
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Awaitable, Callable, Protocol

import httpx
import numpy as np
from fastapi import Depends, FastAPI, HTTPException, Request, Response
from fastapi.responses import RedirectResponse
from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel, Field

from conduit_link import (
    HttpConduitLinkClient,
    LinkConfig,
    LinkedServiceKind,
    LinkedServicePanel,
    LinkRequest as _SharedLinkRequest,
    LinkStore,
    make_link_router,
)

LOG = logging.getLogger("memoria")

# Configuration defaults
DEFAULT_SEARCH_LIMIT = 10
DEFAULT_SEARCH_TIMEOUT_MS = 3000
DEFAULT_SIMILARITY_THRESHOLD = 0.7
DEFAULT_SYNC_INTERVAL_SECONDS = 300
DEFAULT_SYNC_MAX_BACKOFF_SECONDS = 900
MIN_ENGRAM_LENGTH = 1
MAX_ENGRAM_LENGTH = 10000
MAX_METADATA_SIZE = 4096


class StorageBackend(Protocol):
    """Storage backend interface for engrams."""

    async def store(self, engram: "Engram") -> "Engram":
        """Store a new engram."""
        ...

    async def get(self, engram_id: str) -> "Engram | None":
        """Retrieve an engram by ID."""
        ...

    async def update(self, engram_id: str, updates: dict[str, Any]) -> "Engram | None":
        """Update an engram."""
        ...

    async def delete(self, engram_id: str) -> bool:
        """Delete an engram."""
        ...

    async def list(
        self,
        scope: str | None = None,
        speaker_id: str | None = None,
        conversation_id: str | None = None,
        limit: int = 100,
    ) -> list["Engram"]:
        """List engrams with optional filters."""
        ...

    async def search(
        self,
        query: str,
        limit: int = 10,
        scope: str | None = None,
        speaker_id: str | None = None,
        conversation_id: str | None = None,
    ) -> list[tuple["Engram", float]]:
        """Search engrams and return (engram, score) tuples."""
        ...

    async def health(self) -> dict[str, Any]:
        """Return backend health status."""
        ...

    async def cleanup(self) -> None:
        """Cleanup resources."""
        ...


class Engram(BaseModel):
    """A single memory record."""

    id: str
    content: str
    speaker_id: str | None = None
    conversation_id: str | None = None
    scope: str = "global"  # global, conversation, speaker
    metadata: dict[str, Any] | None = None
    created_at: str = ""
    updated_at: str = ""

    model_config = {"populate_by_name": True}

    def __init__(self, **data):
        if "created_at" not in data or not data["created_at"]:
            data["created_at"] = datetime.now(timezone.utc).isoformat()
        if "updated_at" not in data or not data["updated_at"]:
            data["updated_at"] = data["created_at"]
        super().__init__(**data)


class EngramStore(BaseModel):
    """Request to store a new engram."""

    content: str = Field(..., min_length=MIN_ENGRAM_LENGTH, max_length=MAX_ENGRAM_LENGTH)
    speaker_id: str | None = None
    conversation_id: str | None = None
    scope: str = Field(default="global", pattern="^(global|conversation|speaker)$")
    metadata: dict[str, Any] | None = Field(default=None, max_length=MAX_METADATA_SIZE)


class EngramUpdate(BaseModel):
    """Request to update an engram."""

    content: str | None = Field(None, min_length=MIN_ENGRAM_LENGTH, max_length=MAX_ENGRAM_LENGTH)
    metadata: dict[str, Any] | None = Field(None, max_length=MAX_METADATA_SIZE)


class EngramSearch(BaseModel):
    """Request to search engrams."""

    query: str = Field(..., min_length=1)
    limit: int = Field(default=DEFAULT_SEARCH_LIMIT, ge=1, le=100)
    scope: str | None = Field(None, pattern="^(global|conversation|speaker)$")
    speaker_id: str | None = None
    conversation_id: str | None = None


class HealthResponse(BaseModel):
    """Health check response."""

    status: str
    backend: str
    engram_count: int
    linked: bool


# Per-service extension: none; Memoria has no extra state to persist beyond
# the 0005 base fields, so the extension dataclass is empty.
class _NoExtension:
    """Placeholder extension for Memoria — no service-specific link state."""

    __slots__: tuple[()] = ()


def _ext_from(_payload: dict[str, object]) -> _NoExtension:
    return _NoExtension()


def _ext_to(_extension: _NoExtension) -> dict[str, object]:
    return {}


def _build_create_body(request: _SharedLinkRequest, _existing: _NoExtension | None) -> dict[str, object]:
    peer_id = request.peer_name.strip().lower().replace(" ", "-")
    return {
        "service_kind": "memoria",
        "peer_name": request.peer_name,
        "peer_id": peer_id,
        "peer_base_url": os.getenv("MEMORIA_BASE_URL", "http://memoria:8080"),
        "panel": {
            "id": "memoria",
            "label": "Memoria",
            "icon": "brain",
            "path": "/ui/",
        },
    }


def _build_extension(_request, _response, _existing) -> _NoExtension:
    return _NoExtension()


def _public(_extension: _NoExtension) -> dict[str, object]:
    return {}


# Global state
storage: StorageBackend | None = None
link_store: LinkStore[_NoExtension] | None = None
api_key: str | None = None
sync_task: asyncio.Task[None] | None = None
sync_lock = asyncio.Lock()


security = HTTPBearer(auto_error=False)


async def verify_token(credentials: HTTPAuthorizationCredentials | None = Depends(security)) -> None:
    """Verify bearer token if API key is configured."""
    if api_key and (credentials is None or credentials.credentials != api_key):
        raise HTTPException(status_code=401, detail="Invalid or missing API key")


def get_storage() -> StorageBackend:
    """Get the current storage backend."""
    if storage is None:
        raise HTTPException(status_code=503, detail="Storage backend not initialized")
    return storage


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Application lifespan manager."""
    global storage, link_store, api_key, sync_task

    # Initialize configuration
    api_key = os.getenv("MEMORIA_API_KEY")
    backend_type = os.getenv("MEMORIA_BACKEND", "builtin")
    data_dir = Path(os.getenv("MEMORIA_DATA_DIR", "/data"))
    data_dir.mkdir(parents=True, exist_ok=True)

    # Initialize storage backend
    if backend_type == "builtin":
        from builtin_backend import BuiltinBackend

        storage = BuiltinBackend(data_dir / "engrams.json")
        LOG.info("Initialized builtin storage backend")
    elif backend_type == "pgvector":
        from pgvector_backend import PgVectorBackend

        db_url = os.getenv("MEMORIA_DATABASE_URL")
        if not db_url:
            raise ValueError("MEMORIA_DATABASE_URL required for pgvector backend")
        storage = PgVectorBackend(db_url)
        LOG.info("Initialized pgvector storage backend")
    elif backend_type == "qdrant":
        from qdrant_backend import QdrantBackend

        qdrant_url = os.getenv("MEMORIA_QDRANT_URL", "http://localhost:6333")
        collection_name = os.getenv("MEMORIA_QDRANT_COLLECTION", "memoria")
        storage = QdrantBackend(qdrant_url, collection_name)
        LOG.info("Initialized qdrant storage backend")
    else:
        raise ValueError(f"Unknown backend: {backend_type}")

    # Initialize link store (via the shared conduit_link module).
    link_store = LinkStore(
        data_dir,
        extension_from_dict=_ext_from,
        extension_to_dict=_ext_to,
    )
    app.state.link_store = link_store

    # If a persisted link is present, note it and start the background sync.
    existing = link_store.load()
    if existing is not None:
        LOG.info("linked to Conduit peer=%s since %s", existing.state.peer_id, existing.state.linked_at)
        sync_task = asyncio.create_task(background_sync())
    else:
        LOG.info("unlinked")

    # Initialize MCP server if enabled
    if os.getenv("MEMORIA_MCP_ENABLED", "false").lower() == "true":
        start_mcp_server()

    yield

    # Cleanup
    if sync_task:
        sync_task.cancel()
        with suppress(asyncio.CancelledError):
            await sync_task
    if storage:
        await storage.cleanup()


app = FastAPI(
    title="Conduit Memoria",
    description="Reference memory service with MCP backing",
    version="1.0.0",
    lifespan=lifespan,
)


def _make_link_router():
    """Router factory delegating to conduit_link's shared implementation.

    Called at module import so route registration happens once. The store is
    resolved from app.state at request time so the lifespan initialiser owns
    creation.
    """
    data_dir = Path(os.getenv("MEMORIA_DATA_DIR", "/data"))
    data_dir.mkdir(parents=True, exist_ok=True)
    store = LinkStore(
        data_dir,
        extension_from_dict=_ext_from,
        extension_to_dict=_ext_to,
    )
    config = LinkConfig(
        service_kind=LinkedServiceKind.MEMORIA,
        peer_name="memoria",
        peer_base_url=os.getenv("MEMORIA_BASE_URL", "http://memoria:8080"),
        panel=LinkedServicePanel(title="Memoria", path="/ui/", icon="brain"),
        storage_dir=data_dir,
    )
    return make_link_router(
        config=config,
        store=store,
        client=HttpConduitLinkClient(),
        build_create_body=_build_create_body,
        build_extension=_build_extension,
        public_response=_public,
    )


app.include_router(_make_link_router())


async def background_sync() -> None:
    """Background task to sync with Conduit."""
    interval = int(os.getenv("MEMORIA_SYNC_INTERVAL_SECONDS", DEFAULT_SYNC_INTERVAL_SECONDS))
    max_backoff = int(os.getenv("MEMORIA_SYNC_MAX_BACKOFF_SECONDS", DEFAULT_SYNC_MAX_BACKOFF_SECONDS))
    backoff = interval

    while True:
        try:
            await asyncio.sleep(backoff)
            async with sync_lock:
                if link_store is None or link_store.load() is None:
                    continue

                # Sync engrams with Conduit
                LOG.debug("Syncing engrams with Conduit")
                # TODO: Implement sync logic
                backoff = interval

        except asyncio.CancelledError:
            break
        except Exception as e:
            LOG.error(f"Sync failed: {e}")
            backoff = min(backoff * 2, max_backoff)


def start_mcp_server() -> None:
    """Start MCP server in background thread."""
    transport = os.getenv("MEMORIA_MCP_TRANSPORT", "stdio")
    LOG.info(f"Starting MCP server with transport: {transport}")

    def run_mcp():
        import asyncio

        async def mcp_loop():
            from mcp_server import MCPServer

            server = MCPServer()
            await server.run(transport)

        asyncio.run(mcp_loop())

    thread = threading.Thread(target=run_mcp, daemon=True)
    thread.start()


@app.get("/health")
async def health_check() -> HealthResponse:
    """Health check endpoint."""
    backend = get_storage()
    health = await backend.health()
    engrams = await backend.list(limit=1)

    return HealthResponse(
        status="ok",
        backend=health.get("backend", "unknown"),
        engram_count=health.get("engram_count", 0),
        linked=link_store is not None and link_store.load() is not None,
    )


@app.post("/engrams", dependencies=[Depends(verify_token)])
async def store_engram(request: EngramStore) -> Engram:
    """Store a new engram."""
    backend = get_storage()
    engram = Engram(
        id=str(uuid.uuid4()),
        content=request.content,
        speaker_id=request.speaker_id,
        conversation_id=request.conversation_id,
        scope=request.scope,
        metadata=request.metadata,
    )
    return await backend.store(engram)


@app.get("/engrams/{engram_id}", dependencies=[Depends(verify_token)])
async def get_engram(engram_id: str) -> Engram:
    """Retrieve a specific engram."""
    backend = get_storage()
    engram = await backend.get(engram_id)
    if engram is None:
        raise HTTPException(status_code=404, detail="Engram not found")
    return engram


@app.patch("/engrams/{engram_id}", dependencies=[Depends(verify_token)])
async def update_engram(engram_id: str, request: EngramUpdate) -> Engram:
    """Update an engram."""
    backend = get_storage()
    updates = {}
    if request.content is not None:
        updates["content"] = request.content
    if request.metadata is not None:
        updates["metadata"] = request.metadata

    engram = await backend.update(engram_id, updates)
    if engram is None:
        raise HTTPException(status_code=404, detail="Engram not found")
    return engram


@app.delete("/engrams/{engram_id}", dependencies=[Depends(verify_token)])
async def delete_engram(engram_id: str) -> Response:
    """Delete an engram."""
    backend = get_storage()
    success = await backend.delete(engram_id)
    if not success:
        raise HTTPException(status_code=404, detail="Engram not found")
    return Response(status_code=204)


@app.get("/engrams", dependencies=[Depends(verify_token)])
async def list_engrams(
    scope: str | None = None,
    speaker_id: str | None = None,
    conversation_id: str | None = None,
    limit: int = 100,
) -> list[Engram]:
    """List engrams with optional filters."""
    backend = get_storage()
    return await backend.list(scope=scope, speaker_id=speaker_id, conversation_id=conversation_id, limit=limit)


@app.post("/engrams/search", dependencies=[Depends(verify_token)])
async def search_engrams(request: EngramSearch) -> list[dict[str, Any]]:
    """Search engrams."""
    backend = get_storage()
    results = await backend.search(
        query=request.query,
        limit=request.limit,
        scope=request.scope,
        speaker_id=request.speaker_id,
        conversation_id=request.conversation_id,
    )
    return [
        {
            "engram": engram.model_dump(),
            "score": score,
        }
        for engram, score in results
    ]


@app.get("/engrams/speakers/{speaker_id}", dependencies=[Depends(verify_token)])
async def get_speaker_engrams(speaker_id: str, limit: int = 100) -> list[Engram]:
    """Get all engrams for a specific speaker."""
    backend = get_storage()
    return await backend.list(speaker_id=speaker_id, limit=limit)


@app.get("/engrams/conversations/{conversation_id}", dependencies=[Depends(verify_token)])
async def get_conversation_engrams(conversation_id: str, limit: int = 100) -> list[Engram]:
    """Get all engrams for a specific conversation."""
    backend = get_storage()
    return await backend.list(conversation_id=conversation_id, limit=limit)


# Serve static UI
app.mount("/ui", StaticFiles(directory="static", html=True), name="ui")

@app.get("/")
async def root():
    """Redirect to UI."""
    return RedirectResponse(url="/ui")


if __name__ == "__main__":
    import uvicorn

    uvicorn.run(app, host="0.0.0.0", port=8080)
