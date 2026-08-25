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
        model_import_dir=tmp_path / "import",
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
    """An engine whose adapter hasn't landed yet (porcupine's NullEngine
    slot) refuses honestly with a structured capability-gap body."""
    phrase_id = _create_phrase(client)
    resp = client.post(
        "/detectors",
        json={
            "phrase_id": phrase_id,
            "model_ref": "/nonexistent/hey_jarvis.onnx",
            "source_device": "kitchen",
            "engine": "porcupine",
        },
    )
    assert resp.status_code == 501
    body = resp.json()
    assert body["code"] == "engine_capability_missing"
    assert body["engine"] == "porcupine"
    assert body["capability"] == "load"
    assert body["message"]


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

    delete_resp = client.delete(f"/detectors/{detector_id}")
    assert delete_resp.status_code == 204
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


# --- engine capability matrix (#213 / ADR-0020) ---


def test_engines_lists_full_roster_with_capabilities(client: TestClient) -> None:
    resp = client.get("/engines")
    assert resp.status_code == 200
    engines = {e["kind"]: e for e in resp.json()}
    assert set(engines) == {"openwakeword", "microwakeword", "nanowakeword", "porcupine"}

    def caps(kind: str) -> dict:
        return engines[kind]["capabilities"]

    assert caps("openwakeword") == {
        "load": True, "feed": True, "score": True, "train": False, "package": True,
    }
    # microWakeWord detects on the ESP32 — no host-side live detection.
    assert caps("microwakeword") == {
        "load": False, "feed": False, "score": True, "train": False, "package": True,
    }
    assert caps("nanowakeword") == {
        "load": True, "feed": True, "score": True, "train": False, "package": True,
    }
    # Porcupine's slot is still a NullEngine until its adapter lands.
    assert all(v is False for v in caps("porcupine").values())

    assert engines["microwakeword"]["package_targets"] == ["tflite_micro"]
    assert engines["nanowakeword"]["package_targets"] == ["onnx"]
    assert engines["openwakeword"]["package_targets"] == ["onnx"]
    assert engines["porcupine"]["package_targets"] == []


def test_arm_microwakeword_returns_structured_501(client: TestClient) -> None:
    """µWW has no host-side load/feed — ADR-0020/0023 structured 501 body."""
    phrase_id = _create_phrase(client)
    resp = client.post(
        "/detectors",
        json={
            "phrase_id": phrase_id,
            "model_ref": "/nonexistent/model.tflite",
            "source_device": "kitchen",
            "engine": "microwakeword",
        },
    )
    assert resp.status_code == 501
    body = resp.json()
    assert body["code"] == "engine_capability_missing"
    assert body["engine"] == "microwakeword"
    assert body["capability"] == "load"
    assert "ESP32" in body["message"]


def test_train_nanowakeword_points_at_train_worker(client: TestClient) -> None:
    phrase_id = _create_phrase(client)
    resp = client.post(
        "/train",
        json={"phrase_id": phrase_id, "engine": "nanowakeword"},
    )
    assert resp.status_code == 501
    body = resp.json()
    assert body["code"] == "engine_capability_missing"
    assert body["engine"] == "nanowakeword"
    assert body["capability"] == "train"
    assert "EXCITA_TRAIN_WORKER_URL" in body["message"]


def _import_engine_placeholder(client: TestClient, engine: str) -> str:
    """Register a minimal model row for an engine whose adapter is still
    null, so capability-gap routes have something to dispatch to."""
    resp = _import_model(
        client,
        metadata=_import_metadata(engine=engine),
        filename=f"placeholder-{engine}.tflite",
    )
    assert resp.status_code == 201, resp.text
    return resp.json()["id"]


def test_score_null_engine_returns_structured_501(client: TestClient) -> None:
    """Capability matrix cell (porcupine, score): structured 501, not a
    hidden failure (ADR-0020/0023)."""
    phrase_id = _create_phrase(client)
    clip = _upload(client, phrase_id, _wav_bytes())
    model_id = _import_engine_placeholder(client, "porcupine")
    resp = client.post(
        "/debug/score", json={"clip_id": clip["id"], "model_id": model_id}
    )
    assert resp.status_code == 501
    body = resp.json()
    assert body == {
        "code": "engine_capability_missing",
        "engine": "porcupine",
        "capability": "score",
        "message": body["message"],
    }


