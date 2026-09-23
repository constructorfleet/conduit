"""Contract tests shared by the SQLite and PostgreSQL backends.

Set ``INSTRUMENTA_TEST_POSTGRES_URL`` to include PostgreSQL in the run. Local
runs skip the database-backed cases when no server is configured; CI starts
the compose database and sets the URL.
"""

from __future__ import annotations

import asyncio
import os
from pathlib import Path

import pytest

from instrumenta.backend import (
    AuditEntry,
    Backend,
    ItemFlag,
    LocalPrompt,
    LocalResource,
    PostgresBackend,
    SqliteBackend,
    UpstreamServer,
)


@pytest.fixture(params=["sqlite", "postgres"])
def backend(request: pytest.FixtureRequest, tmp_path: Path) -> Backend:
    if request.param == "sqlite":
        instance: Backend = SqliteBackend(tmp_path / "instrumenta.db")
    else:
        url = os.getenv("INSTRUMENTA_TEST_POSTGRES_URL")
        if not url:
            pytest.skip("set INSTRUMENTA_TEST_POSTGRES_URL to run PostgreSQL tests")
        instance = PostgresBackend(url)
    yield instance
    # Exercise the public lifecycle seam for every backend.
    asyncio.run(instance.close())


def test_backend_contract(backend: Backend) -> None:
    server = UpstreamServer(
        id="server-1", name="alpha", transport="http", url="https://example.invalid",
        command=None, secret_ciphertext=b"secret", enabled=True, timeout_seconds=30,
    )
    backend.insert_upstream_server(server)
    assert backend.has_encrypted_secret()
    assert backend.get_upstream_server(server.id) == server
    assert backend.list_upstream_servers() == [server]

    updated = server.__class__(**{**server.__dict__, "enabled": False})
    backend.update_upstream_server(updated)
    assert backend.get_upstream_server(server.id) == updated
    assert backend.delete_upstream_server(server.id)
    assert not backend.delete_upstream_server(server.id)

    flag = ItemFlag("alpha", "tool", "echo", False)
    backend.upsert_item_flag(flag)
    assert backend.list_item_flags(origin="alpha") == [flag]
    assert backend.delete_item_flag("alpha", "tool", "echo")

    prompt = LocalPrompt("prompt-1", "greet", "Hello", "desc")
    backend.insert_local_prompt(prompt)
    assert backend.get_local_prompt(prompt.id) == prompt
    assert backend.list_local_prompts() == [prompt]
    assert backend.delete_local_prompt(prompt.id)

    resource = LocalResource("resource-1", "urn:test", "Test", "text/plain", "body")
    backend.insert_local_resource(resource)
    assert backend.get_local_resource(resource.id) == resource
    assert backend.list_local_resources() == [resource]
    assert backend.delete_local_resource(resource.id)

    entry = AuditEntry(0, "2026-01-01T00:00:00Z", "peer", "echo", "hash", 4, "ok")
    backend.insert_audit_entry(entry)
    rows = backend.list_audit_entries(tool_name="echo")
    assert len(rows) == 1
    assert rows[0].tool_name == entry.tool_name
