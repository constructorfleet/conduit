# Changelog

All notable changes to Conduit are recorded here once they are intentionally
released or queued for the next release.

The project version is managed in the workspace `Cargo.toml`; image publishing
and version tags are described in [VERSIONING.md](VERSIONING.md).

## Unreleased

- A provider definition can now carry default request settings — the reusable
  sampling controls and model options an operator sets once on the Configured
  Provider rather than on every pipeline that names it. They are stored beside
  the connection variant, survive a restart through the same file and
  PostgreSQL backends pipeline definitions use, and are omitted from storage
  when empty so definitions written before the field are byte-for-byte
  unchanged. Each default is checked against the schema the provider it
  configures declares (`Descriptor`'s settings schema) when a definition is
  saved and again when definitions are loaded at startup, so a mistyped or
  out-of-range setting fails the write that stored it instead of reaching a
  request and being ignored. The OpenAI-compatible providers apply these
  defaults to every request they serve — a request that names a setting of the
  same value overrides the default, so a pipeline can still overrule what the
  operator configured.
- Turning stored provider definitions into running providers is now a list of
  registered vendor factories rather than one `match` in the middle of the
  server's configuration path. A `conduit_api::factory::ProviderFactory` says
  what it is called, which definitions it builds, and how; `Factories`
  enumerates whatever is registered, so supporting a new vendor is a new type
  and one line in `Factories::builtin` instead of an edit to the code that
  loads every provider a deployment has. A definition no factory claims fails
  the load loudly rather than being skipped, and an embedder can supply its own
  vendor set with `AppState::with_factories`.
- Every provider now describes itself through one `Descriptor`: a stable
  identity distinct from its display label and from the registry key, a
  version, capability metadata, and a declared settings schema. The per-trait
  methods that used to answer those questions one at a time — `models`,
  `languages`, `voices`, `available_phrases`, `configured_phrases`,
  `supports_tools`, `supports_encoding` — are gone, and what they returned is
  read off `descriptor().metadata` instead. The status layer and the operator
  UI can now render and validate a provider of any capability generically.
- Provider-specific request settings are validated instead of passing through
  untyped. The `extra: serde_json::Value` escape hatch on completion,
  transcription, and synthesis requests is replaced by a `Settings` value built
  by checking against the provider's declared schema, so a mistyped setting is
  reported rather than forwarded and silently ignored. The OpenAI providers
  declare what their endpoints actually accept (`top_p`, `frequency_penalty`,
  `presence_penalty`, `seed`; `prompt` and `temperature` for transcription;
  `instructions` for speech) and now send them.
- An MCP tool declares its argument schema as its descriptor's settings — the
  same document the model is shown — so an operator screen can render a tool's
  arguments the same way it renders any other provider's settings.
- A registry no longer conflates its key with the provider's identity: the key
  is the deployment's selector, `Descriptor::id` is what the provider calls
  itself, and `Descriptor::label` is what an operator screen shows.
  `Registry::register` keys a provider by its own identity, and
  `RegistryHandle::descriptors` lists every registration as a selector paired
  with a descriptor. The label an operator typed on a provider definition now
  reaches the registered provider's descriptor for every capability, so a
  screen reading the registry shows that rather than the id repeated back.
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
