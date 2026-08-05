# Voice Activity Detection As Two Decisions

Voice activity detection enters Conduit as **two separate changes, decided
separately and shipped in order**. The first is a `vad` transport stage that
trims silence from capture on the input path; it is a modality transform in the
existing graph and needs nothing that does not already exist. The second is
barge-in — noticing someone speaking over the assistant — which is **not**
blocked on a detector at all. It is blocked on the device protocol, and that
protocol decision is deferred to its own ADR rather than folded into this one.

Treating these as one feature is what made the work look like "add a provider".
It is not. Reading the code in the order a frame travels makes the split
obvious.

**What the audio path actually does**

A device's socket takes `Command::End` to mean the utterance is over, and it
implements that by dropping the sender: `crates/conduit-api/src/converse.rs`
sets `sender = None` on `End`, and every binary frame that arrives afterwards is
discarded with `ignoring audio sent after the end of the utterance`. Reading
continues only so that a later `Stop` is still heard.

Barge-in requires listening *while the assistant speaks*, which is strictly
after `End`. So there is no audio stream in existence during playback for any
detector to be attached to. No provider, however good, changes that: a VAD
handed no frames reports no voice. The blocker is the protocol, not the model.

This also explains the shape of the second stage. The two stage precedents in
the runtime are `conduit-runtime/src/wake.rs::gate`, which withholds audio from
the recognizer until a phrase fires and keeps a 500 ms pre-roll because a
detector reports some way after the phrase ended, and
`conduit-runtime/src/identity.rs::fork`, which clones the stream so
identification runs *beside* recognition instead of ahead of it. Both are called
from `turn.rs::listen`, and both operate on capture, before the reply exists. A
silence-trimming VAD is the gate shape and inherits the pre-roll problem
verbatim — trimming at the instant a detector says "speech started" clips the
first word, which is the same bug the wake gate's `PRE_ROLL` constant exists to
prevent. A barge-in detector is **neither** shape: it has to run while the
output stage is active, concurrently with synthesis and playback, which is a
position `listen` does not have and which no existing stage occupies. That is a
new execution position in the turn, not a new node in the spine.

**Decision one: a `vad` transport stage on the input path**

`ProviderCapability` has eight variants today — `Stt`, `Llm`, `Tts`,
`Transform`, `Tool`, `Wake`, `SpeakerId`, `Memory` — in
`conduit-provider/src/storage/mod.rs`, and `NodeKind` has eight — `Source`,
`WakeWord`, `Stt`, `SpeakerId`, `Core`, `Transform`, `Tts`, `Sink` — in
`conduit-core/src/graph.rs`. Neither has a `Vad`. Detection therefore needs both
a capability and a node kind, which is a vocabulary change reaching storage, the
graph, the generated frontend contract, and the editor's palette.

A `vad` node reads audio and writes audio, so it declares
`Modality::Audio` on both sides and its edges **are** checked. Per
[ADR-0012](0012-transport-pipeline-and-reasoning-core.md) every kind left in the
graph is a transport stage and answers both `output_modality` and
`accepted_modalities`; there is no unchecked category left to hide in, because
the two former exceptions — tools and memory — were moved onto the reasoning
core and no longer have edges. A graph wiring a `vad` to a text edge must fail
validation naming the offending edge, exactly as `tts -> core` does now.

A VAD that fails does **not** end the turn. The two precedents differ here on
purpose: a wake detector that fails ends the turn, because a pipeline that
cannot tell whether it was addressed should not guess, while an identifier that
fails does not, because not knowing who spoke is how every pipeline behaved
before the stage existed. VAD is the identifier case. Falling back to untrimmed
audio is precisely the old behaviour, and a recognizer that receives a little
silence is strictly better than a turn that receives nothing.

The provider descriptor reports supported sample rates and frame sizes, and a
mismatch is refused rather than resampled silently. These models are
fixed-window — a wrong frame size does not degrade a detector, it invalidates
it — so the refusal belongs at registration where an operator sees it.

**Decision two: barge-in is deferred, and depends on a protocol ADR**

Barge-in is not decided here. What is decided is that it does not begin until
the duplex-audio question is answered in its own ADR, because the candidate
answers have materially different compatibility stories for satellites already
deployed: a second logical stream that outlives `End`; redefining `End` as
"utterance over, keep listening"; or an explicit duplex capability negotiated at
socket open. Only the third leaves existing devices behaving identically, and
only the first two avoid adding negotiation to a socket that has none.

More is already wired for barge-in than the issue's framing suggests, which
shrinks the remaining work to the audio path and the emitter:

- `CancelReason::BargeIn` exists and is terminal
  (`conduit-core/src/event.rs:482`, asserted terminal at `:538`).
- `conduit-metrics` already maps it to the `barge_in` outcome label
  (`collector.rs:325`), and `tests/derived.rs:174-177` asserts the mapping
  while noting the runtime does not publish it — so a dashboard counting
  interruptions works the moment something emits it.
- The frontend contract already names it: `frontend_contract.rs:1387` and
  `frontend/src/contracts/events.ts:9`.
- The TTS and LLM provider traits already document cancellation as the mechanism
  barge-in uses — dropping the generation stream cancels it
  (`conduit-provider/src/llm.rs:160`, `tts.rs:76`), and the ElevenLabs provider
  has a stalled-reply test for exactly that path
  (`conduit-elevenlabs/tests/failures.rs:118-122`).
- [ADR-0010](0010-server-owned-turn-reconstruction.md) already states that
  interruption is presented as `cancelled` plus a reason rather than as a
  separate status, so no status vocabulary changes.

