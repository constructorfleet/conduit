"""SQLite backend for Excita.

Stores wake-word phrases, clip metadata, and labels. Raw audio bytes live on
disk (`clip_store.py`); this module only tracks the rows that describe them.

Kept deliberately narrow (`Backend` protocol) so a postgres backend can slot
in without touching `app.py`. Mirrors the Instrumenta pattern
(`services/instrumenta/backend.py`).
"""

from __future__ import annotations

import sqlite3
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol


@dataclass(frozen=True)
class Phrase:
    id: str
    name: str
    display_label: str
    language: str


@dataclass(frozen=True)
class Clip:
    id: str
    phrase_id: str
    sample_rate: int
    duration_ms: int
    source: str
    source_peer: str | None
    sha256: str
    mime_type: str
    stored_path: str
    created_at: str


@dataclass(frozen=True)
class Label:
    clip_id: str
    verdict: str
    labeller: str
    split: str | None
    notes: str | None
    labelled_at: str


class Backend(Protocol):
    async def close(self) -> None: ...

    def list_phrases(self) -> list[Phrase]: ...
    def get_phrase(self, phrase_id: str) -> Phrase | None: ...
    def insert_phrase(self, phrase: Phrase) -> None: ...

    def list_clips(
        self,
        phrase_id: str | None = None,
        verdict: str | None = None,
        limit: int = 100,
    ) -> list[Clip]: ...
    def get_clip(self, clip_id: str) -> Clip | None: ...
    def get_clip_by_sha256(self, phrase_id: str, sha256: str) -> Clip | None: ...
    def insert_clip(self, clip: Clip) -> None: ...

    def get_label(self, clip_id: str, labeller: str) -> Label | None: ...
    def upsert_label(self, label: Label) -> None: ...


_SCHEMA = """
CREATE TABLE IF NOT EXISTS phrases (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    display_label TEXT NOT NULL,
    language TEXT NOT NULL DEFAULT 'en',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS clips (
    id TEXT PRIMARY KEY,
    phrase_id TEXT NOT NULL REFERENCES phrases(id),
    sample_rate INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('detector', 'upload', 'browser')),
    source_peer TEXT,
    sha256 TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    stored_path TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (phrase_id, sha256)
);
CREATE INDEX IF NOT EXISTS idx_clips_phrase ON clips(phrase_id);

CREATE TABLE IF NOT EXISTS labels (
    clip_id TEXT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
    verdict TEXT NOT NULL CHECK (verdict IN ('positive', 'negative', 'ambiguous', 'discard')),
    labeller TEXT NOT NULL,
    split TEXT CHECK (split IN ('train', 'val', 'test')),
    notes TEXT,
    labelled_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (clip_id, labeller)
);
CREATE INDEX IF NOT EXISTS idx_labels_verdict ON labels(verdict);
"""


class SqliteBackend:
    """Synchronous sqlite backend.

    Same pragmatic pattern as Instrumenta: stdlib sqlite3, one connection,
    called directly from request handlers. Excita's workload is one operator
    labelling one clip at a time — the connection contention argument for
    `asyncio.to_thread` doesn't apply until multi-operator lands.
    """

    def __init__(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        self._conn = sqlite3.connect(str(path), check_same_thread=False)
        self._conn.execute("PRAGMA foreign_keys = ON")
        self._conn.executescript(_SCHEMA)
        self._conn.commit()

    async def close(self) -> None:
        self._conn.close()

    # --- phrases ---

    def list_phrases(self) -> list[Phrase]:
        rows = self._conn.execute(
            "SELECT id, name, display_label, language FROM phrases ORDER BY name"
        ).fetchall()
        return [Phrase(*r) for r in rows]

    def get_phrase(self, phrase_id: str) -> Phrase | None:
        row = self._conn.execute(
            "SELECT id, name, display_label, language FROM phrases WHERE id = ?",
            (phrase_id,),
        ).fetchone()
        return Phrase(*row) if row else None

    def insert_phrase(self, phrase: Phrase) -> None:
        self._conn.execute(
            "INSERT INTO phrases (id, name, display_label, language) VALUES (?, ?, ?, ?)",
            (phrase.id, phrase.name, phrase.display_label, phrase.language),
        )
        self._conn.commit()

    # --- clips ---

    _CLIP_COLS = (
        "id, phrase_id, sample_rate, duration_ms, source, source_peer, "
        "sha256, mime_type, stored_path, created_at"
    )

    def list_clips(
        self,
        phrase_id: str | None = None,
        verdict: str | None = None,
        limit: int = 100,
    ) -> list[Clip]:
        where: list[str] = []
        args: list[object] = []
        if phrase_id is not None:
            where.append("c.phrase_id = ?")
            args.append(phrase_id)
        if verdict is not None:
            # `verdict` filters against the label if present. `unlabeled` is a
            # sentinel that returns clips with no row in `labels` at all.
            if verdict == "unlabeled":
                where.append("NOT EXISTS (SELECT 1 FROM labels l WHERE l.clip_id = c.id)")
            else:
                where.append(
                    "EXISTS (SELECT 1 FROM labels l WHERE l.clip_id = c.id AND l.verdict = ?)"
                )
                args.append(verdict)
        where_sql = ("WHERE " + " AND ".join(where)) if where else ""
        args.append(limit)
        rows = self._conn.execute(
            f"SELECT {self._CLIP_COLS} FROM clips c {where_sql} "
            "ORDER BY c.created_at DESC LIMIT ?",
            args,
        ).fetchall()
        return [Clip(*r) for r in rows]

    def get_clip(self, clip_id: str) -> Clip | None:
        row = self._conn.execute(
            f"SELECT {self._CLIP_COLS} FROM clips WHERE id = ?", (clip_id,)
        ).fetchone()
        return Clip(*row) if row else None

    def get_clip_by_sha256(self, phrase_id: str, sha256: str) -> Clip | None:
        row = self._conn.execute(
            f"SELECT {self._CLIP_COLS} FROM clips WHERE phrase_id = ? AND sha256 = ?",
            (phrase_id, sha256),
        ).fetchone()
        return Clip(*row) if row else None

    def insert_clip(self, clip: Clip) -> None:
        self._conn.execute(
            f"INSERT INTO clips ({self._CLIP_COLS}) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                clip.id, clip.phrase_id, clip.sample_rate, clip.duration_ms,
                clip.source, clip.source_peer, clip.sha256, clip.mime_type,
                clip.stored_path, clip.created_at,
            ),
        )
        self._conn.commit()

    # --- labels ---

    def get_label(self, clip_id: str, labeller: str) -> Label | None:
        row = self._conn.execute(
            "SELECT clip_id, verdict, labeller, split, notes, labelled_at "
            "FROM labels WHERE clip_id = ? AND labeller = ?",
            (clip_id, labeller),
        ).fetchone()
        return Label(*row) if row else None

    def upsert_label(self, label: Label) -> None:
        self._conn.execute(
            "INSERT INTO labels (clip_id, verdict, labeller, split, notes, labelled_at) "
            "VALUES (?, ?, ?, ?, ?, ?) "
            "ON CONFLICT(clip_id, labeller) DO UPDATE SET "
            "verdict = excluded.verdict, split = excluded.split, "
            "notes = excluded.notes, labelled_at = excluded.labelled_at",
            (
                label.clip_id, label.verdict, label.labeller,
                label.split, label.notes, label.labelled_at,
            ),
        )
        self._conn.commit()


def new_id() -> str:
    return uuid.uuid4().hex
