"""Engine adapter Protocol + null implementation."""

from __future__ import annotations

from enum import Enum
from typing import Protocol


class EngineKind(str, Enum):
    OPENWAKEWORD = "openwakeword"
    MICROWAKEWORD = "microwakeword"
    NANOWAKEWORD = "nanowakeword"
    PORCUPINE = "porcupine"


# The five cells an adapter can honestly advertise (#213 §Capability
# contract). `feed` is only reachable through a loaded `Detector`, so it is
# declared alongside `load` — no adapter supports one without the other.
CAPABILITIES = ("load", "feed", "score", "train", "package")


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
    # Capability advertisement (ADR-0020). Declared next to the methods that
    # would raise `NotSupportedError` — the declaration and the behaviour live
    # in the same file so they can't drift unnoticed.
    capabilities: frozenset[str]
    package_targets: tuple[str, ...]

    def load(self, model_ref: str, phrase_id: str) -> Detector: ...
    def score(self, audio: bytes, model_ref: str) -> list[float]: ...
    def train(self, dataset_snapshot_id: str, base: str | None) -> str: ...
    def package(self, model_ref: str, target_kind: str) -> bytes: ...


def capability_view(engine: WakeWordEngine) -> dict[str, object]:
    """Serialisation for `GET /engines` (#213 §Capability contract)."""
    declared = getattr(engine, "capabilities", frozenset())
    return {
        "kind": engine.kind.value,
        "capabilities": {c: c in declared for c in CAPABILITIES},
        "package_targets": list(getattr(engine, "package_targets", ())),
    }


class NullEngine:
    """Placeholder that answers the API surface without doing any work.

    Every method raises `NotSupportedError` with a message naming the engine
    kind and the operation. Wired at boot so the HTTP surface can be
    exercised end-to-end before a real adapter lands.
    """

    kind: EngineKind
    capabilities = frozenset[str]()
    package_targets: tuple[str, ...] = ()

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
