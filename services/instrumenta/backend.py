"""SQLite and PostgreSQL backends for Instrumenta configuration.

Stores upstream server configs (with encrypted secrets), per-item enable
flags, local prompts and resources, and audit-log rows. Uses `sqlite3` from
the stdlib run inside `asyncio.to_thread` so the FastAPI event loop stays
unblocked without pulling in an async-sqlite dependency for a workload
measured in a few hundred rows.

The interface is deliberately narrow (`Backend` protocol) so a postgres
backend can slot in without touching `app.py`.
"""

from __future__ import annotations

import asyncio
import sqlite3
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol


@dataclass(frozen=True)
class UpstreamServer:
    """Row for `upstream_servers`.

    HTTP-only in v1: `transport` is always `"http"` and `command` stays None.
    The stdio slice will exercise `command`.
    """

    id: str
    name: str
    transport: str
    url: str | None
    command: str | None
    secret_ciphertext: bytes | None
    enabled: bool
    timeout_seconds: int | None


@dataclass(frozen=True)
class ItemFlag:
    """Row for `item_flags` — enable/disable a tool, prompt, or resource."""

    origin: str
    item_kind: str
    item_name: str
    enabled: bool = True


@dataclass(frozen=True)
class LocalPrompt:
    """Row for `local_prompts` — a locally-authored prompt template."""

    id: str
    name: str
    template: str
    description: str | None = None


@dataclass(frozen=True)
class LocalResource:
    """Row for `local_resources` — a locally-authored static resource."""

    id: str
    uri: str
    name: str
    mime_type: str | None = None
    content: str | None = None


@dataclass(frozen=True)
class AuditEntry:
    """Row for `audit_log` — a recorded tool invocation."""

    id: int
    called_at: str
    peer_id: str | None
    tool_name: str
    args_hash: str
    duration_ms: int | None
    outcome: str


class Backend(Protocol):
    async def close(self) -> None:
        raise NotImplementedError

    def has_encrypted_secret(self) -> bool:
        raise NotImplementedError

    def list_upstream_servers(self) -> list[UpstreamServer]:
        raise NotImplementedError

    def get_upstream_server(self, server_id: str) -> UpstreamServer | None:
        raise NotImplementedError

    def insert_upstream_server(self, server: UpstreamServer) -> None:
        raise NotImplementedError

    def update_upstream_server(self, server: UpstreamServer) -> None:
        raise NotImplementedError

    def delete_upstream_server(self, server_id: str) -> bool:
        raise NotImplementedError

    # ── item_flags ──────────────────────────────────────────────────────

    def list_item_flags(
        self, *, origin: str | None = None, item_kind: str | None = None
    ) -> list[ItemFlag]:
        raise NotImplementedError

    def upsert_item_flag(self, flag: ItemFlag) -> None:
        raise NotImplementedError

    def delete_item_flag(self, origin: str, item_kind: str, item_name: str) -> bool:
        raise NotImplementedError

    # ── local_prompts ───────────────────────────────────────────────────

    def list_local_prompts(self) -> list[LocalPrompt]:
        raise NotImplementedError

    def get_local_prompt(self, prompt_id: str) -> LocalPrompt | None:
        raise NotImplementedError

    def insert_local_prompt(self, prompt: LocalPrompt) -> None:
        raise NotImplementedError

    def update_local_prompt(self, prompt: LocalPrompt) -> None:
        raise NotImplementedError

    def delete_local_prompt(self, prompt_id: str) -> bool:
        raise NotImplementedError

    # ── local_resources ─────────────────────────────────────────────────

    def list_local_resources(self) -> list[LocalResource]:
        raise NotImplementedError

    def get_local_resource(self, resource_id: str) -> LocalResource | None:
        raise NotImplementedError

    def insert_local_resource(self, resource: LocalResource) -> None:
        raise NotImplementedError

    def update_local_resource(self, resource: LocalResource) -> None:
        raise NotImplementedError

    def delete_local_resource(self, resource_id: str) -> bool:
        raise NotImplementedError

    # ── audit_log ───────────────────────────────────────────────────────

    def insert_audit_entry(self, entry: AuditEntry) -> None:
        raise NotImplementedError

    def list_audit_entries(
        self,
        *,
        tool_name: str | None = None,
        outcome: str | None = None,
        limit: int = 50,
    ) -> list[AuditEntry]:
        raise NotImplementedError


