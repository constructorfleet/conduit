"""What the recognizer owes Conduit.

`crates/conduit-wyoming/src/stt.rs` is the client these tests stand in for. It
writes `audio-start`, then one `audio-chunk` per buffer with the format repeated
on every chunk, then `audio-stop`, and it waits for a `transcript` event — so
that exact sequence is what is exercised here, on the real wire encoding rather
than against a mocked handler.

The engine is a fake throughout. A test that needs a gigabyte of model weights
is a test that does not run, and none of the behaviour below is a property of
Canary: it is a property of the protocol, the refusals, and the selector.
"""

from __future__ import annotations

import asyncio
import struct
from typing import Any

import numpy as np
import pytest
from wyoming.asr import Transcribe, Transcript
from wyoming.audio import AudioChunk, AudioStart, AudioStop
from wyoming.event import Event, async_read_event, async_write_event
from wyoming.info import Describe, Info

from app import (
    DEFAULT_MAX_SECONDS,
    MODEL_CHANNELS,
    MODEL_SAMPLE_RATE,
    MODEL_WIDTH,
    AsrHandler,
    Limits,
    build_engine,
    engine_from_environment,
    limits_from_environment,
)


class EchoEngine:
    """An engine that reports what it was given rather than what was said.

    Transcription is the one thing a fake cannot do, so it does the next most
    useful thing: it proves the handler assembled the samples it was sent, in
    order, at the length it was sent. A real recognizer's accuracy is not
    something CI can assert without downloading it.
    """

    name = "echo"
    description = "a fake recognizer for tests"
    languages = ("en",)

    def __init__(self, model: str = "echo-1") -> None:
        self.model = model
        self.calls: list[tuple[int, str | None]] = []

    def transcribe(self, samples: np.ndarray, language: str | None) -> str:
        self.calls.append((samples.size, language))
        return f"{samples.size} samples"


class RecordingWriter:
    """Collects the bytes a handler writes, so they can be read back as events.

    Parsing the real bytes rather than intercepting `write_event` is deliberate:
    Conduit reads a newline-framed header and a `payload_length`, and a test
    that skipped the encoding would pass on an event Conduit cannot parse.
    """

    def __init__(self) -> None:
        self.buffer = bytearray()

    def write(self, data: bytes) -> None:
        self.buffer.extend(data)

    def writelines(self, chunks: Any) -> None:
        # What `async_write_event` actually calls: it hands the header line, the
        # framed data, and the payload over in one go.
        for chunk in chunks:
            self.buffer.extend(chunk)

    async def drain(self) -> None:
        return None

    def close(self) -> None:
        return None


async def written_events(writer: RecordingWriter) -> list[Event]:
    reader = asyncio.StreamReader()
    reader.feed_data(bytes(writer.buffer))
    reader.feed_eof()
    events = []
    while True:
        event = await async_read_event(reader)
        if event is None:
            return events
        events.append(event)


def pcm(seconds: float, rate: int = MODEL_SAMPLE_RATE, channels: int = 1) -> bytes:
    """Signed 16-bit little-endian samples, which is all Wyoming audio carries."""
    frames = int(rate * seconds)
    return struct.pack(f"<{frames * channels}h", *([1_000] * (frames * channels)))


@pytest.fixture
def engine() -> EchoEngine:
    return EchoEngine()


@pytest.fixture
def writer() -> RecordingWriter:
    return RecordingWriter()


def handler(engine: EchoEngine, writer: RecordingWriter, **limits: Any) -> AsrHandler:
    return AsrHandler(
        asyncio.StreamReader(),
        writer,  # type: ignore[arg-type]
        engine=engine,
        limits=Limits(**limits),
    )


async def feed(session: AsrHandler, *events: Any) -> list[bool]:
    """Hands the handler a sequence of events, reporting whether each was kept.

    `False` means the handler asked to disconnect, which is how a Wyoming server
    refuses a session it cannot serve.
    """
    kept = []
    for event in events:
        kept.append(await session.handle_event(event.event()))
    return kept


