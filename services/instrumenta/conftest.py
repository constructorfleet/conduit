"""Shared test fixtures for Instrumenta.

Provides a fake upstream MCP server (in-memory transport) that tests can
use to exercise the aggregator, forwarding, and tool-call audit paths
without real network calls.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest
from cryptography.fernet import Fernet
from fastapi.testclient import TestClient
from mcp.server.mcpserver import MCPServer

from instrumenta.app import Config, create_app


# ── Shared fixtures ─────────────────────────────────────────────────────


@pytest.fixture(autouse=True)
def clean_postgres_database() -> None:
    """Keep the shared CI PostgreSQL database isolated per test."""
    url = os.getenv("INSTRUMENTA_TEST_POSTGRES_URL")
    if not url:
        return
    import psycopg

    with psycopg.connect(url, autocommit=True) as connection:
        with connection.cursor() as cursor:
            try:
                cursor.execute(
                    "TRUNCATE TABLE upstream_servers, item_flags, local_prompts, "
                    "local_resources, audit_log RESTART IDENTITY CASCADE"
                )
            except psycopg.errors.UndefinedTable:
                # Some test runs hit this before migrations/schema setup; skipping
                # cleanup is intentional when target tables do not yet exist.
                pass


@pytest.fixture
def secret_key() -> str:
    return Fernet.generate_key().decode()


@pytest.fixture(params=["sqlite", "postgres"])
def config(request: pytest.FixtureRequest, tmp_path: Path, secret_key: str) -> Config:
    if request.param == "postgres":
        database_url = os.getenv("INSTRUMENTA_TEST_POSTGRES_URL")
        if not database_url:
            pytest.skip("set INSTRUMENTA_TEST_POSTGRES_URL to run PostgreSQL tests")
        return Config(
            data_dir=tmp_path,
            backend_type="postgres",
            database_url=database_url,
            api_key=None,
            base_url="http://localhost:8085",
            secret_key=secret_key,
        )
    return Config(
        data_dir=tmp_path,
        backend_type="sqlite",
        api_key=None,
        base_url="http://localhost:8085",
        secret_key=secret_key,
    )


@pytest.fixture
def client(config: Config):
    with TestClient(create_app(config)) as c:
        yield c


# ── Fake upstream MCP server ────────────────────────────────────────────


def _echo_tool(text: str = "hello") -> str:
    """Echo tool: returns the input text."""
    return text


_echo_tool.__name__ = "echo"


def _add_tool(a: float = 0, b: float = 0) -> float:
    """Add tool: returns a + b."""
    return a + b


_add_tool.__name__ = "add"


def build_fake_upstream() -> MCPServer:
    """Build a fake MCP server with two tools for testing aggregation."""
    server = MCPServer(name="fake-upstream", version="0.1.0")
    server.add_tool(_echo_tool, name="echo", description="Echo back the input text")
    server.add_tool(_add_tool, name="add", description="Add two numbers")
    return server
