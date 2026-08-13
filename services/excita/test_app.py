"""End-to-end tests.

Ops surface: upload → label → list.
Detection surface: arm openWakeWord against the fetched models + real
hey_jarvis fixture, feed silence and real audio, verify fires and clips.

One external seam: the FastAPI app via `TestClient`. Fixtures use a per-test
temp data dir so nothing leaks between runs. Detection tests are marked to
skip when the pinned openWakeWord ONNX models aren't present — CI runs
`scripts/fetch-wake-models.sh` first so the models are always there in CI.
"""

from __future__ import annotations

import io
import wave
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from excita.app import Config, create_app


REPO_ROOT = Path(__file__).resolve().parents[2]
WAKE_MODELS_DIR = REPO_ROOT / "crates" / "conduit-wake" / "tests" / "models"
AUDIO_DIR = REPO_ROOT / "crates" / "conduit-wake" / "tests" / "audio"
HEY_JARVIS_MODEL = WAKE_MODELS_DIR / "hey_jarvis_v0.1.onnx"


def _wake_models_available() -> bool:
    return all(
        (WAKE_MODELS_DIR / name).exists()
        for name in ("melspectrogram.onnx", "embedding_model.onnx", "hey_jarvis_v0.1.onnx")
    )


requires_wake_models = pytest.mark.skipif(
    not _wake_models_available(),
    reason="openWakeWord ONNX models missing; run scripts/fetch-wake-models.sh",
)