_SCHEMA = """
CREATE TABLE IF NOT EXISTS upstream_servers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    transport TEXT NOT NULL CHECK (transport IN ('http', 'stdio')),
    url TEXT,
    command TEXT,
    secret_ciphertext BLOB,
    enabled INTEGER NOT NULL DEFAULT 1,
    timeout_seconds INTEGER,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS item_flags (
    origin TEXT NOT NULL,
    item_kind TEXT NOT NULL CHECK (item_kind IN ('tool', 'prompt', 'resource')),
    item_name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (origin, item_kind, item_name)
);

CREATE TABLE IF NOT EXISTS local_prompts (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    template TEXT NOT NULL,
    description TEXT
);

CREATE TABLE IF NOT EXISTS local_resources (
    id TEXT PRIMARY KEY,
    uri TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    mime_type TEXT,
    content TEXT
);

CREATE TABLE IF NOT EXISTS audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    called_at TEXT NOT NULL DEFAULT (datetime('now')),
    peer_id TEXT,
    tool_name TEXT NOT NULL,
    args_hash TEXT NOT NULL,
    duration_ms INTEGER,
    outcome TEXT NOT NULL CHECK (outcome IN ('ok', 'error', 'timeout'))
);
"""


def _row_to_server(row: sqlite3.Row) -> UpstreamServer:
    return UpstreamServer(
        id=row["id"],
        name=row["name"],
        transport=row["transport"],
        url=row["url"],
        command=row["command"],
        secret_ciphertext=row["secret_ciphertext"],
        enabled=bool(row["enabled"]),
        timeout_seconds=row["timeout_seconds"],
    )


def _row_to_flag(row: sqlite3.Row) -> ItemFlag:
    return ItemFlag(
        origin=row["origin"],
        item_kind=row["item_kind"],
        item_name=row["item_name"],
        enabled=bool(row["enabled"]),
    )


def _row_to_prompt(row: sqlite3.Row) -> LocalPrompt:
    return LocalPrompt(
        id=row["id"],
        name=row["name"],
        template=row["template"],
        description=row["description"],
    )


def _row_to_resource(row: sqlite3.Row) -> LocalResource:
    return LocalResource(
        id=row["id"],
        uri=row["uri"],
        name=row["name"],
        mime_type=row["mime_type"],
        content=row["content"],
    )


def _row_to_audit(row: sqlite3.Row) -> AuditEntry:
    return AuditEntry(
        id=row["id"],
        called_at=row["called_at"],
        peer_id=row["peer_id"],
        tool_name=row["tool_name"],
        args_hash=row["args_hash"],
        duration_ms=row["duration_ms"],
        outcome=row["outcome"],
    )


