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
from typing import Any, Protocol


@dataclass(frozen=True)
class Phrase:
    id: str
    name: str
    display_label: str
    language: str
    notes: str | None = None
    deleted_at: str | None = None


@dataclass(frozen=True)
class Model:
    """A trained wake-word model registered against a phrase.

    `engine_phrase_key` is the engine-native tag the adapter reads from
    the model's own output (ADR-0022) — e.g. openWakeWord's ONNX output
    key or nanoWakeWord's artifact stem. Two models bound to the same
    phrase may carry different keys; they are different files.
    """

    id: str
    phrase_id: str
    engine: str
    version: str
    engine_phrase_key: str | None
    source: str  # 'upload' | 'filesystem'
    filesystem_path: str | None  # relative to the scanner mount root
    artifact_path: str
    metrics_json: str  # {"envelope": {...}, "raw": {...}}
    notes: str | None
    file_mtime: str | None
    file_size: int
    created_at: str
    deleted_at: str | None


@dataclass(frozen=True)
class DeployTarget:
    """Where a packaged model gets published (spec 0011 §Configure & publish).

    Publishing is a single row change (`current_model_id`) plus a push;
    a failed push never rolls back the row (spec 0011, at-least-once).
    """

    id: str
    kind: str  # 'file' | 'http_push' | 'linked_service_config'
    config_json: str
    current_model_id: str | None
    last_publish_at: str | None
    last_publish_status: str | None
    last_publish_error: str | None
    created_at: str


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
    """Interface only — every body is a stub. CodeQL's no-effect rule is
    satisfied with explicit `raise NotImplementedError` bodies rather than
    bare `...`."""

    async def close(self) -> None:
        raise NotImplementedError

    def list_phrases(self) -> list[Phrase]:
        raise NotImplementedError

    def get_phrase(self, phrase_id: str) -> Phrase | None:
        raise NotImplementedError

    def insert_phrase(self, phrase: Phrase) -> None:
        raise NotImplementedError

    def list_clips(
        self,
        phrase_id: str | None = None,
        verdict: str | None = None,
        limit: int = 100,
    ) -> list[Clip]:
        raise NotImplementedError

    def get_clip(self, clip_id: str) -> Clip | None:
        raise NotImplementedError

    def get_clip_by_sha256(self, phrase_id: str, sha256: str) -> Clip | None:
        raise NotImplementedError

    def insert_clip(self, clip: Clip) -> None:
        raise NotImplementedError

    def get_label(self, clip_id: str, labeller: str) -> Label | None:
        raise NotImplementedError

    def upsert_label(self, label: Label) -> None:
        raise NotImplementedError

    def get_phrase_by_name(self, name: str) -> Phrase | None:
        raise NotImplementedError

    def list_models(
        self,
        phrase_id: str | None = None,
        source: str | None = None,
        include_deleted: bool = False,
    ) -> list[Model]:
        raise NotImplementedError

    def get_model(
        self, model_id: str, include_deleted: bool = False
    ) -> Model | None:
        raise NotImplementedError

    def get_filesystem_model(
        self, filesystem_path: str, version: str
    ) -> Model | None:
        raise NotImplementedError

    def get_model_by_version(
        self, phrase_id: str, engine: str, version: str
    ) -> Model | None:
        raise NotImplementedError

    def promote_upload_model(
        self, model_id: str, filesystem_path: str, mtime: str, size: int
    ) -> None:
        raise NotImplementedError

    def active_filesystem_paths(self) -> set[str]:
        raise NotImplementedError

    def insert_model(self, model: Model) -> None:
        raise NotImplementedError

    def touch_model(self, model_id: str, mtime: str, size: int) -> None:
        raise NotImplementedError

    def resurrect_model(self, model_id: str, mtime: str, size: int) -> None:
        raise NotImplementedError

    def soft_delete_model(self, model_id: str) -> None:
        raise NotImplementedError

    def list_deploy_targets(self) -> list[DeployTarget]:
        raise NotImplementedError

    def get_deploy_target(self, target_id: str) -> DeployTarget | None:
        raise NotImplementedError

    def insert_deploy_target(self, target: DeployTarget) -> None:
        raise NotImplementedError

    def record_publish(
        self,
        target_id: str,
        current_model_id: str,
        status: str,
        error: str | None,
        at: str,
    ) -> None:
        raise NotImplementedError


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

-- A trained model. Phrases are engine-agnostic (ADR-0022); models carry
-- the engine. UNIQUE(phrase_id, engine, version): the same engine can't
-- have two v3s of the same phrase.
CREATE TABLE IF NOT EXISTS models (
    id TEXT PRIMARY KEY,
    phrase_id TEXT NOT NULL REFERENCES phrases(id),
    engine TEXT NOT NULL,
    version TEXT NOT NULL,
    engine_phrase_key TEXT,
    source TEXT NOT NULL CHECK (source IN ('upload', 'filesystem')),
    filesystem_path TEXT,
    artifact_path TEXT NOT NULL,
    metrics_json TEXT NOT NULL DEFAULT '{}',
    notes TEXT,
    file_mtime TEXT,
    file_size INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    deleted_at TEXT,
    UNIQUE (phrase_id, engine, version)
);
CREATE INDEX IF NOT EXISTS idx_models_phrase ON models(phrase_id);
CREATE INDEX IF NOT EXISTS idx_models_filesystem ON models(filesystem_path);