def test_package_null_engine_returns_structured_501(
    client: TestClient, tmp_path: Path
) -> None:
    """Capability matrix cell (porcupine, package)."""
    model_id = _import_engine_placeholder(client, "porcupine")
    target = _create_target(client, "file", {"directory": str(tmp_path / "out")})
    resp = client.post(
        f"/deploy_targets/{target['id']}/publish", json={"model_id": model_id}
    )
    assert resp.status_code == 501
    body = resp.json()
    assert body["code"] == "engine_capability_missing"
    assert body["capability"] == "package"
    assert body["engine"] == "porcupine"


# --- model import surface (#213 §Model import) ---


def _import_metadata(
    *,
    engine: str = "microwakeword",
    phrase_name: str = "hey jarvis",
    version: str = "v1",
    engine_phrase_key: str | None = None,
    metrics: dict | None = None,
    notes: str | None = None,
) -> str:
    import json

    meta: dict = {"engine": engine, "phrase_name": phrase_name, "version": version}
    if engine_phrase_key is not None:
        meta["engine_phrase_key"] = engine_phrase_key
    if metrics is not None:
        meta["metrics_json"] = metrics
    if notes is not None:
        meta["notes"] = notes
    return json.dumps(meta)


def _import_model(
    client: TestClient,
    *,
    metadata: str,
    artifact: bytes = b"fake-tflite-blob",
    filename: str = "hey_jarvis_v1.tflite",
):
    return client.post(
        "/models/import",
        data={"metadata": metadata},
        files={"file": (filename, artifact, "application/octet-stream")},
    )


def test_import_creates_phrase_and_model(client: TestClient) -> None:
    resp = _import_model(
        client, metadata=_import_metadata(engine_phrase_key="hey_jarvis_v1")
    )
    assert resp.status_code == 201, resp.text
    body = resp.json()
    assert body["engine"] == "microwakeword"
    assert body["version"] == "v1"
    assert body["source"] == "upload"
    assert body["engine_phrase_key"] == "hey_jarvis_v1"

    # Unknown phrase_name was created on the fly.
    phrases = client.get("/phrases").json()
    assert [p["name"] for p in phrases] == ["hey jarvis"]
    assert body["phrase_id"] == phrases[0]["id"]


def test_import_reuses_existing_phrase(client: TestClient) -> None:
    phrase_id = _create_phrase(client, name="hey jarvis")
    resp = _import_model(client, metadata=_import_metadata())
    assert resp.status_code == 201
    assert resp.json()["phrase_id"] == phrase_id
    assert len(client.get("/phrases").json()) == 1


def test_import_writes_sidecar_next_to_artifact(client: TestClient) -> None:
    """Sidecar round-trip: copying the storage dir into the scanner mount
    promotes an uploaded model without a rewrite step (user story 15)."""
    import json

    resp = _import_model(client, metadata=_import_metadata(version="v2"))
    body = resp.json()
    artifact_path = Path(body["artifact_path"])
    sidecar_path = artifact_path.with_name(artifact_path.name + ".excita.json")
    assert artifact_path.exists()
    assert artifact_path.read_bytes() == b"fake-tflite-blob"
    assert sidecar_path.exists()

    sidecar = json.loads(sidecar_path.read_text())
    assert sidecar["engine"] == "microwakeword"
    assert sidecar["phrase_name"] == "hey jarvis"
    assert sidecar["version"] == "v2"


def test_import_preserves_metrics_envelope_and_raw(client: TestClient) -> None:
    """Normalized envelope for cross-engine ranking; raw keeps the engine's
    native numbers intact (user stories 16–17)."""
    metrics = {
        "envelope": {"samples_val": 40, "samples_test": 20, "auc": 0.912},
        "raw": {"streaming_false_accepts_per_hour": 0.4},
    }
    resp = _import_model(client, metadata=_import_metadata(metrics=metrics))
    assert resp.status_code == 201
    got = resp.json()["metrics"]
    assert got["envelope"]["auc"] == 0.912
    assert got["raw"]["streaming_false_accepts_per_hour"] == 0.4


def test_import_rejects_unknown_engine(client: TestClient) -> None:
    resp = _import_model(client, metadata=_import_metadata(engine="picovoice"))
    assert resp.status_code == 422


def test_import_rejects_blank_version(client: TestClient) -> None:
    resp = _import_model(client, metadata=_import_metadata(version=" "))
    assert resp.status_code == 422


