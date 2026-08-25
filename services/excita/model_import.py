"""Filesystem model-import scanner (#213 §Model import).

Watches `EXCITA_MODEL_IMPORT_DIR` — the bind-mounted directory operators
drop pre-trained models into. Each artifact is described by an
`<artifact>.excita.json` sidecar next to it:

    {"engine": "microwakeword", "phrase_name": "hey jarvis",
     "version": "v3", "engine_phrase_key": "hey_jarvis_v3"}

Reconciliation rules (the volume is the source of truth, ADR-0021):

- new sidecar → insert a `source=filesystem` model row
- file gone   → soft-delete the row (`deleted_at`)
- file present, sidecar `version` unchanged → update-in-place metadata
  refresh only; never a new row (a `cp -p` round trip must not mint a
  spurious model)
- sidecar `version` changed → insert a new row; the previous row keeps
  its metrics and deploy history
- malformed or missing sidecar → log and skip; nothing partially registers

Phrase names resolve through ADR-0022: a sidecar naming an unknown phrase
creates the phrase row — phrases are shared across engines.
"""

from __future__ import annotations

import json
import logging
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

from .backend import Backend, Model, Phrase, new_id
from .engines import EngineKind

LOG = logging.getLogger("excita.model_import")

SIDECAR_SUFFIX = ".excita.json"


@dataclass
class ScanResult:
    imported_ids: list[str] = field(default_factory=list)
    updated_ids: list[str] = field(default_factory=list)
    removed_ids: list[str] = field(default_factory=list)
    errors: int = 0


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


