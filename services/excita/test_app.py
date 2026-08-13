"""End-to-end scaffold tests: upload → label → list.

One external seam: the FastAPI app via `TestClient`. Fixtures use a per-test
temp data dir so nothing leaks between runs.
"""

from __future__ import annotations

import io
import wave
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from excita.app import Config, create_app


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


def test_audio_frame_returns_501_with_null_engine(client: TestClient) -> None:
    """Honest gap: engine says `NotSupported`, endpoint surfaces it as 501."""
    resp = client.post("/v1/audio/kitchen/frames", content=b"\x00\x01\x02\x03")
    assert resp.status_code == 501
    assert "openwakeword" in resp.json()["detail"]


def test_upload_to_missing_phrase_404s(client: TestClient) -> None:
    resp = client.post(
        "/clips",
        data={"phrase_id": "nope"},
        files={"file": ("x.wav", _wav_bytes(), "audio/wav")},
    )
    assert resp.status_code == 404
