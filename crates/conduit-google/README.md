# conduit-google

Google Cloud speech provider implementations.

| Provider | Trait | Endpoint |
| --- | --- | --- |
| `GoogleTts` | `TextToSpeech` | `POST https://texttospeech.googleapis.com/v1/text:synthesize` |
| `GoogleStt` | `SpeechToText` | `POST https://speech.googleapis.com/v1/speech:recognize` |

Both speak REST directly, authenticated with a bearer token from Application
Default Credentials. Nobody types a Google key.

## Configuration Model

`GoogleConfig` describes one credential and its settings, and both capabilities
are built from it — a deployment authorizes itself to Google once, not once per
capability.

- `credentials`, defaulting to Application Default Credentials
- `language`, a BCP-47 tag; `voice` for synthesis; `model` for recognition
- `tts_base_url` and `stt_base_url`, overridden only by tests and by a proxy
- connect timeout, and a read timeout rather than a total one
- `chunk_bytes`, how much decoded audio one synthesis chunk carries
- `default_settings`, the stored settings every request starts from

Construction never touches the network beyond fetching a token, so registering a
provider from a saved definition cannot fail because Google is busy.

## Credentials

`Credentials::Adc` discovers credentials the way every other Google client does,
in this order: the `GOOGLE_APPLICATION_CREDENTIALS` service-account JSON, the
gcloud user credentials a developer already has, then the metadata server on GCE,
GKE, and Cloud Run. A container running as a service account configures nothing.

`Credentials::Token` supplies an access token directly, for a deployment that
mints its own. A token is fetched per request, so a refreshed ADC token is the
one that gets used.

Neither variant renders its secret: `Credentials` and the token cache have
hand-written `Debug` implementations, and the `Authorization` header is marked
sensitive so it cannot reach a log through a request dump.

## The `google` Feature

`google` is on by default and gates only credential *discovery* — the `gcp_auth`
dependency.

With it off, both providers still exist and still register, and each refuses to
construct with a `Error::Config` naming the feature:

```text
provider `google` authenticates with Application Default Credentials, which this
build cannot discover: it was compiled without the `google` feature. Supply an
access token directly, or rebuild with `--features google`
```

That refusal is the point. A lean build reports the missing feature when an
operator saves a provider definition, not the first time somebody speaks to it.
Because only discovery is gated, a deployment that supplies its own access token
works in either build.

## Why Not The Official SDK

`google-cloud-texttospeech-v1` and `google-cloud-speech-v2` exist. This crate
hand-rolls REST instead, for two reasons that were checked rather than assumed.

**The official SDK would link a second crypto provider.** This workspace links
exactly one: rustls with `ring`. `google-cloud-auth` enables by default:

```toml
default-rustls-provider = ["reqwest/default-tls", "rustls/aws_lc_rs"]
```

and `google-cloud-gax-internal` adds `tonic?/tls-aws-lc` on top. Turning that
default off is possible, but upstream's own comment says what it costs:

> Applications with specific requirements for cryptography (such as exclusively
> using the `ring` crate) should disable this default and call
> `rustls::CryptoProvider::install_default()`.

And the reason that call is not optional, again upstream's own words:

> reqwest panics if (1) configured to support TLS and (2) no default crypto
> provider is configured via features or installed at runtime. Even if reqwest is
> being used without TLS.

So the choice the SDK offers is a second crypto provider in the binary, or a
process-global `install_default()` that a library has no business making on an
application's behalf — and that would fight the single-provider invariant if
anything else ever made it too.

**The streaming paths are not there anyway.** `google-cloud-speech-v2` documents:

> WARNING: some RPCs have no corresponding Rust function to call them. Typically
> these are streaming RPCs.

Streaming recognition and streaming synthesis are gRPC-only in the API and
unimplemented in the SDK, so the SDK buys no streaming this crate is giving up.

`gcp_auth` is what remains: credential discovery alone, reaching TLS through
`hyper-rustls`, pinned to `ring` with `default-features = false` so a future
change to its default cannot quietly introduce a competing backend.

## Speech Synthesis

`GoogleTts` posts the utterance, decodes the base64 `audioContent`, and emits it
as chunks.

LINEAR16 `audioContent` arrives with a WAV header — Google documents this
plainly, and it is stripped rather than passed on, because 44 bytes of `RIFF`
played as samples is a click. That header is also the only trustworthy statement
about what actually arrived: Google resamples to the requested rate where it can
and answers at the voice's own rate where it cannot, so the first chunk reports
**what was produced**, not what was asked for. Believing the request over the
response pitches the audio.

A voice travels with its own language code, because Google rejects a request
whose `languageCode` and `name` disagree. Naming no voice sends only a language
and lets Google choose.

The encoding map is `PcmS16Le → LINEAR16` and `Opus → OGG_OPUS`. FLAC and 32-bit
float PCM are refused before anything is sent, since the endpoint cannot produce
them.

`refresh_voices()` fetches `GET /v1/voices` into the descriptor, which is what a
status screen reads. `health()` is that same call: the cheapest authenticated
request Google offers, and not a billed one.

## Speech Recognition

`GoogleStt` buffers the complete utterance, base64-encodes it, and emits one
final transcript. It does not invent partials, because the endpoint has none to
give — inventing them would make the pipeline look more responsive than it is.

`sampleRateHertz` and `audioChannelCount` come from the request's declared
format and are never assumed. This is the most consequential thing this crate
does: audio described at the wrong rate does not fail, it transcribes the wrong
words with high confidence.

`audioChannelCount` comes from the same place. Google recognizes only the first
channel of multi-channel audio unless `enableSeparateRecognitionPerChannel` is
set, which this provider does not set: separate recognition is billed per channel
and returns one transcript per channel, which is not what a single-speaker
utterance wants. Stereo capture therefore transcribes its left channel.

Consecutive result segments are joined in order, and the confidence reported is
the **minimum** across them — a transcript is only as trustworthy as its shakiest
segment. Silence comes back as an empty final rather than an error, so the
pipeline still knows the turn is over.

Accepted encodings are `PcmS16Le → LINEAR16` and `Flac → FLAC`. Opus is refused
because this crate does not build an Ogg container, and 32-bit float PCM because
the endpoint does not read it.

## Accepted Limitations

**Synthesis is not streaming.** `text:synthesize` returns one complete payload.
There is no REST streaming synthesis in `v1` or `v1beta1` — `StreamingSynthesize`
exists only over gRPC. So `TextToSpeech` is honoured by awaiting the whole
response and cutting the decoded buffer into chunks.

The consequence, stated plainly rather than hidden behind a stream that looks
live: **time to first audio is the full synthesis latency of the entire
utterance.** A long reply is silent until all of it is ready. Reducing
`chunk_bytes` does not make the first chunk arrive sooner; it only bounds how
much a consumer holds at once. For latency-sensitive turns, a genuinely streaming
provider is the answer, not a smaller chunk size.

**Recognition is batch, and inline audio is capped.** `speech:recognize` accepts
roughly one minute of audio in the request body. Longer recordings need
`longrunningrecognize` with Cloud Storage, which this crate does not implement.

**Recognition holds the utterance in memory.** One request carries one recording,
so the audio is buffered before anything is sent. A capture that fails partway
aborts the session rather than transcribing half an utterance and presenting it
as the whole one.