def test_import_same_phrase_engine_version_conflicts(client: TestClient) -> None:
    """UNIQUE(phrase_id, engine, version) — the same engine can't have two
    v3s of the same phrase (ADR-0022)."""
    assert _import_model(client, metadata=_import_metadata()).status_code == 201
    resp = _import_model(client, metadata=_import_metadata(), filename="other.tflite")
    assert resp.status_code == 409


def test_same_phrase_across_engines_is_one_row_set(client: TestClient) -> None:
    """Phrase is engine-agnostic (ADR-0022): one 'hey Jarvis' carries models
    across engines."""
    oww = _import_model(
        client,
        metadata=_import_metadata(engine="openwakeword", version="v3"),
        filename="hey_jarvis_oww.onnx",
    )
    uww = _import_model(client, metadata=_import_metadata(version="v1"))
    assert oww.status_code == 201 and uww.status_code == 201
    phrase_id = uww.json()["phrase_id"]
    assert oww.json()["phrase_id"] == phrase_id

    detail = client.get(f"/phrases/{phrase_id}")
    assert detail.status_code == 200
    assert {m["engine"] for m in detail.json()["models"]} == {
        "openwakeword", "microwakeword",
    }
    listed = client.get("/models", params={"phrase_id": phrase_id}).json()
    assert len(listed) == 2


def test_delete_upload_model_soft_deletes(client: TestClient) -> None:
    model_id = _import_model(client, metadata=_import_metadata()).json()["id"]
    delete_resp = client.delete(f"/models/{model_id}")
    assert delete_resp.status_code == 204
    assert client.get("/models").json() == []
    assert client.get(f"/models/{model_id}").status_code == 404


def test_delete_filesystem_imported_model_refused(client: TestClient) -> None:
    """The volume is the source of truth — retiring means removing the file
    on disk (ADR-0021)."""
    import shutil

    # Import through the UI, then promote via copy into the scanner mount.
    model = _import_model(client, metadata=_import_metadata()).json()
    artifact = Path(model["artifact_path"])
    import_dir = client.app.state.config.model_import_dir
    import_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy(artifact, import_dir / artifact.name)
    shutil.copy(
        artifact.with_name(artifact.name + ".excita.json"),
        import_dir / (artifact.name + ".excita.json"),
    )
    scan = client.post("/models/scan")
    assert scan.status_code == 200
    fs_model_id = scan.json()["imported_ids"][0]

    resp = client.delete(f"/models/{fs_model_id}")
    assert resp.status_code == 409
    assert resp.json()["code"] == "filesystem_imported_read_only"


# --- filesystem scanner lifecycle (#213 §Filesystem scanner) ---


@pytest.fixture
def import_config(tmp_path: Path) -> Config:
    return Config(
        data_dir=tmp_path / "data",
        backend_type="sqlite",
        base_url="http://localhost:8084",
        wake_models_dir=WAKE_MODELS_DIR if _wake_models_available() else None,
        pre_roll_ms=2000,
        model_import_dir=tmp_path / "import",
    )


@pytest.fixture
def import_client(import_config: Config):
    from fastapi.testclient import TestClient as _TC

    with _TC(create_app(import_config)) as c:
        yield c


def _write_import(
    import_dir: Path,
    *,
    name: str = "hey_jarvis_v1.tflite",
    version: str = "v1",
    blob: bytes = b"fake-tflite-blob",
    engine: str = "microwakeword",
) -> None:
    import json

    artifact = import_dir / name
    artifact.write_bytes(blob)
    sidecar = artifact.with_name(artifact.name + ".excita.json")
    sidecar.write_text(
        json.dumps(
            {
                "engine": engine,
                "phrase_name": "hey jarvis",
                "version": version,
                "engine_phrase_key": Path(name).stem,
            }
        )
    )


def test_scan_discovers_new_sidecars(import_client: TestClient) -> None:
    import_dir = import_client.app.state.config.model_import_dir
    import_dir.mkdir(parents=True, exist_ok=True)
    _write_import(import_dir)

    scan = import_client.post("/models/scan")
    assert scan.status_code == 200
    assert len(scan.json()["imported_ids"]) == 1

    models = import_client.get("/models").json()
    assert len(models) == 1
    row = models[0]
    assert row["source"] == "filesystem"
    assert row["filesystem_path"] == "hey_jarvis_v1.tflite"


