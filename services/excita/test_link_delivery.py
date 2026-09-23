"""Retry and authentication behavior for the linked wake-event sender."""

from __future__ import annotations

import httpx
import pytest

from conduit_link.models import LinkState, LinkedServicePanel
from excita.link_delivery import deliver_wake_event
from excita.supervisor import WakeEvent


def link() -> LinkState:
    return LinkState(
        conduit_url="http://conduit:8080",
        peer_id="excita-kitchen",
        peer_name="Kitchen Excita",
        sync_token="sync-secret",
        panel=LinkedServicePanel(title="Excita", path="/ui/"),
        linked_at="2026-08-10T14:00:00Z",
    )


def event() -> WakeEvent:
    return WakeEvent(
        detector_id="detector-1",
        phrase_id="hey-conduit",
        source_device="satellite-kitchen",
        confidence=0.91,
        detected_at="2026-08-10T14:22:03.412Z",
        audio_clip_id="clip-1",
    )


@pytest.mark.asyncio
async def test_linked_wake_delivery_retries_transient_failure_with_idempotency_key() -> None:
    attempts: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        attempts.append(request)
        return httpx.Response(503 if len(attempts) == 1 else 202)

    delays: list[float] = []

    async def sleep(delay: float) -> None:
        delays.append(delay)

    await deliver_wake_event(link(), event(), transport=httpx.MockTransport(handler), sleep=sleep)

    assert len(attempts) == 2
    assert attempts[0].url == "http://conduit:8080/v1/wake-events"
    assert attempts[0].headers["authorization"] == "Bearer sync-secret"
    assert attempts[0].headers["idempotency-key"] == attempts[1].headers["idempotency-key"]
    assert attempts[0].read() == attempts[1].read()
    assert delays == [1.0]


@pytest.mark.asyncio
async def test_linked_wake_delivery_stops_retrying_on_unauthorized() -> None:
    attempts = 0

    def handler(_request: httpx.Request) -> httpx.Response:
        nonlocal attempts
        attempts += 1
        return httpx.Response(401)

    await deliver_wake_event(link(), event(), transport=httpx.MockTransport(handler))

    assert attempts == 1
