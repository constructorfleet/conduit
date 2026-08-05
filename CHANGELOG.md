# Changelog

All notable changes to Conduit are recorded here once they are intentionally
released or queued for the next release.

The project version is managed in the workspace `Cargo.toml`; image publishing
and version tags are described in [VERSIONING.md](VERSIONING.md).

## Unreleased

- Voice activity detection on the input path, as a `vad` capability, a `vad` pipeline
  stage, and a `conduit-vad` crate scoring Silero. The stage trims the silence around
  an utterance before the recognizer sees it: a recognizer billed per second, or one
  deciding for itself where speech ended, is handed the speech rather than the room.
- The detector takes the wake gate's *position* and the speaker identifier's *failure
  semantics*, which [ADR-0014](docs/adr/0014-voice-activity-detection-as-two-decisions.md)
  records as two separate decisions. It sits in the stream, so it can hold audio back
  — but a detector that fails does **not** end the turn: the remaining audio is
  forwarded untrimmed, which is the behaviour without the stage at all. A gate decides
  whether there is a turn; a trimmer only decides how much of one to forward.
- A detector returns exactly one verdict per chunk, in order, and the trimming stage
  pairs them positionally. That is the trait's contract rather than a convenience: a
  detector answering only about the chunks it had opinions on would leave the stage
  unable to tell which chunk a verdict skipped, and every later pairing would land on
  the wrong audio. A chunk too short to complete a scoring window repeats the previous
  verdict — the sound has not been re-evaluated rather than fallen silent — because
  reporting silence there punches a hole in the middle of a word for any device sending
  chunks under 32 ms.
- `silence_ms` is tunable per node over a **300 ms floor** an operator cannot cut
  below. Two levels because they are two different things: how long a mid-sentence
  pause may run is a judgement about how someone speaks, and how much trailing silence
  a recognizer needs to hear an ending is a fact about the recognizer. A setting that
  could cut the second would let a pipeline configure clipped final words.
- A chunk whose duration rounds to zero still counts against that pause. Found while
  testing it: a tiny buffer, or an encoding whose byte count says nothing about its
  length, would otherwise never close the tail — and a stage that never closes the
  tail forwards the whole stream while appearing to trim.
- Silero's rates — 8 kHz and 16 kHz — are declared on the descriptor and a mismatch is
  **refused rather than resampled**. A wrong rate does not degrade the detector, it
  makes each window the wrong *length of sound*: the same 512 samples become 11 ms
  instead of 32 ms, and the model reports confidently about audio it never heard.
  Resampling silently would also be deciding on an operator's behalf that a rate they
  configured was wrong.
- Samples are never rewritten, only dropped or passed through, so a recognizer
  downstream hears exactly the bytes the microphone produced.
- PCM is normalized to `-1.0..=1.0` here, which is the **opposite** of
  `conduit-wake`'s convention: openWakeWord scores raw `i16` magnitudes. Two models,
  two conventions, and getting either backwards produces a detector that calls
  everything speech or nothing — a trimmer that looks like it is working. This is the
  one hazard a fake detector cannot catch, so it is what the real-model test asserts.
- WebRTC's VAD, Picovoice Cobra, MarbleNet and TEN VAD are deliberately not offered.
  WebRTC's is energy-based and mistakes a fan for a voice, which is the failure a
  learned detector exists to avoid. Cobra is proprietary, needing an access key and a
  per-device license for something Conduit does in process from a file. The other two
  sit behind runtimes Conduit does not carry.
- Coqui is deliberately not supported, recorded alongside the PicoTTS refusal it
  resembles. The live fork, `idiap/coqui-ai-TTS`, ships an HTTP server, but a bespoke
  one rather than an OpenAI-compatible one — so it is not reachable by changing a base
  URL, and support would mean a hand-written variant tracking one project's own API or
  a wrapper service Conduit would then own. What that buys over the vendors already
  here is voice cloning, which is the part least suited to offering: XTTS clones from a
  recording of a real person's voice, usually not the operator's, so consent and
  retention become a question about biometric data. Better answered before building a
  feature than after.
- Amazon Polly synthesis, as `conduit-polly` and a `polly` TTS variant. A region
  rather than a base URL, like `conduit-bedrock`: the SDK resolves the endpoint and
  the credential is SigV4 over the AWS chain, so neither is reachable by pointing
  `conduit-openai` somewhere else.
