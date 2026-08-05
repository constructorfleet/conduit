"""Conduit's reference speech recognition service.

Canary, Qwen audio, and Granite Speech ship as weights with no hosted endpoint,
and Conduit's speech-to-text trait reaches a *service*. This is that service:
one Wyoming server in front of whichever set of weights the deployment chose.

Wyoming rather than an OpenAI-compatible shim because Conduit already speaks it.
`SttVariant::Wyoming` takes a `url` and transcribes against this process with no
Rust change at all, and the same server is reachable from Home Assistant's voice
pipelines for free.

The protocol is the stable part. The engine is not: `ASR_ENGINE` chooses it, and
adding Qwen or Granite means adding a class here rather than changing anything
Conduit knows about — the same seam `services/speaker-id/` keeps at
`build_encoder`.

## What it does not do

It does not stream partial transcripts. See the README: the recognizers this
wraps are offline encoder-decoder models that answer once, and a server that
accepted a streaming request and never sent a partial would be lying. `describe`
reports `supports_transcript_streaming: false`, and a Conduit recognizer with
`streaming` on reads exactly that and falls back to one final per utterance,
logging the reason once. So the honest answer is not merely documentation — it is
what keeps a client from waiting for partials that are never coming.

It does not resample, and it does not mix down. A 16 kHz recognizer fed 48 kHz
samples produces fluent nonsense rather than an error, which an operator reads as
a bad model instead of as a misconfigured satellite — so a format the engine was
not trained on is refused by name.
"""

from __future__ import annotations

import argparse
import asyncio
import logging
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

import numpy as np
from wyoming.asr import Transcribe, Transcript
from wyoming.audio import AudioChunk, AudioStart, AudioStop
from wyoming.error import Error
from wyoming.event import Event
from wyoming.info import AsrModel, AsrProgram, Attribution, Describe, Info
from wyoming.server import AsyncEventHandler, AsyncServer

LOG = logging.getLogger("wyoming-asr")

# The format every engine here is trained on, and the only one a Wyoming payload
# carries in practice: `crates/conduit-wyoming/src/stt.rs` advertises
# `pcm_s16le` alone and sends Conduit's own 16 kHz mono interchange format.
MODEL_SAMPLE_RATE = 16_000
MODEL_WIDTH = 2
MODEL_CHANNELS = 1

# The model each engine loads when the deployment names none. One entry per
# engine this image can actually serve, so the error naming them is not a list of
# things that do not exist yet: a Qwen or Granite class adds itself here and in
# `build_engine`, and nothing else changes.
DEFAULT_MODELS = {"canary": "nvidia/canary-1b-v2"}

# Who to credit. Canary is CC-BY-4.0, which makes attribution an obligation
# rather than a courtesy, so it is reported in `describe` where a client can pass
# it on as well as written in the README where a person reads it.
ATTRIBUTION = {
    "canary": Attribution(
        name="NVIDIA NeMo Canary (CC-BY-4.0)",
        url="https://huggingface.co/nvidia/canary-1b-v2",
    )
}

# How much audio one utterance may hold. Samples are buffered until `audio-stop`
# because these engines transcribe a whole utterance at once, so without a
# ceiling an unauthenticated client can exhaust the host one chunk at a time.
# Two minutes is far longer than any voice command and short enough that a
# thousand of them do not add up to a machine.
DEFAULT_MAX_SECONDS = 120.0


class Engine(Protocol):
    """Turns mono 16 kHz float samples into text."""

    #: Which backend this is, as `ASR_ENGINE` spells it.
    name: str
    #: The one model this process loaded. A request for another is refused.
    model: str
    #: What `describe` reports, for a client listing what it can reach.
    description: str
    languages: tuple[str, ...]

    def transcribe(self, samples: np.ndarray, language: str | None) -> str: ...


@dataclass(frozen=True)
class Limits:
    """The bounds a session is held to, so a stream cannot outgrow memory."""

    max_seconds: float = DEFAULT_MAX_SECONDS


@dataclass(frozen=True)
class Selection:
    """Which engine to load and how, read from the environment.

    Separate from loading it: the process must be able to say what it will serve
    before it has paid for several gigabytes of weights, and the tests need the
    selector without the model.
    """

    engine: str
    model: str
    cache: Path
    device: str

    def load(self) -> Engine:
        return build_engine(self.engine, self.model, self.cache, self.device)


