"""Conduit Vox — the reference speaker identification service.

Conduit does not recognize voices itself. It packages an utterance and asks a
service over the three-request contract documented on the `conduit-speaker`
crate, and this is that contract implemented once, over a swappable embedding
model.

The product name is Conduit Vox; the capability name — the term Conduit itself
uses for pipeline stages, provider variants, and environment variables — is
still "speaker identification", because that is what a `speaker_id` stage
does. So the `SPEAKER_ID_*` environment variables stay: they describe the
capability, not the product.

The routes are the stable part. The encoder is not: `SPEAKER_ID_ENGINE`
chooses it, and adding pyannote or NeMo means adding a class here rather than
changing anything Conduit knows about.

## What it does not do

It does not decide who is speaking. It reports the closest enrolled voice and
how close it was, and Conduit applies the `threshold_percent` from the provider
definition. Two deployments sharing one service can then disagree about how
sure they want to be before a voice unlocks a door.

It also does not diarize. "Who is speaking, out of the people I know" and "how
many people are in this recording and when did each talk" are different
questions, and only the first one has a stage in a Conduit pipeline.
"""

from __future__ import annotations

import asyncio
import io
import json
import logging
import os
import secrets
import tempfile
import threading
import uuid
from contextlib import asynccontextmanager, suppress
from dataclasses import dataclass, replace
from datetime import datetime, timezone
from pathlib import Path
from typing import Awaitable, Callable, Protocol

import httpx
import numpy as np
import soundfile as sf
from fastapi import Depends, FastAPI, HTTPException, Request, Response
from fastapi.responses import RedirectResponse
from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel, Field

from conduit_link import (
    ConduitLinkClient as _SharedConduitLinkClient,
    HttpConduitLinkClient as _HttpConduitLinkClient,
    LinkedServicePanel,
    LinkRecord,
    LinkState,
    LinkStore as _SharedLinkStore,
    LinkStoreSecurityError as _SharedLinkStoreSecurityError,
)

LOG = logging.getLogger("vox")

# The embedding models each engine uses when the deployment names none.
DEFAULT_MODELS = {
    "speechbrain": "speechbrain/spkrec-ecapa-voxceleb",
    "pyannote": "pyannote/embedding",
    "nemo": "nvidia/speakerverification_en_titanet_large",
}

# How wide a vector each engine's default model produces.
#
# Recorded rather than discovered, because it is what decides whether an
# enrolment store survives an engine change, and an operator needs to know that
# before they swap engines rather than after the 409. The store's own guard
# (`VoicePrints.add`) is still the thing that enforces it; this table is what
# lets the README state it and what each encoder checks its model against.
EMBEDDING_WIDTHS = {"speechbrain": 192, "pyannote": 512, "nemo": 192}

# ECAPA-TDNN is trained on 16 kHz speech. Anything else is resampled before it
# reaches the encoder, because feeding 48 kHz audio to a 16 kHz model does not
# fail — it quietly embeds a voice an octave too high, which reads as a
# stranger.
MODEL_SAMPLE_RATE = 16_000

# Enrolling on a fragment produces a voice print that matches everybody, and it
# poisons the store in a way that only shows up later as false matches.
MIN_ENROLL_SECONDS = 1.0

# Below this, identification answers "nobody" rather than scoring noise. A turn
# whose wake gate never opened captured almost nothing, and a confident match
# against 200 ms of silence is worse than no answer.
MIN_IDENTIFY_SECONDS = 0.35

# The longest label Vox will store for a speaker. Vox owns the label only for
# its own UI: Conduit is the source of truth and enforces its own limits. Long
# enough for a real name; short enough that a paste from an unrelated buffer is
# refused before it lands as a label somebody has to unpick.
MAX_LABEL_LENGTH = 100


class Encoder(Protocol):
    """Turns mono 16 kHz samples into one embedding vector."""

    def embed(self, samples: np.ndarray) -> np.ndarray: ...


@dataclass
class Match:
    """The closest enrolled voice, and how close it was."""

    speaker: uuid.UUID | None
    confidence: float


class VoicePrints:
    """Enrolled embeddings, one file per speaker.

    A speaker's file holds every embedding enrolled for them rather than a
    running average, so a print built from three samples can be rebuilt if the
    encoder changes, and one bad sample could be dropped without re-enrolling
    the others.

    The file name is the speaker's UUID, which is why every route parses one
    before it reaches here: the identifier arrives from a URL, and a store that
    turned `../../etc/passwd` into a path would be the whole vulnerability.
    """

    def __init__(self, directory: Path) -> None:
        # The directory is created on the first write rather than here, so
        # importing this module does no filesystem work: `uvicorn app:app`
        # builds the app at import, and a service that cannot be imported
        # without write access to its volume fails in the wrong place with the
        # wrong error.
        self.directory = directory

    def _path(self, speaker: uuid.UUID) -> Path:
        return self.directory / f"{speaker}.npy"

    def add(self, speaker: uuid.UUID, embedding: np.ndarray) -> int:
        """Adds one embedding, returning how many that speaker now has."""
        self.directory.mkdir(parents=True, exist_ok=True)
        path = self._path(speaker)
        existing = np.load(path) if path.exists() else np.empty((0, embedding.size))
        if existing.size and existing.shape[1] != embedding.size:
            # The encoder changed under a store built by another one. Refused
            # rather than stacked, because comparing a 192-dimension print to a
            # 512-dimension voice is not a low score — it is nonsense.
            raise HTTPException(
                status_code=409,
                detail=(
                    f"speaker {speaker} was enrolled with {existing.shape[1]}-dimension "
                    f"embeddings and this engine produces {embedding.size}; re-enroll "
                    "them or point at the engine they were enrolled with"
                ),
            )
        updated = np.vstack([existing, embedding.reshape(1, -1)])
        # Written beside the target and renamed, so a crash mid-write leaves
        # the previous voice print rather than a truncated one.
        #
        # Saved through an open handle rather than by path: `np.save` appends
        # `.npy` to any name that does not already end in it, so passing
        # `<uuid>.npy.tmp` writes `<uuid>.npy.tmp.npy` and the rename below
        # then has nothing to rename.
        temporary = path.with_suffix(".npy.tmp")
        with temporary.open("wb") as file:
            np.save(file, updated)
        temporary.replace(path)
        return int(updated.shape[0])

    def remove(self, speaker: uuid.UUID) -> bool:
        """Removes a speaker's voice print, reporting whether it existed."""
        path = self._path(speaker)
        if not path.exists():
            return False
        path.unlink()
        return True

    def closest(self, embedding: np.ndarray) -> Match:
        """The enrolled speaker whose voice print is nearest `embedding`."""
        best = Match(speaker=None, confidence=0.0)
        for path in sorted(self._enrolled()):
            try:
                speaker = uuid.UUID(path.stem)
            except ValueError:
                # Something else put a file here. Skipping it is better than
                # refusing every identification because of one stray name.
                LOG.warning("ignoring voice print with a non-UUID name: %s", path.name)
                continue
            prints = np.load(path)
            if prints.size == 0 or prints.shape[1] != embedding.size:
                continue
            # Compared against the mean of a speaker's enrollments rather than
            # the best of them: matching the single closest sample rewards a
            # speaker who enrolled many times with a higher score against
            # everyone, including people who are not them.
            score = cosine(embedding, prints.mean(axis=0))
            if score > best.confidence:
                best = Match(speaker=speaker, confidence=score)
        return best

    def count(self) -> int:
        return len(self._enrolled())

    def samples(self, speaker: uuid.UUID) -> int:
        path = self._path(speaker)
        return Roster._samples_on_disk(path)

    def _enrolled(self) -> list[Path]:
        """Every stored voice print. Nothing enrolled yet is not an error."""
        if not self.directory.is_dir():
            return []
        return list(self.directory.glob("*.npy"))


