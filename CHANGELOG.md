# Changelog

All notable changes to Conduit are recorded here once they are intentionally
released or queued for the next release.

The project version is managed in the workspace `Cargo.toml`; image publishing
and version tags are described in [VERSIONING.md](VERSIONING.md).

## Unreleased

- The runtime provider bundle (`Providers`) is now capability-indexed rather
  than one hand-written registry field per capability. Registering and
  enumerating a wake, speaker, or memory provider goes through the exact same
  path recognition, reasoning, synthesis, and tools always have, and adding a
  capability is a new `conduit_provider::Capability` variant and a typed
  accessor pair rather than an edit to the bundle's fields, its constructor, or
  its debug output.
- Added `transform` pipeline nodes, which rewrite what a model said on its way
  to being rendered. What reaches a synthesizer is now the pipeline's decision
  rather than the model's willingness to honour "do not use emoji". Three rules
  ship built in — `markdown_to_speech`, `strip_emoji`, `collapse_whitespace` —
  applied in the order a definition lists them.
- Because a transform is a node, its edges say which rendering it changes: one
  wired only to the `tts` node cleans up what is spoken while a text sink keeps
  the markdown the model wrote. Transforms chain, a transform nothing renders
  through is refused at prepare time, and one that fails ends the turn rather
  than delivering what it was configured to prevent.
- Added wake word detection as a runnable pipeline stage. A wake stage holds
  captured audio back until a phrase is accepted, and publishes both
  activations and near misses.
- A wake provider definition names its engine as its type — `openwakeword`,
  `nanowakeword`, or `microwakeword` — and where that detector runs as a
  `runtime` inside it. Each engine offers only the places it can actually run,
  so a detector on hardware too small for it is no longer expressible rather
  than merely rejected. Definitions written in the older shape, which named an
  engine and a place as independent fields, are still read and are rewritten
  the next time they are saved.
- Added `conduit-wake`, which scores openWakeWord models in the Conduit process
  with no service to run: a definition with a `local` runtime reads models from
  disk and detects directly. About 3 ms of work per 80 ms of audio. Its models
  are fetched by `scripts/fetch-wake-models.sh` rather than carried in the
  repository. microWakeWord cannot be scored this way — its models need the
  tflite-micro micro-frontend operator — and nanoWakeWord's phrase models are
  recurrent, which is a second scorer rather than a setting on this one; both
  run on a Wyoming server, and microWakeWord on the satellite built for it.
- Added `GET /v1/providers/{id}/phrases`, and the provider form now offers the
  phrases a saved detector reports having models for.
- Added speaker identification as a runnable pipeline stage. `http_speaker_id`
  provider definitions talk to a SpeechBrain, Resemblyzer, or pyannote service
  over the contract documented on the new `conduit-speaker` crate. The identity
  found reaches a tool's per-speaker permission check; an identification service
  that is down costs a turn its per-speaker policies rather than its answer.
- Added `services/speaker-id`, a reference implementation of the speaker
  identification contract over SpeechBrain ECAPA-TDNN embeddings, published as
  `conduit-speaker-id` with `latest-speechbrain` and `latest-speechbrain-gpu`
  tags. Its encoder is pluggable, so a further engine is a class rather than a
  new contract.
- Added `docker-compose.yml`, which runs Conduit alone by default and adds the
  identification service under `--profile speaker-id` and a wake word server
  under `--profile openwakeword`, `--profile microwakeword`, or
  `--profile nanowakeword`. Waking on an openWakeWord phrase needs no profile:
  the models go in the `wake-models` volume and Conduit scores them itself.
- Added a `diarization_server_speaker_id` provider definition for deployments
  already running a [Diarization_Server](https://github.com/CptCamembert/Diarization_Server).
- Added `GET /v1/providers/{id}/voices`, and the pipeline editor now offers a
  synthesizer's own voices rather than a free text box. Providers that
  enumerate none, and providers that cannot be reached, still accept a typed
  voice.
- MCP tool provider definitions are now reachability-probed automatically along
  with every other capability, both when definitions change and at startup. A
  tool provider's server being healthy no longer shows `configured` with "no
  successful reachability check yet" until an operator presses Test.
- `/v1/events` no longer refuses `wake_word` or `identity` stage
  subscriptions. Both now carry traffic, so `Stage::has_emitter` and the
  refusal it powered are gone.
- Added project documentation covering contribution workflow, architecture, API
  routes, configuration, and crate responsibilities.
- **Breaking:** provider definition variants are now two-level. An outer `type`
  names the capability (`llm`, `stt`, `tts`, `tool`, `wake`, `speaker_id`) and
  an inner `variant.type` names the vendor (`openai`, `wyoming`, `mcp`,
  `device`, `http`, `diarization_server`), e.g.
  `{ "type": "llm", "variant": { "type": "openai", ... } }`. Flat tags such as
  `openai_llm` are still accepted when reading stored records and upgrade to
  the new shape when re-saved. See
  [ADR-0013](docs/adr/0013-nested-provider-definition-variants.md).

## 0.1.0

- Initial workspace version.
- Provides the core graph and event model, provider traits, runtime execution,
  OpenAI-compatible LLM/STT/TTS adapters, pipeline storage backends, Prometheus
  metrics, the HTTP API, and ESPHome firmware targets.
