"""Engine adapter registry (see spec 0011 §Engine abstraction).

Only the null adapter ships in the scaffold. Real openWakeWord /
microWakeWord / Porcupine adapters are follow-ons.
"""

from .base import Detector, EngineKind, NotSupportedError, NullEngine, WakeWordEngine
from .openwakeword import OpenWakeWordEngine

__all__ = [
    "Detector",
    "EngineKind",
    "NotSupportedError",
    "NullEngine",
    "OpenWakeWordEngine",
    "WakeWordEngine",
]
