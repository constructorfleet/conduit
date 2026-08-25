"""nanoWakeWord engine adapter.

Directly wraps the `nanowakeword` PyPI package's `NanoInterpreter`
(ADR-0020). Follows the openWakeWord adapter shape: 16 kHz mono int16,
1280-sample chunks, a residual buffer so sources that don't send exact
80 ms frames still stream cleanly, and `reset()` clearing both the
residual and the interpreter's hidden state.

The engine-native phrase key is the artifact stem — nanoWakeWord names
its output channel after the model file (`hey_jarvis.onnx` scores under
`"hey_jarvis"`), which is what lands in the model row's
`engine_phrase_key`.

The gate-on-MCU + remote-verifier cascade mode is a separate spec and is
not wired here; models load as single verifiers.
"""

from __future__ import annotations

import wave
from io import BytesIO
from pathlib import Path

import numpy as np
from nanowakeword.interpreter import NanoInterpreter

from .base import Detector, EngineKind, NotSupportedError, gap_reason

SAMPLE_RATE = 16000
CHUNK_SAMPLES = 1280
DEFAULT_THRESHOLD = 0.5


def phrase_key_of(model_ref: str) -> str:
    """nanoWakeWord's native output key: the artifact file stem."""
    return Path(model_ref).stem


class _Detector:
    """Live-audio handle around one `NanoInterpreter`."""

    kind = EngineKind.NANOWAKEWORD
    sample_rate = SAMPLE_RATE

    def __init__(
        self,
        *,
        phrase_id: str,
        interpreter: NanoInterpreter,
        threshold: float,
        phrase_key: str,
    ) -> None:
        self.phrase_id = phrase_id
        self._interpreter = interpreter
        self._threshold = threshold
        self._phrase_key = phrase_key
        # Trailing PCM that didn't reach a full chunk (see openWakeWord
        # adapter — same contract, same reason).
        self._residual = np.zeros(0, dtype=np.int16)

    def feed(self, pcm_frame: bytes) -> tuple[float, bool] | None:
        if not pcm_frame:
            return None
        incoming = np.frombuffer(pcm_frame, dtype=np.int16)
        buffered = np.concatenate([self._residual, incoming])
        n_chunks = len(buffered) // CHUNK_SAMPLES
        if n_chunks == 0:
            self._residual = buffered
            return None

        max_score = 0.0
        for i in range(n_chunks):
            start = i * CHUNK_SAMPLES
            chunk = buffered[start : start + CHUNK_SAMPLES]
            result = self._interpreter.predict(chunk)
            score = float(result.get(self._phrase_key, result.score))
            if score > max_score:
                max_score = score
        self._residual = buffered[n_chunks * CHUNK_SAMPLES :]
        return max_score, max_score >= self._threshold

    def reset(self) -> None:
        self._residual = np.zeros(0, dtype=np.int16)
        self._interpreter.reset()


class NanoWakeWordEngine:
    """nanoWakeWord engine: live host-side detection + offline scoring."""

    kind = EngineKind.NANOWAKEWORD
    capabilities = frozenset({"load", "feed", "score", "package"})
    package_targets = ("onnx",)

    def __init__(self, *, default_threshold: float = DEFAULT_THRESHOLD) -> None:
        self._default_threshold = default_threshold

    def load(
        self,
        model_ref: str,
        phrase_id: str,
        threshold: float | None = None,
    ) -> Detector:
        model_path = Path(model_ref)
        if not model_path.exists():
            raise FileNotFoundError(f"nanowakeword model not found: {model_ref}")
        interpreter = NanoInterpreter.load_model(str(model_path))
        return _Detector(
            phrase_id=phrase_id,
            interpreter=interpreter,
            threshold=self._default_threshold if threshold is None else threshold,
            phrase_key=phrase_key_of(model_ref),
        )

    def score(self, audio: bytes, model_ref: str) -> list[float]:
        """Per-chunk scores across a full PCM WAV.

        A fresh interpreter per call keeps the debug view deterministic —
        live streaming state must not leak into an offline re-score.
        """
        detector = self.load(model_ref, phrase_id="_debug_")
        with wave.open(BytesIO(audio)) as wav:
            if wav.getnchannels() != 1 or wav.getframerate() != SAMPLE_RATE:
                raise ValueError(
                    "nanowakeword expects 16 kHz mono; got "
                    f"{wav.getframerate()} Hz {wav.getnchannels()}ch"
                )
            pcm = wav.readframes(wav.getnframes())
        samples = np.frombuffer(pcm, dtype=np.int16)
        curve: list[float] = []
        for i in range(0, len(samples) - CHUNK_SAMPLES + 1, CHUNK_SAMPLES):
            result = detector._interpreter.predict(samples[i : i + CHUNK_SAMPLES])
            curve.append(float(result.get(detector._phrase_key, result.score)))
        return curve

    def train(self, dataset_snapshot_id: str, base: str | None) -> str:
        raise NotSupportedError(gap_reason(self.kind, "train"))

    def package(self, model_ref: str, target_kind: str) -> bytes:
        if target_kind != "onnx":
            raise NotSupportedError(
                f"nanowakeword: package target '{target_kind}' not supported; "
                "the only native target is 'onnx'"
            )
        if not Path(model_ref).exists():
            raise FileNotFoundError(f"nanowakeword model not found: {model_ref}")
        # The trained ONNX verifier IS the package for host-side runtimes;
        # cross-engine conversion is out of contract (#213).
        return Path(model_ref).read_bytes()
