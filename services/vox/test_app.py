"""What the reference service owes Conduit.

The contract is the point: `conduit-speaker` is written against these three
routes, and its own tests use a fake server. These are the other half — they
check that a real service answers the way that client expects, without a model
download standing between a developer and a test run.
"""

from __future__ import annotations

import asyncio
import io
import json
import os
import tempfile
import uuid
import wave
from dataclasses import dataclass
from pathlib import Path

import httpx
import numpy as np
import pytest
from fastapi.testclient import TestClient

from app import (
    DEFAULT_MODELS,
    EMBEDDING_WIDTHS,
    ENGINE_CLASSES,
    ConduitSpeaker,
    MAX_LABEL_LENGTH,
    MODEL_SAMPLE_RATE,
    LinkStore,
    NeMoEncoder,
    PyannoteEncoder,
    Roster,
    Syncer,
    SpeechBrainEncoder,
    VoicePrints,
    WidthMismatchError,
    build_encoder,
    check_width,
    cosine,
    create_app,
    hugging_face_token,
    resample,
)


@dataclass
class RecordedConduitRequest:
    url: str
    bearer: str
    body: dict[str, str]


class FakeConduitClient:
    def __init__(self) -> None:
        self.create_requests: list[RecordedConduitRequest] = []
        self.delete_requests: list[tuple[str, str, str]] = []

    def create_link(
        self,
        conduit_url: str,
        operator_token: str,
        body: dict[str, str],
    ) -> dict[str, str]:
        self.create_requests.append(
            RecordedConduitRequest(conduit_url, operator_token, body)
        )
        return {
            "sync_token": "sync-token-from-conduit",
            "provider_definition_id": "vox-kitchen-vox-01",
        }

    def delete_link(self, conduit_url: str, peer_id: str, sync_token: str) -> None:
        self.delete_requests.append((conduit_url, peer_id, sync_token))


class FakeSpeakerClient:
    def __init__(self, responses: list[list[ConduitSpeaker] | Exception]) -> None:
        self._responses = responses
        self.requests: list[tuple[str, str]] = []

    async def list_speakers(
        self, conduit_url: str, sync_token: str
    ) -> list[ConduitSpeaker]:
        self.requests.append((conduit_url, sync_token))
        response = self._responses.pop(0)
        if isinstance(response, Exception):
            raise response
        return response


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


def test_every_engine_is_reachable_by_name_and_names_a_default_model() -> None:
    # `SPEAKER_ID_ENGINE` is the whole selection mechanism, so an engine with a
    # class and no default model is an engine an operator can only start by
    # also knowing a model name off by heart.
    assert set(ENGINE_CLASSES) == {"speechbrain", "pyannote", "nemo"}
    for engine in ENGINE_CLASSES:
        assert DEFAULT_MODELS[engine]
        assert EMBEDDING_WIDTHS[engine]


@pytest.mark.parametrize(
    ("engine", "expected"),
    [
        ("speechbrain", SpeechBrainEncoder),
        ("pyannote", PyannoteEncoder),
        ("nemo", NeMoEncoder),
    ],
)
def test_selecting_an_engine_reaches_that_engines_class(
    monkeypatch, tmp_path: Path, engine: str, expected: type
) -> None:
    # Checked by interception rather than by loading: constructing the real
    # class downloads gigabytes, and what is under test is the selection, not
    # the model. The class is replaced so a wrong mapping fails here rather
    # than in a container nobody can run in CI.
    built: dict[str, object] = {}

    class Recorded:
        def __init__(self, model: str, cache: Path, device: str) -> None:
            built.update(model=model, cache=cache, device=device)

        def embed(self, samples: np.ndarray) -> np.ndarray:
            return np.zeros(EMBEDDING_WIDTHS[engine])

    monkeypatch.setitem(ENGINE_CLASSES, engine, Recorded)
    encoder = build_encoder(engine, DEFAULT_MODELS[engine], tmp_path, "cpu")

    assert isinstance(encoder, Recorded)
    assert ENGINE_CLASSES[engine] is not expected  # the stub is what ran
    assert built["model"] == DEFAULT_MODELS[engine]
    assert built["device"] == "cpu"