@dataclass(frozen=True)
class SpeakerEntry:
    """What the roster knows about one speaker.

    Vox owns the label only for its own UI. Conduit is the source of truth,
    which is why the field is nullable — a speaker enrolled directly against
    Vox for testing has no name until somebody types one.
    """

    uuid: uuid.UUID
    label: str | None
    samples: int
    created_at: str
    updated_at: str

    def to_dict(self) -> dict[str, object]:
        return {
            "uuid": str(self.uuid),
            "label": self.label,
            "samples": self.samples,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        }


class Roster:
    """Labels and sample counts, one manifest for the store.

    The prints in `VoicePrints` are the load-bearing files — losing one loses
    a voice. This is the incidental sibling: losing it loses a name a person
    typed. So the two are written separately, and the roster reconciles from
    the prints on read (upgrade from a version that had no manifest, or a
    manifest write that lost a race with the print write).
    """

    FILENAME = "roster.json"

    def __init__(self, directory: Path) -> None:
        self.directory = directory
        self._path = directory / self.FILENAME
        self._entries: dict[uuid.UUID, SpeakerEntry] | None = None

    def _now(self) -> str:
        return datetime.now(timezone.utc).isoformat(timespec="seconds")

    def _load(self) -> dict[uuid.UUID, SpeakerEntry]:
        if self._entries is not None:
            return self._entries
        entries: dict[uuid.UUID, SpeakerEntry] = {}
        if self._path.is_file():
            try:
                data = json.loads(self._path.read_text())
            except json.JSONDecodeError as error:
                # A corrupted manifest is not worth crashing over: the prints
                # themselves are on disk and the roster rebuilds from them.
                LOG.warning("roster manifest is not valid JSON, rebuilding: %s", error)
                data = {}
            for row in data.get("speakers", []):
                try:
                    identifier = uuid.UUID(row["uuid"])
                except (KeyError, ValueError, TypeError):
                    LOG.warning("ignoring roster row with a non-UUID name: %r", row)
                    continue
                entries[identifier] = SpeakerEntry(
                    uuid=identifier,
                    label=row.get("label"),
                    samples=int(row.get("samples", 0)),
                    created_at=str(row.get("created_at") or self._now()),
                    updated_at=str(row.get("updated_at") or self._now()),
                )
        self._entries = entries
        return entries

    def _save(self) -> None:
        entries = self._load()
        payload = {"speakers": [entry.to_dict() for entry in entries.values()]}
        self.directory.mkdir(parents=True, exist_ok=True)
        # Written beside the target and renamed, so a crash mid-write leaves
        # the previous manifest rather than a truncated one.
        temporary = self._path.with_suffix(".json.tmp")
        temporary.write_text(json.dumps(payload, indent=2))
        temporary.replace(self._path)

    def reconcile(self, prints: VoicePrints) -> None:
        """Aligns the manifest with what is on disk.

        For every print file, ensure an entry exists (label unknown). Drop
        every entry whose print file no longer exists — a print removed
        out-of-band is a speaker Vox cannot identify, and holding a label for
        them would show a name the operator cannot select.
        """
        entries = self._load()
        seen: set[uuid.UUID] = set()
        changed = False
        for path in prints._enrolled():
            try:
                identifier = uuid.UUID(path.stem)
            except ValueError:
                continue
            seen.add(identifier)
            existing = entries.get(identifier)
            samples = self._samples_on_disk(path)
            if existing is None:
                now = self._now()
                entries[identifier] = SpeakerEntry(
                    uuid=identifier,
                    label=None,
                    samples=samples,
                    created_at=now,
                    updated_at=now,
                )
                changed = True
            elif existing.samples != samples:
                entries[identifier] = replace(
                    existing, samples=samples, updated_at=self._now()
                )
                changed = True
        for stale in [u for u in entries if u not in seen]:
            existing = entries[stale]
            if existing.label is None:
                del entries[stale]
            else:
                entries[stale] = replace(existing, samples=0, updated_at=self._now())
            changed = True
        if changed:
            try:
                self._save()
            except OSError as error:
                # Read paths still work from memory even if the disk write
                # failed; a read-only volume is a diagnosable state rather
                # than a crashing service.
                LOG.warning("could not persist reconciled roster: %s", error)

    def list(self, prints: VoicePrints) -> list[SpeakerEntry]:
        self.reconcile(prints)
        return sorted(
            self._load().values(),
            key=lambda entry: ((entry.label or "").casefold(), str(entry.uuid)),
        )

    def touch(self, speaker: uuid.UUID, samples: int) -> SpeakerEntry:
        """Records that `speaker` has `samples` prints on file.

        Called after an enrol succeeds. Preserves an existing label; creates
        the entry if this is a first enrolment.
        """
        entries = self._load()
        now = self._now()
        existing = entries.get(speaker)
        if existing is None:
            entry = SpeakerEntry(
                uuid=speaker,
                label=None,
                samples=samples,
                created_at=now,
                updated_at=now,
            )
        else:
            entry = replace(existing, samples=samples, updated_at=now)
        entries[speaker] = entry
        self._save()
        return entry

    def remove(self, speaker: uuid.UUID) -> bool:
        entries = self._load()
        if speaker not in entries:
            return False
        del entries[speaker]
        self._save()
        return True

    def set_label(self, speaker: uuid.UUID, label: str | None) -> SpeakerEntry | None:
        entries = self._load()
        existing = entries.get(speaker)
        if existing is None:
            return None
        entry = replace(existing, label=label, updated_at=self._now())
        entries[speaker] = entry
        self._save()
        return entry

    def upsert(self, speaker: uuid.UUID, *, label: str | None, samples: int) -> SpeakerEntry:
        entries = self._load()
        now = self._now()
        existing = entries.get(speaker)
        if existing is None:
            entry = SpeakerEntry(
                uuid=speaker,
                label=label,
                samples=samples,
                created_at=now,
                updated_at=now,
            )
        else:
            entry = replace(existing, label=label, samples=samples, updated_at=now)
        entries[speaker] = entry
        self._save()
        return entry

    def count(self) -> int:
        return len(self._load())

    @staticmethod
    def _samples_on_disk(path: Path) -> int:
        try:
            data = np.load(path)
        except (OSError, ValueError):
            return 0
        return int(data.shape[0]) if data.ndim == 2 else 0


