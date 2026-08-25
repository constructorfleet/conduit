"""CRUD routes for item flags, local prompts, and local resources.

Mounted at `/items`. Item flags control per-item enable/disable across all
origins (built-in, upstream, local). Local prompts and resources are
authored directly in Instrumenta and merged into the MCP surface.
"""

from __future__ import annotations

import uuid

from fastapi import APIRouter, Depends, HTTPException, Request
from pydantic import BaseModel, Field

from .backend import Backend, ItemFlag, LocalPrompt, LocalResource


# ── Pydantic models ────────────────────────────────────────────────────


class ItemFlagUpsert(BaseModel):
    origin: str = Field(..., min_length=1)
    item_kind: str = Field(..., pattern=r"^(tool|prompt|resource)$")
    item_name: str = Field(..., min_length=1)
    enabled: bool = True


class ItemFlagRead(BaseModel):
    origin: str
    item_kind: str
    item_name: str
    enabled: bool

    @classmethod
    def from_row(cls, row: ItemFlag) -> "ItemFlagRead":
        return cls(
            origin=row.origin,
            item_kind=row.item_kind,
            item_name=row.item_name,
            enabled=row.enabled,
        )


class PromptCreate(BaseModel):
    name: str = Field(..., min_length=1, max_length=128)
    template: str = Field(..., min_length=1)
    description: str | None = None


class PromptUpdate(BaseModel):
    name: str | None = Field(default=None, min_length=1, max_length=128)
    template: str | None = Field(default=None, min_length=1)
    description: str | None = None


class PromptRead(BaseModel):
    id: str
    name: str
    template: str
    description: str | None

    @classmethod
    def from_row(cls, row: LocalPrompt) -> "PromptRead":
        return cls(
            id=row.id,
            name=row.name,
            template=row.template,
            description=row.description,
        )


class ResourceCreate(BaseModel):
    uri: str = Field(..., min_length=1)
    name: str = Field(..., min_length=1, max_length=128)
    mime_type: str | None = None
    content: str | None = None


class ResourceUpdate(BaseModel):
    uri: str | None = Field(default=None, min_length=1)
    name: str | None = Field(default=None, min_length=1, max_length=128)
    mime_type: str | None = None
    content: str | None = None


class ResourceRead(BaseModel):
    id: str
    uri: str
    name: str
    mime_type: str | None
    content: str | None

    @classmethod
    def from_row(cls, row: LocalResource) -> "ResourceRead":
        return cls(
            id=row.id,
            uri=row.uri,
            name=row.name,
            mime_type=row.mime_type,
            content=row.content,
        )


# ── Dependency ──────────────────────────────────────────────────────────


def _backend(request: Request) -> Backend:
    return request.app.state.backend


# ── Router factory ──────────────────────────────────────────────────────