def test_an_unknown_engine_names_the_ones_this_image_serves() -> None:
    with pytest.raises(RuntimeError) as refused:
        build_encoder("resemblyzer", "", Path("/models"), "cpu")

    assert "resemblyzer" in str(refused.value)
    assert "pyannote" in str(refused.value)


@pytest.mark.parametrize(
    ("engine", "width"),
    [("speechbrain", 192), ("pyannote", 512), ("nemo", 192)],
)
def test_each_engine_declares_the_width_its_model_produces(
    engine: str, width: int
) -> None:
    # The widths are what decide whether an enrolment store survives an engine
    # change, so they are asserted rather than left to a README that drifts.
    # Only a real model download proves a loaded model agrees; `check_width`
    # is the runtime half of this pair.
    assert EMBEDDING_WIDTHS[engine] == width
    assert ENGINE_CLASSES[engine].width == width


def test_a_model_of_the_wrong_width_for_its_engine_is_refused_at_load() -> None:
    # An operator may point `SPEAKER_ID_MODEL` at a variant of another size.
    # Caught while the model name is still in hand, because the store's own
    # guard can only report two numbers.
    check_width("pyannote", 512)

    with pytest.raises(RuntimeError) as refused:
        check_width("pyannote", 192)

    assert "512" in str(refused.value)
    assert "192" in str(refused.value)


def test_reloading_the_engine_swaps_to_the_requested_model(
    monkeypatch, tmp_path: Path
) -> None:
    built: list[str] = []

    def fake_build_encoder(engine: str, model: str, cache: Path, device: str) -> ToneEncoder:
        assert engine == "speechbrain"
        assert cache == tmp_path / "models"
        assert device == "cpu"
        built.append(model)
        return ToneEncoder(width=EMBEDDING_WIDTHS["speechbrain"])

    monkeypatch.setenv("SPEAKER_ID_MODEL_DIR", str(tmp_path / "models"))
    monkeypatch.setattr("app.build_encoder", fake_build_encoder)
    client = TestClient(create_app(prints=VoicePrints(tmp_path)))

    response = client.post("/engine/reload", json={"model": "speechbrain/custom-vox"})

    assert response.status_code == 200
    assert built == ["speechbrain/custom-vox"]
    assert response.json()["model"] == "speechbrain/custom-vox"
    assert response.json()["model_loaded"] is True
    assert client.get("/health").json()["model"] == "speechbrain/custom-vox"


def test_reloading_refuses_a_width_mismatch_when_prints_exist(
    monkeypatch, tmp_path: Path
) -> None:
    prints = VoicePrints(tmp_path)
    speaker = uuid.uuid4()
    prints.add(speaker, np.zeros(EMBEDDING_WIDTHS["speechbrain"], dtype=np.float32))

    def mismatched_build_encoder(
        engine: str, model: str, cache: Path, device: str
    ) -> ToneEncoder:
        raise WidthMismatchError(
            "engine `speechbrain` produces 192-dimension embeddings but the loaded "
            "model produces 256"
        )

    monkeypatch.setattr("app.build_encoder", mismatched_build_encoder)
    client = TestClient(create_app(prints=prints))

    response = client.post("/engine/reload", json={"model": "speechbrain/wide"})

    assert response.status_code == 409
    assert "192" in response.json()["detail"]
    assert client.get("/health").json()["model"] == DEFAULT_MODELS["speechbrain"]


def test_a_gated_pyannote_model_without_a_token_says_which_terms_to_accept(
    monkeypatch, tmp_path: Path
) -> None:
    # The failure Hugging Face itself returns is a 401 that mentions neither the
    # agreement nor the token, and an operator cannot act on that.
    monkeypatch.delenv("HF_TOKEN", raising=False)
    monkeypatch.delenv("HUGGING_FACE_HUB_TOKEN", raising=False)

    with pytest.raises(RuntimeError) as refused:
        PyannoteEncoder("pyannote/embedding", tmp_path, "cpu")

    message = str(refused.value)
    assert "gated" in message
    assert "huggingface.co/pyannote/embedding" in message
    assert "HF_TOKEN" in message


