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
import json
import logging
import os
import secrets
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


class LinkRequest(BaseModel):
    """Request to link with Conduit."""

    conduit_url: str
    operator_token: str
    peer_name: str = "memoria"
    force: bool = False


class LinkStatus(BaseModel):
    """Link status with Conduit."""

    status: str  # linked, unlinked, config-managed
    peer_id: str | None = None
    peer_name: str | None = None
    conduit_url: str | None = None


class HealthResponse(BaseModel):
    """Health check response."""

    status: str
    backend: str
    engram_count: int
    linked: bool


# Global state
storage: StorageBackend | None = None
link_status: LinkStatus | None = None
link_file_path: Path | None = None
api_key: str | None = None
conduit_client: httpx.AsyncClient | None = None
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
    global storage, link_status, link_file_path, api_key, conduit_client, sync_task

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

    # Initialize link status
    link_file_path = data_dir / "link.json"
    link_status = load_link_status(link_file_path)

    # Initialize conduit client if linked
    if link_status and link_status.status == "linked" and link_status.conduit_url:
        conduit_client = httpx.AsyncClient(timeout=30.0)
        sync_task = asyncio.create_task(background_sync())

    # Initialize MCP server if enabled
    if os.getenv("MEMORIA_MCP_ENABLED", "false").lower() == "true":
        start_mcp_server()

    yield

    # Cleanup
    if sync_task:
        sync_task.cancel()
        with suppress(asyncio.CancelledError):
            await sync_task
    if conduit_client:
        await conduit_client.aclose()
    if storage:
        await storage.cleanup()


app = FastAPI(
    title="Conduit Memoria",
    description="Reference memory service with MCP backing",
    version="1.0.0",
    lifespan=lifespan,
)


def load_link_status(path: Path) -> LinkStatus | None:
    """Load link status from file."""
    if not path.exists():
        return None
    try:
        data = json.loads(path.read_text())
        return LinkStatus(**data)
    except Exception as e:
        LOG.warning(f"Failed to load link status: {e}")
        return None


def save_link_status(path: Path, status: LinkStatus) -> None:
    """Save link status to file."""
    path.write_text(status.model_dump_json(indent=2))
    os.chmod(path, 0o600)


async def background_sync() -> None:
    """Background task to sync with Conduit."""
    global link_status, sync_lock

    interval = int(os.getenv("MEMORIA_SYNC_INTERVAL_SECONDS", DEFAULT_SYNC_INTERVAL_SECONDS))
    max_backoff = int(os.getenv("MEMORIA_SYNC_MAX_BACKOFF_SECONDS", DEFAULT_SYNC_MAX_BACKOFF_SECONDS))
    backoff = interval

    while True:
        try:
            await asyncio.sleep(backoff)
            async with sync_lock:
                if not link_status or link_status.status != "linked":
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
        linked=link_status is not None and link_status.status == "linked",
    )


@app.get("/link")
async def get_link() -> LinkStatus:
    """Get current link status."""
    if link_status is None:
        return LinkStatus(status="unlinked")
    return link_status


@app.post("/link")
async def create_link(request: LinkRequest) -> LinkStatus:
    """Create link with Conduit."""
    global link_status, conduit_client, sync_task

    async with sync_lock:
        # Generate local API key if not set
        local_api_key = api_key or secrets.token_urlsafe(32)

        # Exchange operator token for sync token with Conduit
        try:
            async with httpx.AsyncClient(timeout=30.0) as client:
                response = await client.post(
                    f"{request.conduit_url}/v1/links/memoria",
                    json={
                        "operator_token": request.operator_token,
                        "peer_name": request.peer_name,
                        "local_api_key": local_api_key,
                        "local_base_url": os.getenv("MEMORIA_BASE_URL", "http://memoria:8080"),
                    },
                )
                response.raise_for_status()
                data = response.json()

        except Exception as e:
            LOG.error(f"Failed to link with Conduit: {e}")
            raise HTTPException(status_code=500, detail=f"Link failed: {e}")

        # Store link status
        link_status = LinkStatus(
            status="linked",
            peer_id=data["peer_id"],
            peer_name=request.peer_name,
            conduit_url=request.conduit_url,
        )
        if link_file_path:
            save_link_status(link_file_path, link_status)

        # Initialize conduit client and sync task
        conduit_client = httpx.AsyncClient(timeout=30.0)
        sync_task = asyncio.create_task(background_sync())

        return link_status


@app.delete("/link")
async def delete_link() -> Response:
    """Delete link with Conduit."""
    global link_status, conduit_client, sync_task

    async with sync_lock:
        if link_status and link_status.status == "linked":
            # Revoke link in Conduit
            try:
                if conduit_client and link_status.peer_id:
                    await conduit_client.delete(f"{link_status.conduit_url}/v1/links/{link_status.peer_id}")
            except Exception as e:
                LOG.warning(f"Failed to revoke link in Conduit: {e}")

        # Stop sync task
        if sync_task:
            sync_task.cancel()
            sync_task = None

        # Close conduit client
        if conduit_client:
            await conduit_client.aclose()
            conduit_client = None

        # Clear link status
        link_status = None
        if link_file_path and link_file_path.exists():
            link_file_path.unlink()

        return Response(status_code=204)


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