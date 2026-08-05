# Conduit Wyoming ASR

NVIDIA Canary, Qwen audio, and IBM Granite Speech ship as model weights with no
hosted endpoint, and Conduit's speech-to-text providers reach a *service*. This
is that service: one [Wyoming](https://github.com/rhasspy/wyoming) server in
front of whichever set of weights the deployment chose.

**Conduit needs no changes to use it.** `SttVariant::Wyoming` already takes a
`url`, so this is reachable today.

```
docker compose --profile wyoming-asr up
```

Then create a provider definition of type `wyoming` whose `url` is
`tcp://wyoming-asr:10300`, and add a Speech-to-text stage to a pipeline.

## The Conduit-side config

The `wyoming` STT variant, with `url` required and the rest optional:

```json
{
  "type": "wyoming",
  "url": "tcp://wyoming-asr:10300",
  "streaming": false
}
```

- **`url`** must be `tcp://host:port`. Any other scheme is refused when the
  provider is built.
- **`streaming`** must be `false` or absent. This service is batch-only (see
  below), and it says so in its own handshake.
- **`model`** is best left unset. Conduit sends it as a hint on `audio-start`,
  one process holds exactly one model, and a hint naming a different one is
  **refused** rather than quietly served by the model that is loaded. Set it only
  to the value of `ASR_MODEL`, as an assertion that you reached the process you
  meant to.

Because this is a plain Wyoming ASR server it also works as-is with Home
Assistant's Wyoming integration.

## Engines

`ASR_ENGINE` picks the backend at startup. One process serves one model.

| Engine | Framework | Default model | Model licence |
| --- | --- | --- | --- |
| `canary` | NeMo | `nvidia/canary-1b-v2` | **CC-BY-4.0** |

Canary is the worked example, and the shape the others follow.

### Model licences

The code here is licensed with the rest of Conduit. **The weights are not**, and
their terms are yours to honour:

- **NVIDIA Canary** (`nvidia/canary-1b-v2`) is
  [CC-BY-4.0](https://creativecommons.org/licenses/by/4.0/). Commercial use is
  permitted **and attribution is required** — if you ship a product whose speech
  recognition is Canary, you must credit NVIDIA. The service reports the
  attribution in its `describe` response so a client can pass it on, but that
  does not discharge the obligation for a user-facing product.
- **Qwen audio** and **IBM Granite Speech**, when added, carry their own terms
  per model. Granite Speech is Apache-2.0 at the time of writing and Qwen's
  varies by release; check the model card for the exact ID you load rather than
  assuming the family's licence.

Pointing `ASR_MODEL` at some other checkpoint is supported and its licence is
then also yours to check. Nothing here inspects one.

### Adding an engine

`build_engine` in [`app.py`](app.py) is the whole seam. A Qwen or Granite backend
is a class with a `transcribe(samples, language) -> str` method, an entry in
`DEFAULT_MODELS`, an `ATTRIBUTION` entry, and a branch in that function. The
protocol, the refusals, and everything Conduit knows about are unchanged —
Conduit points at a `url` and never names the backend, so a new engine is a tag
on an image and nothing the pipeline or the editor has to learn.

Two things a new engine owes the rest of the file: it takes **mono 16 kHz float
samples in `-1.0..=1.0`**, and its `model` attribute is the exact string a client
may name.

## Streaming or batch: batch, and it says so

**This service is batch-only.** It buffers an utterance until `audio-stop`,
transcribes it once, and answers with a single `transcript` event. It never sends
`transcript-chunk`.

That is the honest shape for these models rather than a shortcut. Canary is an
offline encoder-decoder that attends over a whole utterance; there is no partial
hypothesis to emit halfway through, and faking one by re-transcribing a growing
prefix would multiply the compute by the number of chunks and still produce
partials that jump around as the context grows. A streaming recognizer is a
different class of model, not a flag on this one.

So the flag is made honest in both directions:

- `describe` reports `supports_transcript_streaming: false`, which is what Home
  Assistant reads before it decides what to ask for.
- Conduit gates partials on its own per-request `partials` option, and a request
  that asks for them simply receives the one final transcript rather than
  silently waiting for chunks that are not coming.

Set `streaming: false` in the provider definition. If a streaming engine is added
later it will advertise `true` from the same handshake, and no Conduit-side
configuration will have to change to find out.

## Refusals

The service refuses rather than resamples:

| Refusal | Why |
| --- | --- |
| A sample rate other than 16 kHz | Feeding a 16 kHz recognizer 48 kHz samples does not fail — it returns a fluent transcript of nothing that was said, which an operator reads as a broken model rather than as a misconfigured satellite. |
| More than one channel | Interleaved channels read as mono are a recording at double speed. |
| A sample width other than 2 bytes | `pcm_s16le` is the one encoding a Wyoming payload carries, and Conduit's provider advertises only that. |
| A `model` naming weights this process did not load | Transcribing with a different model than was asked for is the quiet substitution that makes a comparison between two models meaningless. |
| An utterance longer than `ASR_MAX_SECONDS` | Samples are held in memory until `audio-stop`, so without a ceiling an unauthenticated client can exhaust the host one chunk at a time. Refused on the chunk that crosses the line, not after the whole payload arrived. |

Each is reported as a Wyoming `error` event naming what arrived and what was
wanted, and then the connection is closed — a client left holding the socket open
for a transcript that is never coming times out with no reason recorded, which
reaches Conduit as "connection closed before final transcript".

`audio-stop` with no audio at all is *not* an error: a turn whose gate opened on
silence recognized nothing, and an empty final transcript is that answer.

## The protocol, as implemented

Only what Conduit sends, plus `describe` so Home Assistant can see the service.

| Received | Behaviour |
| --- | --- |
| `describe` | Answers `info` with the engine, the one loaded model, its languages, its attribution, and `supports_transcript_streaming: false`. |
| `transcribe` | Optional. Records the `language`; refuses a `name` that is not the loaded model. Home Assistant sends this; Conduit does not. |
| `audio-start` | Checks rate, width, channels, and Conduit's `model` hint. Starts a fresh utterance. |
| `audio-chunk` | Checked again per chunk, because Wyoming carries the format on each one and a chunk is where the samples actually arrive. Buffered under the length limit. |
| `audio-stop` | Transcribes and answers one `transcript`. |
| anything else | Ignored. Wyoming grows events, and a client sending one this service has not heard of is not misbehaving. |

The connection stays open after a transcript, so a second utterance reuses it.
Conduit stops reading once it has the final transcript and holds its own socket
open until then; hanging up from this side raced the transcript it had already
produced.

## Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `ASR_ENGINE` | `canary` | Which backend to load. |
| `ASR_MODEL` | per engine | Model the engine loads. |
| `ASR_DEVICE` | `cpu` | `cuda` on the GPU image. Startup fails loudly if CUDA was asked for and is not there, because a GPU image silently running on the CPU transcribes an utterance in the time a conversation has already moved on. |
| `ASR_MODEL_DIR` | `/models` | Model cache, so a restart does not re-download several gigabytes. |
| `ASR_MAX_SECONDS` | `120` | Longest utterance accepted. |
| `ASR_URI` | `tcp://0.0.0.0:10300` | Where to listen. `unix://` and `stdio://` also work. |
| `ASR_LOG` | `INFO` | Log level. |

The model loads **at startup**, before the listener opens, unlike
`services/speaker-id/`'s lazy encoder. There is no health route here to report
"the model failed" on, so a process that accepted a connection it cannot serve
would fail as a hung turn rather than as a startup error somebody can read.
Expect the first start of a fresh volume to take a while.

There is no authentication. Wyoming has none, which is why the compose file does
not publish the port: Conduit reaches it over the compose network by service name
and a mapping would put an unauthenticated model server on your LAN.

## Images

One Dockerfile builds both tags, because two would drift and only one of them
would be tested.

```
# CPU
docker build -t conduit-wyoming-asr:canary services/wyoming-asr

# GPU
docker build -t conduit-wyoming-asr:canary-gpu \
  --build-arg BASE_IMAGE=nvidia/cuda:12.6.3-runtime-ubuntu24.04 \
  --build-arg TORCH_INDEX=https://download.pytorch.org/whl/cu126 \
  --build-arg DEVICE=cuda \
  services/wyoming-asr
```

NeMo and torch dominate the image size, and Canary on a CPU is slow enough that
the GPU tag is the realistic one for a live pipeline. A transformers-based engine
can skip NeMo entirely with `--build-arg ENGINE_REQUIREMENTS=…`.

## Tests

The tests inject their own engine, so they download no model weights and touch no
network:

```
python -m venv .venv && .venv/bin/pip install -r requirements-dev.txt
.venv/bin/python -m pytest
```

Two of them speak to a real socket using the exact header bytes
`crates/conduit-wyoming/src/protocol.rs` writes, which differ from what the
`wyoming` package's own writer produces — so a server that could only parse its
own dialect would fail there.

What they do not prove is transcription accuracy, or that Canary loads: both need
the weights, and a test that needs a gigabyte of download is a test that will not
run.
