# conduit-elevenlabs

Speech synthesis and batch transcription over the ElevenLabs API.

| Provider | Endpoint | Trait |
| --- | --- | --- |
| `ElevenLabsTts` | `POST /v1/text-to-speech/{voice_id}/stream` | `TextToSpeech` |
| `ElevenLabsStt` | `POST /v1/speech-to-text` | `SpeechToText` |

## Why a Separate Crate

`conduit-openai` reaches most speech servers by changing a base URL. This one is
not among them: neither endpoint is a chat-completions endpoint wearing a
different host, and the differences are structural rather than cosmetic.

| | OpenAI speech | ElevenLabs |
| --- | --- | --- |
| Credential | `Authorization: Bearer …` | `xi-api-key` header |
| Voice | a body field | **a URL path segment** |
| Output format | `response_format` in the body | `output_format` in the query |
| Voice controls | one `speed` | `stability`, `similarity_boost`, `style`, `use_speaker_boost`, `speed` |
| Voice catalogue | six fixed names | `GET /v1/voices`, per account |
| Transcription request | `model` | `model_id`, and `language_code` rather than `language` |
| Transcription response | `{text, language}` | `{text, language_code, language_probability, words}` |

Everything both crates share — sending an authenticated request, classifying a
failure, deciding whether a retry could help — comes from `conduit-http`, so only
the translation lives here.

## The Voice Id Is a Path Segment

This is the reason `voice_id.rs` exists, and the one thing in this crate that is
a security boundary rather than a correctness one.

A voice id is interpolated into `/v1/text-to-speech/{voice_id}/stream`. It is
also not a constant: it arrives from a stored provider definition, a pipeline
node's settings, a synthesis request, or the account's own voice catalogue. A
value containing `../` — or `..%2f`, or `%2e%2e%2f` — would move the request to a
*different API path* with the account's credential attached.

Every voice id is therefore checked against an allowlist before it can reach a
URL: ASCII letters, digits, `-`, and `_`, up to 64 characters. An allowlist
rather than a denylist, because a denylist has to anticipate every spelling of
"go up a level" and an allowlist has to anticipate nothing. Rejection is an
`Error::Config` naming the `voice_id` field, so an operator is told which field
is wrong rather than shown a 404 from a path nobody meant to call.

The check runs at three points, because there are three ways in:

- **Construction.** A stored definition carrying a traversal attempt fails when
  it is saved, while the operator is still looking at the form.
- **Synthesis.** A pipeline node's `voice` setting is checked before the path is
  built, and no request is sent.
- **The catalogue.** `GET /v1/voices` is *not* a trusted input — a cloned voice's
  id is chosen by whatever created it — so an entry whose id could not be a path
  segment is dropped rather than offered in an operator's menu.

`a_voice_id_can_never_escape_its_path_segment` covers sixteen spellings of the
attempt, and `a_traversal_attempt_never_reaches_the_wire` proves the refusal end
to end against a server that records every path it is asked for, including ones
it does not serve.

## Configuration

`ElevenLabsConfig` describes one *account*, not one capability, so a deployment
using both capabilities describes it once. It carries `base_url`, `api_key`, the
registration `name`, an optional `label`, connect and read timeouts, advertised
`models`, a `voice_id`, a `voices` catalogue, and `default_settings`.

The key travels as `xi-api-key` via `conduit_http::Credential::header`, which is
what keeps it out of a log line: `Credential`'s `Debug` prints `<redacted>`,
while `HttpConfig::headers` prints itself, so the key is never pinned as an
ordinary header. A bearer token is the most common way to misconfigure this
vendor — it is accepted by the transport and rejected by the API with a 401 that
says nothing about the header.

Naming no models advertises the current ones rather than leaving an empty menu:
`eleven_flash_v2_5` first, because a spoken turn is latency-bound and the
expressive `eleven_multilingual_v2` should be opt-in. `scribe_v2_realtime` is
deliberately absent from `DEFAULT_STT_MODELS` — it is a websocket protocol this
crate does not implement, and advertising it would produce a provider that fails
every utterance with a 4xx an operator cannot act on.

There is no built-in default voice. Unlike OpenAI's six fixed names, an
ElevenLabs voice id is account-scoped, so a hard-coded fallback would be a guess
at another account's data. A provider with no `voice_id` and no catalogue refuses
with a message naming the field. `ElevenLabsTts::load_voices` reads the account's
catalogue when a caller has an async context and wants the menu; it is not done
during construction, because a provider that cannot be built cannot report its
own health.

## Audio Format

Always PCM, never MP3, and that is a decision rather than an oversight.
`conduit_core::audio::Encoding` has no MP3 variant, so an MP3 chunk could only be
labelled as something it is not — and a mislabelled chunk is worse than a refused
request, because it plays back as noise several stages later with nothing pointing
here. PCM is also what the pipeline's interchange format already is, so the
common case needs no transcode at all.