def cosine(left: np.ndarray, right: np.ndarray) -> float:
    """Cosine similarity, clamped into the `0.0..=1.0` Conduit expects.

    Negative similarities are floored rather than rescaled. A voice pointing
    away from a print is not "35% of a match" — it is not a match, and
    stretching the range would hand a confidence to something that has none.
    """
    magnitude = float(np.linalg.norm(left) * np.linalg.norm(right))
    if magnitude == 0.0:
        return 0.0
    return max(0.0, min(1.0, float(np.dot(left, right) / magnitude)))


def require_device(torch: object, device: str) -> None:
    """Refuses `cuda` on a container that has no GPU.

    Said out loud rather than falling back. A GPU image silently running on the
    CPU is a deployment that looks fine and is twenty times slower.
    """
    if device == "cuda" and not torch.cuda.is_available():  # type: ignore[attr-defined]
        raise RuntimeError(
            "SPEAKER_ID_DEVICE=cuda but torch reports no CUDA device; "
            "check the container has GPU access"
        )


def check_width(engine: str, produced: int) -> None:
    """Checks a model produced the width this service says the engine does.

    The store already refuses a print of one width against a voice of another,
    and this is the check one layer earlier: a model an operator named with
    `SPEAKER_ID_MODEL` may be a variant of a different size, and finding that
    out at load time names the model, where finding it out at the store names
    only the numbers. Neither replaces the other.
    """
    expected = EMBEDDING_WIDTHS.get(engine)
    if expected is not None and produced != expected:
        raise WidthMismatchError(
            f"engine `{engine}` produces {expected}-dimension embeddings but the "
            f"loaded model produces {produced}; voice prints enrolled against "
            f"this engine elsewhere will not compare — set SPEAKER_ID_ENGINE to "
            "the engine this model belongs to, or enroll into an empty store"
        )


class SpeechBrainEncoder:
    """SpeechBrain's ECAPA-TDNN, loaded once and reused.

    The CPU and GPU images differ only in `SPEAKER_ID_DEVICE` and the torch
    wheel underneath, so this class is the same in both.
    """

    # 192, which is what `speechbrain/spkrec-ecapa-voxceleb` emits.
    width = EMBEDDING_WIDTHS["speechbrain"]

    def __init__(self, model: str, cache: Path, device: str) -> None:
        # Imported here rather than at module scope so the tests, which supply
        # their own encoder, do not pay for torch.
        import torch
        from speechbrain.inference.speaker import EncoderClassifier

        self._torch = torch
        require_device(torch, device)
        LOG.info("loading %s onto %s", model, device)
        self._model = EncoderClassifier.from_hparams(
            source=model, savedir=str(cache), run_opts={"device": device}
        )
        self._device = device

    def embed(self, samples: np.ndarray) -> np.ndarray:
        waveform = self._torch.from_numpy(samples).float().unsqueeze(0)
        with self._torch.no_grad():
            embedding = self._model.encode_batch(waveform.to(self._device))
        return embedding.squeeze().cpu().numpy()


