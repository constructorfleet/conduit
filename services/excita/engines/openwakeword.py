"""openWakeWord engine adapter.

Uses the upstream `openwakeword` Python package with the ONNX inference
framework — same three ONNX files (`melspectrogram.onnx`,
`embedding_model.onnx`, `<phrase>.onnx`) the Rust `conduit-wake` crate loads.
Fetch them with `scripts/fetch-wake-models.sh`.

`load()` returns a warm `_Detector` that holds a `Model` per detector; every
`feed()` calls `predict()` on a fixed-size chunk of PCM and reports fire vs
no-fire against the configured threshold. `score()` runs the same predict
across a whole stored clip and returns the per-chunk curve for the debug
view.
"""

from __future__ import annotations

import wave
from io import BytesIO
from pathlib import Path

import numpy as np

from .base import Detector, EngineKind, NotSupportedError


# openWakeWord's own frame rate: 80 ms of 16 kHz mono. Predict expects
# exactly this many int16 samples per call; anything else the wrapper
# accepts is re-chunked into blocks of this size, so keeping our contract
# aligned means no hidden buffering.
CHUNK_SAMPLES = 1280
SAMPLE_RATE = 16000
DEFAULT_THRESHOLD = 0.5


class _Detector:
    """Live-audio handle around one openWakeWord `Model`."""

    kind = EngineKind.OPENWAKEWORD
    sample_rate = SAMPLE_RATE

    def __init__(
        self,
        *,
        phrase_id: str,
        model: object,
        threshold: float,
        phrase_key: str,
    ) -> None:
        self.phrase_id = phrase_id
        self._model = model
        self._threshold = threshold
        self._phrase_key = phrase_key
        # Trailing PCM that didn't reach a full chunk. Concatenated with
        # the next `feed()` — the alternative is dropping frames on any
        # source that doesn't happen to send exact 80 ms chunks, which is
        # every real satellite.
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
            scores = self._model.predict(chunk)
            score = float(scores.get(self._phrase_key, 0.0))
            if score > max_score:
                max_score = score
        self._residual = buffered[n_chunks * CHUNK_SAMPLES :]
        return max_score, max_score >= self._threshold

    def reset(self) -> None:
        self._residual = np.zeros(0, dtype=np.int16)
        if hasattr(self._model, "reset"):
            self._model.reset()


class OpenWakeWordEngine:
    """openWakeWord engine.

    `melspec_path` and `embedding_path` are the two shared preprocessing
    ONNX models — one pair per process is enough. `load()` produces a new
    `Model` per detector because openWakeWord's `Model` holds per-phrase
    state (the rolling embedding window that feeds the classifier); one
    shared `Model` across bindings would cross-contaminate.
    """

    kind = EngineKind.OPENWAKEWORD

    def __init__(
        self,
        *,
        melspec_path: Path,
        embedding_path: Path,
        default_threshold: float = DEFAULT_THRESHOLD,
    ) -> None:
        self._melspec_path = Path(melspec_path)
        self._embedding_path = Path(embedding_path)
        self._default_threshold = default_threshold

    def load(
        self,
        model_ref: str,
        phrase_id: str,
        threshold: float | None = None,
    ) -> Detector:
        # Deferred import so a service that never arms an openWakeWord
        # detector never pays the numpy / onnxruntime import cost.
        from openwakeword.model import Model as _Model

        model_path = Path(model_ref)
        if not model_path.exists():
            raise FileNotFoundError(f"openwakeword model not found: {model_ref}")

        model = _Model(
            wakeword_models=[str(model_path)],
            melspec_model_path=str(self._melspec_path),
            embedding_model_path=str(self._embedding_path),
            inference_framework="onnx",
        )
        keys = list(model.models.keys())
        if not keys:
            raise RuntimeError(f"model loaded but exposed no phrase key: {model_ref}")
        return _Detector(
            phrase_id=phrase_id,
            model=model,
            threshold=self._default_threshold if threshold is None else threshold,
            phrase_key=keys[0],
        )

    def score(self, audio: bytes, model_ref: str) -> list[float]:
        """Per-chunk scores across a full PCM WAV.

        Rewinds a fresh detector so the debug view is deterministic — a
        live detector's rolling state must not leak into an offline
        re-score, or "why did it fire" and "does it still fire on this
        clip" answer different questions.
        """
        detector = self.load(model_ref, phrase_id="_debug_")
        with wave.open(BytesIO(audio)) as wav:
            if wav.getnchannels() != 1 or wav.getframerate() != SAMPLE_RATE:
                raise ValueError(
                    "openwakeword expects 16 kHz mono; got "
                    f"{wav.getframerate()} Hz {wav.getnchannels()}ch"
                )
            pcm = wav.readframes(wav.getnframes())
        samples = np.frombuffer(pcm, dtype=np.int16)
        curve: list[float] = []
        for i in range(0, len(samples) - CHUNK_SAMPLES + 1, CHUNK_SAMPLES):
            scores = detector._model.predict(samples[i : i + CHUNK_SAMPLES])
            curve.append(float(scores.get(detector._phrase_key, 0.0)))
        return curve

    def train(self, dataset_snapshot_id: str, base: str | None) -> str:
        # Training openWakeWord classifiers is a real feature — spec 0011
        # names it as follow-up work. Refusing here instead of returning a
        # fake job id keeps the contract honest.
        raise NotSupportedError("openwakeword: train not implemented (see spec 0011)")

    def package(self, model_ref: str, target_kind: str) -> bytes:
        # The trained model IS the package for openWakeWord's own runtime
        # (single ONNX classifier). For microWakeWord/Porcupine targets a
        # cross-engine conversion would live here — none exists yet.
        raise NotSupportedError(
            f"openwakeword: package for '{target_kind}' not implemented"
        )