- The Polly definition has **no credential field at all** — not in the variant, not
  in the component schema, not in the console. This is where it differs from
  Bedrock, which carries an optional key because Bedrock added API keys as an
  alternative to signing; Polly has none. A box that does nothing is worse than no
  box, because an operator who pasted a key into it would reasonably believe they
  had configured something. The absence is pinned by a test rather than left to
  look like an oversight.
- Only PCM is requested, and Polly's other formats are refused rather than
  mislabelled. `Encoding` can name none of `mp3`, `ogg_vorbis`, `ogg_opus`, `alaw`,
  or `mulaw`, so their bytes could only be labelled as something they are not — and
  a mislabelled chunk plays back as noise several stages later with nothing pointing
  at the request. `json` is not audio at all: it is the speech-marks channel, and a
  viseme has nowhere in a `SpeechChunk` to go, so that format and the four
  speech-mark types are absent from the schema entirely.
- The cost of that is the sample rates: `pcm` comes at 8 kHz and 16 kHz only.
  Conduit's default is 16 kHz so the common case is exact; anything else is served
  at the nearer of the two and logged, because a rate mismatch is something the
  pipeline can resample and an encoding mismatch is not.
- The engine is validated against a closed set and offered as a menu; the voice is
  checked for shape and not against a list. There are four engines and they are the
  same in every region, so a wrong one fails every turn and is worth refusing at the
  field. The 106 voices are AWS's to add, so a build that refused one released after
  it shipped would be worse than one that let the API say so — the shape check
  catches the real mistake, which is pasting Google's `en-US-Neural2-F` spelling
  into the box.
- `neural` is the default engine rather than `generative`. Generative sounds better
  and is available for far fewer voices in far fewer regions, so defaulting to it
  would mean a definition naming a region and nothing else often fails.
- Polly synthesis does **not** stream from the first byte, unlike
  `conduit-deepgram`. The SDK's `ByteStream` offers no chunk-by-chunk reader that
  does not go through `collect`, so an utterance arrives as one chunk; this is
  recorded rather than papered over.
- The AWS SDK reaches Polly with the same rustls-ring HTTP client `conduit-bedrock`
  builds, rather than the SDK's own `rustls` feature, which selects `aws-lc-rs` — C,
  wants cmake, and would put a second crypto provider in a binary that already links
  ring for every other provider. `aws-lc` stays absent from `Cargo.lock`, and CI
  checks it.
- Compiled without the `polly` feature the factory still *claims* a Polly
  definition and refuses it by naming the feature. An unclaimed definition would
  read as a typo in the variant name, and it is spelled correctly.
- Deepgram Aura synthesis, as `conduit-deepgram` and a `deepgram` TTS variant.
  Three things stop this being the `openai` variant with a different base URL, and
  the first is the one that costs an afternoon: the key travels as
  `Authorization: Token`, not `Bearer`. A `Bearer` key is accepted by the
  transport and refused by the API with a 401, which reads as a wrong key rather
  than a wrong scheme — so an operator checks their key against the dashboard and
  finds nothing wrong with it. The other two: the voice *is* the model and it
  travels in the query string, and the body is `{"text": …}` rather than
  `{"input": …}`.
- The Deepgram definition has no `voice` field, in the schema or the console,
  because `aura-2-thalia-en` is family, voice, and language in one id and there is
  nowhere on the wire to send a second one. A `model` sent in the *body* is
  silently ignored in favour of the account default, which presents as a provider
  that will not honour a voice choice.
- Synthesis asks Deepgram for `container=none`. The parameter defaults to `wav`,
  so leaving it unset rides a 44-byte RIFF header into a stream the pipeline
  treats as samples: every utterance would start with a click, and nothing
  downstream would point back at the request that caused it.
- `PcmF32Le` and `Opus` are refused by name rather than approximated. Deepgram
  does not produce the former, and its Opus is Ogg-encapsulated at a fixed rate
  while `Encoding::Opus` here means raw frames — mislabelled Opus decodes to
  silence, which is harder to diagnose than a refusal.
- An utterance over Deepgram's 2 000-character limit is refused here with both the
  limit and the actual length named, rather than relayed as a vendor 400. The
  count is characters rather than bytes, because counting bytes would refuse a
  legitimate utterance a third of the way in and would do so only for
  non-English speech.
- Deepgram model ids are validated loosely and explicitly not as a security
  boundary: unlike an ElevenLabs voice id this is a query parameter rather than a
  URL path segment, and there is no traversal to prevent. The check exists to name
  the wrong field at save time; it admits anything that could plausibly be an id,
  because a provider that refuses a voice the vendor has released is worse than
  one that forwards an unknown name.
