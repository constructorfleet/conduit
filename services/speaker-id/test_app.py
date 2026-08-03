"""What the reference service owes Conduit.

The contract is the point: `conduit-speaker` is written against these three
routes, and its own tests use a fake server. These are the other half — they
check that a real service answers the way that client expects, without a model
download standing between a developer and a test run.
"""

from __future__ import annotations

import io
import tempfile
import uuid
import wave
from pathlib import Path

import numpy as np
import pytest
from fastapi.testclient import TestClient

from app import MODEL_SAMPLE_RATE, VoicePrints, cosine, create_app, resample


class ToneEncoder:
    """An encoder that hears pitch.

    Real embeddings put one person's utterances near each other and everyone
    else's far away. A dominant frequency does the same for a test: two
    recordings of one tone embed identically, a different tone embeds
    elsewhere. That is the property identification depends on, and it holds
    without loading a model.
    """

    def __init__(self, width: int = 16) -> None:
        self.width = width

    def embed(self, samples: np.ndarray) -> np.ndarray:
        spectrum = np.abs(np.fft.rfft(samples))
        # The peak as a frequency rather than as a bin index: a bin index
        # depends on how long the recording is, so the same tone recorded for
        # two seconds and for a tenth of one would embed differently — and two
        # unrelated tones could land on the same bucket, which would make this
        # fixture agree that strangers match.
        hertz = np.argmax(spectrum) * MODEL_SAMPLE_RATE / max(1, samples.size)
        buckets = np.zeros(self.width, dtype=np.float64)
        buckets[int(hertz // 100) % self.width] = 1.0
        return buckets


def tone(hertz: int, seconds: float = 2.0, rate: int = MODEL_SAMPLE_RATE) -> bytes:
    """A WAV file of a sine wave, shaped like what Conduit uploads."""
    time = np.linspace(0.0, seconds, int(rate * seconds), endpoint=False)
    samples = (np.sin(2 * np.pi * hertz * time) * 20_000).astype(np.int16)
    buffer = io.BytesIO()
    with wave.open(buffer, "wb") as file:
        file.setnchannels(1)
        file.setsampwidth(2)
        file.setframerate(rate)
        file.writeframes(samples.tobytes())
    return buffer.getvalue()


@pytest.fixture
def store(tmp_path: Path) -> VoicePrints:
    return VoicePrints(tmp_path)


@pytest.fixture
def client(store: VoicePrints) -> TestClient:
    return TestClient(create_app(encoder=ToneEncoder(), prints=store))


def test_an_enrolled_voice_is_recognized(client: TestClient) -> None:
    speaker = uuid.uuid4()

    enrolled = client.post(f"/speakers/{speaker}/enroll", content=tone(440))
    assert enrolled.status_code == 200
    assert enrolled.json()["samples"] == 1

    identified = client.post("/identify", content=tone(440))
    assert identified.status_code == 200
    assert identified.json()["speaker"] == str(speaker)
    assert identified.json()["confidence"] == pytest.approx(1.0)


def test_a_voice_nobody_enrolled_matches_nobody(client: TestClient) -> None:
    # An unknown voice is a normal outcome, not a failure. Conduit reports it
    # as an unidentified speaker and the turn carries on.
    client.post(f"/speakers/{uuid.uuid4()}/enroll", content=tone(440))

    identified = client.post("/identify", content=tone(1_200))

    assert identified.status_code == 200
    assert identified.json()["speaker"] is None
    assert identified.json()["confidence"] == 0.0


def test_identification_reports_the_score_rather_than_deciding(
    client: TestClient,
) -> None:
    # Conduit holds the threshold. A service that filtered by its own would
    # override the operator's `threshold_percent` and hide the near misses they
    # tune it with.
    identified = client.post("/identify", content=tone(440))

    assert identified.status_code == 200
    body = identified.json()
    assert body["speaker"] is None
    assert 0.0 <= body["confidence"] <= 1.0


def test_enrolling_twice_keeps_both_samples(client: TestClient) -> None:
    speaker = uuid.uuid4()

    first = client.post(f"/speakers/{speaker}/enroll", content=tone(440))
    second = client.post(f"/speakers/{speaker}/enroll", content=tone(440))

    assert first.json()["samples"] == 1
    assert second.json()["samples"] == 2


def test_a_forgotten_speaker_stops_matching(client: TestClient) -> None:
    speaker = uuid.uuid4()
    client.post(f"/speakers/{speaker}/enroll", content=tone(440))

    forgotten = client.delete(f"/speakers/{speaker}")
    assert forgotten.status_code == 204

    identified = client.post("/identify", content=tone(440))
    assert identified.json()["speaker"] is None


def test_forgetting_an_unknown_speaker_is_a_404(client: TestClient) -> None:
    # Conduit treats this as success, because the voice print the caller wanted
    # gone is gone. The status still distinguishes the two for anyone driving
    # the service directly.
    response = client.delete(f"/speakers/{uuid.uuid4()}")
    assert response.status_code == 404


def test_a_speaker_that_is_not_a_uuid_is_refused(client: TestClient) -> None:
    # The identifier becomes a file name, so this is the check that keeps the
    # store a store rather than a way to write anywhere on the disk.
    refused = client.post("/speakers/..%2F..%2Fetc%2Fpasswd/enroll", content=tone(440))
    assert refused.status_code in (400, 404)

    deleted = client.delete("/speakers/not-a-uuid")
    assert deleted.status_code == 400


def test_audio_too_short_to_enroll_is_refused(client: TestClient) -> None:
    # A print built from a fragment matches everybody, and it poisons the store
    # in a way that only shows up later as false matches.
    refused = client.post(
        f"/speakers/{uuid.uuid4()}/enroll", content=tone(440, seconds=0.2)
    )

    assert refused.status_code == 422
    assert "too short" in refused.json()["detail"]


def test_audio_too_short_to_identify_matches_nobody(client: TestClient) -> None:
    # Not an error: a turn whose wake gate never opened captured almost
    # nothing, and a confident match against 200 ms of silence is worse than no
    # answer.
    client.post(f"/speakers/{uuid.uuid4()}/enroll", content=tone(440))

    identified = client.post("/identify", content=tone(440, seconds=0.1))

    assert identified.status_code == 200
    assert identified.json()["speaker"] is None


def test_something_that_is_not_audio_is_refused(client: TestClient) -> None:
    refused = client.post("/identify", content=b"this is not a wav file")
    assert refused.status_code == 415


def test_health_answers_before_the_model_is_loaded() -> None:
    # A container that has answered no requests has not paid for the model.
    # Saying so is the difference between a slow first request and a service
    # somebody restarts because they think it is wedged.
    store = VoicePrints(Path(tempfile.mkdtemp()))
    with TestClient(create_app(prints=store)) as client:
        body = client.get("/health").json()

    assert body["status"] == "ok"
    assert body["model_loaded"] is False
    assert body["engine"] == "speechbrain"


def test_an_api_key_is_required_when_one_is_configured(monkeypatch, store) -> None:
    monkeypatch.setenv("SPEAKER_ID_API_KEY", "secret-key")
    client = TestClient(create_app(encoder=ToneEncoder(), prints=store))

    assert client.post("/identify", content=tone(440)).status_code == 401
    assert (
        client.post(
            "/identify",
            content=tone(440),
            headers={"authorization": "Bearer secret-key"},
        ).status_code
        == 200
    )
    # Health stays open, so an orchestrator can check it without holding a
    # credential.
    assert client.get("/health").status_code == 200


def test_audio_at_another_rate_is_resampled_onto_the_models_own(
    client: TestClient,
) -> None:
    # ECAPA is trained at 16 kHz. Feeding it 48 kHz audio does not fail; it
    # embeds a voice an octave off, which reads as a stranger.
    speaker = uuid.uuid4()
    client.post(f"/speakers/{speaker}/enroll", content=tone(440))

    identified = client.post("/identify", content=tone(440, rate=48_000))

    assert identified.json()["speaker"] == str(speaker)


def test_changing_the_encoder_under_a_store_is_refused_not_scored(store) -> None:
    # Comparing a 16-dimension print against a 32-dimension voice is not a low
    # score, it is nonsense. Enrolling says so; identifying steps over the
    # prints it cannot compare rather than failing the whole request.
    speaker = uuid.uuid4()
    narrow = TestClient(create_app(encoder=ToneEncoder(width=16), prints=store))
    narrow.post(f"/speakers/{speaker}/enroll", content=tone(440))

    wide = TestClient(create_app(encoder=ToneEncoder(width=32), prints=store))
    refused = wide.post(f"/speakers/{speaker}/enroll", content=tone(440))
    assert refused.status_code == 409
    assert "re-enroll" in refused.json()["detail"]

    identified = wide.post("/identify", content=tone(440))
    assert identified.status_code == 200
    assert identified.json()["speaker"] is None


def test_resampling_preserves_duration() -> None:
    samples = np.sin(np.linspace(0, 100, 48_000)).astype(np.float32)
    resampled = resample(samples, 48_000, 16_000)
    assert resampled.size == pytest.approx(16_000, abs=1)


def test_a_voice_pointing_away_scores_nothing_rather_than_half() -> None:
    # Rescaling cosine from [-1, 1] into [0, 1] would report an opposed vector
    # as a 50% match, handing a confidence to something that has none.
    assert cosine(np.array([1.0, 0.0]), np.array([-1.0, 0.0])) == 0.0
    assert cosine(np.array([1.0, 0.0]), np.array([1.0, 0.0])) == pytest.approx(1.0)
    assert cosine(np.zeros(2), np.ones(2)) == 0.0