class SqliteBackend:
    """SQLite-backed configuration store.

    A single connection with `check_same_thread=False` is held for the
    lifetime of the app; writes are wrapped in `asyncio.to_thread` at call
    sites that need it. The connection uses autocommit (`isolation_level=None`)
    so every INSERT/UPDATE is immediately durable.
    """

    def __init__(self, path: Path):
        self.path = path
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._conn = sqlite3.connect(
            self.path, check_same_thread=False, isolation_level=None
        )
        self._conn.row_factory = sqlite3.Row
        self._conn.executescript(_SCHEMA)

    def has_encrypted_secret(self) -> bool:
        """Any row in `upstream_servers` with a non-null `secret_ciphertext`.

        Used by the app factory to fail loud on boot when secrets exist but
        `INSTRUMENTA_SECRET_KEY` is not set — the "refuses to start" contract
        from spec #198.
        """
        cur = self._conn.execute(
            "SELECT 1 FROM upstream_servers WHERE secret_ciphertext IS NOT NULL LIMIT 1"
        )
        return cur.fetchone() is not None

    def list_upstream_servers(self) -> list[UpstreamServer]:
        cur = self._conn.execute(
            "SELECT * FROM upstream_servers ORDER BY name"
        )
        return [_row_to_server(row) for row in cur.fetchall()]

    def get_upstream_server(self, server_id: str) -> UpstreamServer | None:
        cur = self._conn.execute(
            "SELECT * FROM upstream_servers WHERE id = ?", (server_id,)
        )
        row = cur.fetchone()
        return _row_to_server(row) if row else None

    def insert_upstream_server(self, server: UpstreamServer) -> None:
        self._conn.execute(
            "INSERT INTO upstream_servers "
            "(id, name, transport, url, command, secret_ciphertext, enabled, timeout_seconds) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            (
                server.id,
                server.name,
                server.transport,
                server.url,
                server.command,
                server.secret_ciphertext,
                1 if server.enabled else 0,
                server.timeout_seconds,
            ),
        )

    def update_upstream_server(self, server: UpstreamServer) -> None:
        self._conn.execute(
            "UPDATE upstream_servers "
            "SET name = ?, transport = ?, url = ?, command = ?, "
            "secret_ciphertext = ?, enabled = ?, timeout_seconds = ? "
            "WHERE id = ?",
            (
                server.name,
                server.transport,
                server.url,
                server.command,
                server.secret_ciphertext,
                1 if server.enabled else 0,
                server.timeout_seconds,
                server.id,
            ),
        )

    def delete_upstream_server(self, server_id: str) -> bool:
        cur = self._conn.execute(
            "DELETE FROM upstream_servers WHERE id = ?", (server_id,)
        )
        return cur.rowcount > 0

    # ── item_flags ──────────────────────────────────────────────────────

    def list_item_flags(
        self, *, origin: str | None = None, item_kind: str | None = None
    ) -> list[ItemFlag]:
        clauses: list[str] = []
        params: list[object] = []
        if origin is not None:
            clauses.append("origin = ?")
            params.append(origin)
        if item_kind is not None:
            clauses.append("item_kind = ?")
            params.append(item_kind)
        where = f" WHERE {' AND '.join(clauses)}" if clauses else ""
        cur = self._conn.execute(
            f"SELECT * FROM item_flags{where} ORDER BY origin, item_kind, item_name",
            params,
        )
        return [_row_to_flag(row) for row in cur.fetchall()]

    def upsert_item_flag(self, flag: ItemFlag) -> None:
        self._conn.execute(
            "INSERT INTO item_flags (origin, item_kind, item_name, enabled) "
            "VALUES (?, ?, ?, ?) "
            "ON CONFLICT (origin, item_kind, item_name) DO UPDATE SET enabled = excluded.enabled",
            (flag.origin, flag.item_kind, flag.item_name, 1 if flag.enabled else 0),
        )

    def delete_item_flag(self, origin: str, item_kind: str, item_name: str) -> bool:
        cur = self._conn.execute(
            "DELETE FROM item_flags WHERE origin = ? AND item_kind = ? AND item_name = ?",
            (origin, item_kind, item_name),
        )
        return cur.rowcount > 0

    # ── local_prompts ───────────────────────────────────────────────────

    def list_local_prompts(self) -> list[LocalPrompt]:
        cur = self._conn.execute(
            "SELECT * FROM local_prompts ORDER BY name"
        )
        return [_row_to_prompt(row) for row in cur.fetchall()]

    def get_local_prompt(self, prompt_id: str) -> LocalPrompt | None:
        cur = self._conn.execute(
            "SELECT * FROM local_prompts WHERE id = ?", (prompt_id,)
        )
        row = cur.fetchone()
        return _row_to_prompt(row) if row else None

    def insert_local_prompt(self, prompt: LocalPrompt) -> None:
        self._conn.execute(
            "INSERT INTO local_prompts (id, name, template, description) "
            "VALUES (?, ?, ?, ?)",
            (prompt.id, prompt.name, prompt.template, prompt.description),
        )

    def update_local_prompt(self, prompt: LocalPrompt) -> None:
        self._conn.execute(
            "UPDATE local_prompts SET name = ?, template = ?, description = ? "
            "WHERE id = ?",
            (prompt.name, prompt.template, prompt.description, prompt.id),
        )

    def delete_local_prompt(self, prompt_id: str) -> bool:
        cur = self._conn.execute(
            "DELETE FROM local_prompts WHERE id = ?", (prompt_id,)
        )
        return cur.rowcount > 0

    # ── local_resources ─────────────────────────────────────────────────

    def list_local_resources(self) -> list[LocalResource]:
        cur = self._conn.execute(
            "SELECT * FROM local_resources ORDER BY name"
        )
        return [_row_to_resource(row) for row in cur.fetchall()]

    def get_local_resource(self, resource_id: str) -> LocalResource | None:
        cur = self._conn.execute(
            "SELECT * FROM local_resources WHERE id = ?", (resource_id,)
        )
        row = cur.fetchone()
        return _row_to_resource(row) if row else None

    def insert_local_resource(self, resource: LocalResource) -> None:
        self._conn.execute(
            "INSERT INTO local_resources (id, uri, name, mime_type, content) "
            "VALUES (?, ?, ?, ?, ?)",
            (resource.id, resource.uri, resource.name, resource.mime_type, resource.content),
        )

    def update_local_resource(self, resource: LocalResource) -> None:
        self._conn.execute(
            "UPDATE local_resources SET uri = ?, name = ?, mime_type = ?, content = ? "
            "WHERE id = ?",
            (resource.uri, resource.name, resource.mime_type, resource.content, resource.id),
        )

    def delete_local_resource(self, resource_id: str) -> bool:
        cur = self._conn.execute(
            "DELETE FROM local_resources WHERE id = ?", (resource_id,)
        )
        return cur.rowcount > 0

    # ── audit_log ───────────────────────────────────────────────────────

    def insert_audit_entry(self, entry: AuditEntry) -> None:
        self._conn.execute(
            "INSERT INTO audit_log (called_at, peer_id, tool_name, args_hash, duration_ms, outcome) "
            "VALUES (?, ?, ?, ?, ?, ?)",
            (
                entry.called_at,
                entry.peer_id,
                entry.tool_name,
                entry.args_hash,
                entry.duration_ms,
                entry.outcome,
            ),
        )

    def list_audit_entries(
        self,
        *,
        tool_name: str | None = None,
        outcome: str | None = None,
        limit: int = 50,
    ) -> list[AuditEntry]:
        clauses: list[str] = []
        params: list[object] = []
        if tool_name is not None:
            clauses.append("tool_name = ?")
            params.append(tool_name)
        if outcome is not None:
            clauses.append("outcome = ?")
            params.append(outcome)
        where = f" WHERE {' AND '.join(clauses)}" if clauses else ""
        params.append(limit)
        cur = self._conn.execute(
            f"SELECT * FROM audit_log{where} ORDER BY id DESC LIMIT ?",
            params,
        )
        return [_row_to_audit(row) for row in cur.fetchall()]

    async def close(self) -> None:
        await asyncio.to_thread(self._conn.close)