def test_scan_bumps_version_to_new_row(import_client: TestClient) -> None:
    """Version bump = new model row; previous row keeps its metrics and
    deploy history (user story 13)."""
    import_dir = import_client.app.state.config.model_import_dir
    import_dir.mkdir(parents=True, exist_ok=True)
    _write_import(import_dir, version="v3")
    first_scan = import_client.post("/models/scan").json()
    assert len(first_scan["imported_ids"]) == 1

    _write_import(import_dir, version="v4")
    scan = import_client.post("/models/scan").json()
    assert len(scan["imported_ids"]) == 1

    models = import_client.get("/models").json()
    assert sorted(m["version"] for m in models) == ["v3", "v4"]
    # Both rows share one filesystem_path — they're the same file's history.
    assert len({m["filesystem_path"] for m in models}) == 1


def test_scan_rescan_without_changes_is_stable(import_client: TestClient) -> None:
    """Re-saving a file without a version bump must not mint spurious new
    models (user story 14)."""
    import_dir = import_client.app.state.config.model_import_dir
    import_dir.mkdir(parents=True, exist_ok=True)
    _write_import(import_dir)
    import_client.post("/models/scan")

    again = import_client.post("/models/scan").json()
    assert again["imported_ids"] == []
    assert len(import_client.get("/models").json()) == 1


def test_scan_removes_rows_for_missing_files(import_client: TestClient) -> None:
    """The volume is the source of truth: remove the file, the model goes
    away from Excita too (user story 12, ADR-0021)."""
    import os

    import_dir = import_client.app.state.config.model_import_dir
    import_dir.mkdir(parents=True, exist_ok=True)
    _write_import(import_dir)
    import_client.post("/models/scan")
    assert len(import_client.get("/models").json()) == 1

    os.remove(import_dir / "hey_jarvis_v1.tflite")
    scan = import_client.post("/models/scan").json()
    assert scan["removed_ids"], "missing file must soft-delete its row"
    assert import_client.get("/models").json() == []


def test_scan_skips_malformed_sidecar_without_partial_register(
    import_client: TestClient,
) -> None:

    import_dir = import_client.app.state.config.model_import_dir
    import_dir.mkdir(parents=True, exist_ok=True)
    (import_dir / "broken.onnx").write_bytes(b"blob")
    (import_dir / "broken.onnx.excita.json").write_text("{not json")

    scan = import_client.post("/models/scan").json()
    assert scan["errors"] >= 1
    assert import_client.get("/models").json() == []


def test_scan_boot_runs_on_startup(import_config: Config) -> None:
    """First-boot deployments come up with a usable wake word before the
    operator ever opens the UI (user story 9)."""
    import_config.model_import_dir.mkdir(parents=True, exist_ok=True)
    _write_import(import_config.model_import_dir)
    # Files land before the app boots; the lifespan scan must find them
    # with no explicit scan call.
    with TestClient(create_app(import_config)) as boot_client:
        models = boot_client.get("/models").json()
        assert len(models) == 1
        assert models[0]["source"] == "filesystem"



# --- debug scoring (#213 story 5) ---


@requires_wake_models
def test_debug_score_returns_curve(client: TestClient) -> None:
    """Score a stored clip against an imported openWakeWord model through
    the app seam — the regression-check loop before flashing anything."""
    phrase_id = _create_phrase(client)
    clip = _upload(client, phrase_id, (AUDIO_DIR / "hey_jarvis.wav").read_bytes())
    resp = _import_model(
        client,
        metadata=_import_metadata(
            engine="openwakeword",
            version="v0.1",
            engine_phrase_key="hey_jarvis",
        ),
        artifact=HEY_JARVIS_MODEL.read_bytes(),
        filename="hey_jarvis_v0.1.onnx",
    )
    assert resp.status_code == 201, resp.text
    model_id = resp.json()["id"]

    score = client.post(
        "/debug/score", json={"clip_id": clip["id"], "model_id": model_id}
    )
    assert score.status_code == 200, score.text
    results = score.json()
    assert len(results) == 1
    assert results[0]["model_id"] == model_id
    assert results[0]["engine"] == "openwakeword"
    curve = results[0]["curve"]
    assert curve, "real wake audio must produce a non-empty curve"
    assert all(0.0 <= v <= 1.0 for v in curve)
    assert max(curve) >= 0.5, "hey_jarvis fixture should peak on its own model"


