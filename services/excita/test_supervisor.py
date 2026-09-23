"""Tests for wake signals emitted by the detector supervisor."""

from __future__ import annotations

import excita.supervisor as supervisor_module
from excita.supervisor import DetectorSupervisor


class Detector:
    def __init__(self, result: tuple[float, bool]) -> None:
        self.result = result

    def feed(self, _frame: bytes) -> tuple[float, bool]:
        return self.result

    def reset(self) -> None:
        pass


def test_scored_near_miss_is_reported_without_entering_local_fire_history() -> None:
    supervisor = DetectorSupervisor(backend=None, clip_store=None)
    supervisor.arm(
        phrase_id="hey-conduit",
        model_ref="model",
        source_device="kitchen",
        detector=Detector((0.21, False)),  # type: ignore[arg-type]
    )

    signals = supervisor.feed("kitchen", b"\x01\x00")

    assert len(signals) == 1
    assert signals[0].event_type == "rejected"
    assert supervisor.recent_events(10) == []


def test_detected_signal_remains_in_local_fire_history() -> None:
    supervisor = DetectorSupervisor(backend=None, clip_store=None)
    supervisor.arm(
        phrase_id="hey-conduit",
        model_ref="model",
        source_device="kitchen",
        detector=Detector((0.91, True)),  # type: ignore[arg-type]
    )

    signals = supervisor.feed("kitchen", b"\x01\x00")

    assert signals[0].event_type == "detected"
    assert supervisor.recent_events(10) == signals


def test_rejected_signals_are_coalesced_per_detector(monkeypatch) -> None:
    moments = iter([0.0, 0.0, 1.0])
    monkeypatch.setattr(supervisor_module.time, "monotonic", lambda: next(moments))
    supervisor = DetectorSupervisor(backend=None, clip_store=None)
    supervisor.arm(
        phrase_id="hey-conduit",
        model_ref="model",
        source_device="kitchen",
        detector=Detector((0.21, False)),  # type: ignore[arg-type]
    )

    assert len(supervisor.feed("kitchen", b"\x01\x00")) == 1
    assert supervisor.feed("kitchen", b"\x01\x00") == []
    assert len(supervisor.feed("kitchen", b"\x01\x00")) == 1
