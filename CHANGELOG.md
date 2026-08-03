# Changelog

All notable changes to Conduit are recorded here once they are intentionally
released or queued for the next release.

The project version is managed in the workspace `Cargo.toml`; image publishing
and version tags are described in [VERSIONING.md](VERSIONING.md).

## Unreleased

- Added wake word detection as a runnable pipeline stage. `wyoming_wake`
  provider definitions talk to openWakeWord, microWakeWord, or nanoWakeWord over
  the Wyoming protocol; `device_wake` definitions describe a satellite that
  wakes itself. A wake stage holds captured audio back until a phrase is
  accepted, and publishes both activations and near misses.
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
  identification service under `--profile speaker-id`.
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
