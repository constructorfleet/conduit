"""Prometheus metrics for wyoming-asr.

Wyoming is a framed TCP protocol, not HTTP, so scrapes cannot ride the service
port. This module runs a standalone Prometheus HTTP listener (default
`0.0.0.0:9090`, override with `ASR_METRICS_BIND`) alongside the Wyoming
server, and exposes counters/histograms for transcribe events. Default
process/GC collectors come from prometheus_client for free.
"""

from __future__ import annotations

import logging
import os

from prometheus_client import Counter, Histogram, start_http_server

LOG = logging.getLogger(__name__)

_SERVICE = "wyoming-asr"

TRANSCRIBE_REQUESTS = Counter(
    "asr_transcribe_requests_total",
    "Wyoming Transcribe events handled.",
    ["service", "outcome"],
)
TRANSCRIBE_DURATION = Histogram(
    "asr_transcribe_duration_seconds",
    "End-to-end transcribe duration in seconds.",
    ["service"],
)
TRANSCRIBE_AUDIO_SECONDS = Histogram(
    "asr_transcribe_audio_seconds",
    "Audio duration transcribed per request, in seconds.",
    ["service"],
)


def label(outcome: str) -> dict:
    """Return the standard label set for TRANSCRIBE_REQUESTS."""
    return {"service": _SERVICE, "outcome": outcome}


_started = False


def start_metrics_server(env_var: str = "ASR_METRICS_BIND", default: str = "0.0.0.0:9090") -> None:
    """Start the Prometheus HTTP server on a background thread. Idempotent."""
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
