# Conduit

Conduit is a local-first voice assistant framework where operators configure, observe, and diagnose voice pipelines.

## Language

**Operator**:
The person using Conduit to configure, observe, and diagnose voice assistant pipelines. The operator is the primary user of the product, not a separate administrative role.
_Avoid_: Admin, end user

**Operator Access**:
The way an operator authenticates to the Conduit UI and management API. The first UI version uses a management bearer token or explicit anonymous mode rather than a separate user account system; browser token persistence is session-only unless the operator explicitly chooses to remember the token on that browser.
_Avoid_: Login

**First-Run Setup**:
The product state when no usable pipeline has been configured yet. In this state, Conduit should guide the operator through creating and validating an initial pipeline.
_Avoid_: Empty state, onboarding

**Guided Setup**:
The first-run path that helps the operator create an initial usable pipeline without manually assembling every graph detail. Guided setup should produce a real pipeline definition, prioritize a minimal working voice loop, and offer optional tool setup without requiring it.
_Avoid_: Wizard

**Provider-First Setup**:
A guided setup sequence that creates required provider definitions before saving a pipeline graph that references them. Pipeline creation depends on provider definitions existing rather than embedding or inventing provider configuration.
_Avoid_: Graph-first setup, inferred provider setup

**Provider Reference Block**:
The delete refusal for a provider definition still referenced by one or more pipeline graphs. The refusal names the affected pipelines so the operator can navigate to edit those references.
_Avoid_: Fake deletion, silent unlink

**Graph Editor**:
The configuration surface for inspecting and editing a pipeline as nodes and edges. The graph editor is the advanced and ongoing configuration view, not the required first step for a new operator.
_Avoid_: Flowchart

**Transport Pipeline**:
The acyclic dataflow portion of a pipeline graph: the modality transforms from source through sink. A transport pipeline runs once per turn in topological order, and its edges describe what feeds what.
_Avoid_: Spine, main flow

**Reasoning Core**:
The single graph node holding a language model binding together with its tool and memory bindings. A reasoning core runs a model-driven iteration whose length is decided at runtime, so its bindings have no execution order and are not pipeline stages.
_Avoid_: Agent, LLM node, brain

**Core Binding**:
A tool or memory attachment on a reasoning core, referencing a provider definition by id and carrying the settings that belong to that attachment in that pipeline. A core binding is configuration on the core, not an edge in the transport pipeline.
_Avoid_: Augment, orbital, spoke

**Modality**:
The kind of data an edge carries: audio, text, or utterance. Sources and sinks declare their modality and every other stage derives one from its kind, so an incompatible connection is a validation error rather than a runtime surprise.
_Avoid_: Media type, format

**Utterance**:
What a reasoning core emits, before any decision about how to render it. Speech is an utterance rendered by synthesis and text is an utterance rendered by a text sink, which is why a core is unchanged by adding a modality.
_Avoid_: Reply, response text

**Pipeline Validation**:
The backend check that a pipeline graph is structurally valid, references existing compatible provider definitions or injected test providers, and can be prepared by the current runtime provider registry snapshot. Pipeline validation does not perform provider reachability checks or create missing providers.
_Avoid_: Provider test, turn test, graph repair

**Pipeline Test Turn**:
A synthetic conversation turn run through a stored pipeline using mock or test audio and the current runtime provider registry snapshot. A successful pipeline test turn proves the providers that were actually invoked during that runtime execution.
_Avoid_: Provider reachability test, graph validation

**Provider Settings**:
The reusable configuration surface for provider credentials, endpoints, model choices, and reachability checks. Guided setup may invoke provider settings inline, but provider configuration is not owned by a single pipeline.
_Avoid_: Pipeline config

**Provider Reachability Test**:
A narrow active check for one provider definition, such as connecting, listing capabilities, or running a tiny non-destructive request. A provider reachability test can make a provider reachable, but it does not prove the provider inside a real pipeline turn.
_Avoid_: Pipeline test turn, configured provider

**Provider Definition**:
A server-owned saved provider configuration with a stable id, capability, component type, and runtime settings. Pipeline graphs reference provider definitions by id; provider definitions are not owned by any single pipeline.
_Avoid_: Provider Settings, node config, runtime provider