class PyannoteEncoder:
    """pyannote.audio's x-vector embeddings.

    `pyannote/embedding` is gated on Hugging Face: the weights are MIT, but
    downloading them needs an account that has accepted the model's conditions
    and a read token. The token is read from the environment and never logged —
    what gets logged is the model name and whether a token was present, because
    "no token" and "token that has not been granted access" are different
    operator actions and a bare 401 tells them apart from neither.
    """

    # 512. pyannote's `XVectorSincNet` defaults to `dimension: int = 512` and
    # `pyannote/embedding` ships that default, which is why an enrolment store
    # cannot be shared with the two 192-dimension engines.
    width = EMBEDDING_WIDTHS["pyannote"]

    def __init__(self, model: str, cache: Path, device: str) -> None:
        # The token is checked before torch is imported, so the error an
        # operator sees for a missing token is the one about the agreement
        # rather than whatever a framework says first.
        token = hugging_face_token()
        if token is None:
            # Refused before the download rather than after it, because the
            # failure Hugging Face returns for a missing token on a gated repo
            # is a 401 with no mention of the agreement that would fix it.
            raise RuntimeError(
                f"`{model}` is gated on Hugging Face: accept its conditions on "
                f"https://huggingface.co/{model} with an account, then set "
                "HF_TOKEN to a read token for that account"
            )

        import torch
        from pyannote.audio import Inference, Model

        self._torch = torch
        require_device(torch, device)
        LOG.info("loading gated model %s onto %s", model, device)
        try:
            loaded = Model.from_pretrained(model, token=token, cache_dir=str(cache))
        except Exception as error:  # noqa: BLE001 - hub and torch raise broadly
            # The token exists but the account may not have been granted the
            # model. Saying which of the two failed is the whole point of
            # wrapping this, and the token itself stays out of the message.
            raise RuntimeError(
                f"could not fetch `{model}` with the token in HF_TOKEN; confirm "
                f"that account has accepted the conditions on "
                f"https://huggingface.co/{model} and that the token has read "
                f"access: {error}"
            ) from error
        if loaded is None:
            raise RuntimeError(f"pyannote returned no model for `{model}`")
        check_width("pyannote", int(loaded.dimension))
        # Whole-window rather than sliding: identification embeds one utterance
        # into one vector, and a sliding window would return a sequence the
        # cosine comparison downstream has no way to reduce.
        self._inference = Inference(loaded, window="whole", device=torch.device(device))

    def embed(self, samples: np.ndarray) -> np.ndarray:
        waveform = self._torch.from_numpy(samples).float().unsqueeze(0)
        embedding = self._inference(
            {"waveform": waveform, "sample_rate": MODEL_SAMPLE_RATE}
        )
        return np.asarray(embedding).reshape(-1)


class NeMoEncoder:
    """NVIDIA NeMo's TitaNet-Large speaker verification embeddings.

    The weights are CC-BY-4.0 and ungated, so unlike pyannote this needs no
    token — but NeMo wants a file on disk rather than samples in memory, so an
    utterance is written to a temporary WAV before it is embedded.
    """

    # 192, from TitaNet-Large's `emb_sizes: 192` decoder configuration. The
    # same width as ECAPA, which does *not* make the two interchangeable: the
    # store's guard cannot catch this swap, so the README says so instead.
    width = EMBEDDING_WIDTHS["nemo"]

    def __init__(self, model: str, cache: Path, device: str) -> None:
        import torch
        import nemo.collections.asr as nemo_asr

        self._torch = torch
        require_device(torch, device)
        LOG.info("loading %s onto %s", model, device)
        # NeMo caches into its own directory rather than taking one, so the
        # cache is pointed at through the environment the Dockerfile sets.
        os.environ.setdefault("NEMO_CACHE_DIR", str(cache))
        self._model = nemo_asr.models.EncDecSpeakerLabelModel.from_pretrained(
            model_name=model
        )
        self._model = self._model.to(device)
        self._model.eval()

    def embed(self, samples: np.ndarray) -> np.ndarray:
        # Through a file because `get_embedding` takes a path. Written into a
        # temporary that is removed on the way out, so a long-running container
        # does not accumulate one WAV per utterance it was asked about.
        with tempfile.NamedTemporaryFile(suffix=".wav") as scratch:
            sf.write(scratch.name, samples, MODEL_SAMPLE_RATE, format="WAV")
            with self._torch.no_grad():
                embedding = self._model.get_embedding(scratch.name)
        return embedding.squeeze().cpu().numpy()


def hugging_face_token() -> str | None:
    """The Hugging Face read token, from whichever name the operator used.

    Both names are read because the hub itself honours both, and an operator who
    set the one this service ignored would see a gating error they had already
    fixed. The value is never logged.
    """
    for name in ("HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"):
        value = os.environ.get(name)
        if value:
            return value
    return None


ENGINE_CLASSES: dict[str, type] = {
    "speechbrain": SpeechBrainEncoder,
    "pyannote": PyannoteEncoder,
    "nemo": NeMoEncoder,
}


def build_encoder(engine: str, model: str, cache: Path, device: str) -> Encoder:
    """The encoder for `engine`.

    One place per backend. Conduit's `SpeakerEngine` names the engine in the
    provider definition, so a new backend here is a class and a tag on an image
    — nothing the pipeline or the contract has to learn about.
    """
    encoder = ENGINE_CLASSES.get(engine)
    if encoder is not None:
        return encoder(model, cache, device)
    raise RuntimeError(
        f"unknown SPEAKER_ID_ENGINE `{engine}`; this image serves "
        f"{', '.join(sorted(DEFAULT_MODELS))}"
    )


def decode(body: bytes) -> np.ndarray:
    """Reads an uploaded file as mono 16 kHz float samples.

    Conduit sends a WAV container it built around the pipeline's own samples,
    or FLAC when that is what the pipeline captured. libsndfile reads both, so
    the format is sniffed from the bytes rather than trusted from a header a
    client set.
    """
    try:
        samples, rate = sf.read(io.BytesIO(body), dtype="float32", always_2d=True)
    except Exception as error:  # noqa: BLE001 - libsndfile raises broadly
        raise HTTPException(
            status_code=415, detail=f"could not decode the audio: {error}"
        ) from error

    # Averaged rather than taking the first channel: a stereo satellite with
    # one dead microphone would otherwise enroll a voice print of silence.
    mono = samples.mean(axis=1)
    if rate != MODEL_SAMPLE_RATE:
        mono = resample(mono, rate, MODEL_SAMPLE_RATE)
    return mono