def test_either_hugging_face_token_variable_is_honoured(monkeypatch) -> None:
    # The hub honours both names, so a service that read only one would report
    # gating an operator had already resolved.
    monkeypatch.delenv("HF_TOKEN", raising=False)
    monkeypatch.setenv("HUGGING_FACE_HUB_TOKEN", "hf-read-token")
    assert hugging_face_token() == "hf-read-token"

    monkeypatch.setenv("HF_TOKEN", "preferred")
    assert hugging_face_token() == "preferred"

    monkeypatch.delenv("HF_TOKEN")
    monkeypatch.delenv("HUGGING_FACE_HUB_TOKEN")
    assert hugging_face_token() is None


@pytest.mark.parametrize(
    ("enrolled_with", "then"), [("speechbrain", "pyannote"), ("pyannote", "nemo")]
)
def test_swapping_between_engines_of_different_widths_is_refused_not_scored(
    store, enrolled_with: str, then: str
) -> None:
    # The most damaging failure this service has: a 512-dimension voice scored
    # against a 192-dimension print is not a low score, it is confident
    # nonsense that identifies the wrong person. Adding engines must not make
    # that reachable.
    speaker = uuid.uuid4()
    first = TestClient(
        create_app(
            encoder=ToneEncoder(width=EMBEDDING_WIDTHS[enrolled_with]), prints=store
        )
    )
    first.post(f"/speakers/{speaker}/enroll", content=tone(440))

    second = TestClient(
        create_app(encoder=ToneEncoder(width=EMBEDDING_WIDTHS[then]), prints=store)
    )
    refused = second.post(f"/speakers/{speaker}/enroll", content=tone(440))

    assert refused.status_code == 409
    assert "re-enroll" in refused.json()["detail"]
    assert second.post("/identify", content=tone(440)).json()["speaker"] is None


def test_two_engines_of_the_same_width_cannot_be_caught_by_the_store(store) -> None:
    # SpeechBrain and TitaNet both emit 192 dimensions, so the guard cannot
    # tell a store built by one from a voice embedded by the other — it will
    # score them, and score them wrongly. There is no check that fixes this,
    # which is exactly why the README says an engine swap needs a re-enrolment
    # and a re-tuned threshold. Pinned as a known limit rather than a bug.
    assert EMBEDDING_WIDTHS["speechbrain"] == EMBEDDING_WIDTHS["nemo"]

    speaker = uuid.uuid4()
    ecapa = TestClient(create_app(encoder=ToneEncoder(width=192), prints=store))
    ecapa.post(f"/speakers/{speaker}/enroll", content=tone(440))

    titanet = TestClient(create_app(encoder=ToneEncoder(width=192), prints=store))
    accepted = titanet.post(f"/speakers/{speaker}/enroll", content=tone(440))
    assert accepted.status_code == 200


def test_resampling_preserves_duration() -> None:
    samples = np.sin(np.linspace(0, 100, 48_000)).astype(np.float32)
    resampled = resample(samples, 48_000, 16_000)
    assert resampled.size == pytest.approx(16_000, abs=1)


def test_the_speaker_list_is_empty_when_nobody_is_enrolled(client: TestClient) -> None:
    response = client.get("/speakers")

    assert response.status_code == 200
    assert response.json() == {"speakers": []}


def test_enrolling_adds_the_speaker_to_the_roster(client: TestClient) -> None:
    speaker = uuid.uuid4()
    client.post(f"/speakers/{speaker}/enroll", content=tone(440))

    body = client.get("/speakers").json()

    assert len(body["speakers"]) == 1
    entry = body["speakers"][0]
    assert entry["uuid"] == str(speaker)
    assert entry["label"] is None
    assert entry["samples"] == 1
    assert entry["created_at"]
    assert entry["updated_at"]


def test_enrolling_the_same_speaker_again_bumps_their_sample_count(
    client: TestClient,
) -> None:
    speaker = uuid.uuid4()
    client.post(f"/speakers/{speaker}/enroll", content=tone(440))
    client.post(f"/speakers/{speaker}/enroll", content=tone(440))

    entry = client.get("/speakers").json()["speakers"][0]

    assert entry["samples"] == 2


def test_forgetting_a_speaker_also_removes_them_from_the_roster(
    client: TestClient,
) -> None:
    speaker = uuid.uuid4()
    client.post(f"/speakers/{speaker}/enroll", content=tone(440))
    client.delete(f"/speakers/{speaker}")

    assert client.get("/speakers").json()["speakers"] == []


