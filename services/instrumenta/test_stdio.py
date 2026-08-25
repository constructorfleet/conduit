"""Tests for the stdio supervisor and PATH probe.

The stdio supervisor spawns child MCP servers as subprocesses, connects
via stdin/stdout, and autorestarts with capped exponential backoff on
failure. The PATH probe reports which runtimes are available.
"""

from __future__ import annotations

import asyncio
import shutil

import pytest
from fastapi.testclient import TestClient

from instrumenta.path_probe import probe_runtimes
from instrumenta.stdio_supervisor import StdioSupervisor


class TestPathProbe:
    def test_probe_returns_dict(self) -> None:
        result = probe_runtimes()
        assert isinstance(result, dict)
        assert "python3" in result

    def test_python3_found_on_path(self) -> None:
        result = probe_runtimes()
        assert result["python3"] is True

    def test_nonexistent_binary_not_found(self) -> None:
        result = probe_runtimes(runtimes=("nonexistent_binary_xyz",))
        assert result.get("nonexistent_binary_xyz") is False

    def test_reflects_actual_PATH(self) -> None:
        result = probe_runtimes()
        # If python3 is on PATH (which it must be to run this test),
        # it should be found.
        assert shutil.which("python3") is not None
        assert result["python3"] is True


class TestStdioSupervisor:
    def test_spawn_and_list_tools(self, tmp_path) -> None:
        """Spawn a tiny echo MCP server script and list its tools."""
        script = tmp_path / "echo_server.py"
        script.write_text(
            'import sys, json\n'
            'line = sys.stdin.readline()\n'
            'resp = {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"echo","version":"0.1.0"}}}\n'
            'sys.stdout.write(json.dumps(resp) + "\\n")\n'
            'sys.stdout.flush()\n'
            'line = sys.stdin.readline()\n'
            'tools = [{"name":"echo","description":"Echo input","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]\n'
            'resp = {"jsonrpc":"2.0","id":2,"result":{"tools":tools}}\n'
            'sys.stdout.write(json.dumps(resp) + "\\n")\n'
            'sys.stdout.flush()\n'
            'sys.stdin.readline()\n'
        )
        supervisor = StdioSupervisor()
        try:
            tools = asyncio.run(
                supervisor.spawn_and_list_tools(
                    command=f"{sys.executable} {script}",
                    server_name="echo-test",
                )
            )
            assert len(tools) == 1
            assert tools[0]["name"] == "echo-test.echo"
        finally:
            supervisor.close_all()

    def test_nonexistent_command_raises(self) -> None:
        supervisor = StdioSupervisor()
        try:
            with pytest.raises(Exception):
                asyncio.run(
                    supervisor.spawn_and_list_tools(
                        command="nonexistent_binary_xyz_12345",
                        server_name="bad",
                    )
                )
        finally:
            supervisor.close_all()


# Need sys for executable path
import sys