def resample(samples: np.ndarray, source_rate: int, target_rate: int) -> np.ndarray:
    """Linear resampling onto the rate the model was trained at.

    Linear interpolation is not the best resampler available, and it is enough
    here: an embedding is robust to the artefacts it introduces, and the
    alternative is a scipy dependency for a path most deployments never take,
    because Conduit's own interchange format is already 16 kHz mono.
    """
    if source_rate == target_rate or samples.size == 0:
        return samples
    duration = samples.size / source_rate
    target_length = max(1, int(round(duration * target_rate)))
    source_positions = np.linspace(0.0, duration, num=samples.size, endpoint=False)
    target_positions = np.linspace(0.0, duration, num=target_length, endpoint=False)
    return np.interp(target_positions, source_positions, samples).astype(np.float32)


def seconds(samples: np.ndarray) -> float:
    return samples.size / MODEL_SAMPLE_RATE


class LabelUpdate(BaseModel):
    """Body of `PATCH /speakers/{uuid}`.

    `label` is `None` to clear an existing label; the field is nullable
    rather than optional so a caller can distinguish "leave it alone" (omit)
    from "clear it" (send `null`) — but since PATCH always writes what is
    sent, both are equivalent here and the null case is the meaningful one.
    """

    label: str | None = Field(default=None, max_length=MAX_LABEL_LENGTH)


class WidthMismatchError(RuntimeError):
    """Raised when a model's embedding width cannot coexist with saved prints."""


# Re-exports so callers of the old symbols keep working while we complete the
# extraction. `LinkStore` is `conduit_link.LinkStore[VoxLinkExtension]`;
# `LinkStoreSecurityError` is imported straight from the shared module.
LinkStoreSecurityError = _SharedLinkStoreSecurityError


@dataclass(frozen=True)
class VoxLinkExtension:
    """Vox-specific link state that doesn't fit the base spec-0005 shape.

    Both fields are per-peer secrets or provisioned ids: `provider_definition_id`
    is the `http_speaker_id` provider Conduit auto-provisioned for this peer,
    and `local_api_key` is what Vox's own routes authenticate against when the
    deployment didn't supply one.
    """

    provider_definition_id: str
    local_api_key: str


def _vox_ext_from_dict(payload: dict[str, object]) -> VoxLinkExtension:
    return VoxLinkExtension(
        provider_definition_id=str(payload["provider_definition_id"]),
        local_api_key=str(payload["local_api_key"]),
    )


def _vox_ext_to_dict(extension: VoxLinkExtension) -> dict[str, object]:
    return {
        "provider_definition_id": extension.provider_definition_id,
        "local_api_key": extension.local_api_key,
    }


class LinkStore(_SharedLinkStore[VoxLinkExtension]):
    """Vox-shaped `conduit_link.LinkStore` — carries `VoxLinkExtension`.

    Behaviour is inherited unchanged; this subclass exists so the constructor
    matches the legacy Vox call sites (`LinkStore(directory)`) rather than the
    keyword-only shared shape, and so `LinkStore.FILENAME` still resolves for
    tests that assert the on-disk path.
    """

    FILENAME = _SharedLinkStore.FILENAME

    def __init__(self, directory: Path) -> None:
        super().__init__(
            directory,
            extension_from_dict=_vox_ext_from_dict,
            extension_to_dict=_vox_ext_to_dict,
        )


def _public_link(record: LinkRecord[VoxLinkExtension]) -> dict[str, str]:
    """Vox's redacted view of a linked state. Mirrors the legacy shape."""
    return {
        "status": "linked",
        "conduit_url": record.state.conduit_url,
        "peer_id": record.state.peer_id,
        "peer_name": record.state.peer_name,
        "provider_definition_id": record.extension.provider_definition_id,
        "linked_at": record.state.linked_at,
    }


class LinkRequest(BaseModel):
    conduit_url: str
    # Optional so operators of anonymous (no-auth) Conduit deployments can
    # link without inventing a token. Handler strips whitespace and sends the
    # result as-is to Conduit, which falls back to "anonymous" caller when
    # no bearer was required.
    operator_token: str = ""
    peer_name: str
    force: bool = False


class ReloadRequest(BaseModel):
    model: str


class ConduitLinkClient(Protocol):
    """Vox-shaped view of the peer→Conduit link handshake.

    Tests may pass a fake that records what Vox tried to send. Production code
    uses `HttpConduitClient`, a thin adapter over `conduit_link`'s shared
    HTTP client that translates Vox's flat call arguments (`peer_id`,
    `vox_base_url`, `vox_api_key`) into the generic `/v1/linked-services`
    request body.
    """

    def create_link(
        self,
        conduit_url: str,
        operator_token: str,
        body: dict[str, str],
    ) -> dict[str, str]: ...

    def delete_link(self, conduit_url: str, peer_id: str, sync_token: str) -> None: ...


@dataclass(frozen=True)
class ConduitSpeaker:
    id: str
    name: str


class ConduitSpeakerClient(Protocol):
    async def list_speakers(
        self, conduit_url: str, sync_token: str
    ) -> list[ConduitSpeaker]: ...


