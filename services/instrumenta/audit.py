"""Audit log: structured stdout writer + query endpoint.

Every tool invocation is recorded with hashed args, duration, and outcome.
Structured JSON lines are written to stdout for log-stack ingestion; the
SQLite table mirrors the same fields for the `/audit` UI viewer.

Args are hashed (SHA-256, first 16 hex chars) so raw args never reach
persistent storage — tool args commonly include prompts/credentials.
"""

from __future__ import annotations

import hashlib
import json
import logging
from datetime import datetime, timezone

from fastapi import APIRouter, Depends, Request
from pydantic import BaseModel, Field

from .backend import AuditEntry, Backend

LOG = logging.getLogger("instrumenta.audit")


def hash_args(args: dict[str, object]) -> str:
    """SHA-256 of the JSON-serialised args, truncated to 16 hex chars."""
    raw = json.dumps(args, sort_keys=True, default=str).encode()
    return hashlib.sha256(raw).hexdigest()[:16]


class AuditWriter:
    """Writes audit entries to both stdout (structured JSON) and the backend."""

    def __init__(self, backend: Backend) -> None:
        self.backend = backend

    def record(
        self,
        *,
        peer_id: str | None,
        tool_name: str,
        args: dict[str, object],
        duration_ms: int,
        outcome: str,
    ) -> AuditEntry:
        entry = AuditEntry(
            id=0,  # autoincrement; backend assigns
            called_at=datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S"),
            peer_id=peer_id,
            tool_name=tool_name,
            args_hash=hash_args(args),
            duration_ms=duration_ms,
            outcome=outcome,
        )
        self.backend.insert_audit_entry(entry)
        # Structured stdout line for log-stack ingestion.
        LOG.info(
            "audit",
            extra={
                "peer_id": peer_id,
                "tool_name": tool_name,
                "args_hash": entry.args_hash,
                "duration_ms": duration_ms,
                "outcome": outcome,
            },
        )
        return entry


# ── Pydantic models for /audit route ────────────────────────────────────


class AuditEntryRead(BaseModel):
    id: int
    called_at: str
    peer_id: str | None
    tool_name: str
    args_hash: str
    duration_ms: int | None
    outcome: str

    @classmethod
    def from_row(cls, row: AuditEntry) -> "AuditEntryRead":
        return cls(
            id=row.id,
            called_at=row.called_at,
            peer_id=row.peer_id,
            tool_name=row.tool_name,
            args_hash=row.args_hash,
            duration_ms=row.duration_ms,
            outcome=row.outcome,
        )


class AuditEntryCreate(BaseModel):
    peer_id: str | None = None
    tool_name: str = Field(..., min_length=1)
    args_hash: str = Field(..., min_length=1)
    duration_ms: int | None = None
    outcome: str = Field(..., pattern=r"^(ok|error|timeout)$")


def _backend(request: Request) -> Backend:
    return request.app.state.backend


def make_audit_router() -> APIRouter:
    router = APIRouter(tags=["audit"])

    @router.get("/audit", response_model=list[AuditEntryRead])
    async def list_audit(
        tool_name: str | None = None,
        outcome: str | None = None,
        limit: int = 50,
        backend: Backend = Depends(_backend),
    ) -> list[AuditEntryRead]:
        return [
            AuditEntryRead.from_row(e)
            for e in backend.list_audit_entries(
                tool_name=tool_name, outcome=outcome, limit=limit
            )
        ]

    @router.post("/audit", response_model=AuditEntryRead, status_code=201)
    async def create_audit(
        payload: AuditEntryCreate,
        backend: Backend = Depends(_backend),
    ) -> AuditEntryRead:
        entry = AuditEntry(
            id=0,
            called_at=datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S"),
            peer_id=payload.peer_id,
            tool_name=payload.tool_name,
            args_hash=payload.args_hash,
            duration_ms=payload.duration_ms,
            outcome=payload.outcome,
        )
        backend.insert_audit_entry(entry)
        return AuditEntryRead.from_row(entry)

    return router
