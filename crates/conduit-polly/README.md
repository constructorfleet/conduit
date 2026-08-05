# conduit-polly

Amazon Polly speech synthesis for [Conduit](https://github.com/Teagan42/conduit).

```toml
[dependencies]
conduit-polly = "0.1"
```

## A region, not a base URL

This is a crate rather than a base URL on `conduit-openai` for the same reason
[`conduit-bedrock`](../conduit-bedrock) is: there is no URL to configure. The SDK
resolves the endpoint from the region, the credential is SigV4 over a chain rather
than a key in a header, and both of those need the SDK to reach Polly at all.

A definition names a region, and optionally a profile, a voice, and an engine:

```json
{
  "id": "house",
  "label": "House (Polly)",
  "variant": {
    "type": "tts",
    "variant": {
      "type": "polly",
      "region": "us-west-2",
      "voice": "Matthew",
      "engine": "generative"
    }
  }
}
```

## There Is No API Key, So There Is No Field

Polly authenticates through the AWS credential chain — environment variables, a
task role, an instance profile, a named profile in the shared config file. It has
no API keys at all, which is where this differs from Bedrock: Bedrock added them
as an alternative to signing, so its definition carries an optional key, and
Polly's carries none.

So there is no `api_key` in the variant, none in the component schema, and no box
in the console. That absence is asserted by a test rather than left to look like an
oversight, because a field that does nothing is worse than no field: an operator
who pasted a key into it would reasonably believe they had configured something,
and the failure would arrive later as a credential error naming the chain they
never touched.

Credentials resolve when a definition is *saved*, not at the first turn, so an
operator on a host with no credentials is told while they are still looking at the
form.

## Only PCM Leaves This Crate

Polly offers `pcm`, `mp3`, `ogg_vorbis`, `ogg_opus`, `alaw`, `mulaw`, and `json`.
This crate requests `pcm` and refuses the rest, which is a decision:

- `conduit_core::audio::Encoding` can name none of the compressed container
  formats, so their bytes could only be labelled as something they are not — and a
  mislabelled chunk plays back as noise several stages later with nothing pointing
  here.
- `json` is not audio. It is the speech-marks channel — sentence, word, viseme, and
  SSML timings — so accepting it would hand timing metadata to a stage expecting
  samples. There is nowhere in a `SpeechChunk` to put a viseme, so that format and
  the four speech-mark types are absent from the schema entirely rather than
  accepted and ignored.

Polly's `pcm` is signed 16-bit little-endian, already the pipeline's interchange
format, so the common case needs no transcode. The cost of refusing the rest is the
sample rates: `pcm` comes at 8 kHz and 16 kHz only. Conduit's own default is
16 kHz, so the common case is exact; anything else is served at the nearer of the
two and logged, because a rate mismatch is something the pipeline can resample and
an encoding mismatch is not.

## The Engine Is Checked, The Voice Is Not

There are four engines — `generative`, `long-form`, `neural`, `standard` — and they
are the same in every region, so the set is closed and a definition naming
something else is refused at the field. The console offers them as a menu.

The 106 voices are not a closed set: AWS adds them, and a build that refused a
voice released after it was compiled would be worse than one that let the API say
so. The voice is checked for *shape* instead — a bare capitalized ASCII name — which
catches the real mistake, which is pasting `en-US-Neural2-F`, Google's spelling,
into the box after configuring that provider first.

`neural` is the default rather than `generative`. Generative sounds better and is
available for far fewer voices in far fewer regions, so defaulting to it would mean
a definition naming a region and nothing else often fails.

## Accepted Limitations

- **Synthesis does not stream from the first byte.** Polly answers
  `SynthesizeSpeech` with a byte stream, but the SDK's `ByteStream` offers no
  chunk-by-chunk reader that does not go through `collect`, so an utterance arrives
  as one `SpeechChunk`. `conduit-deepgram` does stream; this does not, and saying
  so here is better than a provider that looks like it does.
- **No SSML.** The variant carries no text-type field, so every request is plain
  text. This keeps the character limit — 3 000, which is what Polly bills — the
  same as the one this crate enforces, rather than 6 000-including-tags that no
  caller can reach.
- **No lexicons, and no speech marks.** Both are real Polly features with nowhere
  in the pipeline to land.
- **The health check is `DescribeVoices`.** It exercises the credential, the region,
  and the engine together, and unlike a one-character utterance it is not billed. A
  check that skipped the credential would report a rejected role as healthy.

## Without the `polly` feature

The AWS SDK is some forty transitive crates. Compiled without the feature the
provider still exists and its factory still *claims* a Polly definition, refusing
to build it with a message naming the feature — so an operator learns this binary
cannot reach Polly, rather than watching a saved voice fail its first turn with a
credential error that is not the real reason. An unclaimed definition would read as
a typo in the variant name, and it is spelled correctly.

## License

MIT OR Apache-2.0