@pytest.mark.asyncio
async def test_describe_is_answered_with_the_loaded_engine_and_model(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # Home Assistant asks before it sends audio, and an unanswered `describe`
    # makes the service invisible there. Conduit never asks, so this costs it
    # nothing.
    await feed(handler(engine, writer), Describe())

    info = Info.from_event((await written_events(writer))[0])
    assert len(info.asr) == 1
    assert info.asr[0].name == "echo"
    assert [model.name for model in info.asr[0].models] == ["echo-1"]


@pytest.mark.asyncio
async def test_describe_says_transcript_streaming_is_not_supported(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # The service is batch-only. Advertising streaming it does not do would have
    # a client wait for partials that never arrive, so the refusal is stated in
    # the handshake rather than discovered by silence.
    await feed(handler(engine, writer), Describe())

    info = Info.from_event((await written_events(writer))[0])
    assert info.asr[0].supports_transcript_streaming is False


@pytest.mark.asyncio
async def test_audio_start_chunks_and_stop_produce_one_final_transcript(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # Exactly what conduit-wyoming's `transcribe` writes: a start, chunks that
    # each repeat the format, and a stop.
    session = handler(engine, writer)
    await feed(
        session,
        AudioStart(rate=MODEL_SAMPLE_RATE, width=MODEL_WIDTH, channels=MODEL_CHANNELS),
        AudioChunk(
            rate=MODEL_SAMPLE_RATE,
            width=MODEL_WIDTH,
            channels=MODEL_CHANNELS,
            audio=pcm(0.5),
        ),
        AudioChunk(
            rate=MODEL_SAMPLE_RATE,
            width=MODEL_WIDTH,
            channels=MODEL_CHANNELS,
            audio=pcm(0.5),
        ),
        AudioStop(),
    )

    events = await written_events(writer)
    assert [event.type for event in events] == ["transcript"]
    # One `transcript` event carrying `text` is the whole of what Conduit treats
    # as final; a `transcript-chunk` would be read as a partial instead.
    assert Transcript.from_event(events[0]).text == f"{MODEL_SAMPLE_RATE} samples"
    assert engine.calls == [(MODEL_SAMPLE_RATE, None)]


@pytest.mark.asyncio
async def test_a_session_stays_open_after_a_transcript(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # Conduit ends its own read once it has the final transcript, and closing
    # from this side raced that. Keeping the connection means a second utterance
    # reuses it, which is also what faster-whisper does.
    session = handler(engine, writer)
    kept = await feed(
        session,
        AudioStart(rate=MODEL_SAMPLE_RATE, width=MODEL_WIDTH, channels=MODEL_CHANNELS),
        AudioChunk(
            rate=MODEL_SAMPLE_RATE,
            width=MODEL_WIDTH,
            channels=MODEL_CHANNELS,
            audio=pcm(0.2),
        ),
        AudioStop(),
    )

    assert all(kept)


@pytest.mark.asyncio
async def test_a_second_utterance_on_one_connection_is_not_appended_to_the_first(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # State that survived `audio-stop` would make the second transcript contain
    # the first utterance, which reads as a recognizer hallucinating context.
    session = handler(engine, writer)
    for _ in range(2):
        await feed(
            session,
            AudioStart(
                rate=MODEL_SAMPLE_RATE, width=MODEL_WIDTH, channels=MODEL_CHANNELS
            ),
            AudioChunk(
                rate=MODEL_SAMPLE_RATE,
                width=MODEL_WIDTH,
                channels=MODEL_CHANNELS,
                audio=pcm(0.25),
            ),
            AudioStop(),
        )

    quarter = MODEL_SAMPLE_RATE // 4
    assert engine.calls == [(quarter, None), (quarter, None)]


@pytest.mark.asyncio
async def test_audio_at_another_sample_rate_is_refused_rather_than_resampled(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # Feeding 48 kHz samples to a 16 kHz recognizer does not fail: it returns
    # fluent nonsense, which an operator reads as a bad model rather than as a
    # misconfigured satellite. Refusing names the mismatch instead.
    session = handler(engine, writer)
    kept = await feed(
        session, AudioStart(rate=48_000, width=MODEL_WIDTH, channels=MODEL_CHANNELS)
    )

    events = await written_events(writer)
    assert [event.type for event in events] == ["error"]
    assert "48000" in events[0].data["text"]
    assert "16000" in events[0].data["text"]
    # The session is over: a client left waiting on a transcript that will never
    # come times out with no reason attached, and Conduit reports a clean close
    # as "connection closed before final transcript".
    assert kept == [False]
    assert engine.calls == []


@pytest.mark.asyncio
async def test_stereo_audio_is_refused_rather_than_mixed_down(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # Interleaved channels read as mono are a recording at double speed.
    session = handler(engine, writer)
    kept = await feed(
        session, AudioStart(rate=MODEL_SAMPLE_RATE, width=MODEL_WIDTH, channels=2)
    )

    events = await written_events(writer)
    assert events[0].type == "error"
    assert "channel" in events[0].data["text"]
    assert kept == [False]


@pytest.mark.asyncio
async def test_a_sample_width_other_than_16_bit_is_refused(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # `pcm_s16le` is the one encoding a Wyoming payload carries, and conduit's
    # provider advertises only that. Anything else would be decoded as noise.
    session = handler(engine, writer)
    kept = await feed(
        session, AudioStart(rate=MODEL_SAMPLE_RATE, width=4, channels=MODEL_CHANNELS)
    )

    assert (await written_events(writer))[0].type == "error"
    assert kept == [False]


@pytest.mark.asyncio
async def test_a_chunk_that_contradicts_audio_start_is_refused(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # Conduit repeats the format on every chunk because Wyoming reads it from
    # each one. A chunk that disagrees with the stream it belongs to is checked
    # rather than trusted, since a chunk is where the samples actually arrive.
    session = handler(engine, writer)
    kept = await feed(
        session,
        AudioStart(rate=MODEL_SAMPLE_RATE, width=MODEL_WIDTH, channels=MODEL_CHANNELS),
        AudioChunk(rate=8_000, width=MODEL_WIDTH, channels=MODEL_CHANNELS, audio=pcm(0.1)),
    )

    assert (await written_events(writer))[-1].type == "error"
    assert kept[-1] is False


@pytest.mark.asyncio
async def test_audio_chunks_without_a_start_still_carry_their_own_format(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # A client that skips `audio-start` is out of spec but harmless: the format
    # is on every chunk. Taking it from there rather than refusing keeps the
    # service usable by one, while a bad rate is still caught.
    session = handler(engine, writer)
    await feed(
        session,
        AudioChunk(
            rate=MODEL_SAMPLE_RATE,
            width=MODEL_WIDTH,
            channels=MODEL_CHANNELS,
            audio=pcm(0.1),
        ),
        AudioStop(),
    )

    assert (await written_events(writer))[0].type == "transcript"


@pytest.mark.asyncio
async def test_audio_longer_than_the_limit_is_refused_before_it_is_buffered(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # The samples are held in memory until `audio-stop`, so an unbounded stream
    # is an unauthenticated way to exhaust the host. The refusal lands on the
    # chunk that crosses the line rather than after the whole payload arrived.
    session = handler(engine, writer, max_seconds=1.0)
    kept = await feed(
        session,
        AudioStart(rate=MODEL_SAMPLE_RATE, width=MODEL_WIDTH, channels=MODEL_CHANNELS),
        AudioChunk(
            rate=MODEL_SAMPLE_RATE,
            width=MODEL_WIDTH,
            channels=MODEL_CHANNELS,
            audio=pcm(0.75),
        ),
        AudioChunk(
            rate=MODEL_SAMPLE_RATE,
            width=MODEL_WIDTH,
            channels=MODEL_CHANNELS,
            audio=pcm(0.75),
        ),
    )

    events = await written_events(writer)
    assert events[-1].type == "error"
    assert "1.0" in events[-1].data["text"]
    assert kept == [True, True, False]
    assert engine.calls == []


@pytest.mark.asyncio
async def test_audio_stop_with_no_audio_answers_an_empty_transcript(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # Not an error: a turn whose gate opened on silence recognized nothing, and
    # an empty final transcript is that answer. Leaving the client waiting is
    # not, and neither is asking the engine to transcribe zero samples.
    session = handler(engine, writer)
    await feed(
        session,
        AudioStart(rate=MODEL_SAMPLE_RATE, width=MODEL_WIDTH, channels=MODEL_CHANNELS),
        AudioStop(),
    )

    events = await written_events(writer)
    assert Transcript.from_event(events[0]).text == ""
    assert engine.calls == []


@pytest.mark.asyncio
async def test_a_transcribe_language_reaches_the_engine(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # Canary is multilingual, so the language a client names is the difference
    # between a transcript and a translation of it.
    session = handler(engine, writer)
    await feed(
        session,
        Transcribe(language="de"),
        AudioStart(rate=MODEL_SAMPLE_RATE, width=MODEL_WIDTH, channels=MODEL_CHANNELS),
        AudioChunk(
            rate=MODEL_SAMPLE_RATE,
            width=MODEL_WIDTH,
            channels=MODEL_CHANNELS,
            audio=pcm(0.1),
        ),
        AudioStop(),
    )

    assert engine.calls == [(MODEL_SAMPLE_RATE // 10, "de")]


@pytest.mark.asyncio
async def test_a_request_for_a_model_this_process_did_not_load_is_refused(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # Conduit's `wyoming` variant sends its optional `model` hint on
    # `audio-start`. One process holds one model, so honouring the hint is
    # impossible — and transcribing with a different model than was asked for is
    # the kind of quiet substitution that makes a benchmark meaningless.
    session = handler(engine, writer)
    kept = await feed(session, Transcribe(name="canary-1b-v2"))

    events = await written_events(writer)
    assert events[0].type == "error"
    assert "canary-1b-v2" in events[0].data["text"]
    assert "echo-1" in events[0].data["text"]
    assert kept == [False]


@pytest.mark.asyncio
async def test_the_model_hint_conduit_puts_on_audio_start_is_checked_there(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # conduit-wyoming does not send a `transcribe` event at all: its optional
    # `model` hint rides on `audio-start` beside the format. Checking only a
    # `transcribe` name would let Conduit's hint through unread, so the one
    # client this service exists for would be the one it silently ignored.
    session = handler(engine, writer)
    kept = await session.handle_event(
        Event(
            type="audio-start",
            data={
                "rate": MODEL_SAMPLE_RATE,
                "width": MODEL_WIDTH,
                "channels": MODEL_CHANNELS,
                "encoding": "pcm_s16le",
                "model": "canary-1b-v2",
            },
        )
    )

    events = await written_events(writer)
    assert events[0].type == "error"
    assert "canary-1b-v2" in events[0].data["text"]
    assert kept is False


@pytest.mark.asyncio
async def test_a_request_naming_the_loaded_model_is_served(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    session = handler(engine, writer)
    kept = await feed(
        session,
        Transcribe(name="echo-1"),
        AudioStart(rate=MODEL_SAMPLE_RATE, width=MODEL_WIDTH, channels=MODEL_CHANNELS),
        AudioChunk(
            rate=MODEL_SAMPLE_RATE,
            width=MODEL_WIDTH,
            channels=MODEL_CHANNELS,
            audio=pcm(0.1),
        ),
        AudioStop(),
    )

    assert all(kept)
    assert (await written_events(writer))[0].type == "transcript"


@pytest.mark.asyncio
async def test_an_unknown_event_is_ignored_rather_than_fatal(
    engine: EchoEngine, writer: RecordingWriter
) -> None:
    # Wyoming grows events, and a client that sends one this service has never
    # heard of is not misbehaving.
    session = handler(engine, writer)

    assert await session.handle_event(Event(type="ping")) is True
    assert await written_events(writer) == []


@pytest.mark.asyncio
async def test_an_engine_that_fails_reports_an_error_rather_than_hanging(
    writer: RecordingWriter
) -> None:
    class Broken(EchoEngine):
        def transcribe(self, samples: np.ndarray, language: str | None) -> str:
            raise RuntimeError("the weights are not there")

    session = handler(Broken(), writer)
    kept = await feed(
        session,
        AudioStart(rate=MODEL_SAMPLE_RATE, width=MODEL_WIDTH, channels=MODEL_CHANNELS),
        AudioChunk(
            rate=MODEL_SAMPLE_RATE,
            width=MODEL_WIDTH,
            channels=MODEL_CHANNELS,
            audio=pcm(0.1),
        ),
        AudioStop(),
    )

    events = await written_events(writer)
    assert events[0].type == "error"
    assert "the weights are not there" in events[0].data["text"]
    # A failure ends the session for the same reason a bad format does: a client
    # holding the socket open for a transcript would otherwise wait out its own
    # timeout with no reason recorded.
    assert kept[-1] is False


@pytest.mark.asyncio
async def test_conduits_own_bytes_over_a_socket_produce_a_transcript(
    engine: EchoEngine,
) -> None:
    """The end-to-end check, written in the client's encoding rather than ours.

    `write_wyoming_event` in `crates/conduit-wyoming/src/protocol.rs` emits
    `{"type":…,"data":{…}}` inline with no `data_length` key at all, and its
    payload form adds `data_length: 0` — which is not how the `wyoming` package
    writes an event. Every other test here builds events through that package,
    so none of them would catch a server that could only parse its own dialect.
    These are the header lines Conduit actually puts on the wire, byte for byte.
    """
    server = await asyncio.start_server(
        lambda reader, writer: asyncio.ensure_future(
            AsrHandler(reader, writer, engine=engine, limits=Limits()).run()
        ),
        host="127.0.0.1",
        port=0,
    )
    port = server.sockets[0].getsockname()[1]
    reader, writer = await asyncio.open_connection("127.0.0.1", port)

    fmt = f'"rate":{MODEL_SAMPLE_RATE},"width":{MODEL_WIDTH},"channels":{MODEL_CHANNELS}'
    audio = pcm(0.3)
    writer.write(
        f'{{"type":"audio-start","data":{{{fmt},"encoding":"pcm_s16le"}}}}\n'.encode()
    )
    writer.write(
        f'{{"type":"audio-chunk","data":{{{fmt}}},"data_length":0,'
        f'"payload_length":{len(audio)}}}\n'.encode()
    )
    writer.write(audio)
    writer.write(b'{"type":"audio-stop","data":{}}\n')
    await writer.drain()

    transcript = await asyncio.wait_for(async_read_event(reader), timeout=5)
    assert transcript is not None
    assert transcript.type == "transcript"
    assert Transcript.from_event(transcript).text == f"{int(MODEL_SAMPLE_RATE * 0.3)} samples"

    writer.close()
    server.close()
    await server.wait_closed()


@pytest.mark.asyncio
async def test_a_bad_rate_in_conduits_own_bytes_is_refused_over_the_socket(
    engine: EchoEngine,
) -> None:
    # The refusal has to survive the wire too: an `error` event Conduit cannot
    # parse is a connection that closed for no stated reason.
    server = await asyncio.start_server(
        lambda reader, writer: asyncio.ensure_future(
            AsrHandler(reader, writer, engine=engine, limits=Limits()).run()
        ),
        host="127.0.0.1",
        port=0,
    )
    port = server.sockets[0].getsockname()[1]
    reader, writer = await asyncio.open_connection("127.0.0.1", port)

    writer.write(
        b'{"type":"audio-start","data":{"rate":44100,"width":2,"channels":1,'
        b'"encoding":"pcm_s16le"}}\n'
    )
    await writer.drain()

    refusal = await asyncio.wait_for(async_read_event(reader), timeout=5)
    assert refusal is not None
    assert refusal.type == "error"
    assert "44100" in refusal.data["text"]
    # And then the server hangs up, so nothing waits on a transcript.
    assert await asyncio.wait_for(async_read_event(reader), timeout=5) is None

    writer.close()
    server.close()
    await server.wait_closed()


def test_an_unknown_engine_names_the_ones_this_image_serves() -> None:
    # The selector is the extension point. Adding Qwen or Granite is a class and
    # a branch here, and nothing Conduit knows about changes.
    with pytest.raises(RuntimeError) as refused:
        build_engine("whisper.cpp", model="", cache=None, device="cpu")  # type: ignore[arg-type]

    assert "whisper.cpp" in str(refused.value)
    assert "canary" in str(refused.value)


def test_the_engine_selector_defaults_to_canary(monkeypatch: Any) -> None:
    # Recorded as a test because the default is what an operator gets when they
    # set only a port, and changing it silently would change every deployment.
    monkeypatch.delenv("ASR_ENGINE", raising=False)
    monkeypatch.delenv("ASR_MODEL", raising=False)
    selected = engine_from_environment()

    assert selected.engine == "canary"
    assert selected.model == "nvidia/canary-1b-v2"


def test_an_unset_max_seconds_falls_back_to_the_default(monkeypatch: Any) -> None:
    # An unset compose variable arrives as an empty string rather than as
    # absent, and `float("")` is a crash loop.
    monkeypatch.setenv("ASR_MAX_SECONDS", "")
    assert limits_from_environment().max_seconds == DEFAULT_MAX_SECONDS


def test_a_max_seconds_that_is_not_a_number_is_refused(monkeypatch: Any) -> None:
    monkeypatch.setenv("ASR_MAX_SECONDS", "two minutes")
    with pytest.raises(RuntimeError) as refused:
        limits_from_environment()
    assert "ASR_MAX_SECONDS" in str(refused.value)


def test_a_max_seconds_of_zero_is_refused_rather_than_accepting_nothing(
    monkeypatch: Any,
) -> None:
    # Zero is not "no limit", it is a service that refuses every utterance — and
    # a negative one is the same thing with a worse message.
    monkeypatch.setenv("ASR_MAX_SECONDS", "0")
    with pytest.raises(RuntimeError):
        limits_from_environment()


def test_the_engine_and_model_come_from_the_environment(monkeypatch: Any) -> None:
    monkeypatch.setenv("ASR_ENGINE", "canary")
    monkeypatch.setenv("ASR_MODEL", "nvidia/parakeet-tdt-0.6b-v2")
    selected = engine_from_environment()

    assert selected.model == "nvidia/parakeet-tdt-0.6b-v2"