- Deepgram's streaming WebSocket interface is deliberately not implemented. The
  REST endpoint already streams from the first byte, which is the latency property
  the socket is wanted for; what the socket adds is incremental *input*, and
  nothing can feed it because `SynthesisRequest` arrives with its text complete.
  Following `conduit-elevenlabs`, a websocket is a protocol rather than a setting,
  so it would be a second provider and not a flag on this one.
- Kokoro is a synthesis preset, so the presets are no longer chat-only. It points
  the `openai` TTS variant at Kokoro-FastAPI on `8880` and fills the model in as
  well as the endpoint, because that server hosts exactly one model and it is
  called `kokoro` — a required field left blank invites a guess at a name that
  does not exist. No new crate and no factory arm; `conduit-openai` already posts
  to `audio/speech`.
- A speech preset asserts that `/v1/audio/speech` specifically answers, which is
  why it is a sibling helper rather than a flag on the chat one and why these are
  added one verified server at a time. Chat compatibility does not imply audio
  compatibility, and a server advertising "OpenAI compatible" while serving only
  `/v1/chat/completions` produces a definition that looks right and fails on the
  first turn.
- `no_two_presets_name_the_same_endpoint` now keys on capability as well as URL.
  One server can legitimately serve chat and speech under one base URL, and those
  are two presets an operator picks between by what the stage needs; keyed on URL
  alone, the first such pair would have failed the check as a duplicate.
- The docs no longer promise the ASR wrapper service as future work, because it
  shipped: `docs/configuration.md` points at `services/wyoming-asr/` and the
  compose profile that brings it up, in place of a link to a closed issue. An
  operator following that section previously reached the end and was told the
  server they needed did not exist yet.
- Confirmed end to end that a Wyoming ASR server is reachable and transcribes:
  the real `WyomingStt` against the real service over TCP, with a stub engine in
  place of Canary's weights. One final transcript arrives with `streaming` on and
  with it off, and the handshake answers `supports_transcript_streaming: False`,
  so the fallback path is exercised rather than assumed. No new `SttVariant` was
  needed — the point of the exercise.
- The service's own account of streaming is corrected where it had gone stale.
  It said Conduit never sends `describe` and gates partials on a per-request flag
  alone; both stopped being true when the recognizer learned to negotiate. Its
  `supports_transcript_streaming: false` is now load-bearing rather than
  informational, and `streaming: false` in a definition saves a round trip rather
  than avoiding a misconfiguration — leaving it on against this service is
  correct and works.
- Two more OpenAI-compatible chat endpoints arrive as catalogue presets:
  Moonshot (Kimi) and Z.Ai. Both are the existing `openai` variant with the
  `base_url` filled in, so there is no new crate, no factory arm, and nothing to
  configure beyond a key. Moonshot and Kimi are one preset, not two:
  `platform.moonshot.ai` redirects to `platform.kimi.ai` and the one endpoint
  serves both `kimi-*` and `moonshot-v1-*` models, so listing them separately
  would offer an operator a choice where only one answer exists. A new test
  asserts that no two presets name the same endpoint, so the next one cannot
  reintroduce the duplicate.
- Ollama's native `/api/chat` is recorded as deliberately unimplemented rather
  than left open. The case for it was that native errors name a missing model
  better; measured, `/v1` returns the nested `error.message` shape Conduit
  already unwraps and native does not, so the compatible path reads better today.
  What native still offers — `think` levels, `keep_alive`, `options`, timing
  stats — is written down as the case a future change would have to make.
- The `streaming` flag on a Wyoming recognizer does what it says. It was stored,
  shown in the console, and never read: partials were gated solely on a request
  option that defaulted to on, so they were effectively always on and no operator
  could turn them off. Now the flag decides. Off emits no partials whatever the
  server offers — the case that was previously impossible. On sends a `describe`
  and reads the server's `info`: a server advertising
  `supports_transcript_streaming` is asked for partials, and one that says it
  cannot stream still returns a correct single final, logged once, because a
  non-streaming recognizer is a fully working recognizer. A server that answers
  without mentioning the capability, or that will not answer at all, is asked for
  partials anyway — the key postdates transcript streaming itself, so its absence
  means "did not say" rather than "cannot", and negotiation never fails a turn.
  The handshake is a short second connection per session rather than a cache,
  because a Wyoming server can be replaced under a stable address, and it is only
  paid when `streaming` is on.