def test_debug_score_unknown_clip_404s(client: TestClient) -> None:
    resp = client.post("/debug/score", json={"clip_id": "nope", "model_id": "nope"})
    assert resp.status_code == 404


# --- deploy targets (#213 §Deploy targets) ---


def _create_target(client: TestClient, kind: str, config: dict) -> dict:
    resp = client.post("/deploy_targets", json={"kind": kind, "config": config})
    assert resp.status_code == 201, resp.text
    return resp.json()


def test_create_and_list_deploy_target(client: TestClient, tmp_path: Path) -> None:
    target = _create_target(
        client, "file", {"directory": str(tmp_path / "out")}
    )
    assert target["kind"] == "file"
    assert target["current_model_id"] is None
    listed = client.get("/deploy_targets").json()
    assert [t["id"] for t in listed] == [target["id"]]


def test_create_deploy_target_rejects_unknown_kind(client: TestClient) -> None:
    resp = client.post(
        "/deploy_targets", json={"kind": "carrier_pigeon", "config": {}}
    )
    assert resp.status_code == 422


def test_create_file_target_requires_directory(client: TestClient) -> None:
    resp = client.post("/deploy_targets", json={"kind": "file", "config": {}})
    assert resp.status_code == 422


def test_publish_to_file_target_writes_package(
    client: TestClient, tmp_path: Path
) -> None:
    """Story 23: deploying is one call — select the model, bytes land."""
    out_dir = tmp_path / "fleet"
    model = _import_model(
        client,
        metadata=_import_metadata(version="v4"),
        artifact=b"MZ-v4-firmware-blob",
        filename="hey_jarvis_v4.tflite",
    ).json()
    target = _create_target(client, "file", {"directory": str(out_dir)})

    resp = client.post(
        f"/deploy_targets/{target['id']}/publish",
        json={"model_id": model["id"]},
    )
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["current_model_id"] == model["id"]
    assert body["last_publish_status"] == "ok"

    written = list(out_dir.iterdir())
    assert len(written) == 1
    assert written[0].suffix == ".tflite"
    assert written[0].read_bytes() == b"MZ-v4-firmware-blob"


def test_publish_http_push_carries_excita_headers(client: TestClient) -> None:
    """The receiving service learns what it just got without parsing the
    blob (#213 §Deploy targets)."""
    import threading
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

    captured: list[dict] = []

    class Recorder(BaseHTTPRequestHandler):
        def do_POST(self) -> None:  # noqa: N802 - http.server API
            length = int(self.headers.get("Content-Length", "0"))
            captured.append(
                {
                    "headers": dict(self.headers),
                    "body": self.rfile.read(length),
                }
            )
            self.send_response(200)
            self.end_headers()

    server = ThreadingHTTPServer(("127.0.0.1", 0), Recorder)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        url = f"http://127.0.0.1:{server.server_address[1]}/wake-word/model"
        target = _create_target(client, "http_push", {"url": url})
        model = _import_model(
            client,
            metadata=_import_metadata(version="v2"),
            artifact=b"nww-onnx-bytes",
            filename="hey_jarvis_v2.onnx",
        ).json()

        resp = client.post(
            f"/deploy_targets/{target['id']}/publish",
            json={"model_id": model["id"]},
        )
        assert resp.status_code == 200, resp.text

        assert len(captured) == 1
        req = captured[0]
        assert req["headers"].get("X-Excita-Engine") == "microwakeword"
        assert req["headers"].get("X-Excita-Phrase") == "hey jarvis"
        assert req["headers"].get("X-Excita-Version") == "v2"
        assert req["body"] == b"nww-onnx-bytes"

        view = client.get(f"/deploy_targets/{target['id']}").json()
        assert view["last_publish_status"] == "ok"
    finally:
        server.shutdown()
        server.server_close()


def test_publish_failed_push_keeps_selection(client: TestClient) -> None:
    """A failed publish does not roll back the DB row — the operator sees
    'selected, last push failed' and can retry the same call (spec 0011)."""
    target = _create_target(
        client, "http_push", {"url": "http://127.0.0.1:9/unreachable"}
    )
    model = _import_model(
        client,
        metadata=_import_metadata(engine="nanowakeword"),
        artifact=b"onnx",
        filename="hey_jarvis_nww.onnx",
    ).json()

    resp = client.post(
        f"/deploy_targets/{target['id']}/publish", json={"model_id": model["id"]}
    )
    assert resp.status_code == 200
    view = resp.json()
    assert view["current_model_id"] == model["id"], "selection must stick"
    assert view["last_publish_status"] == "failed"
    assert view["last_publish_error"]

    view = client.get(f"/deploy_targets/{target['id']}").json()
    assert view["current_model_id"] == model["id"]