class FormatMismatch(Exception):
    """Audio the loaded engine was not trained on.

    Its own class because it is the one refusal that is a configuration mistake
    somewhere else — a satellite capturing 48 kHz, a client mixing stereo — and
    the message has to name both what arrived and what was wanted for anybody to
    fix it.
    """


def check_format(rate: int, width: int, channels: int) -> None:
    """Refuses audio the engine cannot honestly transcribe.

    Not resampled. Feeding a 16 kHz recognizer 48 kHz samples does not fail: it
    returns a confident transcript of nothing that was said, and that reaches an
    operator as a model that does not work rather than as a device that is
    misconfigured. `services/speaker-id/app.py` refuses an embedding-width
    mismatch for the same reason.
    """
    if rate != MODEL_SAMPLE_RATE:
        raise FormatMismatch(
            f"audio arrived at {rate} Hz and this engine transcribes "
            f"{MODEL_SAMPLE_RATE} Hz; resampling it here would produce a "
            "confident transcript of the wrong thing, so send 16 kHz audio"
        )
    if channels != MODEL_CHANNELS:
        raise FormatMismatch(
            f"audio arrived with {channels} channels and this engine transcribes "
            f"{MODEL_CHANNELS}; interleaved channels read as mono are a "
            "recording at double speed, so send mono audio"
        )
    if width != MODEL_WIDTH:
        raise FormatMismatch(
            f"audio arrived with {width}-byte samples and this engine reads "
            f"{MODEL_WIDTH}-byte (pcm_s16le); anything else decodes as noise"
        )


def to_float_samples(audio: bytes) -> np.ndarray:
    """Signed 16-bit little-endian bytes as the floats every engine here wants.

    Divided by 32768 rather than 32767 so the scale is exact and full-scale
    negative samples do not clip past -1.0.
    """
    return np.frombuffer(audio, dtype="<i2").astype(np.float32) / 32_768.0


class CanaryEngine:
    """NVIDIA Canary through NeMo, loaded once and reused.

    The worked example. Canary is multilingual and answers a whole utterance at
    a time, which is why this service is batch-only.
    """

    name = "canary"
    description = "NVIDIA Canary via NeMo, multilingual, CC-BY-4.0"
    languages = ("en", "de", "es", "fr")

    def __init__(self, model: str, cache: Path, device: str) -> None:
        # Imported here rather than at module scope so the tests, which supply
        # their own engine, pay for neither NeMo nor torch.
        import torch
        from nemo.collections.asr.models import ASRModel

        self._torch = torch
        if device == "cuda" and not torch.cuda.is_available():
            # Said out loud. A GPU image quietly running on the CPU is a
            # deployment that looks fine and transcribes an utterance in the
            # time a conversation has already moved on.
            raise RuntimeError(
                "ASR_DEVICE=cuda but torch reports no CUDA device; "
                "check the container has GPU access"
            )
        self.model = model
        LOG.info("loading %s onto %s", model, device)
        cache.mkdir(parents=True, exist_ok=True)
        self._asr = ASRModel.from_pretrained(model_name=model, map_location=device)
        self._asr.eval()

    def transcribe(self, samples: np.ndarray, language: str | None) -> str:
        # NeMo takes the language as a source/target pair, and Canary translates
        # when they differ. Passing one language for both keeps this a
        # transcription service: translation is a different question with a
        # different stage in a Conduit pipeline.
        options: dict[str, object] = {}
        if language is not None:
            options["source_lang"] = language
            options["target_lang"] = language
        with self._torch.no_grad():
            results = self._asr.transcribe([samples], **options)
        if not results:
            return ""
        first = results[0]
        # NeMo returns either strings or hypothesis objects depending on the
        # model and version, and a repr of a hypothesis is not a transcript.
        return getattr(first, "text", first) or ""


def build_engine(engine: str, model: str, cache: Path, device: str) -> Engine:
    """The engine for `engine`.

    One place to add Qwen or Granite: a class with a `transcribe` method and a
    branch here. Conduit points at a `url` and never names the backend, so a new
    one is a tag on an image and nothing the pipeline or the editor has to learn.
    """
    if engine == "canary":
        return CanaryEngine(model, cache, device)
    raise RuntimeError(
        f"unknown ASR_ENGINE `{engine}`; this image serves "
        f"{', '.join(sorted(DEFAULT_MODELS))}"
    )


def engine_from_environment() -> Selection:
    """What this process will serve, before it has loaded anything."""
    engine = os.environ.get("ASR_ENGINE", "canary")
    return Selection(
        engine=engine,
        model=os.environ.get("ASR_MODEL") or DEFAULT_MODELS.get(engine, ""),
        cache=Path(os.environ.get("ASR_MODEL_DIR", "/models")),
        device=os.environ.get("ASR_DEVICE", "cpu"),
    )