class ModelImporter:
    """Reconciles the import mount against `model` rows."""

    def __init__(self, backend: Backend, import_dir: Path) -> None:
        self._backend = backend
        self._import_dir = Path(import_dir)

    def scan(self) -> ScanResult:
        result = ScanResult()
        seen_paths: set[str] = set()

        if not self._import_dir.exists():
            LOG.warning(
                "model import dir does not exist; nothing to scan path=%s",
                self._import_dir,
            )
            return self._reap(result, seen_paths)

        for sidecar_path in sorted(self._import_dir.rglob(f"*{SIDECAR_SUFFIX}")):
            parsed = self._parse_sidecar(sidecar_path)
            if parsed is None:
                result.errors += 1
                continue
            artifact_path, meta = parsed
            rel_path = artifact_path.relative_to(self._import_dir).as_posix()
            seen_paths.add(rel_path)
            stat = artifact_path.stat()
            mtime = datetime.fromtimestamp(stat.st_mtime, timezone.utc).isoformat(
                timespec="seconds"
            )

            existing = self._backend.get_filesystem_model(rel_path, meta["version"])
            if existing is None:
                # A sidecar whose (phrase, engine, version) matches a
                # previously *uploaded* model is that model promoted: the
                # operator copied it into the mount. Convert in place so
                # metrics and deploy history survive (user story 15).
                phrase = self._resolve_phrase(meta["phrase_name"])
                promoted = False
                if phrase is not None:
                    candidate = self._backend.get_model_by_version(
                        phrase.id, meta["engine"], meta["version"]
                    )
                    if candidate is not None and candidate.source == "upload":
                        self._backend.promote_upload_model(
                            candidate.id, rel_path, mtime, stat.st_size
                        )
                        result.imported_ids.append(candidate.id)
                        promoted = True
                if not promoted:
                    model_id = self._insert_model(
                        artifact_path, rel_path, meta, mtime, stat.st_size
                    )
                    if model_id is not None:
                        result.imported_ids.append(model_id)
                    else:
                        result.errors += 1
            elif existing.deleted_at is not None:
                # The file came back (restore, re-mount). Resurrect rather
                # than collide with its own UNIQUE(phrase, engine, version).
                self._backend.resurrect_model(existing.id, mtime, stat.st_size)
                result.imported_ids.append(existing.id)
            else:
                before = (existing.file_mtime, existing.file_size)
                after = (mtime, stat.st_size)
                if before != after:
                    self._backend.touch_model(existing.id, mtime, stat.st_size)
                    result.updated_ids.append(existing.id)

        return self._reap(result, seen_paths)

    # --- internals ---

    def _parse_sidecar(self, sidecar_path: Path) -> tuple[Path, dict] | None:
        """Returns `(artifact_path, normalized_meta)`, or None on any problem.

        Malformed JSON, missing required fields, unknown engines, and
        artifacts without bytes all land here — logged and skipped, never
        partially registered.
        """
        try:
            raw = json.loads(sidecar_path.read_text())
        except (OSError, json.JSONDecodeError) as error:
            LOG.error("unreadable model sidecar path=%s error=%s", sidecar_path, error)
            return None
        if not isinstance(raw, dict):
            LOG.error("model sidecar is not an object path=%s", sidecar_path)
            return None

        engine = raw.get("engine")
        phrase_name = raw.get("phrase_name")
        version = raw.get("version")
        if (
            not isinstance(engine, str)
            or engine not in {k.value for k in EngineKind}
            or not isinstance(phrase_name, str)
            or not phrase_name.strip()
            or not isinstance(version, str)
            or not version.strip()
        ):
            LOG.error(
                "model sidecar missing/invalid required fields "
                "(engine, phrase_name, version) path=%s",
                sidecar_path,
            )
            return None

        artifact_path = sidecar_path.with_name(
            sidecar_path.name[: -len(SIDECAR_SUFFIX)]
        )
        if not artifact_path.is_file():
            LOG.error(
                "sidecar has no artifact next to it sidecar=%s artifact=%s",
                sidecar_path,
                artifact_path,
            )
            return None

        metrics = raw.get("metrics_json") or {}
        notes = raw.get("notes")
        return artifact_path, {
            "engine": engine,
            "phrase_name": phrase_name.strip(),
            "version": version.strip(),
            "engine_phrase_key": raw.get("engine_phrase_key"),
            "metrics": metrics if isinstance(metrics, dict) else {},
            "notes": notes if isinstance(notes, str) else None,
        }

    def _resolve_phrase(self, name: str) -> Phrase | None:
        phrase = self._backend.get_phrase_by_name(name)
        if phrase is not None:
            return phrase
        phrase = Phrase(id=new_id(), name=name, display_label=name, language="en")
        try:
            self._backend.insert_phrase(phrase)
        except Exception as error:  # noqa: BLE001 — raced creation is fine, re-read below
            LOG.warning("phrase insert raced during import name=%s error=%s", name, error)
            return self._backend.get_phrase_by_name(name)
        return phrase

    def _insert_model(
        self,
        artifact_path: Path,
        rel_path: str,
        meta: dict,
        mtime: str,
        size: int,
    ) -> str | None:
        phrase = self._resolve_phrase(meta["phrase_name"])
        if phrase is None:
            LOG.error("could not resolve phrase during scan name=%s", meta["phrase_name"])
            return None
        engine_phrase_key = meta["engine_phrase_key"]
        model = Model(
            id=new_id(),
            phrase_id=phrase.id,
            engine=meta["engine"],
            version=meta["version"],
            engine_phrase_key=(
                engine_phrase_key
                if isinstance(engine_phrase_key, str) and engine_phrase_key
                else artifact_path.stem
            ),
            source="filesystem",
            filesystem_path=rel_path,
            artifact_path=str(artifact_path),
            metrics_json=json.dumps(meta["metrics"]),
            notes=meta["notes"],
            file_mtime=mtime,
            file_size=size,
            created_at=_now_iso(),
            deleted_at=None,
        )
        try:
            self._backend.insert_model(model)
        except Exception as error:  # noqa: BLE001 — UNIQUE collisions surface here
            LOG.error(
                "failed to register scanned model path=%s version=%s error=%s",
                artifact_path,
                meta["version"],
                error,
            )
            return None
        LOG.info(
            "registered filesystem model engine=%s phrase=%s version=%s path=%s",
            meta["engine"], meta["phrase_name"], meta["version"], rel_path,
        )
        return model.id

    def _reap(self, result: ScanResult, seen_paths: set[str]) -> ScanResult:
        """Soft-delete rows whose files vanished from the mount."""
        for stale_path in sorted(self._backend.active_filesystem_paths() - seen_paths):
            for model in self._backend.list_models(source="filesystem"):
                if model.filesystem_path == stale_path and model.deleted_at is None:
                    self._backend.soft_delete_model(model.id)
                    result.removed_ids.append(model.id)
                    LOG.info(
                        "soft-deleted filesystem model with missing file path=%s",
                        stale_path,
                    )
        return result