def _wav_bytes(freq_hz: int = 440, duration_ms: int = 250, sample_rate: int = 16000) -> bytes:
    """Minimal PCM WAV — content-varying so dedup tests can flip it."""
    import math
    frames = int(sample_rate * duration_ms / 1000)
    buf = io.BytesIO()
    with wave.open(buf, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(sample_rate)
        samples = bytearray()
        for i in range(frames):
            v = int(3000 * math.sin(2 * math.pi * freq_hz * i / sample_rate))
            samples += int.to_bytes(v & 0xFFFF, 2, "little")
        wav.writeframes(bytes(samples))
    return buf.getvalue()


@pytest.fixture
def config(tmp_path: Path) -> Config:
    return Config(
        data_dir=tmp_path,
        backend_type="sqlite",
        base_url="http://localhost:8084",
        wake_models_dir=WAKE_MODELS_DIR if _wake_models_available() else None,
        pre_roll_ms=2000,
    )


@pytest.fixture
def client(config: Config):
    with TestClient(create_app(config)) as c:
        yield c


def _create_phrase(client: TestClient, name: str = "hey jarvis") -> str:
    resp = client.post("/phrases", json={"name": name, "display_label": name})
    assert resp.status_code == 201, resp.text
    return resp.json()["id"]


def _upload(client: TestClient, phrase_id: str, audio: bytes) -> dict:
    resp = client.post(
        "/clips",
        data={"phrase_id": phrase_id},
        files={"file": ("clip.wav", audio, "audio/wav")},
    )
    assert resp.status_code == 201, resp.text
    return resp.json()


def test_health_reports_unlinked(client: TestClient) -> None:
    resp = client.get("/health")
    assert resp.status_code == 200
    body = resp.json()
    assert body["status"] == "ok"
    assert body["linked"] is False


def test_create_phrase_then_list(client: TestClient) -> None:
    phrase_id = _create_phrase(client)
    resp = client.get("/phrases")
    assert resp.status_code == 200
    assert [p["id"] for p in resp.json()] == [phrase_id]


def test_upload_extracts_wav_metadata(client: TestClient) -> None:
    phrase_id = _create_phrase(client)
    clip = _upload(client, phrase_id, _wav_bytes(duration_ms=500))
    assert clip["sample_rate"] == 16000
    assert 480 <= clip["duration_ms"] <= 520
    assert clip["source"] == "upload"
    assert clip["verdict"] is None


def test_upload_dedups_within_phrase(client: TestClient) -> None:
    phrase_id = _create_phrase(client)
    audio = _wav_bytes()
    first = _upload(client, phrase_id, audio)
    second = _upload(client, phrase_id, audio)
    assert first["id"] == second["id"], "identical audio must return the same clip id"


def test_upload_rejects_unsupported_mime(client: TestClient) -> None:
    phrase_id = _create_phrase(client)
    resp = client.post(
        "/clips",
        data={"phrase_id": phrase_id},
        files={"file": ("clip.mp3", b"\x00\x00", "audio/mpeg")},
    )
    assert resp.status_code == 415


def test_browser_source_header_recorded(client: TestClient) -> None:
    phrase_id = _create_phrase(client)
    resp = client.post(
        "/clips",
        data={"phrase_id": phrase_id},
        files={"file": ("blob.wav", _wav_bytes(freq_hz=880), "audio/wav")},
        headers={"X-Excita-Source": "browser"},
    )
    assert resp.status_code == 201
    assert resp.json()["source"] == "browser"


def test_label_and_filter(client: TestClient) -> None:
    phrase_id = _create_phrase(client)
    pos = _upload(client, phrase_id, _wav_bytes(freq_hz=440))
    neg = _upload(client, phrase_id, _wav_bytes(freq_hz=660))
    _unlabeled = _upload(client, phrase_id, _wav_bytes(freq_hz=880))

    assert client.post(
        f"/clips/{pos['id']}/label", json={"verdict": "positive"}
    ).status_code == 200
    assert client.post(
        f"/clips/{neg['id']}/label", json={"verdict": "negative", "split": "train"}
    ).status_code == 200

    positives = client.get("/clips", params={"phrase_id": phrase_id, "verdict": "positive"}).json()
    assert [c["id"] for c in positives] == [pos["id"]]
    assert positives[0]["verdict"] == "positive"

    unlabeled = client.get("/clips", params={"phrase_id": phrase_id, "verdict": "unlabeled"}).json()
    assert {c["id"] for c in unlabeled} == {_unlabeled["id"]}


def test_relabel_supersedes(client: TestClient) -> None:
    phrase_id = _create_phrase(client)
    clip = _upload(client, phrase_id, _wav_bytes())
    client.post(f"/clips/{clip['id']}/label", json={"verdict": "positive"})
    client.post(f"/clips/{clip['id']}/label", json={"verdict": "negative"})
    got = client.get(f"/clips/{clip['id']}").json()
    assert got["verdict"] == "negative"


def test_label_rejects_invalid_verdict(client: TestClient) -> None:
    phrase_id = _create_phrase(client)
    clip = _upload(client, phrase_id, _wav_bytes())
    resp = client.post(f"/clips/{clip['id']}/label", json={"verdict": "maybe"})
    assert resp.status_code == 422


def test_audio_playback_roundtrip(client: TestClient) -> None:
    phrase_id = _create_phrase(client)
    audio = _wav_bytes()
    clip = _upload(client, phrase_id, audio)
    resp = client.get(f"/clips/{clip['id']}/audio")
    assert resp.status_code == 200
    assert resp.headers["content-type"].startswith("audio/wav")
    assert resp.content == audio


def test_detectors_empty_by_default(client: TestClient) -> None:
    """No detectors are armed until an `excita_local` deploy target lands."""
    resp = client.get("/detectors")
    assert resp.status_code == 200
    assert resp.json() == []


def test_reset_unknown_detector_404s(client: TestClient) -> None:
    resp = client.post("/detectors/nope/reset")
    assert resp.status_code == 404


def test_wake_events_recent_empty(client: TestClient) -> None:
    resp = client.get("/v1/wake-events/recent")
    assert resp.status_code == 200
    assert resp.json() == []


def test_audio_frame_rejects_empty(client: TestClient) -> None:
    resp = client.post("/v1/audio/kitchen/frames", content=b"")
    assert resp.status_code == 422


def test_audio_frame_rejects_odd_length(client: TestClient) -> None:
    """int16 mono contract on the wire — an odd byte count means codec drift."""
    resp = client.post("/v1/audio/kitchen/frames", content=b"\x00\x01\x02")
    assert resp.status_code == 422


def test_audio_frame_no_bindings_is_a_no_op(client: TestClient) -> None:
    """A frame with no armed detector still returns 202 — it's absorbed by
    the pre-roll buffer so a satellite that comes online before an operator
    arms a detector isn't punished for it."""
    resp = client.post("/v1/audio/kitchen/frames", content=b"\x00\x00" * 640)
    assert resp.status_code == 202
    assert resp.json() == {"accepted": True, "fires": 0}


def test_arm_detector_without_model_returns_501(client: TestClient) -> None:
    """No wake_models_dir configured (or fetch script not run) → null engine
    still refuses honestly with an engine-named message."""
    phrase_id = _create_phrase(client)
    resp = client.post(
        "/detectors",
        json={
            "phrase_id": phrase_id,
            "model_ref": "/nonexistent/hey_jarvis.onnx",
            "source_device": "kitchen",
            "engine": "microwakeword",  # slot deliberately still NullEngine
        },
    )
    assert resp.status_code == 501
    assert "microwakeword" in resp.json()["detail"]


def test_arm_detector_missing_phrase_404s(client: TestClient) -> None:
    resp = client.post(
        "/detectors",
        json={
            "phrase_id": "nope",
            "model_ref": "irrelevant",
            "source_device": "kitchen",
        },
    )
    assert resp.status_code == 404


@requires_wake_models
def test_arm_openwakeword_and_feed_hey_jarvis(client: TestClient) -> None:
    """End-to-end: arm hey_jarvis, feed the real fixture WAV in 40 ms chunks,
    expect at least one fire, a clip persisted with source=detector, and a
    row in the local wake-events ring buffer."""
    phrase_id = _create_phrase(client)
    arm = client.post(
        "/detectors",
        json={
            "phrase_id": phrase_id,
            "model_ref": str(HEY_JARVIS_MODEL),
            "source_device": "kitchen",
        },
    )
    assert arm.status_code == 201, arm.text
    binding = arm.json()
    assert binding["engine"] == "openwakeword"
    assert binding["source_device"] == "kitchen"

    with wave.open(str(AUDIO_DIR / "hey_jarvis.wav")) as w:
        assert w.getframerate() == 16000 and w.getnchannels() == 1
        pcm = w.readframes(w.getnframes())

    # 40 ms @ 16 kHz mono = 1280 bytes; smaller than one predict window on
    # purpose so the residual-buffer path in the detector is exercised.
    chunk_bytes = 1280
    total_fires = 0
    for offset in range(0, len(pcm), chunk_bytes):
        resp = client.post(
            "/v1/audio/kitchen/frames",
            content=pcm[offset : offset + chunk_bytes],
        )
        assert resp.status_code == 202
        total_fires += resp.json()["fires"]
    assert total_fires >= 1, "hey_jarvis fixture must produce at least one fire"

    events = client.get("/v1/wake-events/recent").json()
    assert events, "wake-event ring buffer must record the fire"
    fire = events[0]
    assert fire["phrase_id"] == phrase_id
    assert fire["source_device"] == "kitchen"
    assert fire["confidence"] >= 0.5
    assert fire["audio_clip_id"] is not None

    clips = client.get("/clips", params={"phrase_id": phrase_id}).json()
    detector_clips = [c for c in clips if c["source"] == "detector"]
    assert detector_clips, "fire must persist a detector-sourced clip"
    assert detector_clips[0]["source_peer"] == "kitchen"


@requires_wake_models
def test_silence_does_not_fire(client: TestClient) -> None:
    """A stream of zero-valued PCM never crosses the threshold."""
    phrase_id = _create_phrase(client)
    client.post(
        "/detectors",
        json={
            "phrase_id": phrase_id,
            "model_ref": str(HEY_JARVIS_MODEL),
            "source_device": "kitchen",
        },
    ).raise_for_status()

    for _ in range(20):
        resp = client.post(
            "/v1/audio/kitchen/frames", content=b"\x00\x00" * 640
        )
        assert resp.status_code == 202
        assert resp.json()["fires"] == 0
    assert client.get("/v1/wake-events/recent").json() == []


@requires_wake_models
def test_disarm_detector_stops_scoring(client: TestClient) -> None:
    phrase_id = _create_phrase(client)
    arm = client.post(
        "/detectors",
        json={
            "phrase_id": phrase_id,
            "model_ref": str(HEY_JARVIS_MODEL),
            "source_device": "kitchen",
        },
    )
    detector_id = arm.json()["id"]

    assert client.delete(f"/detectors/{detector_id}").status_code == 204
    assert client.get("/detectors").json() == []

    with wave.open(str(AUDIO_DIR / "hey_jarvis.wav")) as w:
        pcm = w.readframes(w.getnframes())
    resp = client.post("/v1/audio/kitchen/frames", content=pcm)
    assert resp.status_code == 202
    # No armed binding for the source → nothing scores, nothing fires.
    assert resp.json()["fires"] == 0
    assert client.get("/v1/wake-events/recent").json() == []


@requires_wake_models
def test_bindings_are_source_scoped(client: TestClient) -> None:
    """A binding armed for `kitchen` must not score `bedroom` frames."""
    phrase_id = _create_phrase(client)
    client.post(
        "/detectors",
        json={
            "phrase_id": phrase_id,
            "model_ref": str(HEY_JARVIS_MODEL),
            "source_device": "kitchen",
        },
    ).raise_for_status()

    with wave.open(str(AUDIO_DIR / "hey_jarvis.wav")) as w:
        pcm = w.readframes(w.getnframes())
    # Same audio, wrong source — nothing should fire.
    resp = client.post("/v1/audio/bedroom/frames", content=pcm)
    assert resp.status_code == 202
    assert resp.json()["fires"] == 0


def test_upload_to_missing_phrase_404s(client: TestClient) -> None:
    resp = client.post(
        "/clips",
        data={"phrase_id": "nope"},
        files={"file": ("x.wav", _wav_bytes(), "audio/wav")},
    )
    assert resp.status_code == 404