So the reason, the metric label, the wire contract, and the cancellation
mechanism are all in place. What is missing is audio during playback, and a
detector attached to it.

`crates/conduit-runtime/tests/turn.rs:711`,
`nothing_the_runtime_does_reports_barge_in`, drives four turn endings — a stop, a
dropped listener, a stage failure, and an uninterrupted reply — and asserts none
reports barge-in. It is a tripwire, not an oversight: the comment records that
labelling every lost listener `barge_in` is what let a panel counting
interruptions quietly count dropped connections instead, and `README.md:514`
tells operators their old `barge_in` alerts went quiet for that reason. When
barge-in is implemented, that test is **narrowed** — those four endings must
still assert non-barge-in, and a fifth case is added in which real overlapping
speech produces it. Deleting it, or relaxing the assertion so a new emitter
passes, reintroduces the miscount the tripwire was installed to catch.

**Decision three: which detectors, and dropping Whisper**

Nothing in the repository references a VAD implementation today: `silero`,
`webrtc`, `cobra`, `marblenet`, and `vad` return no hits across sources,
manifests, or docs. Every option below is a new dependency decision, not an
existing one being surfaced.

Whisper is dropped from the list. It is a recognizer, so using it for voice
activity means running transcription to answer a yes/no question: it needs a
full window before it says anything, which is latency a barge-in detector cannot
afford and cost a silence trimmer does not need to pay. `whisper.cpp`'s own VAD
preprocessor is a different component and would have to be named as such rather
than inherited from the word "Whisper".

- **Silero** — ONNX, small, runs in-process with no service. The one worth doing
  first, and the natural `builtin` of this capability; the cost is an ONNX
  runtime dependency in the default build.
- **WebRTC VAD** — a tiny, battle-tested C library, cheap enough for
  frame-by-frame use, but it is FFI and noticeably less accurate than Silero in
  noise. Decide FFI-vs-skip explicitly and write the reasoning down either way;
  the precedent for declining is PicoTTS, refused in `README.md` with its
  rationale recorded rather than left a silent omission.
- **Picovoice Cobra** — accurate and small, but proprietary and gated on an
  access key with licence terms Conduit does not currently surface anywhere. A
  provider that cannot be used without accepting unsurfaced terms is worse than
  no provider, so the licensing posture is decided before any code.
- **TEN VAD, MarbleNet** — served or in-process is unstated, and that choice is
  the whole shape of the integration. Name the target before coding.

**Consequences**

- `ProviderCapability` gains `Vad` and `NodeKind` gains `Vad`, taking both from
  eight variants to nine. Adding a `NodeKind` is not local: it touches
  `Node::kind`, `output_modality`, and `accepted_modalities` in
  `conduit-core/src/graph.rs`; `provider_capability_for_node` in
  `conduit-api/src/pipelines.rs`; the two node-kind mappings in
  `conduit-api/src/status.rs`; the generated TypeScript union checked by
  `conduit-api/tests/frontend_contract.rs`; and, in the editor,
  `LINEAR_STAGE_ORDER` and `outputModality` in `frontend/src/pipelines/graph.ts`.
  The frontend work is owned elsewhere and is listed here as scope, not as
  instruction.
- `vad` edges are modality-checked as audio-to-audio, so a miswired VAD fails
  validation naming the edge rather than surprising the runtime.
- Stored pipelines are **not** migrated. A new optional node kind is additive:
  existing graphs contain no `vad` node and continue to validate and run
  unchanged, so there is nothing to migrate. This is narrower than the
  no-migration stances in [ADR-0011](0011-typed-provider-definitions.md) and
  [ADR-0012](0012-transport-pipeline-and-reasoning-core.md), which removed or
  reshaped variants operators had already saved.
- The silence-trimming stage ships without any protocol change and without any
  new event. It changes what the recognizer hears, not what the turn reports.
- A test asserts trimming does not clip the first word, borrowing the wake
  gate's pre-roll reasoning; a VAD failure is recovered rather than fatal, and a
  test asserts the turn still answers.
- Barge-in remains unimplemented and `CancelReason::BargeIn` remains without an
  emitter. `nothing_the_runtime_does_reports_barge_in` stays exactly as written
  until the protocol ADR lands, and is narrowed rather than removed when it does.
- `README.md`'s "Barge-in is not detected, only requested" gap stays standing,
  because this decision does not close it. The gap text is rewritten by the
  barge-in change, not by this one.
- A `vad` node is a graph stage the runtime must also execute. Consistent with
  [ADR-0012](0012-transport-pipeline-and-reasoning-core.md), the graph model may
  run ahead of the runtime, but a `vad` node that validates and is then silently
  ignored is the failure mode to avoid: prepare-time refusal is preferred over
  accepting a stage that does nothing.

**Open questions**

These need a human answer and are not decided here.

- How does audio reach the server during playback? This is the blocking
  decision, and it is a device-protocol change with a backward-compatibility
  story for deployed satellites. It belongs in its own ADR.
- Is a barge-in detector a graph node at all, or a property of the source? It
  occupies neither existing stage position, and modelling it as a spine node
  would place it somewhere the turn does not have a slot for.
- What ends a turn on barge-in: any detected voice, or voice sustained past a
  threshold? A playback detector that fires on the assistant's own audio leaking
  through a speaker is the obvious hazard, and echo cancellation is not
  something Conduit does.
- Does Silero's ONNX runtime belong in the default build, or behind a feature?
  The same question the rest of the in-process providers answered, asked again.
- What is the licensing posture for Picovoice Cobra, and does Conduit surface
  third-party terms anywhere today?
- Is WebRTC VAD worth an FFI dependency once Silero exists, or is declining it
  the better recorded decision?