def make_items_router() -> APIRouter:
    router = APIRouter(prefix="/items", tags=["items"])

    # ── Item flags ──────────────────────────────────────────────────

    @router.get("/flags", response_model=list[ItemFlagRead])
    async def list_flags(
        origin: str | None = None,
        item_kind: str | None = None,
        backend: Backend = Depends(_backend),
    ) -> list[ItemFlagRead]:
        return [
            ItemFlagRead.from_row(f)
            for f in backend.list_item_flags(origin=origin, item_kind=item_kind)
        ]

    @router.put("/flags", response_model=ItemFlagRead)
    async def upsert_flag(
        payload: ItemFlagUpsert,
        backend: Backend = Depends(_backend),
    ) -> ItemFlagRead:
        flag = ItemFlag(
            origin=payload.origin,
            item_kind=payload.item_kind,
            item_name=payload.item_name,
            enabled=payload.enabled,
        )
        backend.upsert_item_flag(flag)
        return ItemFlagRead.from_row(flag)

    @router.delete("/flags/{origin}/{item_kind}/{item_name}", status_code=204)
    async def delete_flag(
        origin: str,
        item_kind: str,
        item_name: str,
        backend: Backend = Depends(_backend),
    ) -> None:
        deleted = backend.delete_item_flag(origin, item_kind, item_name)
        if not deleted:
            raise HTTPException(status_code=404, detail="flag not found")

    # ── Local prompts ───────────────────────────────────────────────

    @router.get("/prompts", response_model=list[PromptRead])
    async def list_prompts(
        backend: Backend = Depends(_backend),
    ) -> list[PromptRead]:
        return [PromptRead.from_row(p) for p in backend.list_local_prompts()]

    @router.post("/prompts", response_model=PromptRead, status_code=201)
    async def create_prompt(
        payload: PromptCreate,
        backend: Backend = Depends(_backend),
    ) -> PromptRead:
        prompt = LocalPrompt(
            id=str(uuid.uuid4()),
            name=payload.name,
            template=payload.template,
            description=payload.description,
        )
        try:
            backend.insert_local_prompt(prompt)
        except Exception as exc:
            raise HTTPException(status_code=409, detail=str(exc))
        return PromptRead.from_row(prompt)

    @router.get("/prompts/{prompt_id}", response_model=PromptRead)
    async def get_prompt(
        prompt_id: str,
        backend: Backend = Depends(_backend),
    ) -> PromptRead:
        row = backend.get_local_prompt(prompt_id)
        if row is None:
            raise HTTPException(status_code=404, detail="prompt not found")
        return PromptRead.from_row(row)

    @router.patch("/prompts/{prompt_id}", response_model=PromptRead)
    async def update_prompt(
        prompt_id: str,
        payload: PromptUpdate,
        backend: Backend = Depends(_backend),
    ) -> PromptRead:
        current = backend.get_local_prompt(prompt_id)
        if current is None:
            raise HTTPException(status_code=404, detail="prompt not found")
        updated = LocalPrompt(
            id=current.id,
            name=payload.name if payload.name is not None else current.name,
            template=payload.template if payload.template is not None else current.template,
            description=payload.description if payload.description is not None else current.description,
        )
        backend.update_local_prompt(updated)
        return PromptRead.from_row(updated)

    @router.delete("/prompts/{prompt_id}", status_code=204)
    async def delete_prompt(
        prompt_id: str,
        backend: Backend = Depends(_backend),
    ) -> None:
        deleted = backend.delete_local_prompt(prompt_id)
        if not deleted:
            raise HTTPException(status_code=404, detail="prompt not found")

    # ── Local resources ─────────────────────────────────────────────

    @router.get("/resources", response_model=list[ResourceRead])
    async def list_resources(
        backend: Backend = Depends(_backend),
    ) -> list[ResourceRead]:
        return [ResourceRead.from_row(r) for r in backend.list_local_resources()]

    @router.post("/resources", response_model=ResourceRead, status_code=201)
    async def create_resource(
        payload: ResourceCreate,
        backend: Backend = Depends(_backend),
    ) -> ResourceRead:
        resource = LocalResource(
            id=str(uuid.uuid4()),
            uri=payload.uri,
            name=payload.name,
            mime_type=payload.mime_type,
            content=payload.content,
        )
        try:
            backend.insert_local_resource(resource)
        except Exception as exc:
            raise HTTPException(status_code=409, detail=str(exc))
        return ResourceRead.from_row(resource)

    @router.get("/resources/{resource_id}", response_model=ResourceRead)
    async def get_resource(
        resource_id: str,
        backend: Backend = Depends(_backend),
    ) -> ResourceRead:
        row = backend.get_local_resource(resource_id)
        if row is None:
            raise HTTPException(status_code=404, detail="resource not found")
        return ResourceRead.from_row(row)

    @router.patch("/resources/{resource_id}", response_model=ResourceRead)
    async def update_resource(
        resource_id: str,
        payload: ResourceUpdate,
        backend: Backend = Depends(_backend),
    ) -> ResourceRead:
        current = backend.get_local_resource(resource_id)
        if current is None:
            raise HTTPException(status_code=404, detail="resource not found")
        updated = LocalResource(
            id=current.id,
            uri=payload.uri if payload.uri is not None else current.uri,
            name=payload.name if payload.name is not None else current.name,
            mime_type=payload.mime_type if payload.mime_type is not None else current.mime_type,
            content=payload.content if payload.content is not None else current.content,
        )
        backend.update_local_resource(updated)
        return ResourceRead.from_row(updated)

    @router.delete("/resources/{resource_id}", status_code=204)
    async def delete_resource(
        resource_id: str,
        backend: Backend = Depends(_backend),
    ) -> None:
        deleted = backend.delete_local_resource(resource_id)
        if not deleted:
            raise HTTPException(status_code=404, detail="resource not found")

    return router
