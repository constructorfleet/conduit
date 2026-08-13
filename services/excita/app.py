"""Conduit Excita — the wake-word operations service.

Skeleton per spec 0011. Ships with:

- `POST /phrases`, `GET /phrases`
- `POST /clips` (multipart upload; browser record uses the same endpoint)
- `GET /clips` filtered by phrase and verdict (including `unlabeled`)
- `POST /clips/{id}/label`
- `GET /clips/{id}/audio` for playback
- `GET /health` (link-health) and `GET /ready`
- `/link` router from `conduit-link` (0005/0010 shape)

Training and deploy surfaces are defined in the spec but not implemented in
the scaffold — the null engine adapter raises `NotSupportedError` if wired up
so a call site never mistakes silence for success.
"""

from __future__ import annotations

import logging
import os
import wave
from contextlib import asynccontextmanager
from datetime import datetime, timezone
from io import BytesIO
from pathlib import Path

from fastapi import FastAPI, File, Form, HTTPException, Request, UploadFile
from fastapi.responses import RedirectResponse, Response
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from conduit_link import (
    HttpConduitLinkClient,
    LinkConfig,
    LinkedServiceKind,
    LinkedServicePanel,
    LinkRequest as _SharedLinkRequest,
    LinkStore,
    make_link_router,
)

from .backend import Backend, Clip, Label, Phrase, SqliteBackend, new_id
from .clip_store import ClipStore, UnsupportedMimeError
from .engines import (
    EngineKind,
    NotSupportedError,
    NullEngine,
    OpenWakeWordEngine,
    WakeWordEngine,
)
from .supervisor import DetectorSupervisor, WakeEvent, bindings_view

LOG = logging.getLogger("excita")

DEFAULT_PORT = 8084


class _NoExtension:
    __slots__: tuple[()] = ()


def _ext_from(_payload: dict[str, object]) -> _NoExtension:
    return _NoExtension()


def _ext_to(_extension: _NoExtension) -> dict[str, object]:
    return {}


def _build_create_body(
    request: _SharedLinkRequest, _existing: _NoExtension | None
) -> dict[str, object]:
    peer_id = request.peer_name.strip().lower().replace(" ", "-")
    base_url = os.getenv("EXCITA_BASE_URL", f"http://localhost:{DEFAULT_PORT}")
    return {
        "service_kind": LinkedServiceKind.EXCITA.value,
        "peer_name": request.peer_name,
        "peer_id": peer_id,
        "peer_base_url": base_url,
        "panel": {
            "id": "excita",
            "label": "Excita",
            "icon": "waveform",
            "path": "/ui/",
        },
    }


def _build_extension(_request, _response, _existing) -> _NoExtension:
    return _NoExtension()


def _public(_extension: _NoExtension) -> dict[str, object]:
    return {}


class Config(BaseModel):
    data_dir: Path
    backend_type: str
    base_url: str
    # Where the three shared openWakeWord ONNX files live. Defaults to
    # `<data_dir>/wake-models`, populated by `scripts/fetch-wake-models.sh`
    # (or a bind mount in production). When the pair isn't there the
    # openwakeword engine slot stays as `NullEngine`, so the API surface
    # continues to answer 501 instead of the app failing to boot — spec
    # 0011 §Non-goals: replacing engine-specific tooling.
    wake_models_dir: Path | None = None
    pre_roll_ms: int = 2000

    @classmethod
    def from_env(cls) -> "Config":
        data_dir = Path(os.getenv("EXCITA_DATA_DIR", "/data"))
        wake_env = os.getenv("EXCITA_WAKE_MODELS_DIR")
        wake_dir = Path(wake_env) if wake_env else data_dir / "wake-models"
        return cls(
            data_dir=data_dir,
            backend_type=os.getenv("EXCITA_BACKEND", "sqlite"),
            base_url=os.getenv("EXCITA_BASE_URL", f"http://localhost:{DEFAULT_PORT}"),
            wake_models_dir=wake_dir,
            pre_roll_ms=int(os.getenv("EXCITA_PREROLL_MS", "2000")),
        )


class HealthResponse(BaseModel):
    status: str
    backend: str
    linked: bool


class ReadyResponse(BaseModel):
    status: str
    backend_ready: bool


class PhraseIn(BaseModel):
    name: str
    display_label: str
    language: str = "en"


class PhraseOut(BaseModel):
    id: str
    name: str
    display_label: str
    language: str


class ClipOut(BaseModel):
    id: str
    phrase_id: str
    sample_rate: int
    duration_ms: int
    source: str
    source_peer: str | None
    sha256: str
    mime_type: str
    created_at: str
    verdict: str | None