The request asks for the nearest rate the vendor offers among 8 000, 16 000,
22 050, 24 000, 32 000, 44 100, and 48 000 Hz, ties going to the lower. When that
is not the rate the request asked for, or when the request asked for stereo, the
chunks report **what was actually produced** — the documented contract for a
provider that cannot honour a requested format — and a `tracing::info!` records
the mismatch, because a pipeline resampling every utterance is usually a
misconfiguration.

`opus_48000_*` is offered by the endpoint and left unused: the documentation does
not say whether those bytes are Ogg-encapsulated or raw frames, `Encoding::Opus`
means raw frames here, and mislabelled Opus decodes to silence.

## Streaming and Barge-In

Synthesis uses the streaming endpoint rather than the buffered one, so a spoken
turn begins before synthesis finishes. Audio arrives as raw PCM with no framing at
all, so a chunk is however much has landed, forwarded immediately.

Two behaviours follow from that, and both are tested:

- **Dropping the stream stops the synthesis.** Dropping the chunk stream drops the
  response body, which closes the connection and stops the vendor generating —
  which is how barge-in silences the assistant mid-sentence. The test asserts on
  the *server* observing the hangup, not merely on chunks ceasing to arrive, so a
  provider that buffered the whole response first could not pass it.
- **A lost turn is an error item, reported once.** Audio that stops arriving
  partway through becomes an `Err` on the stream rather than a clean end: a turn
  that lost its voice halfway must be distinguishable from one that finished
  speaking, or the pipeline marks the turn done and waits for a reply to half a
  sentence. The stream then *ends* — a failed `reqwest` body reports the same
  failure on every poll, so forwarding it verbatim would yield an endless stream
  of identical errors and a caller draining it would spin rather than give up.

## Transcription

The endpoint takes a complete recording as a multipart upload, so the utterance is
buffered, packaged into a WAV container by `conduit_core::wav::package`, and
uploaded with `model_id` and an optional `language_code`. One final transcript is
emitted.

`language_probability` deliberately does **not** become `Transcript::confidence`.
It is confidence in the detected *language*, not in the transcript, and a caller
thresholding on `confidence` would otherwise be thresholding on the wrong number.

## Settings

Synthesis declares `stability`, `similarity_boost`, `style`, `use_speaker_boost`,
and `language_code`, individually rather than as one open object, so an operator
who writes `similarity-boost` is told at save time instead of having it silently
dropped by the vendor. Bounds are the vendor's documented ones, so a value out of
range is refused here rather than becoming a 422 mid-turn. Unset controls are
*omitted* from the request, because the vendor's defaults are per-voice and
sending `stability: 0.5` because nothing was configured would overwrite whatever
the operator tuned on the voice itself.

`speed` is conspicuously absent from the schema: it is `SynthesisRequest::rate`,
read from the request. Declaring it as well would send the same control twice with
the API choosing a winner.

Transcription declares `diarize`, `num_speakers`, `tag_audio_events`,
`temperature`, `seed`, and `no_verbatim`. The absences are the point:

- `webhook`, `webhook_id`, `source_url`, and `use_multi_channel` change the
  *shape of the response* — an acknowledgement, or `transcripts[]` instead of
  `text`. A provider that accepted one would report an empty utterance as a
  success.
- `additional_formats` asks for SRT and the like, which a spoken turn has nowhere
  to put.
- `entity_detection`, `entity_redaction`, `keyterms`, and `detect_speaker_roles`
  each carry a documented surcharge, so they are not things to enable by
  mistyping a schema.

## Accepted Limitations

**Realtime websocket transcription is not implemented.** `scribe_v2_realtime` is a
different protocol with partial-transcript semantics — a persistent socket
carrying interim results that are later revised — rather than a setting on the
batch endpoint, so it belongs in its own module with its own tests rather than
bolted onto a multipart upload. Until it exists, `ElevenLabsStt` reports one final
transcript and no partials, because it genuinely has none: a provider that
invented them would make the pipeline look more responsive than it is.

**Transcription buffers the whole utterance.** The endpoint takes a file, so
latency is bounded below by the length of what was said. This is the same
limitation the realtime protocol above would lift.

**MP3, μ-law, A-law, and Opus output are unavailable.** `Encoding` can name none
of them except Opus, whose framing the vendor does not document. Requesting any
non-PCM encoding is refused with a message naming `PcmS16Le`, rather than served
as a mislabelled chunk.

**Word timings, entities, and speaker labels are dropped.** `Transcript` carries
one text and one optional offset, so word-level detail has nowhere to go.

**Sample rates the vendor does not offer are approximated, not resampled.** The
nearest offered rate is requested and reported honestly; whatever resampling a
deployment needs happens downstream where the interchange format is enforced.

## Health

`health()` reads `GET /v1/voices`. `/v1/models` returns 404 on this API, and
nothing here is reachable unauthenticated — a probe confirmed `/v1/voices`
answers 200 without a key and 401 with a bad one — so the catalogue is the
cheapest call that exercises the credential as well as the connection. A check
that skipped the key would report a rejected key as healthy, which is worse than
not checking at all. Transcribing a sample would work too, and would bill the
operator for a health check.

A health reason never contains the API key, and neither does a failure message or
the provider's own `Debug`; there is a test for each.