- `SttVariant::OpenAi`'s `stream` flag is documented as reserved rather than left
  looking equivalent. Conduit posts a complete recording to
  `audio/transcriptions` and reads one response, so there are no partials to
  gate; the vendor's streaming transcription is a different request shape and an
  event stream, which is a change rather than a setting.

- A Wyoming server's `error` event reaches the operator. All three Wyoming
  clients — speech-to-text, text-to-speech, and wake word — read the event and
  report the message the server sent, instead of letting it fall through the
  "ignoring an event" branch and then reporting whatever happened next. The
  symptom was a refused sample rate: the server named both what arrived and what
  it wanted, then closed, and Conduit reported `connection closed before final
  transcript` — which reads as a network fault for a configuration mistake the
  operator can fix in the console. Recognition was the worst of the three, since
  the close raced the refusal; synthesis returned empty audio, which sounds like
  a turn with nothing to say, and a refused wake detector was indistinguishable
  from a quiet room. An `error` arriving *after* a final transcript is logged and
  discarded rather than retracting a turn that already answered, and a server
  that closes without explaining itself keeps the existing end-of-connection
  message, because that genuinely is a closed connection.

- Bedrock credential resolution is covered by tests rather than assumed. A named
  `profile` reaches the loader, an `api_key` prefers bearer auth over signing,
  and naming neither leaves the AWS default chain and SigV4 in place. The middle
  one is the load-bearing case: without an explicit auth scheme preference the
  SDK accepts a token and then signs with whatever the chain resolved, so a key
  an operator typed is ignored in favour of an ambient instance role and the
  failure points at the wrong thing. The tests supply a credentials file through
  aws-config's own `Env`/`Fs` overrides, so nothing on the machine running them
  is read and no network is reached.
- `crates/conduit-bedrock/README.md` documents every mechanism the chain covers
  — environment, shared config, a named profile, a role assumed through
  `role_arn` and `source_profile`, web identity and IRSA, ECS task roles, EC2
  instance profiles, SSO, `credential_process`, and the Bedrock API key — and
  states that assuming a role by an ARN passed inline is not among them, because
  the chain only assumes a role that external configuration already named.

- Added `transform` definitions that run a script the operator wrote. The three
  builtin rules are Rust functions somebody had to write and release; a `script`
  definition holds a source, names its engine, and takes effect on the next
  utterance. `conduit-script` is a separate crate because an interpreter is a
  large dependency, and a deployment wanting only `strip_emoji` should not
  compile one.
- Running operator code inside the turn loop is bounded rather than trusted. A
  script carries a deadline — 50ms by default, capped at 5s — and one that does
  not finish fails its segment rather than ending every turn on the pipeline.
  The script is compiled and its deadline checked when the definition is
  *saved*, by asking `conduit-script`'s own validator rather than keeping a
  second copy of the rules, so a typo is refused while it is still on screen
  instead of becoming a jammed pipeline discovered by whoever spoke to it next.
  Compilation catches syntax errors and undefined variables; an unknown
  function, an unknown method, or a non-string return still surface as a failed
  turn on the first sentence.
- The engine is stored in the definition rather than assumed, so a second
  interpreter could arrive without every saved script silently changing
  language. One exists today, and the field is still not optional.
- The Providers page gives a script a box it can be read in: a `multiline`
  format hint on a config field, rendered as a resizable monospaced textarea
  with the operator's own indentation, and read back verbatim — the source is
  the definition rather than a secret in it, so reopening the form shows the
  program there is to edit. `timeout_ms` also reads as `Timeout Ms` rather than
  `Timeout MS`: a unit symbol is not an initialism.
- Added `conduit-memory`, so what the assistant remembers is something an
  operator configures rather than something the runtime happens to hold. The
  runtime has retrieved before it reasons and written after it answers since
  flows landed; what was missing was any way to say *where*. A `memory` provider
  definition is that, and a core binds it the way it binds a tool.
- The two backends are two retrievals rather than two places to put the same
  records, which is why they are separate variants and not one variant with a
  storage field. `builtin` ranks with BM25 over unigrams and needs nothing —
  no service, no database, and with no `path` no file either, so a store
  configured by configuring nothing is ephemeral. Recording every conversation
  to disk is not a thing to get by leaving a field blank. `pgvector` ranks by
  cosine distance over an embedding, so a question phrased in words the stored
  record never used still finds it, and degrades to keyword ranking where the
  extension is missing rather than refusing to answer.