def limits_from_environment() -> Limits:
    """The utterance ceiling, validated where it is read.

    An unset compose variable arrives as an empty string rather than as absent,
    and a nonsensical value is a memory limit that is not one — so both are
    refused here with the variable named, rather than surfacing as a `ValueError`
    traceback or, worse, as no limit at all.
    """
    configured = os.environ.get("ASR_MAX_SECONDS", "").strip()
    if not configured:
        return Limits()
    try:
        max_seconds = float(configured)
    except ValueError as error:
        raise RuntimeError(
            f"ASR_MAX_SECONDS must be a number of seconds, not `{configured}`"
        ) from error
    if max_seconds <= 0:
        raise RuntimeError(
            f"ASR_MAX_SECONDS must be positive, not `{configured}`; "
            "a zero ceiling accepts no audio at all"
        )
    return Limits(max_seconds=max_seconds)


def describe(engine: Engine) -> Info:
    """What this process serves, for a client that asks before it sends audio.

    Conduit asks whenever its `streaming` flag is on, and reads
    `supports_transcript_streaming` off this answer to decide whether to expect
    partials — so the `False` below is load-bearing rather than informational.
    Home Assistant asks unconditionally, and an unanswered `describe` makes the
    service invisible there.
    """
    attribution = ATTRIBUTION.get(
        engine.name, Attribution(name=engine.name, url="")
    )
    return Info(
        asr=[
            AsrProgram(
                name=engine.name,
                description=engine.description,
                attribution=attribution,
                installed=True,
                version=None,
                models=[
                    AsrModel(
                        name=engine.model,
                        description=engine.description,
                        attribution=attribution,
                        installed=True,
                        version=None,
                        languages=list(engine.languages),
                    )
                ],
                # Stated rather than left to a default: this service answers once
                # per utterance, and a client told otherwise waits for partials
                # that are never coming.
                supports_transcript_streaming=False,
            )
        ]
    )


