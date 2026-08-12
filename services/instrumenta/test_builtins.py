"""Tests for the built-in tool functions.

Unit-level tests for the four v1 built-ins. Kept separate from the app-level
test file because these are pure callables with tight contracts — the
external seam here is the function boundary, not HTTP.
"""

from __future__ import annotations

import pytest

from instrumenta.builtins import http_fetch, math_eval, text_regex, time_now


class TestHttpFetch:
    @pytest.mark.asyncio
    async def test_rejects_non_http_scheme(self) -> None:
        with pytest.raises(ValueError, match="http/https"):
            await http_fetch("file:///etc/passwd")

    @pytest.mark.asyncio
    async def test_rejects_unsupported_method(self) -> None:
        with pytest.raises(ValueError, match="GET/HEAD"):
            await http_fetch("http://example.invalid", method="POST")


class TestTimeNow:
    def test_returns_iso_and_unix(self) -> None:
        result = time_now()
        assert "iso" in result and "unix" in result
        # ISO strings for UTC end in +00:00
        assert result["iso"].endswith("+00:00")
        assert int(result["unix"]) > 0

    def test_rejects_non_utc(self) -> None:
        with pytest.raises(ValueError, match="UTC"):
            time_now("America/Denver")


class TestMathEval:
    @pytest.mark.parametrize(
        "expression,expected",
        [
            ("1 + 1", 2.0),
            ("2 * (3 + 4)", 14.0),
            ("10 / 4", 2.5),
            ("2e2", 200.0),
        ],
    )
    def test_evaluates_arithmetic(self, expression: str, expected: float) -> None:
        assert math_eval(expression)["result"] == expected

    @pytest.mark.parametrize(
        "expression",
        [
            "__import__('os').system('ls')",
            "abs(-1)",
            "open('/etc/passwd').read()",
            "1; import os",
            "x + 1",
        ],
    )
    def test_rejects_non_numeric_input(self, expression: str) -> None:
        with pytest.raises(ValueError):
            math_eval(expression)


class TestTextRegex:
    def test_returns_all_matches_with_groups(self) -> None:
        result = text_regex(r"(\w+)@(\w+)", "a@b c@d not-an-email")
        assert result["matches"] == [
            ["a@b", "a", "b"],
            ["c@d", "c", "d"],
        ]

    def test_honors_ignore_case_flag(self) -> None:
        result = text_regex(r"hello", "Hello HELLO hello", flags="i")
        assert len(result["matches"]) == 3

    def test_rejects_unknown_flag(self) -> None:
        with pytest.raises(ValueError, match="unknown regex flag"):
            text_regex(r".", "x", flags="q")
