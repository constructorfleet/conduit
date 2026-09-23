"""Peer-to-Conduit delivery for Excita wake detections."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Awaitable, Callable

import httpx

from conduit_link.models import LinkState

from .supervisor import WakeEvent

LOG = logging.getLogger("excita.link")


async def deliver_wake_event(
    link: LinkState,
    event: WakeEvent,
    *,
    transport: httpx.AsyncBaseTransport | None = None,
    sleep: Callable[[float], Awaitable[None]] = asyncio.sleep,
) -> None:
    """Retry one idempotent event until accepted; stop on invalid credentials."""
    url = f"{link.conduit_url.rstrip('/')}/v1/wake-events"
    payload = {
        "event_type": event.event_type,
        "peer_id": link.peer_id,
        "phrase": event.phrase_id,
        "confidence": event.confidence,
        "detected_at": event.detected_at,
        "source_device": event.source_device,
        "audio_clip_ref": event.audio_clip_id,
    }
    delay = 1.0
    async with httpx.AsyncClient(timeout=3.0, transport=transport) as client:
        while True:
            try:
                response = await client.post(
                    url,
                    json=payload,
                    headers={
                        "Authorization": f"Bearer {link.sync_token}",
                        "Idempotency-Key": event.event_id,
                    },
                )
                if 200 <= response.status_code < 300:
                    return
                if response.status_code == 401:
                    LOG.error(
                        "wake event delivery rejected: peer=%s capability=excita.wake-events status=401",
                        link.peer_id,
                    )
                    return
                error = f"HTTP {response.status_code}"
            except httpx.HTTPError as exc:
                error = str(exc)
            LOG.warning(
                "wake event delivery failed: peer=%s capability=excita.wake-events retry_in=%.0fs error=%s",
                link.peer_id,
                delay,
                error,
            )
            await sleep(delay)
            delay = min(delay * 2, 60.0)