class LabelIn(BaseModel):
    verdict: str
    labeller: str = "operator"
    split: str | None = None
    notes: str | None = None


class LabelOut(BaseModel):
    clip_id: str
    verdict: str
    labeller: str
    split: str | None
    notes: str | None
    labelled_at: str


class DetectorOut(BaseModel):
    """A `(phrase, model, engine, source_device)` binding armed in-process."""

    id: str
    phrase_id: str
    model_ref: str
    engine: str
    source_device: str
    sample_rate: int
    frames_seen: int
    last_frame_at: str | None


class ArmDetectorIn(BaseModel):
    phrase_id: str
    model_ref: str
    source_device: str
    engine: str = EngineKind.OPENWAKEWORD.value
    threshold: float | None = None


class WakeEventOut(BaseModel):
    """Local ring-buffer entry (spec 0011 §Standalone posture)."""

    detector_id: str
    phrase_id: str
    source_device: str
    confidence: float
    detected_at: str
    audio_clip_id: str | None


def _default_engines(config: Config) -> dict[EngineKind, WakeWordEngine]:
    """Real engine where models are available, `NullEngine` otherwise.

    openWakeWord gets a real adapter iff the two shared ONNX models are
    present at boot. When they're not, the slot stays a `NullEngine` so
    the API answers with a 501 naming the missing capability rather than
    a 404 or a crash — spec 0011's "honest gap, not a stub" contract.
    """
    engines: dict[EngineKind, WakeWordEngine] = {
        kind: NullEngine(kind) for kind in EngineKind
    }
    wake_dir = config.wake_models_dir
    if wake_dir is not None:
        melspec = wake_dir / "melspectrogram.onnx"
        embedding = wake_dir / "embedding_model.onnx"
        if melspec.exists() and embedding.exists():
            engines[EngineKind.OPENWAKEWORD] = OpenWakeWordEngine(
                melspec_path=melspec,
                embedding_path=embedding,
            )
            LOG.info("openwakeword engine ready from %s", wake_dir)
        else:
            LOG.info(
                "openwakeword models not found in %s; slot stays null", wake_dir
            )
    return engines


def _make_backend(config: Config) -> Backend:
    if config.backend_type == "sqlite":
        return SqliteBackend(config.data_dir / "excita.db")
    if config.backend_type == "postgres":
        raise NotImplementedError("postgres backend reserved; see spec 0011")
    raise ValueError(f"Unknown EXCITA_BACKEND: {config.backend_type}")


def _wav_metadata(data: bytes) -> tuple[int, int] | None:
    """Return `(sample_rate, duration_ms)` for a PCM WAV, else `None`.

    Only WAV is introspected here — Opus/OGG/WebM containers need a real
    codec dep. Non-WAV uploads get `sample_rate=0, duration_ms=0` and the
    UI shows "duration unknown" until an engine adapter fills it in.
    """
    try:
        with wave.open(BytesIO(data)) as wav:
            frames = wav.getnframes()
            rate = wav.getframerate()
            if rate <= 0:
                return None
            duration_ms = int(round(frames * 1000 / rate))
            return rate, duration_ms
    except (wave.Error, EOFError):
        return None


