"""Built-in tools shipped with Instrumenta.

Four small tools the standalone service can offer on a fresh install with no
upstreams configured. Each is a plain callable registered on the `MCPServer`
in `mcp_app.py`; the aggregator PR will add upstream-forwarded tools alongside
these.

Scope discipline: nothing here touches the filesystem, spawns a subprocess,
or executes untrusted code. `shell.exec` is deferred behind a "dangerous
tools" gate per spec #198.
"""

from __future__ import annotations

import re
from datetime import datetime, timezone
from typing import Any

import httpx


async def http_fetch(url: str, method: str = "GET", timeout_seconds: float = 10.0) -> dict[str, Any]:
    """Fetch a URL and return status, headers, and body.

    Only http:// and https:// are permitted. Bodies larger than 1 MiB are
    truncated so tool output stays under a reasonable MCP frame size.
    """
    if not url.startswith(("http://", "https://")):
        raise ValueError(f"http.fetch only supports http/https URLs, got: {url!r}")
    method_upper = method.upper()
    if method_upper not in {"GET", "HEAD"}:
        raise ValueError(f"http.fetch supports GET/HEAD only in v1, got: {method!r}")

    async with httpx.AsyncClient(timeout=timeout_seconds, follow_redirects=True) as client:
        response = await client.request(method_upper, url)
    body = response.text
    truncated = False
    if len(body) > 1_048_576:
        body = body[:1_048_576]
        truncated = True
    return {
        "status": response.status_code,
        "headers": dict(response.headers),
        "body": body,
        "truncated": truncated,
    }


def time_now(timezone_name: str = "UTC") -> dict[str, str]:
    """Return the current wall-clock time.

    Only UTC is supported in v1 to keep the tool free of tzdata surprises.
    """
    if timezone_name.upper() != "UTC":
        raise ValueError(
            f"time.now supports 'UTC' only in v1, got: {timezone_name!r}"
        )
    now = datetime.now(timezone.utc)
    return {"iso": now.isoformat(), "unix": str(int(now.timestamp()))}


_MATH_TOKEN = re.compile(r"^[\d\s+\-*/().e%]+$")


def math_eval(expression: str) -> dict[str, float]:
    """Evaluate a numeric expression.

    Whitelist-based: the input must match a tight character set before it
    goes anywhere near `eval`. The character set forbids identifiers, so no
    function lookups, no attribute access, no builtins reachable.
    """
    if not _MATH_TOKEN.match(expression):
        raise ValueError(
            "math.eval accepts only digits, whitespace, and + - * / ( ) . e %"
        )
    # `eval` runs with empty globals/builtins; the whitelist already forbids
    # names, so this is belt-and-braces.
    result = eval(expression, {"__builtins__": {}}, {})  # noqa: S307 — guarded by regex
    if not isinstance(result, (int, float)):
        raise ValueError(f"math.eval produced a non-numeric result: {result!r}")
    return {"result": float(result)}


def text_regex(pattern: str, text: str, flags: str = "") -> dict[str, list[list[str]]]:
    """Find all matches of `pattern` in `text`.

    `flags` is a short string with characters in {i, m, s}. Each match is
    returned as `[full_match, group1, group2, ...]`.
    """
    flag_map = {"i": re.IGNORECASE, "m": re.MULTILINE, "s": re.DOTALL}
    compiled_flags = 0
    for ch in flags:
        if ch not in flag_map:
            raise ValueError(f"unknown regex flag: {ch!r}")
        compiled_flags |= flag_map[ch]
    compiled = re.compile(pattern, compiled_flags)
    matches: list[list[str]] = []
    for match in compiled.finditer(text):
        row = [match.group(0)]
        row.extend(match.groups(default=""))
        matches.append(row)
    return {"matches": matches}
