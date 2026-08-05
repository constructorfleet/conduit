# conduit-deepgram

Speech synthesis over Deepgram's Aura API.

| Provider | Endpoint | Trait |
| --- | --- | --- |
| `DeepgramTts` | `POST /v1/speak` | `TextToSpeech` |

## Why a Separate Crate

`conduit-openai` reaches most speech servers by changing a base URL. Deepgram is
not one of them, and the reasons are structural rather than cosmetic — any one of
the three would be enough on its own.

| | OpenAI speech | Deepgram Aura |
| --- | --- | --- |
| Credential | `Authorization: Bearer …` | **`Authorization: Token …`** |
| Voice | a body field, separate from `model` | **the model id itself, in the query** |
| Text | `{"input": …}` | `{"text": …}` |
| Output format | `response_format` in the body | `encoding` + `container` + `sample_rate` in the query |

The credential is the trap. A key sent as `Bearer` is accepted by the transport
and refused by the API with a 401, which reads as a *wrong key* rather than as a
wrong scheme — so an operator checks their key against the dashboard, finds
nothing wrong with it, and has nowhere else to look. `Credential::Header` carries
the scheme inside the value, and redacts itself in `Debug` the way a bearer token
does.

Everything both crates share — sending an authenticated request, classifying a
failure, deciding whether a retry could help — comes from `conduit-http`, so only
the translation lives here.

## The Voice Is the Model

`aura-2-thalia-en` is `[family]-[voice]-[language]` in one string. There is no
separate voice field anywhere in this crate, in the provider definition, or in the
console form, because there is nowhere on the wire to send one: a `model` in the
*body* is silently ignored in favour of the account default, which presents as a
provider that will not honour a voice choice.

`language_of` reads the BCP-47 suffix off the id so the descriptor can report the
language a turn will be spoken in. When the id does not end in something that
could be a language tag it reports `""` rather than guessing — a wrong language on
a descriptor is worse than an absent one, because a pipeline may route on it.

## Model Ids Are Checked Loosely, and Not for Safety

Unlike ElevenLabs' voice id, this one is a query parameter rather than a URL path
segment, and `reqwest` percent-encodes it. There is no traversal to prevent, so
`model_id.rs` is **not** a security boundary. What it is instead: the difference
between being told which field is wrong at save time and being handed a vendor
400 mid-turn.

The check is therefore deliberately loose — non-empty, at most 64 characters,
`[A-Za-z0-9._-]` only. A pattern encoding today's `family-voice-language` shape
would refuse every id Deepgram releases in a form this crate did not anticipate,
and a provider that refuses a working voice is worse than one that forwards an
unknown name and lets the vendor say it does not know it.

## Audio Format

`linear16` and `flac`, and **`container=none` is load-bearing**. The parameter
defaults to `wav`, so leaving it unset rides a 44-byte RIFF header into a stream
the pipeline treats as samples: every utterance would begin with a click, and
nothing downstream would point back here.

Requested rates are the ones the vendor offers for `linear16` — 8 000, 16 000,
24 000, 32 000, and 48 000 Hz.

Two encodings are refused by name rather than approximated:

- **`PcmF32Le`** — Deepgram does not produce it, and labelling 16-bit samples as
  floats decodes to noise.
- **`Opus`** — the vendor's Opus is Ogg-encapsulated at a fixed rate, while
  `Encoding::Opus` here means raw frames. Mislabelled Opus decodes to silence.

## Utterance Length

`/v1/speak` accepts 2 000 characters. An utterance over that is refused here, with
a message naming both the limit and the actual length, rather than relayed as a
vendor 400 an operator cannot act on. The count is **characters, not bytes**:
counting bytes would refuse a legitimate utterance a third of the way in, and
would do so only for non-English speech.

## Settings

There is no settings schema, deliberately. `/v1/speak` takes a voice, an encoding,
and a sample rate — all three are already request fields — so an empty schema
would render as an empty box in the operator console, which reads as a feature
that failed to load.

## Health

`health()` posts the shortest billable utterance, `"."`. Deepgram publishes no
unauthenticated ping, and this is the only probe that exercises the key, the
`Token` scheme, and the model id together. A check that skipped any of the three
would report a broken provider as healthy, which is worse than not checking.

## Accepted Limitations

**The streaming WebSocket interface is not implemented.** Deepgram offers one, and
the REST endpoint this crate uses already streams audio from the first byte —
which is the latency property the socket is usually wanted for. What the socket
adds is incremental *input*: sending text while a language model is still
generating it. Nothing upstream can supply that, because `SynthesisRequest`
arrives with its text already complete. Building the framing before a caller
exists to feed it would be a protocol maintained for no one. This follows the
precedent `conduit-elevenlabs` set for realtime transcription: a websocket is a
different protocol rather than a setting, so it would be a second provider and
not a flag on this one.

**There is no `base_url` in the provider definition.** There is one Deepgram and
nothing else speaks its API, so the field would be a box with exactly one correct
value in it. It remains on `DeepgramTtsConfig` because tests point the provider at
a stand-in server — the same arrangement `conduit-elevenlabs` uses.

**A lost turn ends the stream after one error.** Audio that stops arriving
partway through becomes an `Err` item and then the stream ends. A failed `reqwest`
body reports the same failure on every poll, so forwarding it verbatim would yield
an endless stream of identical errors and a caller draining to the end would spin
rather than give up.
