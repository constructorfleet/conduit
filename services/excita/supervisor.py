"""Detector supervisor — the runtime side of spec 0011.

Owns:

- Zero-or-more armed `(phrase_id, model_ref, source_device)` bindings, each
  with a warm `Detector` and its own threshold.
- A per-source PCM pre-roll ring buffer (default 2 s @ 16 kHz), so a fire
  can persist "the last 2 s of audio that led here" without the ingress
  path having to remember what it sent.
- An in-memory ring buffer of recent fires (spec 0011 §Standalone posture)
  so an unlinked Excita's operator still has a diagnostic view.

Does not know about Conduit — a fire that also needs to POST to Conduit's
`/v1/wake-events` is a caller concern (spec 0007). The supervisor stops at
"clip persisted + event recorded locally"; that boundary is what lets the
supervisor be exercised in-process from tests without a Conduit peer.
"""

from __future__ import annotations

import threading
import uuid
from collections import deque
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Callable, Iterable

from .backend import Backend, Clip
from .clip_store import ClipStore
from .engines import Detector


PRE_ROLL_MS_DEFAULT = 2000
SAMPLE_RATE = 16000
BYTES_PER_SAMPLE = 2  # int16 mono


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds")


@dataclass
class Binding:
    """One armed detector.

    `id` is the handle the arm endpoint returns and the disarm endpoint
    accepts — an opaque string, so callers never form URLs out of a
    filesystem model path.
    """

    id: str
    phrase_id: str
    model_ref: str
    source_device: str
    detector: Detector
    frames_seen: int = 0
    last_frame_at: str | None = None


@dataclass
class WakeEvent:
    """Local ring-buffer entry (spec 0011 §Standalone posture)."""

    detector_id: str
    phrase_id: str
    source_device: str
    confidence: float
    detected_at: str
    audio_clip_id: str | None = None


@dataclass
class _PreRoll:
    """Per-source rolling PCM buffer."""

    max_bytes: int
    buf: deque[bytes] = field(default_factory=deque)
    total: int = 0

    def push(self, chunk: bytes) -> None:
        self.buf.append(chunk)
        self.total += len(chunk)
        while self.total > self.max_bytes and self.buf:
            dropped = self.buf.popleft()
            self.total -= len(dropped)

    def drain_pcm(self) -> bytes:
        return b"".join(self.buf)


