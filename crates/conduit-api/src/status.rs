//! Operator-facing runtime status contract.
//!
//! This module defines the JSON shapes shared by the backend status endpoint
//! and the future Operator Console. It intentionally contains only contract
//! vocabulary and small semantic helpers; runtime projection code belongs in
//! the status API implementation.

use chrono::{DateTime, Utc};
use conduit_core::id::{ConversationId, DeviceId, TraceId, TurnId};
use serde::{Deserialize, Serialize};

/// Coherent source-of-truth snapshot for the Operator Console.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorStatusSnapshot {
    /// When the server produced this snapshot.
    pub generated_at: DateTime<Utc>,
    /// Launch and freshness posture for the UI.
    pub runtime: RuntimeState,
    /// Pipeline health summaries for exception-first scanning.
    pub pipelines: Vec<PipelineStatus>,
    /// Reusable provider status, independent of any single pipeline.
    pub providers: Vec<ProviderStatus>,
    /// Connected and recently active satellite state.
    pub satellites: SatelliteStatus,
    /// Turns currently known to be running.
    pub active_turns: Vec<ActiveTurnStatus>,
    /// Recent durable failures to surface above baseline context.
    pub recent_failures: Vec<RuntimeFailure>,
    /// How this snapshot is kept current with the event stream.
    pub event_stream: EventStreamContract,
}

/// Launch and freshness posture for the Operator Console.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeState {
    /// Which product state should open first.
    pub launch_state: LaunchState,
    /// Whether the browser view should consider the current state live.
    pub stale_state: StaleState,
}

/// Product state selected from whether a usable pipeline exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchState {
    /// No usable pipeline exists; guide the operator through setup.
    FirstRunSetup,
    /// At least one usable pipeline exists; open the operations workspace.
    OperationsWorkspace,
}

/// Browser-visible freshness of snapshot-plus-events state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleState {
    /// The snapshot has loaded and live events are connected.
    Fresh,
    /// The last known state remains visible but live events are disconnected.
    Stale,
}

/// Operator-facing status for one pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineStatus {
    /// Stored pipeline name.
    pub name: String,
    /// Whether the current graph is runnable enough to enter operations.
    pub usable: bool,
    /// Pipeline-level health derived from runnable configuration and turns.
    pub health: PipelineHealth,
    /// Component-level explanation for pipeline health.
    pub components: Vec<ComponentHealth>,
    /// Provider identifiers currently affecting this pipeline.
    pub affected_providers: Vec<String>,
}

/// Pipeline-level health summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineHealth {
    /// Current health state.
    pub state: PipelineHealthState,
    /// Short operator-facing explanation.
    pub summary: String,
    /// Most recent successful turn proving recovery, if one is known.
    pub last_successful_turn: Option<TurnId>,
    /// Most recent failed turn keeping the pipeline unhealthy, if any.
    pub last_failed_turn: Option<TurnId>,
}

/// Pipeline health state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineHealthState {
    /// The stored graph cannot run as a voice pipeline.
    NotRunnable,
    /// The pipeline is runnable, but no real successful turn has proven it.
    Unproven,
    /// Real turns have completed successfully and no unrecovered failure is known.
    Healthy,
    /// The pipeline can run, but related provider or component risk is visible.
    Degraded,
    /// A runtime failure remains uncleared by a later successful turn.
    Unhealthy,
}

/// Component-level health for one invoked or configured pipeline stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentHealth {
    /// Pipeline component kind.
    pub kind: ComponentKind,
    /// Provider backing this component, when it has one.
    pub provider: Option<String>,
    /// Current component health.
    pub state: ComponentHealthState,
    /// Operator-facing explanation of the state.
    pub detail: Option<String>,
    /// Most recent turn that affected this component, if known.
    pub last_turn: Option<TurnId>,
}

/// Pipeline component kinds surfaced to operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    /// Audio capture from a satellite.
    Capture,
    /// Speech-to-text transcription.
    Transcription,
    /// Language-model reasoning.
    Reasoning,
    /// Tool invocation.
    Tools,
    /// Text-to-speech synthesis.
    Synthesis,
}

/// Component health state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentHealthState {
    /// The component is missing or invalid in the pipeline graph.
    NotConfigured,
    /// The component was not needed in the relevant turn.
    Unused,
    /// The component has not succeeded in a real turn yet.
    Unproven,
    /// The component completed successfully when last invoked.
    Healthy,
    /// The component has a warning that has not yet failed a turn.
    Degraded,
    /// The component failed and has not been proven recovered.
    Unhealthy,
}

/// Reusable provider status, separate from pipeline health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    /// Stable provider settings identifier.
    pub id: String,
    /// Capability exposed by this provider.
    pub kind: ProviderKind,
    /// Status distinction for configuration, reachability, and proof.
    pub state: ProviderStatusState,
    /// Whether required settings are present and valid enough to save.
    pub configured: bool,
    /// Whether an active reachability check has succeeded.
    pub reachable: bool,
    /// Most recent turn that proved this provider inside a real pipeline.
    pub proven_by_turn: Option<TurnId>,
    /// Operator-facing status detail.
    pub message: Option<String>,
    /// Pipelines that currently reference or depend on this provider.
    pub affects_pipelines: Vec<String>,
}