-- Where packaged models get published. Three kinds (#213 §Deploy
-- targets); the packaged bytes flow through any of them per engine.
CREATE TABLE IF NOT EXISTS deploy_targets (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('file', 'http_push', 'linked_service_config')),
    config_json TEXT NOT NULL,
    current_model_id TEXT REFERENCES models(id),
    last_publish_at TEXT,
    last_publish_status TEXT,
    last_publish_error TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"""

# Additive column migrations for databases created by earlier builds — an
# existing openWakeWord install must keep working untouched (#213:
# adopting µWW/nWW is additive, not a migration).
_MIGRATIONS = {
    "phrases": {
        "notes": "TEXT",
        "deleted_at": "TEXT",
    },
}


def _migrate(conn: sqlite3.Connection) -> None:
    for table, columns in _MIGRATIONS.items():
        present = {row[1] for row in conn.execute(f"PRAGMA table_info({table})")}
        if not present:
            continue  # table doesn't exist yet; CREATE above handles it
        for name, ddl in columns.items():
            if name not in present:
                conn.execute(f"ALTER TABLE {table} ADD COLUMN {name} {ddl}")


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
        _migrate(self._conn)
        self._conn.commit()

    async def close(self) -> None:
        self._conn.close()


    # --- phrases ---

    _PHRASE_COLS = "id, name, display_label, language, notes, deleted_at"

    def list_phrases(self) -> list[Phrase]:
        rows = self._conn.execute(
            f"SELECT {self._PHRASE_COLS} FROM phrases ORDER BY name"
        ).fetchall()
        return [Phrase(*r) for r in rows]

    def get_phrase(self, phrase_id: str) -> Phrase | None:
        row = self._conn.execute(
            f"SELECT {self._PHRASE_COLS} FROM phrases WHERE id = ?",
            (phrase_id,),
        ).fetchone()
        return Phrase(*row) if row else None

    def get_phrase_by_name(self, name: str) -> Phrase | None:
        row = self._conn.execute(
            f"SELECT {self._PHRASE_COLS} FROM phrases WHERE name = ?",
            (name,),
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


    # --- models ---

    _MODEL_COLS = (
        "id, phrase_id, engine, version, engine_phrase_key, source, "
        "filesystem_path, artifact_path, metrics_json, notes, "
        "file_mtime, file_size, created_at, deleted_at"
    )

    def list_models(
        self,
        phrase_id: str | None = None,
        source: str | None = None,
        include_deleted: bool = False,
    ) -> list[Model]:
        where: list[str] = []
        args: list[object] = []
        if not include_deleted:
            where.append("deleted_at IS NULL")
        if phrase_id is not None:
            where.append("phrase_id = ?")
            args.append(phrase_id)
        if source is not None:
            where.append("source = ?")
            args.append(source)
        where_sql = ("WHERE " + " AND ".join(where)) if where else ""
        rows = self._conn.execute(
            f"SELECT {self._MODEL_COLS} FROM models {where_sql} "
            "ORDER BY created_at, version",
            args,
        ).fetchall()
        return [Model(*r) for r in rows]

    def get_model(self, model_id: str, include_deleted: bool = False) -> Model | None:
        row = self._conn.execute(
            f"SELECT {self._MODEL_COLS} FROM models WHERE id = ?"
            + ("" if include_deleted else " AND deleted_at IS NULL"),
            (model_id,),
        ).fetchone()
        return Model(*row) if row else None

    def get_filesystem_model(self, filesystem_path: str, version: str) -> Model | None:
        """Row for a scanner-managed file at a given sidecar version — any
        deleted state, so a re-appearing file resurrects instead of
        colliding with its own history."""
        row = self._conn.execute(
            f"SELECT {self._MODEL_COLS} FROM models "
            "WHERE source = 'filesystem' AND filesystem_path = ? AND version = ?",
            (filesystem_path, version),
        ).fetchone()
        return Model(*row) if row else None

    def get_model_by_version(
        self, phrase_id: str, engine: str, version: str
    ) -> Model | None:
        """Any-state lookup by the natural key — used when reconciling the
        scanner mount against previously uploaded models."""
        row = self._conn.execute(
            f"SELECT {self._MODEL_COLS} FROM models "
            "WHERE phrase_id = ? AND engine = ? AND version = ?",
            (phrase_id, engine, version),
        ).fetchone()
        return Model(*row) if row else None

    def promote_upload_model(
        self, model_id: str, filesystem_path: str, mtime: str, size: int
    ) -> None:
        """An uploaded model copied into the scanner mount becomes a
        filesystem-imported one in place — history preserved (#213
        user story 15)."""
        self._conn.execute(
            "UPDATE models SET source = 'filesystem', filesystem_path = ?, "
            "file_mtime = ?, file_size = ?, deleted_at = NULL WHERE id = ?",
            (filesystem_path, mtime, size, model_id),
        )
        self._conn.commit()

    def active_filesystem_paths(self) -> set[str]:
        return {
            r[0]
            for r in self._conn.execute(
                "SELECT DISTINCT filesystem_path FROM models "
                "WHERE source = 'filesystem' AND deleted_at IS NULL"
            )
        }

    def insert_model(self, model: Model) -> None:
        self._conn.execute(
            f"INSERT INTO models ({self._MODEL_COLS}) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                model.id, model.phrase_id, model.engine, model.version,
                model.engine_phrase_key, model.source, model.filesystem_path,
                model.artifact_path, model.metrics_json, model.notes,
                model.file_mtime, model.file_size, model.created_at,
                model.deleted_at,
            ),
        )
        self._conn.commit()

    def touch_model(self, model_id: str, mtime: str, size: int) -> None:
        """Update-in-place metadata refresh only — the version (and with it
        the metrics/deploy history) is untouched (#213)."""
        self._conn.execute(
            "UPDATE models SET file_mtime = ?, file_size = ? WHERE id = ?",
            (mtime, size, model_id),
        )
        self._conn.commit()

    def resurrect_model(self, model_id: str, mtime: str, size: int) -> None:
        self._conn.execute(
            "UPDATE models SET deleted_at = NULL, file_mtime = ?, file_size = ? "
            "WHERE id = ?",
            (mtime, size, model_id),
        )
        self._conn.commit()

    def soft_delete_model(self, model_id: str) -> None:
        self._conn.execute(
            "UPDATE models SET deleted_at = datetime('now') WHERE id = ?",
            (model_id,),
        )
        self._conn.commit()

    # --- deploy targets ---

    _TARGET_COLS = (
        "id, kind, config_json, current_model_id, last_publish_at, "
        "last_publish_status, last_publish_error, created_at"
    )

    def list_deploy_targets(self) -> list[DeployTarget]:
        rows = self._conn.execute(
            f"SELECT {self._TARGET_COLS} FROM deploy_targets ORDER BY created_at"
        ).fetchall()
        return [DeployTarget(*r) for r in rows]

    def get_deploy_target(self, target_id: str) -> DeployTarget | None:
        row = self._conn.execute(
            f"SELECT {self._TARGET_COLS} FROM deploy_targets WHERE id = ?",
            (target_id,),
        ).fetchone()
        return DeployTarget(*row) if row else None

    def insert_deploy_target(self, target: DeployTarget) -> None:
        self._conn.execute(
            f"INSERT INTO deploy_targets ({self._TARGET_COLS}) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            (
                target.id, target.kind, target.config_json,
                target.current_model_id, target.last_publish_at,
                target.last_publish_status, target.last_publish_error,
                target.created_at,
            ),
        )
        self._conn.commit()

    def record_publish(
        self,
        target_id: str,
        current_model_id: str,
        status: str,
        error: str | None,
        at: str,
    ) -> None:
        """The row change is the publish; push outcome is recorded beside it
        and never rolls the selection back (spec 0011 §Configure & publish)."""
        self._conn.execute(
            "UPDATE deploy_targets SET current_model_id = ?, last_publish_at = ?, "
            "last_publish_status = ?, last_publish_error = ? WHERE id = ?",
            (current_model_id, at, status, error, target_id),
        )
        self._conn.commit()