**Provider Definition Store**:
The backend-owned storage abstraction for provider definitions, independent of the storage backend used to persist them. Deployments may back it with file formats or databases, but operators and pipelines see the same provider definition semantics.
_Avoid_: PipelineStore, browser storage, status cache

**Runtime Provider**:
An in-memory executable provider registered with the runtime for one capability, built from deployment configuration or a provider definition. Pipeline execution uses runtime providers, while operators save and edit provider definitions.
_Avoid_: Provider Definition, Provider Settings

**Runtime Provider Registry Snapshot**:
An immutable set of runtime providers used by new pipeline validations and turns until replaced by a newer snapshot. Saving or deleting provider definitions rebuilds a fresh snapshot and swaps it in only after validation succeeds.
_Avoid_: Mutable provider registry, partial registration

**Provider Definition Variant**:
A component-specific provider definition shape with named fields for exactly one supported provider component. Variants are closed for now, so adding a provider component expands the typed contract instead of storing arbitrary configuration.
_Avoid_: Config blob, component config map

**Provider Component Catalog**:
The backend-owned catalog of provider component types an operator can create as provider definitions. The catalog describes supported definition variants and form metadata; it is not the runtime registry of executable providers.
_Avoid_: Pipeline component catalog, provider registry

**Provider Secret**:
A secret value used by a provider definition, accepted on writes as an inline value or external reference but never echoed back unredacted by read/status APIs. A redacted provider secret in an update means keeping the existing stored secret.
_Avoid_: API key field, visible credential

**Configured Provider**:
A provider whose definition satisfies the backend schema and is valid enough to save. Configuration does not prove that the provider can currently serve requests.
_Avoid_: Healthy provider

**Untested Provider**:
A configured provider with no current reachability or real-turn proof. Untested providers should be shown as configured rather than unavailable.
_Avoid_: Unavailable provider, failed provider

**Reachable Provider**:
A provider whose endpoint, credentials, and selected model respond to an active check. Reachability does not prove that the provider has succeeded inside a real pipeline turn.
_Avoid_: Healthy provider

**Proven Provider**:
A provider that has completed its role successfully during a real pipeline turn. Proof comes from runtime outcome, not from saved settings or standalone reachability checks.
_Avoid_: Healthy provider

**Operations Workspace**:
The product state when at least one usable pipeline exists. In this state, Conduit should emphasize pipeline health, live activity, connected satellites, and actionable failures.
_Avoid_: Dashboard, home page

**Operator Console**:
The default interface posture for Conduit once a pipeline exists: dense, calm, responsive, and optimized for scanning live operational state. It should feel polished through clarity and low friction rather than decorative marketing patterns.
_Avoid_: Landing page, hero, consumer app

**Responsive Operator Console**:
A desktop-first operator console that remains useful on smaller screens for monitoring, triage, and guided setup. Dense multi-pane workflows belong on desktop/tablet; small screens may simplify graph editing rather than forcing full parity.
_Avoid_: Mobile-first console

**Functional Motion**:
Restrained UI motion that clarifies state changes such as panels appearing, live events arriving, filters changing, or stale state resolving. Motion should support orientation and feedback rather than decorate the interface, and must never be the only way an operator learns that state changed.
_Avoid_: Animation

**Exception-First Overview**:
An overview behavior where failures, degraded components, and affected pipelines rise above normal baseline context. When nothing needs attention, the overview remains compact and calm.
_Avoid_: Balanced dashboard

**Stale State**:
Operator-visible state shown when the UI has lost its live event stream and is no longer receiving updates. Stale state preserves the last known snapshot but must be labeled until a fresh snapshot is loaded and live updates resume.
_Avoid_: Offline mode

**Turn Reconstruction**:
An operator-facing view of a conversation turn as an ordered story of invoked components, events, timing, outputs, and failures. Turn reconstruction is the primary purpose of event inspection; the raw event stream is secondary.
_Avoid_: Event log, firehose

**Turn Reconstruction Contract**:
The server-owned API/read-model shape for a reconstructed turn, derived from raw runtime events while preserving references back to those events. UI clients may render this contract differently, but should not invent turn ordering, tool grouping, spoken segment boundaries, or pipeline attribution themselves.
_Avoid_: Frontend reconstruction, derived event UI

**Live Turn Reconstruction**:
The operator-facing reconstruction of a turn while it is still running. It updates from runtime events as they arrive and may be incomplete until the turn reaches a terminal outcome.
_Avoid_: Live event log

