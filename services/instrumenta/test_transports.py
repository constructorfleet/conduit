"""Tests for the downstream MCP transports and their per-transport toggles.

Spec #198 user stories 16/17: both `/mcp/sse` and `/mcp/http` are mounted
and enabled by default; each can be switched off through `/transports`
and the choice persists across restarts.
"""

from __future__ import annotations

from pathlib import Path

from instrumenta.backend import SqliteBackend


class TestBackendTransportFlags:
    def test_fresh_db_reports_both_transports_enabled(self, tmp_path: Path) -> None:
        backend = SqliteBackend(tmp_path / "instrumenta.db")
        flags = backend.list_transport_flags()
        assert {flag.transport: flag.enabled for flag in flags} == {
            "http": True,
            "sse": True,
        }

    def test_disable_persists_across_reopen(self, tmp_path: Path) -> None:
        db_path = tmp_path / "instrumenta.db"
        SqliteBackend(db_path).set_transport_enabled("sse", False)
        flags = SqliteBackend(db_path).list_transport_flags()
        assert {flag.transport: flag.enabled for flag in flags} == {
            "http": True,
            "sse": False,
        }

    def test_re_enable_overwrites_previous_value(self, tmp_path: Path) -> None:
        backend = SqliteBackend(tmp_path / "instrumenta.db")
        backend.set_transport_enabled("http", False)
        backend.set_transport_enabled("http", True)
        assert backend.is_transport_enabled("http") is True