class HttpConduitClient:
    """Vox-shaped adapter over `conduit_link.HttpConduitLinkClient`.

    Wraps the shared HTTP client so Vox's callers keep their flat body shape
    (`peer_id`, `vox_base_url`, `vox_api_key`) while the wire and TLS handling
    live in one place — `packages/conduit-link`. Translates the request body
    into the generic `/v1/linked-services` shape and unpacks
    `extension.provider_definition_id` from the response.
    """

    def __init__(
        self,
        *,
        timeout: float = 10.0,
        transport: httpx.BaseTransport | None = None,
    ) -> None:
        self._inner: _SharedConduitLinkClient = _HttpConduitLinkClient(
            timeout=timeout, transport=transport
        )

    def create_link(
        self,
        conduit_url: str,
        operator_token: str,
        body: dict[str, str],
    ) -> dict[str, str]:
        request_body: dict[str, object] = {
            "service_kind": "vox",
            "peer_id": body["peer_id"],
            "peer_name": body["peer_name"],
            "peer_base_url": body["vox_base_url"],
            "panel": {
                "id": "vox",
                "label": "Vox",
                "icon": "users",
                "path": "/ui/",
            },
            "extension": {"local_api_key": body["vox_api_key"]},
        }
        try:
            payload = self._inner.create_link(conduit_url, operator_token, request_body)
        except httpx.HTTPStatusError as error:
            raise HTTPException(
                status_code=502,
                detail=(
                    "Conduit refused the Vox link with HTTP "
                    f"{error.response.status_code}"
                ),
            ) from error
        extension = payload.get("extension") or {}
        if not isinstance(extension, dict) or "provider_definition_id" not in extension:
            raise HTTPException(
                status_code=502,
                detail="Conduit response missing extension.provider_definition_id",
            )
        return {
            "sync_token": str(payload["sync_token"]),
            "provider_definition_id": str(extension["provider_definition_id"]),
        }

    def delete_link(self, conduit_url: str, peer_id: str, sync_token: str) -> None:
        try:
            self._inner.delete_link(conduit_url, peer_id, sync_token)
        except httpx.HTTPError as error:
            LOG.warning("best-effort Vox unlink failed: peer=%s error=%s", peer_id, error)


class HttpConduitSpeakerClient:
    """HTTP client for Conduit's `/v1/speakers` roster sync."""

    def __init__(
        self,
        *,
        timeout: float = 10.0,
        transport: httpx.AsyncBaseTransport | None = None,
    ) -> None:
        self._timeout = timeout
        self._transport = transport

    async def list_speakers(
        self, conduit_url: str, sync_token: str
    ) -> list[ConduitSpeaker]:
        url = f"{conduit_url.rstrip('/')}/v1/speakers"
        async with httpx.AsyncClient(
            timeout=self._timeout, transport=self._transport
        ) as client:
            response = await client.get(
                url,
                headers={"authorization": f"Bearer {sync_token}"},
            )
        response.raise_for_status()
        payload = response.json()
        return [
            ConduitSpeaker(id=str(item["id"]), name=str(item["name"]))
            for item in payload
        ]


class Syncer:
    """Keeps Vox's local roster aligned with Conduit's roster labels."""

    def __init__(
        self,
        *,
        prints: VoicePrints,
        roster: Roster,
        links: LinkStore,
        conduit: ConduitSpeakerClient,
        interval_seconds: float = 300.0,
        max_backoff_seconds: float = 900.0,
        sleep: Callable[[float], Awaitable[None]] = asyncio.sleep,
    ) -> None:
        self.prints = prints
        self.roster = roster
        self.links = links
        self.conduit = conduit
        self.interval_seconds = interval_seconds
        self.max_backoff_seconds = max_backoff_seconds
        self._sleep = sleep

    async def sync_once(self) -> int:
        record = self.links.load()
        if record is None:
            return 0
        speakers = await self.conduit.list_speakers(
            record.state.conduit_url, record.state.sync_token
        )
        self.roster.reconcile(self.prints)
        synced = 0
        for remote in speakers:
            try:
                speaker = uuid.UUID(remote.id)
            except ValueError:
                LOG.warning(
                    "skipping Conduit speaker with invalid UUID: peer=%s speaker=%r",
                    record.state.peer_id,
                    remote.id,
                )
                continue
            self.roster.upsert(
                speaker,
                label=remote.name.strip() or None,
                samples=self.prints.samples(speaker),
            )
            synced += 1
        LOG.info(
            "synced Vox roster from Conduit: peer=%s speakers=%d",
            record.state.peer_id,
            synced,
        )
        return synced

    async def run_forever(self) -> None:
        delay = 0.0
        failure_delay = min(max(1.0, self.interval_seconds), self.max_backoff_seconds)
        while True:
            if delay > 0.0:
                await self._sleep(delay)
            try:
                await self.sync_once()
                delay = self.interval_seconds
                failure_delay = min(
                    max(1.0, self.interval_seconds), self.max_backoff_seconds
                )
            except asyncio.CancelledError:
                raise
            except Exception as error:  # noqa: BLE001 - task must never crash
                try:
                    record = self.links.load()
                except LinkStoreSecurityError:
                    record = None
                retry_in = failure_delay
                LOG.warning(
                    "Vox roster sync failed: peer=%s conduit=%s retry_in=%.0fs error=%s",
                    record.state.peer_id if record is not None else "unlinked",
                    record.state.conduit_url if record is not None else "unknown",
                    retry_in,
                    error,
                )
                delay = retry_in
                failure_delay = min(retry_in * 2, self.max_backoff_seconds)


def _trimmed_url(value: str, name: str) -> str:
    trimmed = value.strip().rstrip("/")
    if not trimmed:
        raise HTTPException(status_code=422, detail=f"{name} cannot be empty")
    return trimmed


def _trimmed_field(value: str, name: str) -> str:
    trimmed = value.strip()
    if not trimmed:
        raise HTTPException(status_code=422, detail=f"{name} cannot be empty")
    return trimmed