def test_a_label_can_be_set_and_read_back(client: TestClient) -> None:
    speaker = uuid.uuid4()
    client.post(f"/speakers/{speaker}/enroll", content=tone(440))

    labeled = client.patch(f"/speakers/{speaker}", json={"label": "Alice"})

    assert labeled.status_code == 200
    assert labeled.json()["label"] == "Alice"
    assert client.get("/speakers").json()["speakers"][0]["label"] == "Alice"


def test_a_label_can_be_cleared_by_setting_it_to_null(client: TestClient) -> None:
    speaker = uuid.uuid4()
    client.post(f"/speakers/{speaker}/enroll", content=tone(440))
    client.patch(f"/speakers/{speaker}", json={"label": "Alice"})

    cleared = client.patch(f"/speakers/{speaker}", json={"label": None})

    assert cleared.status_code == 200
    assert cleared.json()["label"] is None


def test_a_label_beyond_the_limit_is_refused(client: TestClient) -> None:
    speaker = uuid.uuid4()
    client.post(f"/speakers/{speaker}/enroll", content=tone(440))

    refused = client.patch(
        f"/speakers/{speaker}", json={"label": "x" * (MAX_LABEL_LENGTH + 1)}
    )

    assert refused.status_code == 422


def test_labeling_a_speaker_nobody_enrolled_is_a_404(client: TestClient) -> None:
    refused = client.patch(f"/speakers/{uuid.uuid4()}", json={"label": "Alice"})

    assert refused.status_code == 404


def test_a_label_survives_a_restart(tmp_path: Path) -> None:
    speaker = uuid.uuid4()
    first = TestClient(create_app(encoder=ToneEncoder(), prints=VoicePrints(tmp_path)))
    first.post(f"/speakers/{speaker}/enroll", content=tone(440))
    first.patch(f"/speakers/{speaker}", json={"label": "Alice"})

    reloaded = TestClient(
        create_app(encoder=ToneEncoder(), prints=VoicePrints(tmp_path))
    )
    entry = reloaded.get("/speakers").json()["speakers"][0]

    assert entry["label"] == "Alice"


def test_a_print_without_a_manifest_entry_is_rebuilt_on_read(tmp_path: Path) -> None:
    # An upgrade from a version that only wrote .npy files should not lose
    # entries the print files already document; the roster reconciles on the
    # first read rather than showing an empty list a user knows is wrong.
    prints = VoicePrints(tmp_path)
    speaker = uuid.uuid4()
    prints.add(speaker, np.array([1.0, 0.0], dtype=np.float32))

    client = TestClient(create_app(encoder=ToneEncoder(), prints=prints))
    entries = client.get("/speakers").json()["speakers"]

    assert len(entries) == 1
    assert entries[0]["uuid"] == str(speaker)
    assert entries[0]["label"] is None
    assert entries[0]["samples"] == 1


def test_the_root_redirects_to_the_embedded_ui(client: TestClient) -> None:
    response = client.get("/", follow_redirects=False)

    assert response.status_code in (302, 307)
    assert response.headers["location"].startswith("/ui")


def test_the_embedded_ui_names_the_service(client: TestClient) -> None:
    response = client.get("/ui/")

    assert response.status_code == 200
    assert "Conduit Vox" in response.text


def test_the_embedded_ui_exposes_the_link_flow(client: TestClient) -> None:
    response = client.get("/ui/")

    assert response.status_code == 200
    assert 'id="link-panel"' in response.text
    assert 'api("/link"' in response.text
    assert 'api("/link", { method: "DELETE" })' in response.text


def test_the_embedded_ui_exposes_engine_reload(client: TestClient) -> None:
    response = client.get("/ui/")

    assert response.status_code == 200
    assert 'id="reload-model"' in response.text
    assert 'id="reload-engine"' in response.text
    assert 'api("/engine/reload"' in response.text


def test_the_embedded_ui_stays_open_when_an_api_key_is_required(
    monkeypatch, store
) -> None:
    # Loading the page has to work without a bearer, or an operator with the
    # key in their head cannot get to the form they would paste it into. The
    # routes the page calls still carry the key.
    monkeypatch.setenv("SPEAKER_ID_API_KEY", "secret-key")
    client = TestClient(create_app(encoder=ToneEncoder(), prints=store))

    assert client.get("/ui/").status_code == 200
    assert client.get("/", follow_redirects=False).status_code in (302, 307)


