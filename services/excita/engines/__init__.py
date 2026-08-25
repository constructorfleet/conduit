"""Engine adapter registry (see spec 0011 §Engine abstraction, #213).

Each engine is one file declaring, honestly, which of the four operations
(`load`/`feed`, `score`, `train`, `package`) it implements — ADR-0020.
Asking for an undeclared capability raises `NotSupportedError`, which the
HTTP surface translates into a structured 501 — ADR-0023.
"""

from .base import (
    CAPABILITIES,
    Detector,
    EngineKind,
    NotSupportedError,
    NullEngine,
    WakeWordEngine,
    capability_view,
    gap_reason,
)
from .microwakeword import MicroWakeWordEngine
from .nanowakeword import NanoWakeWordEngine
from .openwakeword import OpenWakeWordEngine

__all__ = [
    "CAPABILITIES",
    "Detector",
    "EngineKind",
    "MicroWakeWordEngine",
    "NanoWakeWordEngine",
    "NotSupportedError",
    "NullEngine",
    "OpenWakeWordEngine",
    "WakeWordEngine",
    "capability_view",
    "gap_reason",
]
