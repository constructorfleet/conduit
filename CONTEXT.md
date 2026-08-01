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

**Graph Editor**:
The configuration surface for inspecting and editing a pipeline as nodes and edges. The graph editor is the advanced and ongoing configuration view, not the required first step for a new operator.
_Avoid_: Flowchart

**Provider Settings**:
The reusable configuration surface for provider credentials, endpoints, model choices, and reachability checks. Guided setup may invoke provider settings inline, but provider configuration is not owned by a single pipeline.
_Avoid_: Pipeline config

**Configured Provider**:
A provider whose required local settings are present and valid enough to save. Configuration does not prove that the provider can currently serve requests.
_Avoid_: Healthy provider

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