class AsrHandler(AsyncEventHandler):
    """One connection's worth of Wyoming conversation.

    The sequence `crates/conduit-wyoming/src/stt.rs` writes is `audio-start`,
    one `audio-chunk` per buffer with the format repeated on each, then
    `audio-stop` — and it reads until a `transcript` event. That is what this
    implements. A `transcribe` event before the audio is accepted because Home
    Assistant sends one; Conduit puts the same `model` hint on `audio-start`
    instead, so both places are read.

    The connection is kept open after a transcript. Conduit stops reading once
    it has the final transcript and holds its own socket open until then, and
    hanging up from this side raced the transcript it had already produced.
    """

    def __init__(
        self,
        reader: asyncio.StreamReader,
        writer: asyncio.StreamWriter,
        *,
        engine: Engine,
        limits: Limits,
    ) -> None:
        super().__init__(reader, writer)
        self._engine = engine
        self._limits = limits
        self._language: str | None = None
        self._discard_audio()

    def _discard_audio(self) -> None:
        """Forgets the samples of the utterance just answered.

        Audio that survived `audio-stop` would prepend the previous utterance to
        the next one on the same connection, which reads as a recognizer
        inventing context.

        The language is not part of this. A `transcribe` event arrives *before*
        the `audio-start` it describes, so clearing it here threw away the one
        thing the client had bothered to say — it is cleared once it has been
        used instead.
        """
        self._buffers: list[bytes] = []
        self._samples = 0

    async def _refuse(self, text: str, code: str) -> bool:
        """Reports why this session cannot be served, then ends it.

        Both halves matter. Without the `error` event a client sees a connection
        that closed for no stated reason; without the close it waits out its own
        timeout for a transcript that is never coming — Conduit reports the
        latter as "connection closed before final transcript", which names the
        symptom and not the cause.
        """
        LOG.warning("refusing session: %s (%s)", text, code)
        await self.write_event(Error(text=text, code=code).event())
        return False

    def _check_model(self, requested: str | None) -> None:
        """Refuses a request for weights this process did not load.

        One process holds one model. Transcribing with a different one than was
        asked for is the quiet substitution that makes a comparison between two
        models meaningless.
        """
        if requested is None or requested == self._engine.model:
            return
        raise FormatMismatch(
            f"this process loaded `{self._engine.model}` and cannot serve "
            f"`{requested}`; run another instance for that model"
        )

    async def handle_event(self, event: Event) -> bool:
        try:
            return await self._dispatch(event)
        except FormatMismatch as mismatch:
            return await self._refuse(str(mismatch), "invalid-audio-format")

    async def _dispatch(self, event: Event) -> bool:
        if Describe.is_type(event.type):
            await self.write_event(describe(self._engine).event())
            return True

        if Transcribe.is_type(event.type):
            request = Transcribe.from_event(event)
            self._check_model(request.name)
            self._language = request.language
            return True

        if AudioStart.is_type(event.type):
            start = AudioStart.from_event(event)
            check_format(start.rate, start.width, start.channels)
            # Conduit's optional model hint rides here rather than on a
            # `transcribe` event, so it is read here too or its one client is
            # the one whose hint is ignored.
            self._check_model((event.data or {}).get("model"))
            self._discard_audio()
            return True

        if AudioChunk.is_type(event.type):
            chunk = AudioChunk.from_event(event)
            # Checked per chunk because Wyoming carries the format on each one
            # and a chunk is where the samples actually arrive: trusting
            # `audio-start` alone would transcribe audio nobody described.
            check_format(chunk.rate, chunk.width, chunk.channels)
            return await self._collect(chunk)

        if AudioStop.is_type(event.type):
            return await self._answer()

        LOG.debug("ignoring an event this service does not serve: %s", event.type)
        return True

    async def _collect(self, chunk: AudioChunk) -> bool:
        held = (self._samples + chunk.samples) / MODEL_SAMPLE_RATE
        if held > self._limits.max_seconds:
            # Refused on the chunk that crosses the line rather than after the
            # whole payload has been accepted into memory, which is the entire
            # point of having a limit.
            return await self._refuse(
                f"utterance exceeded ASR_MAX_SECONDS={self._limits.max_seconds}"
                f" at {held:.1f}s of audio",
                "audio-too-long",
            )
        self._buffers.append(chunk.audio)
        self._samples += chunk.samples
        return True

    async def _answer(self) -> bool:
        audio = b"".join(self._buffers)
        seconds = self._samples / MODEL_SAMPLE_RATE
        language = self._language
        self._language = None
        self._discard_audio()
        if not audio:
            # Not an error: a turn whose gate opened on silence recognized
            # nothing, and an empty final transcript says so. Leaving the client
            # waiting does not, and neither does asking a model to transcribe
            # zero samples.
            LOG.info("audio-stop with no audio; answering an empty transcript")
            await self.write_event(Transcript(text="").event())
            return True

        loop = asyncio.get_running_loop()
        try:
            # Off the event loop: transcription is seconds of blocking compute
            # and this process serves more than one connection.
            text = await loop.run_in_executor(
                None, self._engine.transcribe, to_float_samples(audio), language
            )
        except Exception as error:  # noqa: BLE001 - torch and the hub raise broadly
            LOG.exception("transcription failed after %.2fs of audio", seconds)
            return await self._refuse(f"transcription failed: {error}", "asr-failed")

        LOG.info("transcribed %.2fs of audio into %d characters", seconds, len(text))
        # One `transcript` event, which is what Conduit treats as final. A
        # `transcript-chunk` would be read as a partial and leave the turn
        # waiting for the transcript it had already been sent.
        await self.write_event(Transcript(text=text).event())
        return True


async def serve(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description="Wyoming ASR server")
    parser.add_argument(
        "--uri",
        default=os.environ.get("ASR_URI", "tcp://0.0.0.0:10300"),
        help="where to listen, e.g. tcp://0.0.0.0:10300",
    )
    arguments = parser.parse_args(argv)

    logging.basicConfig(level=os.environ.get("ASR_LOG", "INFO").upper())
    selection = engine_from_environment()
    limits = limits_from_environment()
    # Loaded before the listener opens, unlike speaker-id's lazy encoder: there
    # is no health route here to report "the model failed" on, so a process that
    # accepted a connection it cannot serve would fail as a hung turn instead of
    # as a startup error somebody can read.
    LOG.info(
        "serving %s (%s) on %s, at most %.0fs per utterance",
        selection.model,
        selection.engine,
        arguments.uri,
        limits.max_seconds,
    )
    engine = selection.load()

    server = AsyncServer.from_uri(arguments.uri)
    await server.run(
        lambda reader, writer: AsrHandler(
            reader, writer, engine=engine, limits=limits
        )
    )


if __name__ == "__main__":
    asyncio.run(serve())