- A built-in store is bounded — a thousand records by default, oldest dropped
  first — because nothing else forgets one: the runtime never calls
  `forget_conversation`, so an unbounded in-process store grows for as long as
  the process runs. A capacity of zero is refused rather than kept as a store
  that remembers nothing.
- A pgvector definition supplies the embedding width instead of discovering it,
  because that number is what the `vector(n)` column is declared with and
  nothing can learn it before the first embedding exists. Its connection URL may
  not carry a password, and one is refused rather than redacted: every other
  credential in a definition lives in its own secret field, which is what lets a
  read hide it and a later save keep it, and a password in a URL's userinfo has
  neither — it would be stored in the clear and handed back in the clear to
  every operator who can read the provider list. The refusal does not echo the
  password it refused.
- The store shares the API's `postgres` feature, since a deployment with a
  database for pipeline definitions has one for records. A build without it
  still claims the definition and refuses by name, so an operator learns which
  feature is missing when they save rather than when someone asks the assistant
  what it remembers.
- Memory is no longer a capability the console could show and not edit. It has a
  definition variant now, so the two stores appear in the provider catalogue,
  save from the Providers page, and reopen with what was stored — which removes
  the branch every kind-to-capability caller carried for the one kind that had
  nothing behind it.
- Added three speech vendors that a base URL does not reach either.
  `conduit-elevenlabs` transcribes and synthesizes, where the credential is an
  `xi-api-key` header and the voice is a URL *path segment* rather than a body
  field. `conduit-google` does both over the Cloud Speech APIs, where the
  credential is not typed at all. `conduit-marytts` synthesizes against a
  self-hosted server that form-encodes its request, answers with a WAV, and has
  no authentication anywhere. All three are selectable in the Providers page and
  bindable in a pipeline.
- A voice id that reaches a URL path is a security boundary rather than a
  correctness one: `../` in a stored definition would move the request to a
  different API path with the account's credential attached. Every ElevenLabs
  voice is checked against an allowlist — letters, digits, `-`, `_` — before it
  can reach a URL, and the console declares the same allowlist as a schema
  pattern so the form refuses a traversal attempt rather than storing it. Google's
  language tags and voice names reach a query string and get the same treatment.
  The management API refuses these by calling the provider crates' own
  validators rather than keeping a second copy of each rule, because two copies
  are how a form comes to accept a definition that fails to build on the next
  server start.
- Google definitions carry no credential field. The default chain resolves
  whatever the host already holds — a workload identity, a service account file,
  a `gcloud` login — and it resolves when the definition is *saved*, so an
  operator on a credential-less host is told while they are still looking at the
  form. Discovery is what sits behind the `google` feature, on by default, and
  only discovery: the REST plumbing is always compiled, so a deployment minting
  its own access tokens works in either build.
- MaryTTS suggests no voice, because it ships none and any default would be
  wrong on every install that did not happen to have it. PicoTTS is deliberately
  absent: an unmaintained C library with no network interface and no streaming,
  so reaching it would mean FFI and a vendored blob in exchange for worse output
  than a MaryTTS container gives.
- Closed a way a saved credential could be dropped silently. The definition
  store's key accessors matched the keyed variants by hand with a catch-all
  fallthrough, so a new keyed variant omitted there lost its credential on every
  redacted save with no compile error to say so. The two new keyed speech
  variants are covered, and so are the three that were already keyed and already
  unguarded — the test that guards this now enumerates every one of them and
  names the variant that failed rather than only its capability.
- Added three ways to reach a language model that were not reachable by changing
  a base URL. `conduit-anthropic` speaks the Messages API, which authenticates
  with `x-api-key`, pins a version header, takes system framing as a top-level
  field, requires `max_tokens`, and streams typed content blocks rather than
  uniform chunks. `conduit-bedrock` speaks Amazon's Converse API, where there is
  no URL to configure at all — a definition names a region, and the AWS
  credential chain resolves whatever the deployment already holds. And Ollama,
  vLLM, LM Studio, and OpenRouter are catalogue presets rather than new provider
  code: the same `openai` variant with the endpoint already typed, because
  knowing a local Ollama is OpenAI-compatible does not tell anyone it listens on
  11434 and wants a `/v1` suffix.
- Bedrock's differences from the Messages API are handled at the edges where an
  operator can see them. Converse insists a conversation alternate between user
  and assistant, which the runtime's history does not — a memory recall, a tool
  result, and a spoken utterance arrive as three consecutive user-side turns — so
  they are joined rather than letting the API refuse the request. `temperature`
  and `max_tokens` are read from the request rather than offered as settings,
  because Converse takes them in a field of its own and declaring them would send
  each control twice. Token counts arrive after the stop reason, so the stop is
  held and one `Finished` is built when they land.