def test_publish_unknown_target_or_model_404s(client: TestClient) -> None:
    assert (
        client.post(
            "/deploy_targets/nope/publish", json={"model_id": "also-nope"}
        ).status_code
        == 404
    )
    target = _create_target(client, "file", {"directory": "/tmp/excita-out"})
    assert (
        client.post(
            f"/deploy_targets/{target['id']}/publish",
            json={"model_id": "missing"},
        ).status_code
        == 404
    )


def test_upload_to_missing_phrase_404s(client: TestClient) -> None:
    resp = client.post(
        "/clips",
        data={"phrase_id": "nope"},
        files={"file": ("x.wav", _wav_bytes(), "audio/wav")},
    )
    assert resp.status_code == 404


# --- microWakeWord / nanoWakeWord adapter coverage ---
#
# Both adapters are exercised through the app seam wherever possible. The
# µWW score() test stays narrow because the app-level suite must not carry
# a TFLite blob fixture (#213 §Testing decisions); both gates follow
# the `_wake_models_available()` pattern — drop the artifacts in place and
# the tests light up.


MWW_MODEL = WAKE_MODELS_DIR / "hey_jarvis_v0.1.tflite"
NWW_MODEL = WAKE_MODELS_DIR / "hey_jarvis_v0.1.nww.onnx"


def _microwakeword_ready() -> bool:
    if not MWW_MODEL.exists():
        return False
    import importlib.util

    return importlib.util.find_spec("tflite_runtime") is not None


requires_microwakeword = pytest.mark.skipif(
    not _microwakeword_ready(),
    reason=(
        "µWW artifact or tflite-runtime missing "
        f"(expected {MWW_MODEL.name} + pip install tflite-runtime)"
    ),
)


def requires_nanowakeword(fn):  # noqa: ANN001, ANN201 - plain decorator

    try:
        import nanowakeword  # noqa: F401

        package_ok = True
    except ImportError:
        package_ok = False
    return pytest.mark.skipif(
        package_ok is False or not NWW_MODEL.exists(),
        reason=f"nanoWakeWord artifact missing (expected {NWW_MODEL.name})",
    )(fn)


@requires_microwakeword
def test_microwakeword_score_curve_over_canned_wav() -> None:
    """Narrow unit seam: canned WAV + known µWW artifact → per-hop curve."""
    import wave as _wave

    from excita.engines.microwakeword import MicroWakeWordEngine

    with _wave.open(str(AUDIO_DIR / "hey_jarvis.wav")) as w:
        assert w.getframerate() == 16000 and w.getnchannels() == 1
        audio = w.readframes(w.getnframes())

    curve = MicroWakeWordEngine().score(audio, str(MWW_MODEL))
    assert curve, "real wake audio must produce a non-empty curve"
    assert all(0.0 <= v <= 1.0 for v in curve)


@requires_nanowakeword
def test_arm_nanowakeword_and_feed_hey_jarvis(client: TestClient) -> None:
    """End-to-end at the app seam (#213 §Testing decisions): arm a
    nanoWakeWord detector, feed the fixture in sub-chunk frames so the
    residual buffer is exercised, expect a fire."""
    phrase_id = _create_phrase(client)
    arm = client.post(
        "/detectors",
        json={
            "phrase_id": phrase_id,
            "model_ref": str(NWW_MODEL),
            "source_device": "satellite",
            "engine": "nanowakeword",
        },
    )
    assert arm.status_code == 201, arm.text
    assert arm.json()["engine"] == "nanowakeword"

    with wave.open(str(AUDIO_DIR / "hey_jarvis.wav")) as w:
        pcm = w.readframes(w.getnframes())

    total_fires = 0
    for offset in range(0, len(pcm), 640):
        resp = client.post(
            "/v1/audio/satellite/frames", content=pcm[offset : offset + 640]
        )
        assert resp.status_code == 202
        total_fires += resp.json()["fires"]
    assert total_fires >= 1, "hey_jarvis fixture must produce at least one fire"

    events = client.get("/v1/wake-events/recent").json()
    assert events and events[0]["phrase_id"] == phrase_id