def create_app(
    encoder: Encoder | None = None,
    prints: VoicePrints | None = None,
    roster: Roster | None = None,
    link_store: LinkStore | None = None,
    conduit_client: ConduitLinkClient | None = None,
    speaker_client: ConduitSpeakerClient | None = None,
    sync_interval_seconds: float | None = None,
    sync_max_backoff_seconds: float | None = None,
) -> FastAPI:
    """Builds the service.

    `encoder`, `prints`, and `roster` are injected by the tests, which have no
    use for a model download to check that a 404 is a 404.
    """
    api_key = os.environ.get("SPEAKER_ID_API_KEY") or None
    store = prints or VoicePrints(
        Path(os.environ.get("SPEAKER_ID_DATA_DIR", "/data"))
    )
    # Roster shares the prints directory so a single volume mount carries
    # both; there is no split between "the voices" and "the names for them"
    # that a deployment would sensibly want to configure separately.
    names = roster or Roster(store.directory)
    links = link_store or LinkStore(store.directory)
    conduit = conduit_client or HttpConduitClient()
    speakers = speaker_client or HttpConduitSpeakerClient()
    engine = os.environ.get("SPEAKER_ID_ENGINE", "speechbrain")
    model_name = os.environ.get("SPEAKER_ID_MODEL") or DEFAULT_MODELS.get(engine, "")
    model_cache = Path(os.environ.get("SPEAKER_ID_MODEL_DIR", "/models"))
    device = os.environ.get("SPEAKER_ID_DEVICE", "cpu")
    sync_interval = sync_interval_seconds or float(
        os.environ.get("SPEAKER_ID_SYNC_INTERVAL_SECONDS", "300")
    )
    sync_backoff = sync_max_backoff_seconds or float(
        os.environ.get("SPEAKER_ID_SYNC_MAX_BACKOFF_SECONDS", "900")
    )
    runtime = {"model": model_name}
    encoder_lock = threading.Lock()

    loaded: dict[str, Encoder] = {}
    if encoder is not None:
        loaded["encoder"] = encoder

    syncer = Syncer(
        prints=store,
        roster=names,
        links=links,
        conduit=speakers,
        interval_seconds=sync_interval,
        max_backoff_seconds=sync_backoff,
    )

    @asynccontextmanager
    async def lifespan(_: FastAPI):
        task: asyncio.Task[None] | None = None
        try:
            try:
                if links.load() is not None:
                    task = asyncio.create_task(syncer.run_forever())
            except LinkStoreSecurityError as error:
                LOG.warning("could not start Vox roster sync: %s", error)
            yield
        finally:
            if task is not None:
                task.cancel()
                with suppress(asyncio.CancelledError):
                    await task

    app = FastAPI(title="Conduit Vox", version="1", lifespan=lifespan)

    def get_encoder() -> Encoder:
        # Loaded on first use rather than at import, so the container starts,
        # answers /health, and reports a model that will not load as an error
        # on a request rather than as a crash loop nobody can query.
        if "encoder" not in loaded:
            with encoder_lock:
                if "encoder" not in loaded:
                    try:
                        loaded["encoder"] = build_encoder(
                            engine, runtime["model"], model_cache, device
                        )
                    except Exception as error:  # noqa: BLE001 - torch and hub raise broadly
                        LOG.exception("could not load the encoder")
                        raise HTTPException(
                            status_code=503, detail=f"encoder unavailable: {error}"
                        ) from error
        return loaded["encoder"]

    bearer = HTTPBearer(auto_error=False)

    def authorize(
        credentials: HTTPAuthorizationCredentials | None = Depends(bearer),
    ) -> None:
        """Checks the bearer token, when the deployment set one.

        A service with no key configured is open, which is only reasonable
        because it is meant to sit on an internal network beside Conduit. The
        compose file does not publish its port for the same reason.
        """
        accepted = api_key
        if accepted is None:
            try:
                record = links.load()
            except LinkStoreSecurityError as error:
                raise HTTPException(status_code=500, detail=str(error)) from error
            accepted = record.extension.local_api_key if record is not None else None
        if accepted is None:
            return
        if credentials is None or credentials.credentials != accepted:
            raise HTTPException(status_code=401, detail="invalid or missing API key")

    def speaker_id(speaker: str) -> uuid.UUID:
        """Parses the speaker in a route, which is also what keeps it a name.

        The identifier becomes a file name, so anything that is not a UUID is
        refused here rather than reaching the store.
        """
        try:
            return uuid.UUID(speaker)
        except ValueError as error:
            raise HTTPException(
                status_code=400, detail=f"`{speaker}` is not a speaker id"
            ) from error

    @app.get("/health")
    def health() -> dict[str, object]:
        # Reconciled here so the count reflects prints somebody added
        # out-of-band (an upgrade, a volume restore) rather than only what the
        # manifest happened to remember.
        names.reconcile(store)
        return {
            "status": "ok",
            "engine": engine,
            "model": runtime["model"],
            "device": device,
            # Reported so an operator can tell, before they mount an existing
            # /data volume, whether the prints in it can be compared at all.
            "embedding_width": EMBEDDING_WIDTHS.get(engine),
            "enrolled": names.count(),
            # Whether the encoder is in memory yet. A container that has
            # answered no requests has not paid for the model, and saying so is
            # the difference between a slow first request and a service
            # somebody restarts because they think it is wedged.
            "model_loaded": "encoder" in loaded,
        }

    @app.post("/engine/reload")
    def reload_engine(
        body: ReloadRequest, _: None = Depends(authorize)
    ) -> dict[str, object]:
        model = _trimmed_field(body.model, "model")
        with encoder_lock:
            try:
                loaded["encoder"] = build_encoder(engine, model, model_cache, device)
            except WidthMismatchError as error:
                status = 409 if store.count() > 0 else 422
                raise HTTPException(status_code=status, detail=str(error)) from error
            except Exception as error:  # noqa: BLE001 - torch and hub raise broadly
                raise HTTPException(
                    status_code=503, detail=f"encoder unavailable: {error}"
                ) from error
            runtime["model"] = model
        LOG.info("reloaded Vox encoder: engine=%s model=%s", engine, model)
        return health()

    @app.get("/link")
    def link_status() -> dict[str, str]:
        try:
            record = links.load()
        except LinkStoreSecurityError as error:
            raise HTTPException(status_code=500, detail=str(error)) from error
        if record is None:
            if api_key is not None:
                return {"status": "config-managed"}
            return {"status": "unlinked"}
        return _public_link(record)

    @app.post("/link")
    def link(request: Request, body: LinkRequest) -> dict[str, str]:
        try:
            existing = links.load()
        except LinkStoreSecurityError as error:
            raise HTTPException(status_code=500, detail=str(error)) from error
        if existing is not None and not body.force:
            raise HTTPException(
                status_code=409,
                detail="Vox is already linked; unlink first or pass force=true",
            )

        conduit_url = _trimmed_url(body.conduit_url, "conduit_url")
        peer_name = _trimmed_field(body.peer_name, "peer_name")
        # A Conduit with no auth configured accepts an empty bearer, so an
        # operator linking a fresh dev instance should not be forced to invent
        # a token here. Kept as a plain strip rather than _trimmed_field so
        # the field is optional but never sent as whitespace.
        operator_token = body.operator_token.strip()
        peer_id = (
            existing.state.peer_id if existing is not None else str(uuid.uuid4())
        )
        local_api_key = api_key or (
            existing.extension.local_api_key
            if existing is not None
            else secrets.token_urlsafe(32)
        )
        vox_base_url = _trimmed_url(
            os.environ.get("SPEAKER_ID_BASE_URL") or str(request.base_url),
            "SPEAKER_ID_BASE_URL",
        )

        try:
            created = conduit.create_link(
                conduit_url,
                operator_token,
                {
                    "peer_name": peer_name,
                    "peer_id": peer_id,
                    "vox_base_url": vox_base_url,
                    "vox_api_key": local_api_key,
                },
            )
        except httpx.HTTPError as error:
            raise HTTPException(
                status_code=502, detail=f"could not reach Conduit: {error}"
            ) from error

        state = LinkState(
            conduit_url=conduit_url,
            peer_id=peer_id,
            peer_name=peer_name,
            sync_token=created["sync_token"],
            panel=LinkedServicePanel(title="Vox", path="/ui/", icon="users"),
            linked_at=datetime.now(timezone.utc).isoformat(timespec="seconds"),
        )
        extension = VoxLinkExtension(
            provider_definition_id=created["provider_definition_id"],
            local_api_key=local_api_key,
        )
        record = links.save(state, extension)
        LOG.info(
            "linked Vox to Conduit peer=%s provider=%s",
            peer_id,
            record.extension.provider_definition_id,
        )
        response = _public_link(record)
        if api_key is None:
            response["local_api_key"] = local_api_key
        return response

    @app.delete("/link")
    def unlink() -> Response:
        try:
            record = links.load()
        except LinkStoreSecurityError as error:
            raise HTTPException(status_code=500, detail=str(error)) from error
        if record is None:
            return Response(status_code=204)
        conduit.delete_link(
            record.state.conduit_url,
            record.state.peer_id,
            record.state.sync_token,
        )
        links.remove()
        LOG.info("unlinked Vox from Conduit peer=%s", record.state.peer_id)
        return Response(status_code=204)

    @app.get("/speakers")
    def list_speakers(_: None = Depends(authorize)) -> dict[str, object]:
        return {"speakers": [entry.to_dict() for entry in names.list(store)]}

    @app.patch("/speakers/{speaker}")
    def label_speaker(
        speaker: str,
        update: LabelUpdate,
        _: None = Depends(authorize),
    ) -> dict[str, object]:
        identity = speaker_id(speaker)
        # Reconciled first: a print that predates the manifest still shows up
        # in the list route, and its label should be settable without an
        # enrol round-trip to create the entry.
        names.reconcile(store)
        entry = names.set_label(identity, update.label)
        if entry is None:
            raise HTTPException(
                status_code=404,
                detail=f"speaker {identity} is not enrolled",
            )
        LOG.info("labelled %s as %r", identity, update.label)
        return entry.to_dict()

    @app.post("/identify")
    async def identify(
        request: Request, _: None = Depends(authorize)
    ) -> dict[str, object]:
        samples = decode(await request.body())
        if seconds(samples) < MIN_IDENTIFY_SECONDS:
            # Not an error: a turn that captured almost nothing is a turn
            # nobody can be identified from, and that is an answer.
            LOG.info("identify: %.2fs is too short to score", seconds(samples))
            return {"speaker": None, "confidence": 0.0}

        match = store.closest(get_encoder().embed(samples))
        LOG.info(
            "identify: closest=%s confidence=%.3f enrolled=%d",
            match.speaker,
            match.confidence,
            store.count(),
        )
        # Reported whole, including a poor match. Conduit holds the threshold,
        # so a service that pre-filtered would silently override the operator's
        # own setting and hide the near misses they tune it with.
        return {
            "speaker": str(match.speaker) if match.speaker else None,
            "confidence": match.confidence,
        }

    @app.post("/speakers/{speaker}/enroll")
    async def enroll(
        speaker: str, request: Request, _: None = Depends(authorize)
    ) -> dict[str, object]:
        identity = speaker_id(speaker)
        samples = decode(await request.body())
        if seconds(samples) < MIN_ENROLL_SECONDS:
            raise HTTPException(
                status_code=422,
                detail=(
                    f"{seconds(samples):.2f}s is too short to enroll a voice; "
                    f"at least {MIN_ENROLL_SECONDS}s is needed"
                ),
            )

        held = store.add(identity, get_encoder().embed(samples))
        names.touch(identity, held)
        LOG.info("enrolled %s from %.2fs (%d samples)", identity, seconds(samples), held)
        return {"speaker": str(identity), "samples": held}

    @app.delete("/speakers/{speaker}")
    def forget(speaker: str, _: None = Depends(authorize)) -> Response:
        identity = speaker_id(speaker)
        removed_print = store.remove(identity)
        removed_entry = names.remove(identity)
        if not removed_print and not removed_entry:
            # Conduit treats this as success — the caller asked for that voice
            # print to be gone and it is — but the status still says which
            # happened, for anyone driving the service directly.
            return Response(status_code=404)
        LOG.info("forgot %s", identity)
        return Response(status_code=204)

    # The embedded UI: one file, no build step. Mounted last so its catch-all
    # does not shadow the JSON routes above.
    ui_directory = Path(__file__).parent / "static"
    if ui_directory.is_dir():
        app.mount("/ui", StaticFiles(directory=str(ui_directory), html=True), name="ui")

        @app.get("/", include_in_schema=False)
        def root() -> RedirectResponse:
            # The root is the UI rather than an empty 404, because an operator
            # who opened the service in a browser is looking for the UI, not
            # the OpenAPI schema at /docs.
            return RedirectResponse(url="/ui/")

    return app


app = create_app()
