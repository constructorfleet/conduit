"""Engine adapter Protocol + null implementation."""

from __future__ import annotations

from enum import Enum
from typing import Protocol


class EngineKind(str, Enum):
    OPENWAKEWORD = "openwakeword"
    MICROWAKEWORD = "microwakeword"
    PORCUPINE = "porcupine"


class NotSupportedError(RuntimeError):
    """Raised by adapters for operations they cannot perform.

    Preferred over a silent stub so a call site never mistakes "engine
    isn't installed" for "engine returned zero score" (spec 0011 §Engine
    abstraction — Porcupine's `train` is the motivating case).
    """


class Detector(Protocol):
    """Warm, per-model handle for the live detection loop (spec 0011)."""

    kind: EngineKind
    phrase_id: str
    sample_rate: int

    def feed(self, pcm_frame: bytes) -> tuple[float, bool] | None:
        """Feed one PCM frame; return `(confidence, fired)` or `None` on silence."""

    def reset(self) -> None: ...


class WakeWordEngine(Protocol):
    kind: EngineKind

    def load(self, model_ref: str, phrase_id: str) -> Detector: ...
    def score(self, audio: bytes, model_ref: str) -> list[float]: ...
    def train(self, dataset_snapshot_id: str, base: str | None) -> str: ...
    def package(self, model_ref: str, target_kind: str) -> bytes: ...


class NullEngine:
    """Placeholder that answers the API surface without doing any work.

    Every method raises `NotSupportedError` with a message naming the engine
    kind and the operation. Wired at boot so the HTTP surface can be
    exercised end-to-end before a real adapter lands.
    """

    def __init__(self, kind: EngineKind) -> None:
        self.kind = kind

    def load(self, model_ref: str, phrase_id: str) -> Detector:
        raise NotSupportedError(f"{self.kind.value}: load not implemented")

    def score(self, audio: bytes, model_ref: str) -> list[float]:
        raise NotSupportedError(f"{self.kind.value}: score not implemented")

    def train(self, dataset_snapshot_id: str, base: str | None) -> str:
        raise NotSupportedError(f"{self.kind.value}: train not implemented")

    def package(self, model_ref: str, target_kind: str) -> bytes:
        raise NotSupportedError(f"{self.kind.value}: package not implemented")