- The AWS SDK is ~40 transitive crates, so it sits behind the `bedrock` feature,
  on by default. A build without it still registers the provider and refuses by
  name, so an operator learns which feature is missing when they save the
  definition rather than when someone speaks to it. The SDK's own `rustls`
  feature would have linked `aws-lc-rs` beside the ring every other provider
  uses; the TLS client is built from `aws-smithy-http-client` with `rustls-ring`
  and handed over instead, so the workspace still links one crypto provider.
- Added the speaker roster: a Speakers page in the operator console, and
  `/v1/speakers` behind it. Identification has been a pipeline stage for a
  while, but a stage that matches a voice against enrolled prints is useless
  until something enrolls one, and nothing did. Now an operator names somebody,
  records a sample in the page or uploads a WAV, and the voice reaches a tool's
  per-speaker permission check under a name a person recognizes. Conduit owns
  the speaker id and the identification service still stores it as an opaque
  label, so the roster is the only place a name exists — which is what lets a
  deployment change embedding models without every enrolled voice becoming a
  stranger. The roster has its own store, in a directory
  (`CONDUIT_SPEAKER_DIR`) or in PostgreSQL beside the pipelines.
- Enrollment audio is uploaded as a WAV file and converted to the pipeline's
  interchange format on the way in, so a recording made at whatever rate the
  microphone runs at arrives correctly rather than pitched wrong — which would
  embed as a different person. `conduit_core::wav::parse` and
  `conduit_core::pcm::to_interchange` are the two halves of that.
- A core's tool binding may now name an MCP definition rather than one of its
  tools, and is offered every tool that server registered. Adding "the weather
  server" to a pipeline is the obvious thing to do and it failed with "no
  provider registered as `weather`"; the way around it was to list each tool by
  hand and revisit the pipeline whenever the server grew one. A definition that
  advertises exactly one tool is no longer separately registered under its bare
  id, because expansion now covers that case under one rule.
- Provider configuration fields in the console are named the way a person reads
  them — `base_url` is `Base URL`, `api_key` is `API Key` — and a required field
  is marked as required and carries the `required` attribute rather than having
  the word appended to its label. A required numeric or boolean field is also no
  longer reported as missing when it has been answered.

- The operator status snapshot now reports every registered provider of every
  capability. It enumerated stt, llm, tts and tool one at a time, so a
  transform, a wake word detector, a speaker identifier or a memory store an
  operator had configured was simply absent from the Providers page. It is now
  one walk over the provider bundle's descriptors — `Providers::descriptors` and
  `Providers::health`, both capability-generic — so a capability added later
  appears without an edit here. `ProviderKind` gains `memory` to match.
- A provider's status now carries the identity, label and version its descriptor
  states, beside the selector a pipeline names. The two were conflated, so a
  diagnostic could not say which implementation or which build was behind a
  configured provider. A selector that names a provider the runtime never built
  reports no identity rather than an invented one.
- Proven is now derived per provider rather than per pipeline, and a failed turn
  takes a provider's proof back until a later turn proves recovery. A turn that
  failed at synthesis used to un-prove every provider in the graph, including
  the model that had answered successfully in it; and a provider that answered a
  health check while failing every real turn still read as reachable with no
  failure recorded against it. A failure now outranks a health check that
  answered, and names the pipelines it affects, so the exception-first overview
  can warn before the next turn fails.
- A pipeline node can now override the request settings of the Configured
  Provider it names, rather than either accepting the provider's defaults or
  needing a provider of its own. An `stt` node, a `tts` node, and a core's model
  binding each take a `settings` map holding only what that pipeline wants
  different; everything it leaves out stays with the Configured Provider, which
  layers the request over its own stored defaults. Overrides are checked against
  the provider's declared settings schema when the pipeline is prepared, naming
  both the offending setting and the node to fix it on, so a mistyped setting is
  a graph to correct rather than a turn that fails. They are checked as
  *overrides* — declared defaults are not filled in and `required` is not
  enforced (`Descriptor::validate_overrides`) — because a node that names one
  setting must not displace every stored default beside it. An empty map is
  omitted from storage, so pipelines written before this are byte-for-byte
  unchanged.
- A configured provider's default request settings are now returned by the
  management API. They were stored and applied but never read back, so an
  operator editing a provider saw an empty form over settings that were still in
  force. Credentials remain redacted; settings are not secret.
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
