# conduit-marytts

Speech synthesis over a self-hosted [MaryTTS](http://mary.dfki.de/) server.

| Provider | Endpoint | Trait |
| --- | --- | --- |
| `MaryTts` | `POST /process` | `TextToSpeech` |

## Why This Crate Exists

There is no API key here, and that is the entire point. Every other synthesizer
Conduit can reach either belongs to a vendor or fronts one, which means a
deployment that has promised its users their voices do not leave the building has
to choose between that promise and having an assistant that talks. MaryTTS is a
Java server an operator runs themselves, so the promise costs nothing.

What it costs instead is latency and voices, both discussed below.

## Configuration

`MaryTtsConfig` carries `base_url`, the registration `name`, an optional `label`,
an optional `voice`, a `locale`, and connect and read timeouts.

There is no credential field. MaryTTS has no authentication of its own, and
`Credential::None` is passed explicitly rather than left as a default, so a
reader can tell the difference between "there is no key" and "somebody forgot the
key". A deployment that wants one puts the server behind a reverse proxy, which
is the only place it can live.

`base_url` takes no version prefix: MaryTTS serves `/process` at the root, and
59125 is the port it binds when started without a `socket.port` override.

Naming no `voice` sends no `VOICE` parameter at all, which asks the server for
its own default for the locale. This crate deliberately guesses no name, because
**MaryTTS ships with no voices** — they are installed as separate jars — so any
constant here would be wrong on some installs and right by luck on others.

The read timeout defaults to 120 seconds, which is longer than a streaming
provider would want. It has to be: it bounds an entire synthesis rather than the
gap between two chunks, for the reason in the next section.

## The Wire Format

Verified against the MaryTTS server source rather than inferred from a client:
`MaryHttpServer` registers the handlers, `SynthesisRequestHandler` reads the
parameters, and `InfoRequestHandler` serves the catalogues.

| Parameter | Value |
| --- | --- |
| `INPUT_TEXT` | the utterance |
| `INPUT_TYPE` | `TEXT` |
| `OUTPUT_TYPE` | `AUDIO` |
| `AUDIO` | `WAVE_FILE` |
| `LOCALE` | `en_US`, and required whenever no voice is named |
| `VOICE` | omitted when nothing was configured or requested |
| `STYLE` | only when the request carries the `style` setting |

**The request is a POST with a form body, not a GET with a query string.**
`BaseHttpRequestHandler` accepts both methods and parses a POST entity as
URL-encoded parameters — but only when the URI carries no query of its own, so
the two cannot be mixed. POST is the right choice because `INPUT_TEXT` is the one
parameter with no length bound: servers and proxies cap a URL somewhere between
4 and 8 KB, and a transform-heavy pipeline produces utterances longer than that.
In a query string those replies would be truncated or refused outright.

`WAVE_FILE` rather than `WAVE_STREAM`, because there is no such thing.
`MaryRuntimeUtils.getAudioFileFormatTypes()` appends the `_STREAM` suffix for MP3
and Vorbis only, and neither of those is samples this crate could decode.

The catalogues are `text/plain`, one item per line — MaryTTS predates the
assumption that an API answers in JSON. A `/voices` line is name, locale, gender,
and type, with a fifth domain field on unit-selection voices:

```text
cmu-slt-hsmm en_US female hmm
dfki-pavoque-neutral de male unitselection general
```

Only the first two fields are read. A line this crate cannot parse is skipped
rather than failing the whole catalogue: one unrecognized line from a server
build we have not seen should not leave an operator with an empty voice menu.

## Audio

`/process` returns a WAV *file* — a RIFF header, then samples — and both halves
of that matter.

The header is not audio. Forwarding it as though it were prepends 44 bytes of
`RIFF`/`WAVE`/`fmt ` to the utterance, which is an audible click before every
reply.

And **the rate in that header is the voice's, not the pipeline's.** MaryTTS
voices are built at whatever rate their source recordings used — 16 kHz for some
HMM voices, 22.05 kHz and 44.1 kHz for others — and the server resamples to
nothing on the way out. Playing 22.05 kHz samples as if they were the pipeline's
16 kHz stretches the utterance and drops the pitch by roughly a fifth. That is
the same bug the speaker-enrollment path already had to fix, so the rate and
channel count are read out of the header and the samples converted to the
interchange format, whatever they turn out to be.

Neither step is implemented here. `conduit_core::wav::parse` reads the container
and `conduit_core::pcm::to_interchange` does the conversion, both already tested;
a second copy of either would be a second thing to get wrong.

## Streaming

**This provider does not stream, and says so rather than looking as though it
does.** `/process` with `WAVE_FILE` computes the whole utterance and answers with
one file. There is no incremental framing to forward and no streaming audio
format that is also PCM, so `synthesize` returns a stream that yields exactly one
`SpeechChunk`, once synthesis has finished.

The consequence is the one worth stating plainly: **time to first audio is the
time to synthesize the entire reply**, and it grows with the length of that
reply. A short confirmation is imperceptible; a long answer is a silence the user
sits through before hearing a word, where a streaming provider would have started
speaking. A deployment that needs speech to begin sooner should split the text
upstream and synthesize the parts — a decision this provider does not make on its
caller's behalf, because sentence segmentation belongs to whoever knows the
language and the pipeline.

A mid-stream failure still arrives as an error item on the stream rather than as
an empty one. A caller that received an empty stream would render the turn as the
assistant having chosen to say nothing, which is indistinguishable from success
and worse than an error.

## Security

`VOICE` and `LOCALE` are chosen by whoever configured the provider or sent the
turn, and both reach the server as request parameters. They are checked against a
strict allowlist — letters, digits, `-`, `_`, `.` for a voice; a two- or
three-letter language with optional alphanumeric region and variant for a locale
— rather than escaped, because an allowlist states what is permitted while an
escape only states what one author remembered to escape. A rejection is
`Error::Config` naming the offending field, and it happens both when the provider
is built and when a request is served, so a bad configuration is caught at
start-up rather than the first time somebody speaks.

There is no credential to protect, so this crate invents none.

## Health

`health()` issues `GET /version`. It is the cheapest thing the server will
answer, and unlike `/voices` it does not depend on any voice being installed. A
server that is down, wedged, or behind a proxy that is refusing reports
`Unhealthy` with the reason rather than looking fine until someone speaks, and a
server still loading its voices — up at the socket, unable to synthesize —
reports the status it actually returned. A `200` with an empty body reports
`Degraded`, because something is answering on that port but it is not MaryTTS.

## Accepted Limitations

**No streaming, therefore no early first audio.** Described above. It is a
property of the endpoint, not of this implementation, and it is the main reason
to prefer a Wyoming or OpenAI-compatible synthesizer where one is available.

**`rate` is ignored.** `SynthesisRequest::rate` is a speaking-rate multiplier,
and `/process` has no parameter for one. MaryTTS can vary duration through its
audio effects mechanism — `effect_Rate_selected` and friends — but the effect
list is configured per install via `audioeffects.classes.list`, so an effect this
crate sent might not be loaded on the server it was sent to. Silently ignoring a
requested rate is the honest failure here; the alternative is refusing every
request that carries one.

**The voice catalogue is not fetched automatically.** Constructing a provider is
synchronous and must not depend on a server being up, so `MaryTts::new` starts
with whatever voice was configured and nothing more. Call `refresh_catalogue` to
read the server's real list. Until it is called, an operator screen shows only
the configured voice.

**One voice per request, and the locale follows the voice.** A voice determines
its own locale, and sending a `LOCALE` that disagrees with the `VOICE` is how a
MaryTTS request gets rejected — so when a request names a voice from the
catalogue, that voice's language wins over the configured locale.

**Audio effects other than `STYLE` are not exposed.** For the same
per-install reason as `rate`.

**Only 16-bit PCM is produced.** The server can encode MP3 and Vorbis; this crate
has no decoder and the pipeline carries samples, so `WAVE_FILE` is the only
format ever requested.
