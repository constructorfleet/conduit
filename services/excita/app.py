"""Conduit Excita — the wake-word operations service.

Per spec 0011 and its µWW / nWW extension (#213). Ships with:

- `POST /phrases`, `GET /phrases`, `GET /phrases/{id}` (models across engines)
- `POST /clips` (multipart upload; browser record uses the same endpoint)
- `GET /clips` filtered by phrase and verdict (including `unlabeled`)
- `POST /clips/{id}/label`, `GET /clips/{id}/audio` for playback
- `POST /models/import` + filesystem scanner over `EXCITA_MODEL_IMPORT_DIR`
  (`GET`/`DELETE /models`, `POST /models/scan`)
- `GET /engines` — capability matrix per engine (ADR-0020)
- Engine dispatch with structured 501s for capability gaps (ADR-0023):
  `POST /detectors` (load), `POST /debug/score` (score), `POST /train`
  (train), deploy-target publish (package)
- `POST /deploy_targets` + publish through `file`, `http_push`,
  `linked_service_config`
- `GET /health` (link-health) and `GET /ready`
- `/link` router from `conduit-link` (0005/0010 shape)
"""

from __future__ import annotations

import json
import logging
import os
import re
import wave
from contextlib import asynccontextmanager
from datetime import datetime, timezone
from io import BytesIO
from pathlib import Path

import httpx
from fastapi import FastAPI, File, Form, HTTPException, Request, UploadFile
from fastapi.responses import JSONResponse, RedirectResponse, Response
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