/// Provider capability kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Speech-to-text provider.
    Stt,
    /// Language-model provider.
    Llm,
    /// Tool provider or registry.
    Tool,
    /// Text-to-speech provider.
    Tts,
}

/// Provider status state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatusState {
    /// No usable provider settings are available.
    Unavailable,
    /// Provider settings exist but have not passed reachability.
    Configured,
    /// A standalone reachability check passed.
    Reachable,
    /// The provider succeeded inside a real pipeline turn.
    Proven,
}

/// Satellite presence and recent activity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SatelliteStatus {
    /// Satellites with an open conversation connection right now.
    pub connected: Vec<ConnectedSatellite>,
    /// Satellites that emitted events within the recent activity window.
    pub recently_active: Vec<RecentlyActiveSatellite>,
    /// Window used for recent activity, in seconds.
    pub recent_window_seconds: u64,
}

/// A satellite with an open conversation connection right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectedSatellite {
    /// Device identifier attached to events.
    pub device: DeviceId,
    /// Operator-facing device name.
    pub name: String,
    /// When this connection opened.
    pub connected_since: DateTime<Utc>,
    /// Conversation held by this connection, if already started.
    pub conversation: Option<ConversationId>,
    /// Pipeline this connection is using.
    pub pipeline: String,
}

/// A satellite that emitted recent events, whether or not it is connected now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentlyActiveSatellite {
    /// Device identifier attached to events.
    pub device: DeviceId,
    /// Operator-facing device name.
    pub name: String,
    /// Most recent event time.
    pub last_seen_at: DateTime<Utc>,
    /// Last event type observed for this satellite.
    pub last_event: String,
}

/// A currently running turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveTurnStatus {
    /// Pipeline running the turn.
    pub pipeline: String,
    /// Conversation containing the turn.
    pub conversation: ConversationId,
    /// Turn identifier.
    pub turn: TurnId,
    /// Trace that correlates events for this turn.
    pub trace: TraceId,
    /// When the turn began.
    pub started_at: DateTime<Utc>,
    /// Components invoked so far.
    pub invoked_components: Vec<ComponentKind>,
}

/// Durable failure surfaced in the exception-first overview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeFailure {
    /// Pipeline affected by the failure.
    pub pipeline: String,
    /// Turn that failed, if the failure happened inside one.
    pub turn: Option<TurnId>,
    /// Component that failed.
    pub component: ComponentKind,
    /// Provider involved in the failure, if any.
    pub provider: Option<String>,
    /// Operator-facing failure message.
    pub message: String,
    /// When the failure occurred.
    pub at: DateTime<Utc>,
}

/// Contract for applying live events after loading the snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventStreamContract {
    /// SSE route used for live updates.
    pub route: String,
    /// Freshness state the UI must show when the stream disconnects.
    pub stale_state_on_disconnect: StaleState,
    /// Whether reconnect requires a new snapshot before applying events again.
    pub refresh_snapshot_after_reconnect: bool,
    /// Snapshot resources updated by event variants.
    pub bindings: Vec<SnapshotEventBinding>,
}

/// Event variants that update one snapshot resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEventBinding {
    /// Snapshot resource updated by these events.
    pub resource: SnapshotResource,
    /// Event variant names from `conduit_core::event::Event`.
    pub events: Vec<String>,
}

/// Snapshot resource category updated by live events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotResource {
    /// Runtime launch and freshness state.
    RuntimeState,
    /// Pipeline and component health.
    PipelineHealth,
    /// Provider status.
    ProviderStatus,
    /// Connected and recently active satellite state.
    SatelliteStatus,
    /// Active turn list.
    ActiveTurns,
    /// Durable recent failure list.
    RecentFailures,
}

/// Outcome of a completed turn for status projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnOutcome {
    /// Turn identifier.
    pub turn: TurnId,
    /// Completed turn result.
    pub result: TurnResult,
    /// Components actually invoked by the turn.
    pub invoked_components: Vec<ComponentKind>,
    /// Components with unrecovered failures.
    pub failed_components: Vec<ComponentKind>,
}

impl TurnOutcome {
    /// Whether this successful turn proves recovery for `component`.
    #[must_use]
    pub fn proves_recovery_for(&self, component: ComponentKind) -> bool {
        self.result == TurnResult::Successful
            && self.invoked_components.contains(&component)
            && !self.failed_components.contains(&component)
    }
}

/// Completed turn result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnResult {
    /// Every actually invoked component completed without unrecovered error.
    Successful,
    /// At least one actually invoked component has an unrecovered error.
    Failed,
    /// The turn ended before completion.
    Cancelled,
}
