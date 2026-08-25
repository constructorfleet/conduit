"""microWakeWord engine adapter.

microWakeWord exists to run on the ESP32 — its whole point is streaming
TFLite-Micro inference inside an MCU's memory budget (ADR-0020). Excita's
adapter is therefore deliberately partial:

- `score` runs the µWW TFLite model over a stored clip on CPU via
  `tflite-runtime` and returns the per-hop score curve, so a new model can
  be regression-checked against stored clips before anything is flashed.
- `package(target_kind="tflite_micro")` returns the flashable blob the
  ESPHome device consumes.
- `load`/`feed` raise `NotSupportedError`: arming a live µWW detector on
  the Excita host was never going to work, and saying so plainly beats a
  silently useless binding.
- `train` raises `NotSupportedError` pointing at `EXCITA_TRAIN_WORKER_URL`
  — µWW's TF pipeline does not belong in the process serving HTTP.
"""

from __future__ import annotations

import wave
from io import BytesIO
from pathlib import Path

import numpy as np

from .base import Detector, EngineKind, NotSupportedError

try:  # pragma: no cover - import guard, exercised only on wheels-less hosts
    from tflite_runtime.interpreter import Interpreter as _TfLiteInterpreter
except ImportError:  # noqa: F401 - scored below via `_tflite_available`
    _TfLiteInterpreter = None  # type: ignore[assignment,misc]

SAMPLE_RATE = 16000


def _tflite_available() -> bool:
    return _TfLiteInterpreter is not None


class MicroWakeWordEngine:
    """microWakeWord engine: offline scoring + packaging, nothing live."""

    kind = EngineKind.MICROWAKEWORD
    capabilities = frozenset({"score", "package"})
    package_targets = ("tflite_micro",)

    def load(self, model_ref: str, phrase_id: str) -> Detector:
        raise NotSupportedError(
            "microwakeword does not run live host-side detection in Excita; "
            "detection happens on the ESP32. Use score() against stored clips "
            "or package() for the device."
        )

    def score(self, audio: bytes, model_ref: str) -> list[float]:
        """Per-hop scores across a full 16 kHz mono PCM WAV."""
        if not Path(model_ref).exists():
            raise FileNotFoundError(f"microwakeword model not found: {model_ref}")
        if _TfLiteInterpreter is None:
            raise RuntimeError(
                "tflite-runtime is not installed; microWakeWord scoring "
                "requires it (pip install tflite-runtime)"
            )

        with wave.open(BytesIO(audio)) as wav:
            if wav.getnchannels() != 1 or wav.getframerate() != SAMPLE_RATE:
                raise ValueError(
                    "microwakeword expects 16 kHz mono; got "
                    f"{wav.getframerate()} Hz {wav.getnchannels()}ch"
                )
            pcm = wav.readframes(wav.getnframes())

        interpreter = _TfLiteInterpreter(model_path=model_ref)
        interpreter.allocate_tensors()
        input_detail = interpreter.get_input_details()[0]
        output_detail = interpreter.get_output_details()[0]
        # µWW models consume int16 audio windows; the window length is a
        # property of the trained model, so read it off the artifact rather
        # than hardcoding it.
        window = int(input_detail["shape"][-1])

        samples = np.frombuffer(pcm, dtype=np.int16)
        curve: list[float] = []
        for start in range(0, len(samples) - window + 1, window):
            hop = samples[start : start + window]
            interpreter.set_tensor(input_detail["index"], hop.reshape(1, -1))
            interpreter.invoke()
            out = interpreter.get_tensor(output_detail["index"])
            curve.append(float(out.reshape(-1)[-1]))
        return curve

    def train(self, dataset_snapshot_id: str, base: str | None) -> str:
        raise NotSupportedError(
            "microwakeword training does not run in-process; configure "
            "EXCITA_TRAIN_WORKER_URL to route training to an external worker."
        )

    def package(self, model_ref: str, target_kind: str) -> bytes:
        if target_kind != "tflite_micro":
            raise NotSupportedError(
                f"microwakeword: package target '{target_kind}' not supported; "
                "the only native target is 'tflite_micro'"
            )
        if not Path(model_ref).exists():
            raise FileNotFoundError(f"microwakeword model not found: {model_ref}")
        # The trained .tflite IS the flashable blob — ESPHome's microwakeword
        # component consumes it verbatim.
        return Path(model_ref).read_bytes()