_POSTGRES_SCHEMA = """
CREATE TABLE IF NOT EXISTS upstream_servers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    transport TEXT NOT NULL CHECK (transport IN ('http', 'stdio')),
    url TEXT,
    command TEXT,
    secret_ciphertext BYTEA,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    timeout_seconds INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE IF NOT EXISTS item_flags (
    origin TEXT NOT NULL,
    item_kind TEXT NOT NULL CHECK (item_kind IN ('tool', 'prompt', 'resource')),
    item_name TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    PRIMARY KEY (origin, item_kind, item_name)
);
CREATE TABLE IF NOT EXISTS local_prompts (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    template TEXT NOT NULL,
    description TEXT
);
CREATE TABLE IF NOT EXISTS local_resources (
    id TEXT PRIMARY KEY,
    uri TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    mime_type TEXT,
    content TEXT
);
CREATE TABLE IF NOT EXISTS audit_log (
    id BIGSERIAL PRIMARY KEY,
    called_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::text,
    peer_id TEXT,
    tool_name TEXT NOT NULL,
    args_hash TEXT NOT NULL,
    duration_ms INTEGER,
    outcome TEXT NOT NULL CHECK (outcome IN ('ok', 'error', 'timeout'))
);
"""


class PostgresBackend:
    """PostgreSQL-backed implementation of the Instrumenta backend protocol."""

    def __init__(self, database_url: str):
        try:
            import psycopg
            from psycopg.rows import dict_row
        except ImportError as exc:  # pragma: no cover - packaging/configuration error
            raise RuntimeError(
                "PostgresBackend requires the psycopg package; install instrumenta requirements"
            ) from exc
        self._conn = psycopg.connect(database_url, row_factory=dict_row)
        self._conn.autocommit = True
        with self._conn.cursor() as cur:
            cur.execute(_POSTGRES_SCHEMA)

    def has_encrypted_secret(self) -> bool:
        with self._conn.cursor() as cur:
            cur.execute("SELECT 1 FROM upstream_servers WHERE secret_ciphertext IS NOT NULL LIMIT 1")
            return cur.fetchone() is not None

    def list_upstream_servers(self) -> list[UpstreamServer]:
        return self._fetch_servers("SELECT * FROM upstream_servers ORDER BY name")

    def get_upstream_server(self, server_id: str) -> UpstreamServer | None:
        with self._conn.cursor() as cur:
            cur.execute("SELECT * FROM upstream_servers WHERE id = %s", (server_id,))
            row = cur.fetchone()
        return _row_to_server(row) if row else None

    def insert_upstream_server(self, server: UpstreamServer) -> None:
        self._execute(
            "INSERT INTO upstream_servers (id, name, transport, url, command, secret_ciphertext, enabled, timeout_seconds) VALUES (%s, %s, %s, %s, %s, %s, %s, %s)",
            (server.id, server.name, server.transport, server.url, server.command, server.secret_ciphertext, server.enabled, server.timeout_seconds),
        )

    def update_upstream_server(self, server: UpstreamServer) -> None:
        self._execute(
            "UPDATE upstream_servers SET name = %s, transport = %s, url = %s, command = %s, secret_ciphertext = %s, enabled = %s, timeout_seconds = %s WHERE id = %s",
            (server.name, server.transport, server.url, server.command, server.secret_ciphertext, server.enabled, server.timeout_seconds, server.id),
        )

    def delete_upstream_server(self, server_id: str) -> bool:
        return self._execute("DELETE FROM upstream_servers WHERE id = %s", (server_id,)) > 0

    def list_item_flags(self, *, origin: str | None = None, item_kind: str | None = None) -> list[ItemFlag]:
        clauses, params = self._filters(origin=origin, item_kind=item_kind)
        rows = self._query(f"SELECT * FROM item_flags{clauses} ORDER BY origin, item_kind, item_name", params)
        return [_row_to_flag(row) for row in rows]

    def upsert_item_flag(self, flag: ItemFlag) -> None:
        self._execute(
            "INSERT INTO item_flags (origin, item_kind, item_name, enabled) VALUES (%s, %s, %s, %s) ON CONFLICT (origin, item_kind, item_name) DO UPDATE SET enabled = EXCLUDED.enabled",
            (flag.origin, flag.item_kind, flag.item_name, flag.enabled),
        )

    def delete_item_flag(self, origin: str, item_kind: str, item_name: str) -> bool:
        return self._execute("DELETE FROM item_flags WHERE origin = %s AND item_kind = %s AND item_name = %s", (origin, item_kind, item_name)) > 0

    def list_local_prompts(self) -> list[LocalPrompt]:
        return [_row_to_prompt(row) for row in self._query("SELECT * FROM local_prompts ORDER BY name")]

    def get_local_prompt(self, prompt_id: str) -> LocalPrompt | None:
        rows = self._query("SELECT * FROM local_prompts WHERE id = %s", (prompt_id,))
        return _row_to_prompt(rows[0]) if rows else None

    def insert_local_prompt(self, prompt: LocalPrompt) -> None:
        self._execute("INSERT INTO local_prompts (id, name, template, description) VALUES (%s, %s, %s, %s)", (prompt.id, prompt.name, prompt.template, prompt.description))

    def update_local_prompt(self, prompt: LocalPrompt) -> None:
        self._execute("UPDATE local_prompts SET name = %s, template = %s, description = %s WHERE id = %s", (prompt.name, prompt.template, prompt.description, prompt.id))

    def delete_local_prompt(self, prompt_id: str) -> bool:
        return self._execute("DELETE FROM local_prompts WHERE id = %s", (prompt_id,)) > 0

    def list_local_resources(self) -> list[LocalResource]:
        return [_row_to_resource(row) for row in self._query("SELECT * FROM local_resources ORDER BY name")]

    def get_local_resource(self, resource_id: str) -> LocalResource | None:
        rows = self._query("SELECT * FROM local_resources WHERE id = %s", (resource_id,))
        return _row_to_resource(rows[0]) if rows else None

    def insert_local_resource(self, resource: LocalResource) -> None:
        self._execute("INSERT INTO local_resources (id, uri, name, mime_type, content) VALUES (%s, %s, %s, %s, %s)", (resource.id, resource.uri, resource.name, resource.mime_type, resource.content))

    def update_local_resource(self, resource: LocalResource) -> None:
        self._execute("UPDATE local_resources SET uri = %s, name = %s, mime_type = %s, content = %s WHERE id = %s", (resource.uri, resource.name, resource.mime_type, resource.content, resource.id))

    def delete_local_resource(self, resource_id: str) -> bool:
        return self._execute("DELETE FROM local_resources WHERE id = %s", (resource_id,)) > 0

    def insert_audit_entry(self, entry: AuditEntry) -> None:
        self._execute("INSERT INTO audit_log (called_at, peer_id, tool_name, args_hash, duration_ms, outcome) VALUES (%s, %s, %s, %s, %s, %s)", (entry.called_at, entry.peer_id, entry.tool_name, entry.args_hash, entry.duration_ms, entry.outcome))

    def list_audit_entries(self, *, tool_name: str | None = None, outcome: str | None = None, limit: int = 50) -> list[AuditEntry]:
        clauses, params = self._filters(tool_name=tool_name, outcome=outcome)
        params.append(limit)
        rows = self._query(f"SELECT * FROM audit_log{clauses} ORDER BY id DESC LIMIT %s", params)
        return [_row_to_audit(row) for row in rows]

    def _fetch_servers(self, query: str) -> list[UpstreamServer]:
        return [_row_to_server(row) for row in self._query(query)]

    def _query(self, query: str, params: object = ()) -> list[dict[str, object]]:
        with self._conn.cursor() as cur:
            cur.execute(query, params)
            return list(cur.fetchall())

    def _execute(self, query: str, params: object = ()) -> int:
        with self._conn.cursor() as cur:
            cur.execute(query, params)
            return cur.rowcount

    @staticmethod
    def _filters(**values: object) -> tuple[str, list[object]]:
        clauses = [f"{key} = %s" for key, value in values.items() if value is not None]
        params = [value for value in values.values() if value is not None]
        return (f" WHERE {' AND '.join(clauses)}" if clauses else ""), params

    async def close(self) -> None:
        await asyncio.to_thread(self._conn.close)