def _verdict_of(backend: Backend, clip_id: str) -> str | None:
    """Verdict from the default (`operator`) labeller if present.

    Kept single-labeller for the scaffold — multi-labeller reconciliation is
    a spec 0011 open question, not scaffold work.
    """
    label = backend.get_label(clip_id, "operator")
    return label.verdict if label else None


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def create_app(config: Config | None = None) -> FastAPI:
    if config is None:
        config = Config.from_env()

    config.data_dir.mkdir(parents=True, exist_ok=True)
    backend = _make_backend(config)
    clip_store = ClipStore(config.data_dir / "clips")
    engines = _default_engines(config)
    supervisor = DetectorSupervisor(
        backend=backend,
        clip_store=clip_store,
        pre_roll_ms=config.pre_roll_ms,
    )

    link_store = LinkStore[_NoExtension](
        config.data_dir,
        extension_from_dict=_ext_from,
        extension_to_dict=_ext_to,
    )

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        app.state.backend = backend
        app.state.clip_store = clip_store
        app.state.link_store = link_store
        app.state.engines = engines
        app.state.supervisor = supervisor
        app.state.config = config
        yield
        await backend.close()

    app = FastAPI(
        title="Conduit Excita",
        description="Wake-word ops: label, debug, train, configure (spec 0011).",
        version="0.1.0",
        lifespan=lifespan,
    )

    link_config = LinkConfig(
        service_kind=LinkedServiceKind.EXCITA,
        peer_name="excita",
        peer_base_url=config.base_url,
        panel=LinkedServicePanel(title="Excita", path="/ui/", icon="waveform"),
        storage_dir=config.data_dir,
    )
    app.include_router(
        make_link_router(
            config=link_config,
            store=link_store,
            client=HttpConduitLinkClient(),
            build_create_body=_build_create_body,
            build_extension=_build_extension,
            public_response=_public,
        )
    )

    @app.get("/health")
    async def health_check() -> HealthResponse:
        return HealthResponse(
            status="ok",
            backend=config.backend_type,
            linked=link_store.load() is not None,
        )

    @app.get("/ready")
    async def ready_check() -> ReadyResponse:
        return ReadyResponse(status="ok", backend_ready=True)

    # --- phrases ---

    @app.get("/phrases")
    async def list_phrases() -> list[PhraseOut]:
        return [
            PhraseOut(**p.__dict__) for p in backend.list_phrases()
        ]

    @app.post("/phrases", status_code=201)
    async def create_phrase(body: PhraseIn) -> PhraseOut:
        name = body.name.strip()
        if not name:
            raise HTTPException(422, "name must not be blank")
        phrase = Phrase(
            id=new_id(),
            name=name,
            display_label=body.display_label.strip() or name,
            language=body.language,
        )
        try:
            backend.insert_phrase(phrase)
        except Exception as error:
            raise HTTPException(409, f"phrase exists: {name}") from error
        return PhraseOut(**phrase.__dict__)

    # --- clips ---

    @app.post("/clips", status_code=201)
    async def upload_clip(
        request: Request,
        phrase_id: str = Form(...),
        file: UploadFile = File(...),
    ) -> ClipOut:
        if backend.get_phrase(phrase_id) is None:
            raise HTTPException(404, f"phrase not found: {phrase_id}")

        mime_type = file.content_type or "application/octet-stream"
        data = await file.read()
        if not data:
            raise HTTPException(422, "empty upload")

        try:
            digest, stored_path = clip_store.store(data, mime_type)
        except UnsupportedMimeError as error:
            raise HTTPException(415, str(error)) from error

        # Dedup within the phrase — same audio uploaded twice returns the
        # existing row rather than a 409. Rationale: the operator is often
        # sanity-checking that a clip already made it in, and reflecting the
        # existing id is more useful than an error.
        existing = backend.get_clip_by_sha256(phrase_id, digest)
        if existing is not None:
            return _clip_out(backend, existing)

        source = request.headers.get("x-excita-source", "upload").lower()
        if source not in {"upload", "browser", "detector"}:
            source = "upload"

        meta = _wav_metadata(data) if mime_type.startswith("audio/wav") or mime_type == "audio/x-wav" else None
        sample_rate, duration_ms = meta if meta else (0, 0)

        clip = Clip(
            id=new_id(),
            phrase_id=phrase_id,
            sample_rate=sample_rate,
            duration_ms=duration_ms,
            source=source,
            source_peer=None,
            sha256=digest,
            mime_type=mime_type,
            stored_path=str(stored_path),
            created_at=_now_iso(),
        )
        backend.insert_clip(clip)
        return _clip_out(backend, clip)

    @app.get("/clips")
    async def list_clips(
        phrase_id: str | None = None,
        verdict: str | None = None,
        limit: int = 100,
    ) -> list[ClipOut]:
        if verdict is not None and verdict not in {
            "positive", "negative", "ambiguous", "discard", "unlabeled",
        }:
            raise HTTPException(422, f"invalid verdict filter: {verdict}")
        return [
            _clip_out(backend, c)
            for c in backend.list_clips(phrase_id, verdict, limit)
        ]

    @app.get("/clips/{clip_id}")
    async def get_clip(clip_id: str) -> ClipOut:
        clip = backend.get_clip(clip_id)
        if clip is None:
            raise HTTPException(404, f"clip not found: {clip_id}")
        return _clip_out(backend, clip)

    @app.get("/clips/{clip_id}/audio")
    async def get_clip_audio(clip_id: str) -> Response:
        clip = backend.get_clip(clip_id)
        if clip is None:
            raise HTTPException(404, f"clip not found: {clip_id}")
        data = clip_store.read(clip.stored_path)
        return Response(content=data, media_type=clip.mime_type)

    @app.post("/clips/{clip_id}/label")
    async def label_clip(clip_id: str, body: LabelIn) -> LabelOut:
        if body.verdict not in {"positive", "negative", "ambiguous", "discard"}:
            raise HTTPException(422, f"invalid verdict: {body.verdict}")
        if body.split is not None and body.split not in {"train", "val", "test"}:
            raise HTTPException(422, f"invalid split: {body.split}")
        if backend.get_clip(clip_id) is None:
            raise HTTPException(404, f"clip not found: {clip_id}")

        label = Label(
            clip_id=clip_id,
            verdict=body.verdict,
            labeller=body.labeller,
            split=body.split,
            notes=body.notes,
            labelled_at=_now_iso(),
        )
        backend.upsert_label(label)
        return LabelOut(**label.__dict__)

    # --- detection surface (spec 0011 §Runtime detection loop) ---

    @app.get("/detectors")
    async def list_detectors() -> list[DetectorOut]:
        return [DetectorOut(**row) for row in bindings_view(supervisor.list_bindings())]

    @app.post("/detectors", status_code=201)
    async def arm_detector(body: ArmDetectorIn) -> DetectorOut:
        try:
            kind = EngineKind(body.engine)
        except ValueError as error:
            raise HTTPException(422, f"unknown engine: {body.engine}") from error
        if backend.get_phrase(body.phrase_id) is None:
            raise HTTPException(404, f"phrase not found: {body.phrase_id}")
        engine = engines[kind]
        try:
            detector = engine.load(body.model_ref, body.phrase_id, threshold=body.threshold) \
                if kind is EngineKind.OPENWAKEWORD \
                else engine.load(body.model_ref, body.phrase_id)  # type: ignore[call-arg]
        except NotSupportedError as error:
            # An engine slot that stayed `NullEngine` at boot is what
            # happens when its model files are missing. Reporting the
            # engine's own message keeps the operator's diagnostic honest
            # (spec 0011: honest gap, not a stub).
            raise HTTPException(501, str(error)) from error
        except FileNotFoundError as error:
            raise HTTPException(404, str(error)) from error
        except Exception as error:  # noqa: BLE001
            raise HTTPException(500, f"engine load failed: {error}") from error
        binding = supervisor.arm(
            phrase_id=body.phrase_id,
            model_ref=body.model_ref,
            source_device=body.source_device,
            detector=detector,
        )
        return DetectorOut(**bindings_view([binding])[0])

    @app.delete("/detectors/{detector_id}", status_code=204)
    async def disarm_detector(detector_id: str) -> Response:
        if not supervisor.disarm(detector_id):
            raise HTTPException(404, f"detector not armed: {detector_id}")
        return Response(status_code=204)

    @app.post("/detectors/{detector_id}/reset", status_code=204)
    async def reset_detector(detector_id: str) -> Response:
        if not supervisor.reset(detector_id):
            raise HTTPException(404, f"detector not armed: {detector_id}")
        return Response(status_code=204)

    @app.post("/v1/audio/{source_device}/frames", status_code=202)
    async def ingest_frame(source_device: str, request: Request) -> dict[str, object]:
        body = await request.body()
        if not body:
            raise HTTPException(422, "empty frame")
        if len(body) % 2 != 0:
            # int16 mono contract on the wire — an odd-length frame means
            # the sender is speaking a different codec, and silently
            # trimming would delay the diagnostic to the score curve.
            raise HTTPException(422, "frame length not a multiple of 2 (int16 mono)")
        fires = supervisor.feed(source_device, body)
        return {"accepted": True, "fires": len(fires)}

    @app.get("/v1/wake-events/recent")
    async def recent_wake_events(limit: int = 64) -> list[WakeEventOut]:
        limit = max(1, min(limit, 256))
        return [
            WakeEventOut(
                detector_id=e.detector_id,
                phrase_id=e.phrase_id,
                source_device=e.source_device,
                confidence=e.confidence,
                detected_at=e.detected_at,
                audio_clip_id=e.audio_clip_id,
            )
            for e in supervisor.recent_events(limit)
        ]

    static_dir = Path(__file__).parent / "static"
    if static_dir.exists():
        app.mount("/ui", StaticFiles(directory=str(static_dir), html=True), name="ui")

    @app.get("/")
    async def root() -> RedirectResponse:
        return RedirectResponse(url="/ui/")

    return app


def _clip_out(backend: Backend, clip: Clip) -> ClipOut:
    return ClipOut(
        id=clip.id,
        phrase_id=clip.phrase_id,
        sample_rate=clip.sample_rate,
        duration_ms=clip.duration_ms,
        source=clip.source,
        source_peer=clip.source_peer,
        sha256=clip.sha256,
        mime_type=clip.mime_type,
        created_at=clip.created_at,
        verdict=_verdict_of(backend, clip.id),
    )


if __name__ == "__main__":
    import uvicorn

    logging.basicConfig(level=logging.INFO)
    uvicorn.run(create_app(), host="0.0.0.0", port=DEFAULT_PORT)
