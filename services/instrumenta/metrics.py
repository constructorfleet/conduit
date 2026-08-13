"""Prometheus metrics for Instrumenta.

Exposes a `/metrics` endpoint on a separate listener so scrapes do not touch
the authenticated service surface and cannot be blocked by application-level
middleware. Default bind is `0.0.0.0:9090`; override with
`INSTRUMENTA_METRICS_BIND` (host:port).
"""

from __future__ import annotations

import logging
import os
import time

from prometheus_client import Counter, Histogram, start_http_server
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request

LOG = logging.getLogger(__name__)

_SERVICE = "instrumenta"

REQUESTS = Counter(
    "http_requests_total",
    "HTTP requests handled by the service.",
    ["service", "method", "path", "status"],
)
LATENCY = Histogram(
    "http_request_duration_seconds",
    "HTTP request duration in seconds.",
    ["service", "method", "path"],
)


class PrometheusMiddleware(BaseHTTPMiddleware):
    async def dispatch(self, request: Request, call_next):
        start = time.perf_counter()
        response = await call_next(request)
        duration = time.perf_counter() - start
        # Label by the matched route template, not the raw URL, so path params
        # do not explode metric cardinality. Unmatched requests (404s) collapse
        # to a single bucket.
        route = request.scope.get("route")
        path = getattr(route, "path", "__unmatched__")
        REQUESTS.labels(_SERVICE, request.method, path, str(response.status_code)).inc()
        LATENCY.labels(_SERVICE, request.method, path).observe(duration)
        return response


_started = False


def start_metrics_server(env_var: str = "INSTRUMENTA_METRICS_BIND", default: str = "0.0.0.0:9090") -> None:
    """Start the Prometheus HTTP server on a background thread.

    Idempotent: safe to call multiple times (e.g. from tests that repeatedly
    invoke `create_app`). On bind failure the error is logged and swallowed so
    the service still starts — a service that refused to run because Prometheus
    could not bind would trade a scrape for the workload itself.
    """
    global _started
    if _started:
        return
    bind = os.getenv(env_var, default)
    host, _, port = bind.rpartition(":")
    try:
        start_http_server(int(port), addr=host or "0.0.0.0")
    except OSError as error:
        LOG.warning("prometheus metrics server failed to bind %s: %s", bind, error)
        return
    _started = True
    LOG.info("prometheus metrics listening on %s", bind)