class DetectorSupervisor:
    """Serialises frame dispatch, arm/disarm, and fire handling.

    Detectors are not thread-safe (they hold ONNX runtime state and a
    residual buffer). One process-wide `Lock` around the arm map + `feed`
    calls is the smallest thing that keeps arm/disarm-during-fire honest
    without turning the supervisor into a queue of workers — the frame
    ingress is already async-per-request, and openWakeWord scoring an
    80 ms chunk is on the order of a millisecond, so a lock is cheaper
    than the coordination it would replace.
    """

    def __init__(
        self,
        *,
        backend: Backend,
        clip_store: ClipStore,
        pre_roll_ms: int = PRE_ROLL_MS_DEFAULT,
        wake_event_history: int = 256,
        clock: Callable[[], str] = _now_iso,
    ) -> None:
        self._backend = backend
        self._clip_store = clip_store
        self._pre_roll_bytes = SAMPLE_RATE * BYTES_PER_SAMPLE * pre_roll_ms // 1000
        self._bindings: dict[str, Binding] = {}
        self._pre_rolls: dict[str, _PreRoll] = {}
        self._events: deque[WakeEvent] = deque(maxlen=wake_event_history)
        self._lock = threading.Lock()
        self._clock = clock

    # --- arm / disarm ---

    def arm(
        self,
        *,
        phrase_id: str,
        model_ref: str,
        source_device: str,
        detector: Detector,
    ) -> Binding:
        binding = Binding(
            id=uuid.uuid4().hex,
            phrase_id=phrase_id,
            model_ref=model_ref,
            source_device=source_device,
            detector=detector,
        )
        with self._lock:
            self._bindings[binding.id] = binding
        return binding

    def disarm(self, binding_id: str) -> bool:
        with self._lock:
            return self._bindings.pop(binding_id, None) is not None

    def list_bindings(self) -> list[Binding]:
        with self._lock:
            return list(self._bindings.values())

    def get_binding(self, binding_id: str) -> Binding | None:
        with self._lock:
            return self._bindings.get(binding_id)

    def reset(self, binding_id: str) -> bool:
        with self._lock:
            binding = self._bindings.get(binding_id)
            if binding is None:
                return False
            binding.detector.reset()
            return True

    # --- ingress ---

    def feed(self, source_device: str, pcm_frame: bytes) -> list[WakeEvent]:
        """Dispatch one PCM frame to every binding armed for `source_device`.

        Returns the fire events produced by this frame (usually zero, one
        if a wake). Called from the HTTP handler.
        """
        if not pcm_frame:
            return []
        stamp = self._clock()
        fires: list[WakeEvent] = []

        with self._lock:
            targets = [
                b for b in self._bindings.values() if b.source_device == source_device
            ]
            pre_roll = self._pre_rolls.setdefault(
                source_device, _PreRoll(max_bytes=self._pre_roll_bytes)
            )
            pre_roll.push(pcm_frame)

            for binding in targets:
                binding.frames_seen += 1
                binding.last_frame_at = stamp
                result = binding.detector.feed(pcm_frame)
                if result is None:
                    continue
                confidence, fired = result
                if not fired:
                    continue
                clip_id = self._persist_fire_clip(
                    binding=binding,
                    pcm=pre_roll.drain_pcm(),
                    stamp=stamp,
                )
                event = WakeEvent(
                    detector_id=binding.id,
                    phrase_id=binding.phrase_id,
                    source_device=source_device,
                    confidence=confidence,
                    detected_at=stamp,
                    audio_clip_id=clip_id,
                )
                self._events.append(event)
                fires.append(event)
                # Reset so a single wake produces one event, not a fire on
                # every subsequent overlapping frame from the same phrase.
                binding.detector.reset()

        return fires

    # --- diagnostics ---

    def recent_events(self, limit: int) -> list[WakeEvent]:
        with self._lock:
            events = list(self._events)
        return events[-limit:][::-1]

    # --- internals ---

    def _persist_fire_clip(
        self, *, binding: Binding, pcm: bytes, stamp: str
    ) -> str | None:
        if not pcm:
            return None
        try:
            wav_bytes = _pcm_to_wav(pcm)
            digest, path = self._clip_store.store(wav_bytes, "audio/wav")
        except Exception:  # noqa: BLE001 — a persist failure MUST NOT swallow the fire event itself
            return None

        existing = self._backend.get_clip_by_sha256(binding.phrase_id, digest)
        if existing is not None:
            return existing.id

        duration_ms = int(len(pcm) / (SAMPLE_RATE * BYTES_PER_SAMPLE) * 1000)
        clip = Clip(
            id=uuid.uuid4().hex,
            phrase_id=binding.phrase_id,
            sample_rate=SAMPLE_RATE,
            duration_ms=duration_ms,
            source="detector",
            source_peer=binding.source_device,
            sha256=digest,
            mime_type="audio/wav",
            stored_path=str(path),
            created_at=stamp,
        )
        try:
            self._backend.insert_clip(clip)
        except Exception:  # noqa: BLE001 — a phrase-less DB is a config error, not a runtime one
            return None
        return clip.id


def _pcm_to_wav(pcm: bytes, sample_rate: int = SAMPLE_RATE) -> bytes:
    """Wrap raw int16 mono PCM in a WAV container so it plays in a browser.

    Stdlib `wave` writes headers correctly and adds nothing to Excita's
    dep footprint.
    """
    import wave
    from io import BytesIO

    buf = BytesIO()
    with wave.open(buf, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(BYTES_PER_SAMPLE)
        wav.setframerate(sample_rate)
        wav.writeframes(pcm)
    return buf.getvalue()


def bindings_view(bindings: Iterable[Binding]) -> list[dict[str, object]]:
    """Serialisation used by the HTTP surface."""
    return [
        {
            "id": b.id,
            "phrase_id": b.phrase_id,
            "model_ref": b.model_ref,
            "engine": b.detector.kind.value,
            "source_device": b.source_device,
            "sample_rate": b.detector.sample_rate,
            "frames_seen": b.frames_seen,
            "last_frame_at": b.last_frame_at,
        }
        for b in bindings
    ]