def test_health_reports_the_roster_count(client: TestClient) -> None:
    client.post(f"/speakers/{uuid.uuid4()}/enroll", content=tone(440))
    client.post(f"/speakers/{uuid.uuid4()}/enroll", content=tone(440))

    assert client.get("/health").json()["enrolled"] == 2


def test_a_voice_pointing_away_scores_nothing_rather_than_half() -> None:
    # Rescaling cosine from [-1, 1] into [0, 1] would report an opposed vector
    # as a 50% match, handing a confidence to something that has none.
    assert cosine(np.array([1.0, 0.0]), np.array([-1.0, 0.0])) == 0.0
    assert cosine(np.array([1.0, 0.0]), np.array([1.0, 0.0])) == pytest.approx(1.0)
    assert cosine(np.zeros(2), np.ones(2)) == 0.0


def test_link_status_is_unlinked_without_a_saved_link(tmp_path: Path) -> None:
    client = TestClient(
        create_app(encoder=ToneEncoder(), prints=VoicePrints(tmp_path))
    )

    response = client.get("/link")

    assert response.status_code == 200
    assert response.json() == {"status": "unlinked"}


def test_linking_posts_to_conduit_and_persists_redacted_status(
    monkeypatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("SPEAKER_ID_BASE_URL", "http://vox.internal:8081/")
    conduit = FakeConduitClient()
    client = TestClient(
        create_app(
            encoder=ToneEncoder(),
            prints=VoicePrints(tmp_path),
            conduit_client=conduit,
        )
    )

    response = client.post(
        "/link",
        json={
            "conduit_url": "http://conduit.internal:8080/",
            "operator_token": "operator-secret",
            "peer_name": "Kitchen Vox",
        },
    )

    assert response.status_code == 200
    body = response.json()
    public = {
        "status": "linked",
        "conduit_url": "http://conduit.internal:8080",
        "peer_id": body["peer_id"],
        "peer_name": "Kitchen Vox",
        "provider_definition_id": "vox-kitchen-vox-01",
        "linked_at": body["linked_at"],
    }
    assert body == {**public, "local_api_key": body["local_api_key"]}
    assert uuid.UUID(body["peer_id"])
    assert len(conduit.create_requests) == 1
    request = conduit.create_requests[0]
    assert request.url == "http://conduit.internal:8080"
    assert request.bearer == "operator-secret"
    assert request.body["peer_name"] == "Kitchen Vox"
    assert request.body["peer_id"] == body["peer_id"]
    assert request.body["vox_base_url"] == "http://vox.internal:8081"
    assert request.body["vox_api_key"] == body["local_api_key"]
    assert len(body["local_api_key"]) >= 32

    persisted = client.get("/link")
    assert persisted.status_code == 200
    assert persisted.json() == public
    assert "sync_token" not in persisted.text
    assert "operator-secret" not in persisted.text
    assert "vox_api_key" not in persisted.text
    assert "local_api_key" not in persisted.text

    mode = (tmp_path / LinkStore.FILENAME).stat().st_mode & 0o777
    assert mode == 0o600


def test_link_generated_api_key_authorizes_vox_routes(tmp_path: Path) -> None:
    conduit = FakeConduitClient()
    client = TestClient(
        create_app(
            encoder=ToneEncoder(),
            prints=VoicePrints(tmp_path),
            conduit_client=conduit,
        )
    )
    client.post(
        "/link",
        json={
            "conduit_url": "http://conduit.internal:8080",
            "operator_token": "operator-secret",
            "peer_name": "Kitchen Vox",
        },
    )
    generated_key = conduit.create_requests[0].body["vox_api_key"]

    assert client.post("/identify", content=tone(440)).status_code == 401
    assert (
        client.post(
            "/identify",
            content=tone(440),
            headers={"authorization": f"Bearer {generated_key}"},
        ).status_code
        == 200
    )


def test_configured_api_key_is_sent_to_conduit_and_keeps_env_precedence(
    monkeypatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("SPEAKER_ID_API_KEY", "configured-key")
    conduit = FakeConduitClient()
    client = TestClient(
        create_app(
            encoder=ToneEncoder(),
            prints=VoicePrints(tmp_path),
            conduit_client=conduit,
        )
    )

    response = client.post(
        "/link",
        json={
            "conduit_url": "http://conduit.internal:8080",
            "operator_token": "operator-secret",
            "peer_name": "Kitchen Vox",
        },
    )

    assert response.status_code == 200
    assert conduit.create_requests[0].body["vox_api_key"] == "configured-key"
    assert client.post("/identify", content=tone(440)).status_code == 401
    assert (
        client.post(
            "/identify",
            content=tone(440),
            headers={"authorization": "Bearer configured-key"},
        ).status_code
        == 200
    )


def test_linking_again_is_refused_without_force(tmp_path: Path) -> None:
    conduit = FakeConduitClient()
    client = TestClient(
        create_app(
            encoder=ToneEncoder(),
            prints=VoicePrints(tmp_path),
            conduit_client=conduit,
        )
    )
    payload = {
        "conduit_url": "http://conduit.internal:8080",
        "operator_token": "operator-secret",
        "peer_name": "Kitchen Vox",
    }
    assert client.post("/link", json=payload).status_code == 200

    refused = client.post("/link", json=payload)

    assert refused.status_code == 409
    assert len(conduit.create_requests) == 1


def test_force_link_replaces_the_saved_link(tmp_path: Path) -> None:
    conduit = FakeConduitClient()
    client = TestClient(
        create_app(
            encoder=ToneEncoder(),
            prints=VoicePrints(tmp_path),
            conduit_client=conduit,
        )
    )
    first = client.post(
        "/link",
        json={
            "conduit_url": "http://conduit.internal:8080",
            "operator_token": "operator-secret",
            "peer_name": "Kitchen Vox",
        },
    ).json()

    second = client.post(
        "/link",
        json={
            "conduit_url": "http://conduit.internal:8080",
            "operator_token": "operator-secret",
            "peer_name": "Office Vox",
            "force": True,
        },
    )

    assert second.status_code == 200
    assert second.json()["peer_name"] == "Office Vox"
    assert second.json()["peer_id"] == first["peer_id"]
    assert len(conduit.create_requests) == 2


def test_unlink_best_effort_revokes_conduit_then_removes_local_state(
    tmp_path: Path,
) -> None:
    conduit = FakeConduitClient()
    client = TestClient(
        create_app(
            encoder=ToneEncoder(),
            prints=VoicePrints(tmp_path),
            conduit_client=conduit,
        )
    )
    linked = client.post(
        "/link",
        json={
            "conduit_url": "http://conduit.internal:8080",
            "operator_token": "operator-secret",
            "peer_name": "Kitchen Vox",
        },
    ).json()

    response = client.delete("/link")

    assert response.status_code == 204
    assert conduit.delete_requests == [
        ("http://conduit.internal:8080", linked["peer_id"], "sync-token-from-conduit")
    ]
    assert not (tmp_path / LinkStore.FILENAME).exists()
    assert client.get("/link").json() == {"status": "unlinked"}


def test_startup_sync_pulls_conduit_labels_into_the_local_roster(tmp_path: Path) -> None:
    speaker = uuid.uuid4()
    remote_only = uuid.uuid4()
    local_only = uuid.uuid4()
    prints = VoicePrints(tmp_path)
    roster = Roster(tmp_path)
    links = LinkStore(tmp_path)
    links.save(
        conduit_url="http://conduit.internal:8080",
        sync_token="sync-token",
        peer_id="peer-1",
        peer_name="Kitchen Vox",
        provider_definition_id="vox-peer-1",
        local_api_key="local-key",
    )
    prints.add(speaker, np.array([1.0, 0.0], dtype=np.float32))
    roster.touch(speaker, 1)
    roster.set_label(speaker, "Wrong Local Label")
    prints.add(local_only, np.array([0.0, 1.0], dtype=np.float32))
    roster.touch(local_only, 1)
    roster.set_label(local_only, "Bench Voice")
    conduit = FakeSpeakerClient(
        [[
            ConduitSpeaker(id=str(speaker), name="Ada Lovelace"),
            ConduitSpeaker(id=str(remote_only), name="Grace Hopper"),
        ]]
    )

    with TestClient(
        create_app(
            encoder=ToneEncoder(),
            prints=prints,
            roster=roster,
            link_store=links,
            speaker_client=conduit,
            sync_interval_seconds=60.0,
        )
    ) as client:
        synced = client.get(
            "/speakers", headers={"authorization": "Bearer local-key"}
        )

    assert synced.status_code == 200
    speakers = {entry["uuid"]: entry for entry in synced.json()["speakers"]}
    assert speakers[str(speaker)]["label"] == "Ada Lovelace"
    assert speakers[str(speaker)]["samples"] == 1
    assert speakers[str(remote_only)]["label"] == "Grace Hopper"
    assert speakers[str(remote_only)]["samples"] == 0
    assert speakers[str(local_only)]["label"] == "Bench Voice"
    assert conduit.requests == [("http://conduit.internal:8080", "sync-token")]


def test_syncer_retries_with_exponential_backoff_without_crashing(
    tmp_path: Path,
) -> None:
    links = LinkStore(tmp_path)
    links.save(
        conduit_url="http://conduit.internal:8080",
        sync_token="sync-token",
        peer_id="peer-1",
        peer_name="Kitchen Vox",
        provider_definition_id="vox-peer-1",
        local_api_key="local-key",
    )
    conduit = FakeSpeakerClient(
        [
            httpx.ConnectError("dial tone of the damned"),
            httpx.ConnectError("still dead"),
            [ConduitSpeaker(id=str(uuid.uuid4()), name="Ada Lovelace")],
        ]
    )
    sleeps: list[float] = []

    async def fake_sleep(seconds: float) -> None:
        sleeps.append(seconds)
        if len(sleeps) == 3:
            raise asyncio.CancelledError()

    syncer = Syncer(
        prints=VoicePrints(tmp_path),
        roster=Roster(tmp_path),
        links=links,
        conduit=conduit,
        interval_seconds=5.0,
        max_backoff_seconds=12.0,
        sleep=fake_sleep,
    )

    with pytest.raises(asyncio.CancelledError):
        asyncio.run(syncer.run_forever())

    assert sleeps == [5.0, 10.0, 5.0]
    assert len(conduit.requests) == 3


def test_a_saved_link_with_group_or_world_permissions_is_refused(tmp_path: Path) -> None:
    store = LinkStore(tmp_path)
    store.save(
        conduit_url="http://conduit.internal:8080",
        sync_token="sync-token",
        peer_id="peer-1",
        peer_name="Kitchen Vox",
        provider_definition_id="vox-peer-1",
        local_api_key="local-key",
    )
    os.chmod(tmp_path / LinkStore.FILENAME, 0o644)
    client = TestClient(
        create_app(encoder=ToneEncoder(), prints=VoicePrints(tmp_path))
    )

    status = client.get("/link")
    protected = client.post(
        "/identify",
        content=tone(440),
        headers={"authorization": "Bearer local-key"},
    )

    assert status.status_code == 500
    assert "permissions" in status.json()["detail"]
    assert protected.status_code == 500


def test_http_conduit_client_sends_the_expected_link_request() -> None:
    from app import HttpConduitClient

    seen: dict[str, object] = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["method"] = request.method
        seen["url"] = str(request.url)
        seen["authorization"] = request.headers["authorization"]
        seen["body"] = json_body = json.loads(request.content)
        assert json_body["vox_api_key"] == "local-key"
        return httpx.Response(
            201,
            json={
                "sync_token": "sync-token",
                "provider_definition_id": "vox-peer-1",
            },
        )

    client = HttpConduitClient(transport=httpx.MockTransport(handler))

    response = client.create_link(
        "http://conduit.internal:8080/",
        "operator-secret",
        {
            "peer_name": "Kitchen Vox",
            "peer_id": "peer-1",
            "vox_base_url": "http://vox.internal:8081",
            "vox_api_key": "local-key",
        },
    )

    assert response == {
        "sync_token": "sync-token",
        "provider_definition_id": "vox-peer-1",
    }
    assert seen["method"] == "POST"
    assert seen["url"] == "http://conduit.internal:8080/v1/vox/links"
    assert seen["authorization"] == "Bearer operator-secret"