**Turn History**:
The queryable set of completed or recently observed turn reconstructions available after live streaming has moved on. Turn history exists so operators can inspect failures and outcomes without depending on a browser-local event buffer.
_Avoid_: Browser history, cached events

**Raw Event Evidence**:
The original event envelopes retained behind a turn reconstruction for diagnostics, contract verification, and reconstruction debugging. Raw event evidence supports the reconstruction contract but is not the primary operator-facing representation of a turn.
_Avoid_: Primary event view, UI source of truth

**Sensitive Tool Evidence**:
Tool arguments and result payloads retained with raw event evidence for diagnostics but omitted from the default operator-facing reconstruction. Sensitive tool evidence may be exposed only through an explicit inspection path that can apply redaction and access controls.
_Avoid_: Timeline detail, default tool output

**Diagnostic Payload Access**:
The explicit, higher-trust inspection path for sensitive tool evidence. Diagnostic payload access is separate from ordinary turn reconstruction viewing and should support redaction before any unredacted payload exposure is considered.
_Avoid_: Normal operator view, raw details toggle

**Utterance Segment**:
A text span a turn intentionally emitted as one unit, carrying the modality it was rendered in. Utterance segments distinguish assistant preambles, tool-spoken output, and final assistant responses rather than treating language-model token deltas as boundaries.
_Avoid_: Token stream, transcript chunk

**Spoken Segment**:
An utterance segment rendered as audio by speech synthesis. The narrower term is correct only for a turn that synthesized speech; a text pipeline emits utterance segments that were never spoken.
_Avoid_: Using this for every segment

**Tool Batch**:
The set of tool calls requested by one model response and executed concurrently before the next model round. A tool batch may overlap a spoken assistant preamble and contains the lifecycle, outcome, and errors of each requested call.
_Avoid_: Tool list, tool event group

**Reconstruction Item**:
A stable, addressable part of a turn reconstruction, such as an utterance segment, tool batch, tool call, component step, or raw event reference. Reconstruction item identity should be stable across live updates and later history queries.
_Avoid_: Timeline row, projection index

**Turn Status**:
The coarse outcome state of a reconstructed turn, such as running, completed, cancelled, failed, or degraded. Interruption is presented from a cancelled turn's reason rather than modeled as its own top-level status.
_Avoid_: Interruption status

**Reconstruction Boundary Event**:
A runtime event that declares a boundary the runtime knows directly, such as an utterance segment starting or a tool batch beginning. Reconstruction boundary events prevent the server read model and UI from inferring utterance or concurrency boundaries from nearby lower-level events.
_Avoid_: Inferred boundary, UI grouping hint

**Pipeline Health**:
An operator-facing state that combines whether a pipeline is configured and runnable with the recent outcomes of real turns through that pipeline. A pipeline can be unhealthy because a recent turn failed, including synthesis failures after speech generation was attempted, and remains unhealthy until a later successful turn proves recovery.
_Avoid_: Validity, readiness

**Provider Status**:
An operator-facing state that can warn before a pipeline turn fails by distinguishing unavailable, configured, reachable, and proven providers. Provider status informs pipeline risk, but pipeline health is still determined by runnable configuration and real turn outcomes.
_Avoid_: Pipeline health

**Component Health**:
An operator-facing state for an individual pipeline component, such as recognition, reasoning, tool execution, or synthesis. Component health explains pipeline health by identifying which invoked component is failing, recovering, or unproven.
_Avoid_: Node status

**Successful Turn**:
A completed conversation turn in which every pipeline component that was actually invoked completed without an unrecovered error. Components that were not needed for that turn, such as tools when the model requested none, do not have to run for the turn to be successful.
_Avoid_: Full pipeline pass

**Satellite**:
A device that connects to Conduit to hold voice conversations through a pipeline. A satellite is identified by device authentication and event attribution, not by speaker identity.
_Avoid_: Client, endpoint, speaker

**Connected Satellite**:
A satellite with an open conversation connection right now. This is a presence state, not evidence that the satellite recently completed a successful turn.
_Avoid_: Active satellite

**Recently Active Satellite**:
A satellite that has emitted events within a recent operator-facing time window. This is activity history, not proof that the satellite is currently connected.
_Avoid_: Active satellite
