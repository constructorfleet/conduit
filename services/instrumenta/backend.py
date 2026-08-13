"""SQLite backend for Instrumenta configuration.

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

    async def close(self) -> None:
        await asyncio.to_thread(self._conn.close)