from .backend import (
    Backend,
    Clip,
    DeployTarget,
    Label,
    Model,
    Phrase,
    SqliteBackend,
    new_id,
)
from .clip_store import ClipStore, UnsupportedMimeError
from .model_import import ModelImporter
from .engines import (
    EngineKind,
    MicroWakeWordEngine,
    NanoWakeWordEngine,
    NotSupportedError,
    NullEngine,
    OpenWakeWordEngine,
    WakeWordEngine,
    capability_view,
    gap_reason,
)
from .supervisor import DetectorSupervisor, bindings_view

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
    # Bind-mounted directory of pre-trained models scanned on boot and on
    # SIGHUP (#213 §Model import). Unset disables filesystem import.
    model_import_dir: Path | None = None

    @classmethod
    def from_env(cls) -> "Config":
        data_dir = Path(os.getenv("EXCITA_DATA_DIR", "/data"))
        wake_env = os.getenv("EXCITA_WAKE_MODELS_DIR")
        wake_dir = Path(wake_env) if wake_env else data_dir / "wake-models"
        import_env = os.getenv("EXCITA_MODEL_IMPORT_DIR")
        return cls(
            data_dir=data_dir,
            backend_type=os.getenv("EXCITA_BACKEND", "sqlite"),
            base_url=os.getenv("EXCITA_BASE_URL", f"http://localhost:{DEFAULT_PORT}"),
            wake_models_dir=wake_dir,
            pre_roll_ms=int(os.getenv("EXCITA_PREROLL_MS", "2000")),
            model_import_dir=Path(import_env) if import_env else None,
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


class ModelOut(BaseModel):
    id: str
    phrase_id: str
    engine: str
    version: str
    engine_phrase_key: str | None
    source: str
    filesystem_path: str | None
    artifact_path: str
    metrics: dict[str, object]
    notes: str | None
    created_at: str


class PhraseDetailOut(PhraseOut):
    """A phrase plus its models across every engine (ADR-0022)."""

    models: list[ModelOut]


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


class TrainIn(BaseModel):
    phrase_id: str
    engine: str
    base_model_id: str | None = None


class DebugScoreIn(BaseModel):
    clip_id: str
    model_id: str | None = None  # null = every active model on the clip's phrase


class ScoreResultOut(BaseModel):
    model_id: str
    engine: str
    curve: list[float]


class DeployTargetIn(BaseModel):
    kind: str
    config: dict[str, object]


class PublishIn(BaseModel):
    model_id: str


class DeployTargetOut(BaseModel):
    id: str
    kind: str
    config: dict[str, object]
    current_model_id: str | None
    last_publish_at: str | None
    last_publish_status: str | None
    last_publish_error: str | None
    created_at: str


class WakeEventOut(BaseModel):
    """Local ring-buffer entry (spec 0011 §Standalone posture)."""

    detector_id: str
    phrase_id: str
    source_device: str
    confidence: float
    detected_at: str
    audio_clip_id: str | None


def _default_engines(config: Config) -> dict[EngineKind, WakeWordEngine]:
    """Real engine where the runtime is a hard dep, `NullEngine` otherwise.

    nanoWakeWord and microWakeWord adapters are real unconditionally —
    their packages ship in the image (#213 §Dependencies: one image,
    one behavior). openWakeWord gets a real adapter iff its two shared
    ONNX models are present at boot; when they're not, the slot stays a
    `NullEngine` so the API answers with an honest gap rather than a 404
    or a crash. Porcupine has no adapter yet.
    """
    engines: dict[EngineKind, WakeWordEngine] = {
        EngineKind.MICROWAKEWORD: MicroWakeWordEngine(),
        EngineKind.NANOWAKEWORD: NanoWakeWordEngine(),
    }
    for kind in (EngineKind.OPENWAKEWORD, EngineKind.PORCUPINE):
        engines[kind] = NullEngine(kind)
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


def _capability_missing(
    kind: EngineKind, capability: str, error: NotSupportedError
) -> JSONResponse:
    """Structured 501 body for engine capability gaps (ADR-0023).

    The frontend keys on `code` and renders `message` as a tooltip; it
    never parses error text to figure out what an engine can't do. The
    body carries only the authored reason sentences from
    `engines.base.gap_reason` — exception internals stay in the server
    log, never in a response.
    """
    LOG.info(
        "engine capability gap engine=%s capability=%s detail=%s",
        kind.value,
        capability,
        error,
    )
    return JSONResponse(
        status_code=501,
        content={
            "code": "engine_capability_missing",
            "engine": kind.value,
            "capability": capability,
            "message": gap_reason(kind, capability),
        },
    )


def create_app(config: Config | None = None) -> FastAPI:
    if config is None:
        config = Config.from_env()

    config.data_dir.mkdir(parents=True, exist_ok=True)
    backend = _make_backend(config)
    clip_store = ClipStore(config.data_dir / "clips")
    models_dir = config.data_dir / "models"
    importer = (
        ModelImporter(backend, config.model_import_dir)
        if config.model_import_dir is not None
        else None
    )
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
        if importer is not None:
            # Boot scan: a first-boot deployment comes up with usable wake
            # words before an operator ever opens the UI (#213).
            result = importer.scan()
            LOG.info(
                "model import scan imported=%d updated=%d removed=%d errors=%d",
                len(result.imported_ids), len(result.updated_ids),
                len(result.removed_ids), result.errors,
            )
        yield
        await backend.close()

    app = FastAPI(
        title="Conduit Excita",
        description="Wake-word ops: label, debug, train, configure (spec 0011).",
        version="0.1.0",
        lifespan=lifespan,
    )
    # Set outside the lifespan too so `python -m excita.app` can reach the
    # importer for its SIGHUP handler before uvicorn starts serving.
    app.state.model_importer = importer

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

    @app.get("/phrases/{phrase_id}")
    async def get_phrase(phrase_id: str) -> PhraseDetailOut:
        """Phrase detail with its models across every engine — the
        cross-engine comparison view is the point of the tool (ADR-0022)."""
        phrase = backend.get_phrase(phrase_id)
        if phrase is None:
            raise HTTPException(404, f"phrase not found: {phrase_id}")
        return PhraseDetailOut(
            **phrase.__dict__,
            models=[
                _model_out(m) for m in backend.list_models(phrase_id=phrase_id)
            ],
        )

    # --- models (#213 §Model import / §Data model) ---

    @app.get("/models")
    async def list_models(
        phrase_id: str | None = None,
        source: str | None = None,
    ) -> list[ModelOut]:
        if source is not None and source not in {"upload", "filesystem"}:
            raise HTTPException(422, f"invalid source filter: {source}")
        return [
            _model_out(m) for m in backend.list_models(phrase_id=phrase_id, source=source)
        ]

    @app.post("/models/import", status_code=201)
    async def import_model(
        metadata: str = Form(...),
        file: UploadFile = File(...),
    ) -> ModelOut:
        try:
            meta = json.loads(metadata)
        except json.JSONDecodeError as error:
            raise HTTPException(422, f"metadata is not valid JSON: {error}") from error
        if not isinstance(meta, dict):
            raise HTTPException(422, "metadata must be a JSON object")

        engine_value = meta.get("engine")
        try:
            engine_kind = EngineKind(engine_value)
        except ValueError as error:
            raise HTTPException(422, f"unknown engine: {engine_value}") from error

        phrase_name = str(meta.get("phrase_name") or "").strip()
        if not phrase_name:
            raise HTTPException(422, "phrase_name must not be blank")
        version = str(meta.get("version") or "").strip()
        if not version:
            raise HTTPException(422, "version must not be blank")

        metrics = meta.get("metrics_json") or {}
        if not isinstance(metrics, dict):
            raise HTTPException(422, "metrics_json must be a JSON object")
        notes = meta.get("notes")
        if notes is not None and not isinstance(notes, str):
            raise HTTPException(422, "notes must be a string")

        data = await file.read()
        if not data:
            raise HTTPException(422, "empty upload")

        filename = Path(file.filename or "").name
        if not filename:
            raise HTTPException(422, "filename must not be blank")

        phrase = backend.get_phrase_by_name(phrase_name)
        if phrase is None:
            phrase = Phrase(
                id=new_id(), name=phrase_name,
                display_label=phrase_name, language="en",
            )
            try:
                backend.insert_phrase(phrase)
            except Exception as error:
                raise HTTPException(409, f"phrase exists: {phrase_name}") from error

        models_dir.mkdir(parents=True, exist_ok=True)
        artifact_path = models_dir / filename
        if artifact_path.exists():
            # Excita owns the artifact going forward — never overwrite.
            artifact_path = models_dir / f"{new_id()}-{filename}"
        artifact_path.write_bytes(data)

        engine_phrase_key = meta.get("engine_phrase_key")
        model = Model(
            id=new_id(),
            phrase_id=phrase.id,
            engine=engine_kind.value,
            version=version,
            engine_phrase_key=(
                engine_phrase_key
                if isinstance(engine_phrase_key, str) and engine_phrase_key
                else artifact_path.stem
            ),
            source="upload",
            filesystem_path=None,
            artifact_path=str(artifact_path),
            metrics_json=json.dumps(metrics),
            notes=notes,
            file_mtime=None,
            file_size=len(data),
            created_at=_now_iso(),
            deleted_at=None,
        )
        try:
            backend.insert_model(model)
        except Exception as error:
            artifact_path.unlink(missing_ok=True)
            raise HTTPException(
                409,
                f"model exists for (phrase, engine, version): "
                f"{phrase_name}/{engine_kind.value}/{version}",
            ) from error

        # Sidecar next to the artifact: copying it into the scanner mount
        # promotes this model to a filesystem-imported one without a
        # rewrite step (#213 story 15).
        sidecar_path = artifact_path.with_name(artifact_path.name + ".excita.json")
        sidecar_path.write_text(json.dumps({
            "engine": engine_kind.value,
            "phrase_name": phrase_name,
            "version": version,
            "engine_phrase_key": model.engine_phrase_key,
            "metrics_json": metrics,
            "notes": notes,
        }))
        return _model_out(model)

    @app.get("/models/{model_id}")
    async def get_model(model_id: str) -> ModelOut:
        model = backend.get_model(model_id)
        if model is None:
            raise HTTPException(404, f"model not found: {model_id}")
        return _model_out(model)

    @app.delete("/models/{model_id}", status_code=204)
    async def delete_model(model_id: str) -> Response:
        model = backend.get_model(model_id)
        if model is None:
            raise HTTPException(404, f"model not found: {model_id}")
        if model.source == "filesystem":
            # ADR-0021: the volume is the source of truth. UI-deleting would
            # let the next scan resurrect it; retiring means removing the
            # file on disk.
            return JSONResponse(
                status_code=409,
                content={
                    "code": "filesystem_imported_read_only",
                    "message": (
                        "filesystem-imported models cannot be deleted through "
                        "the API; remove the file from the import mount instead"
                    ),
                    "filesystem_path": model.filesystem_path,
                },
            )
        backend.soft_delete_model(model_id)
        return Response(status_code=204)

    @app.post("/models/scan")
    async def scan_models() -> dict[str, object]:
        if importer is None:
            raise HTTPException(409, "EXCITA_MODEL_IMPORT_DIR is not configured")
        result = importer.scan()
        return {
            "imported_ids": result.imported_ids,
            "updated_ids": result.updated_ids,
            "removed_ids": result.removed_ids,
            "errors": result.errors,
        }

    # --- debug scoring (spec 0011 §Debug) ---

    @app.post("/debug/score",
              responses={501: {"description": "engine capability missing"}})
    async def debug_score(body: DebugScoreIn) -> list[ScoreResultOut]:
        clip = backend.get_clip(body.clip_id)
        if clip is None:
            raise HTTPException(404, f"clip not found: {body.clip_id}")
        if body.model_id is not None:
            model = backend.get_model(body.model_id)
            if model is None:
                raise HTTPException(404, f"model not found: {body.model_id}")
            targets = [model]
        else:
            # No model named → every active model on the clip's phrase, so
            # a cross-engine regression check is one call (#213 story 8).
            targets = backend.list_models(phrase_id=clip.phrase_id)
            if not targets:
                raise HTTPException(
                    404, f"no active models registered for phrase: {clip.phrase_id}"
                )

        audio = clip_store.read(clip.stored_path)
        results: list[ScoreResultOut] = []
        for model in targets:
            kind = EngineKind(model.engine)
            engine = engines[kind]
            try:
                curve = engine.score(audio, model.artifact_path)
            except NotSupportedError as error:
                return _capability_missing(kind, "score", error)
            except FileNotFoundError as error:
                raise HTTPException(404, str(error)) from error
            except ValueError as error:
                raise HTTPException(422, str(error)) from error
            results.append(ScoreResultOut(
                model_id=model.id, engine=model.engine, curve=curve,
            ))
        return results

    # --- deploy targets (spec 0011 §Configure & publish, #213) ---

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

    # --- engines (#213 §Capability contract) ---

    @app.get("/engines")
    async def list_engines() -> list[dict[str, object]]:
        """Capability matrix so the UI grays out unsupported controls
        before the operator clicks them — the 501s are the belt to this
        suspenders (ADR-0023 §Consequences)."""
        return [capability_view(engine) for engine in engines.values()]

    @app.post("/train",
              response_model=None,
              responses={501: {"description": "engine capability missing"}})
    async def train(body: TrainIn) -> dict[str, object] | JSONResponse:
        try:
            kind = EngineKind(body.engine)
        except ValueError as error:
            raise HTTPException(422, f"unknown engine: {body.engine}") from error
        if backend.get_phrase(body.phrase_id) is None:
            raise HTTPException(404, f"phrase not found: {body.phrase_id}")
        engine = engines[kind]
        try:
            job_id = engine.train(f"{body.phrase_id}:{_now_iso()}", body.base_model_id)
        except NotSupportedError as error:
            return _capability_missing(kind, "train", error)
        return {"job_id": job_id}

    # --- detection surface (spec 0011 §Runtime detection loop) ---

    @app.get("/detectors")
    async def list_detectors() -> list[DetectorOut]:
        return [DetectorOut(**row) for row in bindings_view(supervisor.list_bindings())]

    @app.post("/detectors", status_code=201,
              response_model=None,
              responses={501: {"description": "engine capability missing"}})
    async def arm_detector(body: ArmDetectorIn) -> DetectorOut | JSONResponse:
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
            # A capability gap (null slot or partial adapter — ADR-0020)
            # is "the server cannot ever fulfil this", not a bad payload:
            # structured 501 per ADR-0023.
            return _capability_missing(kind, "load", error)
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

    @app.get("/deploy_targets")
    async def list_deploy_targets() -> list[DeployTargetOut]:
        return [_target_out(t) for t in backend.list_deploy_targets()]

    @app.get("/deploy_targets/{target_id}")
    async def get_deploy_target(target_id: str) -> DeployTargetOut:
        target = backend.get_deploy_target(target_id)
        if target is None:
            raise HTTPException(404, f"deploy target not found: {target_id}")
        return _target_out(target)

    @app.post("/deploy_targets", status_code=201)
    async def create_deploy_target(body: DeployTargetIn) -> DeployTargetOut:
        if body.kind not in {"file", "http_push", "linked_service_config"}:
            raise HTTPException(422, f"unknown deploy target kind: {body.kind}")
        required = {"file": "directory", "http_push": "url"}
        missing_key = required.get(body.kind)
        if missing_key and not str(body.config.get(missing_key) or "").strip():
            raise HTTPException(
                422, f"deploy target kind '{body.kind}' requires config.{missing_key}"
            )
        target = DeployTarget(
            id=new_id(),
            kind=body.kind,
            config_json=json.dumps(body.config),
            current_model_id=None,
            last_publish_at=None,
            last_publish_status=None,
            last_publish_error=None,
            created_at=_now_iso(),
        )
        backend.insert_deploy_target(target)
        return _target_out(target)

    @app.post("/deploy_targets/{target_id}/publish",
              responses={501: {"description": "engine capability missing"}})
    async def publish_to_deploy_target(target_id: str, body: PublishIn) -> DeployTargetOut:
        """Publishing is a single row change; the push outcome is recorded
        beside the selection and never rolls it back (spec 0011)."""
        target = backend.get_deploy_target(target_id)
        if target is None:
            raise HTTPException(404, f"deploy target not found: {target_id}")
        model = backend.get_model(body.model_id)
        if model is None:
            raise HTTPException(404, f"model not found: {body.model_id}")

        kind = EngineKind(model.engine)
        engine = engines[kind]
        try:
            native_target = _native_target(engine)
            bundle = engine.package(model.artifact_path, native_target)
        except NotSupportedError as error:
            return _capability_missing(kind, "package", error)

        status, error = _dispatch_package(
            target_kind=target.kind,
            config=json.loads(target.config_json),
            bundle=bundle,
            engine=model.engine,
            phrase=backend.get_phrase(model.phrase_id),
            version=model.version,
            file_ext=_PACKAGE_EXTENSIONS.get(native_target, ".bin"),
        )
        at = _now_iso()
        backend.record_publish(target.id, model.id, status, error, at)
        return _target_out(backend.get_deploy_target(target.id))  # type: ignore[arg-type]

    static_dir = Path(__file__).parent / "static"
    if static_dir.exists():
        app.mount("/ui", StaticFiles(directory=str(static_dir), html=True), name="ui")

    @app.get("/")
    async def root() -> RedirectResponse:
        return RedirectResponse(url="/ui/")

    return app


def _model_out(model: Model) -> ModelOut:
    try:
        metrics = json.loads(model.metrics_json)
    except json.JSONDecodeError:
        LOG.warning(
            "model has unparsable metrics_json; surfacing empty id=%s", model.id
        )
        metrics = {}
    return ModelOut(
        id=model.id,
        phrase_id=model.phrase_id,
        engine=model.engine,
        version=model.version,
        engine_phrase_key=model.engine_phrase_key,
        source=model.source,
        filesystem_path=model.filesystem_path,
        artifact_path=model.artifact_path,
        metrics=metrics if isinstance(metrics, dict) else {},
        notes=model.notes,
        created_at=model.created_at,
    )


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


def _native_target(engine: WakeWordEngine) -> str:
    targets = getattr(engine, "package_targets", ())
    if not targets:
        raise NotSupportedError(
            f"{engine.kind.value}: no package target declared"
        )
    return targets[0]


_PACKAGE_EXTENSIONS = {
    "tflite_micro": ".tflite",
    "onnx": ".onnx",
}


def _dispatch_package(
    *,
    target_kind: str,
    config: dict[str, object],
    bundle: bytes,
    engine: str,
    phrase: Phrase | None,
    version: str,
    file_ext: str,
) -> tuple[str, str | None]:
    """Push packaged bytes through one of the three transports.

    Returns `(status, error)` — 'ok' or 'failed'. A failed push is
    recorded on the row, never rolled back (spec 0011, at-least-once).
    """
    phrase_name = phrase.name if phrase else "unknown"
    slug = re.sub(r"[^a-z0-9]+", "-", phrase_name.lower()).strip("-") or "model"

    if target_kind == "file":
        directory = Path(str(config.get("directory") or ""))
        try:
            directory.mkdir(parents=True, exist_ok=True)
            (directory / f"{slug}-{version}{file_ext}").write_bytes(bundle)
        except OSError as err:
            return "failed", f"could not write package: {err}"
        return "ok", None

    if target_kind == "http_push":
        url = str(config.get("url") or "")
        try:
            response = httpx.post(
                url,
                content=bundle,
                headers={
                    "Content-Type": "application/octet-stream",
                    "X-Excita-Engine": engine,
                    "X-Excita-Phrase": phrase_name,
                    "X-Excita-Version": version,
                },
                timeout=10.0,
            )
        except httpx.HTTPError as err:
            return "failed", f"push to {url} failed: {err}"
        if response.is_success:
            return "ok", None
        return "failed", f"push to {url} returned HTTP {response.status_code}"

    # linked_service_config: the linked service pulls its wake-word
    # configuration from Excita's deploy-target view (over the conduit-link
    # channel), so the current_model_id row change IS the publish.
    return "ok", None


def _target_out(target: DeployTarget) -> DeployTargetOut:
    try:
        config = json.loads(target.config_json)
    except json.JSONDecodeError:
        config = {}
    return DeployTargetOut(
        id=target.id,
        kind=target.kind,
        config=config if isinstance(config, dict) else {},
        current_model_id=target.current_model_id,
        last_publish_at=target.last_publish_at,
        last_publish_status=target.last_publish_status,
        last_publish_error=target.last_publish_error,
        created_at=target.created_at,
    )


if __name__ == "__main__":
    import signal

    import uvicorn

    application = create_app()

    def _on_sighup(_signum: int, _frame: object) -> None:
        """Re-scan the model import mount without a restart (#213)."""
        scanner = getattr(application.state, "model_importer", None)
        if scanner is None:
            return
        result = scanner.scan()
        LOG.info(
            "SIGHUP model import scan imported=%d updated=%d removed=%d errors=%d",
            len(result.imported_ids), len(result.updated_ids),
            len(result.removed_ids), result.errors,
        )

    signal.signal(signal.SIGHUP, _on_sighup)
    logging.basicConfig(level=logging.INFO)
    uvicorn.run(application, host="0.0.0.0", port=DEFAULT_PORT)