class _PostgresConnection:
    """Small DB-API compatibility shim for the backend's shared SQL."""

    def __init__(self, connection: Any) -> None:
        self._connection = connection

    def execute(self, query: str, params: tuple[object, ...] = ()) -> Any:
        cursor = self._connection.cursor()
        cursor.execute(
            query.replace("?", "%s").replace("datetime('now')", "CURRENT_TIMESTAMP::text"),
            params,
        )
        return cursor

    def commit(self) -> None:
        self._connection.commit()


class PostgresBackend(SqliteBackend):
    """PostgreSQL implementation sharing Excita's backend behavior and schema."""

    def __init__(self, database_url: str) -> None:
        try:
            import psycopg
        except ImportError as exc:  # pragma: no cover - packaging error
            raise RuntimeError(
                "PostgresBackend requires psycopg; install Excita requirements"
            ) from exc
        connection = psycopg.connect(database_url)
        connection.autocommit = False
        self._conn = _PostgresConnection(connection)
        super().__init__(self._conn)
        schema = _SCHEMA.replace("datetime('now')", "CURRENT_TIMESTAMP::text")
        for statement in schema.split(";"):
            if statement.strip():
                self._conn.execute(statement)
        self._conn.commit()

    async def close(self) -> None:
        self._conn._connection.close()


def new_id() -> str:
    return uuid.uuid4().hex
