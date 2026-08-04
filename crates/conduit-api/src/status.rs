//! Operator-facing runtime status contract.
//!
//! This module defines the JSON shapes shared by the backend status endpoint
//! and the future Operator Console. It intentionally contains only contract
//! vocabulary and small semantic helpers; runtime projection code belongs in
//! the status API implementation.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use axum::extract::State;
use chrono::{DateTime, TimeDelta, Utc};
use conduit_core::bus::{EventBus, Subscription};
use conduit_core::event::{CancelReason, Envelope, Event};
use conduit_core::graph::{NodeKind, PipelineGraph};
use conduit_core::id::{ConversationId, DeviceId, TraceId, TurnId};
use conduit_provider::storage::{ProviderCapability, ProviderDefinition};
use conduit_provider::Health;
use conduit_runtime::Runner;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::auth::ManagementCaller;
use crate::{ApiError, AppState};

/// Operator-facing recent satellite activity window.
pub const RECENT_SATELLITE_WINDOW_SECONDS: u64 = 300;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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
    ///
    /// The selector: what a pipeline node names and what an operator
    /// configured. Distinct from [`provider`](ProviderStatus::provider), which
    /// is what the implementation behind it calls itself.
    pub id: String,
    /// Capability exposed by this provider.
    pub kind: ProviderKind,
    /// Identity the registered implementation states, e.g. `"openai"`.
    ///
    /// Read from the provider's descriptor, so it is present only for a
    /// provider the runtime actually built: a definition whose service would
    /// not start, or a node naming a provider nobody configured, has an id and
    /// no implementation to ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Display name the registered implementation offers for operator screens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Version the registered implementation reports, for diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
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
    /// Tools this provider offers, for a definition that offers several.
    ///
    /// An MCP definition describes a server, and a server advertises any
    /// number of tools. They belong to the definition the way models belong to
    /// a language model provider — listed here rather than reported as
    /// providers of their own, which would put a dozen entries on the
    /// operator's Providers page for one thing they configured.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offers_tools: Vec<String>,
}

/// Provider capability kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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
    /// Utterance transform.
    Transform,
    /// Wake word detector.
    Wake,
    /// Speaker identification provider.
    SpeakerId,
    /// Long-term memory store.
    Memory,
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

/// Runtime status projection fed by the event bus.
#[derive(Debug, Clone, Default)]
pub struct RuntimeStatus {
    inner: Arc<RwLock<Projection>>,
}

impl RuntimeStatus {
    /// Builds an empty runtime status projection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one envelope into the projection.
    pub async fn record(&self, envelope: &Envelope) {
        self.inner.write().await.record(envelope);
    }

    /// Marks a satellite as connected to a conversation socket.
    pub async fn connect_satellite(
        &self,
        device: DeviceId,
        name: impl Into<String>,
        pipeline: impl Into<String>,
        conversation: ConversationId,
    ) {
        self.inner.write().await.connect_satellite(
            device,
            name.into(),
            pipeline.into(),
            conversation,
        );
    }

    /// Marks a satellite conversation socket as closed.
    pub async fn disconnect_satellite(&self, device: DeviceId) {
        self.inner.write().await.disconnect_satellite(device);
    }

    async fn project(&self) -> Projection {
        self.inner.read().await.clone()
    }
}

/// Subscribes to the event bus and keeps [`RuntimeStatus`] current.
pub struct StatusCollector;

impl StatusCollector {
    /// Subscribes to `bus` and runs in the background.
    pub fn spawn(status: RuntimeStatus, bus: &EventBus) -> tokio::task::JoinHandle<()> {
        let subscription = bus.subscribe();
        tokio::spawn(async move { run_collector(status, subscription).await })
    }
}

async fn run_collector(status: RuntimeStatus, mut subscription: Subscription) {
    while let Some(envelope) = subscription.recv().await {
        status.record(&envelope).await;
    }
    tracing::debug!("event bus closed; status collector stopping");
}

/// `GET /v1/status` — coherent operator status snapshot.
pub(crate) async fn get(
    // First so auth is checked before any store or projection work.
    _caller: ManagementCaller,
    State(state): State<AppState>,
) -> Result<axum::Json<OperatorStatusSnapshot>, ApiError> {
    Ok(axum::Json(snapshot(&state).await?))
}

async fn snapshot(state: &AppState) -> Result<OperatorStatusSnapshot, ApiError> {
    let generated_at = Utc::now();
    let projection = state.status().project().await;
    let names = state
        .pipeline_names()
        .await
        .map_err(|error| ApiError::unavailable(error.to_string()))?;
    let providers = state.providers();
    let provider_definitions = provider_definitions(state).await?;
    let provider_reachability = state.provider_reachability();

    // A stored graph that will not decode — one written before a model change,
    // or edited by hand — is reported as its own broken pipeline rather than
    // failing the whole snapshot. Failing it would leave the operator with no
    // console to fix the pipeline from, which is the one thing they need.
    let mut graphs = Vec::new();
    let mut unreadable = Vec::new();
    for name in names {
        match state.pipeline(&name).await {
            Ok(Some(graph)) => graphs.push((name, graph)),
            // Listing named it, so a pipeline that is now absent was removed
            // between the two calls and simply is not there any more.
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    pipeline = %name,
                    error = %error,
                    "stored pipeline cannot be read; reporting it as not runnable"
                );
                unreadable.push(unreadable_pipeline(name, &error));
            }
        }
    }

    let pipelines = graphs
        .iter()
        .map(|(name, graph)| project_pipeline(name, graph, providers.as_deref(), &projection))
        .chain(unreadable)
        .collect::<Vec<_>>();
    let launch_state = if pipelines.iter().any(|pipeline| pipeline.usable) {
        LaunchState::OperationsWorkspace
    } else {
        LaunchState::FirstRunSetup
    };
    let provider_statuses = project_provider_statuses(
        &graphs,
        providers.as_deref(),
        &projection,
        &provider_definitions,
        &provider_reachability,
    )
    .await;

    let recent_after =
        generated_at - TimeDelta::seconds(RECENT_SATELLITE_WINDOW_SECONDS as i64);

    Ok(OperatorStatusSnapshot {
        generated_at,
        runtime: RuntimeState { launch_state, stale_state: StaleState::Fresh },
        pipelines,
        providers: provider_statuses,
        satellites: SatelliteStatus {
            connected: projection.connected_satellites.values().cloned().collect(),
            recently_active: projection
                .recent_satellites
                .values()
                .filter(|satellite| satellite.last_seen_at >= recent_after)
                .cloned()
                .collect(),
            recent_window_seconds: RECENT_SATELLITE_WINDOW_SECONDS,
        },
        active_turns: projection
            .active_turns
            .values()
            .map(|turn| ActiveTurnStatus {
                pipeline: turn.pipeline.clone(),
                conversation: turn.conversation,
                turn: turn.turn,
                trace: turn.trace,
                started_at: turn.started_at,
                invoked_components: turn.invoked_components.iter().copied().collect(),
            })
            .collect(),
        recent_failures: projection
            .pipelines
            .values()
            .flat_map(|pipeline| pipeline.recent_failures.iter().cloned())
            .collect(),
        event_stream: default_event_stream_contract(),
    })
}

async fn provider_definitions(state: &AppState) -> Result<Vec<ProviderDefinition>, ApiError> {
    let ids = state
        .provider_definition_ids()
        .await
        .map_err(|error| ApiError::unavailable(error.to_string()))?;
    let mut definitions = Vec::new();
    for id in ids {
        let Some(definition) = state
            .provider_definition(&id)
            .await
            .map_err(|error| ApiError::unavailable(error.to_string()))?
        else {
            continue;
        };
        definitions.push(definition);
    }
    Ok(definitions)
}

/// Reports a stored pipeline that cannot be decoded.
///
/// There is no graph to project components or providers from, so the status
/// carries only what is known: the name it is stored under and why reading it
/// failed.
fn unreadable_pipeline(name: String, error: &conduit_core::Error) -> PipelineStatus {
    PipelineStatus {
        name,
        usable: false,
        health: PipelineHealth {
            state: PipelineHealthState::NotRunnable,
            summary: format!("this pipeline cannot be read: {error}"),
            last_successful_turn: None,
            last_failed_turn: None,
        },
        components: Vec::new(),
        affected_providers: Vec::new(),
    }
}

fn project_pipeline(
    name: &str,
    graph: &PipelineGraph,
    providers: Option<&conduit_runtime::Providers>,
    projection: &Projection,
) -> PipelineStatus {
    let usable = providers.is_some_and(|providers| {
        Runner::prepare(graph, providers, EventBus::default()).is_ok()
    });
    let runtime = projection.pipelines.get(name);
    let unresolved_failures =
        runtime.map_or_else(BTreeSet::new, |runtime| runtime.unresolved_failures.clone());
    let last_successful_turn = runtime.and_then(|runtime| runtime.last_successful_turn);
    let last_failed_turn = runtime.and_then(|runtime| runtime.last_failed_turn);

    let state = if !usable {
        PipelineHealthState::NotRunnable
    } else if !unresolved_failures.is_empty() {
        PipelineHealthState::Unhealthy
    } else if last_successful_turn.is_some() {
        PipelineHealthState::Healthy
    } else {
        PipelineHealthState::Unproven
    };

    let summary = match state {
        PipelineHealthState::NotRunnable => "pipeline is not runnable".to_owned(),
        PipelineHealthState::Unproven => {
            "no successful turn has proven this pipeline".to_owned()
        }
        PipelineHealthState::Healthy => "last invoked turn completed successfully".to_owned(),
        PipelineHealthState::Degraded => {
            "pipeline has provider or component warnings".to_owned()
        }
        PipelineHealthState::Unhealthy => {
            "a runtime failure remains uncleared by a later successful turn".to_owned()
        }
    };

    PipelineStatus {
        name: name.to_owned(),
        usable,
        health: PipelineHealth { state, summary, last_successful_turn, last_failed_turn },
        components: project_components(graph, runtime, usable),
        affected_providers: pipeline_provider_ids(graph),
    }
}

async fn project_provider_statuses(
    graphs: &[(String, PipelineGraph)],
    providers: Option<&conduit_runtime::Providers>,
    projection: &Projection,
    definitions: &[ProviderDefinition],
    reachability: &BTreeMap<String, Health>,
) -> Vec<ProviderStatus> {
    let references = provider_references(graphs);
    let proven = proven_providers(graphs, projection);
    let failed = failed_providers(graphs, projection);
    let definition_keys = definitions
        .iter()
        .map(|definition| ProviderKey {
            kind: provider_kind_for_capability(definition.capability()),
            id: definition.id.clone(),
        })
        .collect::<HashSet<_>>();
    // What each registered provider says about itself, keyed by the selector it
    // is registered under. Collected once because both loops below need it: a
    // definition reports the identity of the provider it built, and a provider
    // registered without a definition reports its own.
    let registered = providers
        .map(|providers| {
            providers
                .descriptors()
                .into_iter()
                .map(|(capability, key, descriptor)| {
                    (ProviderKey { kind: provider_kind_for(capability), id: key }, descriptor)
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut statuses = Vec::new();
    let mut seen = HashSet::new();

    for definition in definitions {
        let key = ProviderKey {
            kind: provider_kind_for_capability(definition.capability()),
            id: definition.id.clone(),
        };
        push_definition_status(
            definition,
            registered.get(&key),
            discovered_tools(providers, &definition.id),
            reachability.get(&definition.id),
            &references,
            &proven,
            &failed,
            &mut statuses,
            &mut seen,
        );
    }

    if let Some(providers) = providers {
        collect_registered_statuses(
            providers,
            &references,
            &proven,
            &failed,
            &definition_keys,
            &mut statuses,
            &mut seen,
        )
        .await;
    }
    for kind in [ProviderKind::Llm, ProviderKind::Stt, ProviderKind::Tts] {
        if provider_kind_missing(kind, &seen) && !references.keys().any(|key| key.kind == kind)
        {
            let id = unavailable_slot_id(kind);
            let key = ProviderKey { kind, id: id.to_owned() };
            if seen.insert(key.clone()) {
                let pipelines = references.get(&key).cloned().unwrap_or_default();
                statuses.push(unavailable_provider(key, unavailable_message(kind), pipelines));
            }
        }
    }

    for (key, pipelines) in references {
        if seen.insert(key.clone()) {
            statuses.push(unavailable_provider(
                key.clone(),
                format!("provider `{}` is referenced but not registered", key.id),
                pipelines,
            ));
        }
    }

    statuses.sort_by(|left, right| left.id.cmp(&right.id).then(left.kind.cmp(&right.kind)));
    statuses
}

/// Reports every registered provider of every capability.
///
/// One walk over the bundle's descriptors rather than one function per
/// capability: enumerating stt, llm, tts and tool by hand is what left
/// transforms, detectors, identifiers and memory stores out of the snapshot
/// entirely, and would leave out the next capability too. What a provider is
/// called, what it is, and what version it runs all come off its descriptor,
/// so nothing here needs to know which capability it is looking at.
async fn collect_registered_statuses(
    providers: &conduit_runtime::Providers,
    references: &HashMap<ProviderKey, BTreeSet<String>>,
    proven: &HashMap<ProviderKey, TurnId>,
    failed: &HashMap<ProviderKey, TurnId>,
    definitions: &HashSet<ProviderKey>,
    statuses: &mut Vec<ProviderStatus>,
    seen: &mut HashSet<ProviderKey>,
) {
    for (capability, name, descriptor) in providers.descriptors() {
        let key = ProviderKey { kind: provider_kind_for(capability), id: name.clone() };
        if definitions.contains(&key) {
            continue;
        }
        // A tool discovered from a definition is that definition's, not a
        // provider beside it. Reporting each one separately listed a dozen
        // entries for one configured server — and health-checked every one of
        // them, which for MCP is a full session per tool per snapshot.
        if key.kind == ProviderKind::Tool && owning_definition(&name, definitions).is_some() {
            continue;
        }
        let Some(health) = providers.health(capability, &name).await else {
            continue;
        };
        push_registered_status(
            key,
            &descriptor,
            health,
            references,
            proven,
            failed,
            statuses,
            seen,
        );
    }
}

/// The definition a qualified tool id belongs to, if any.
///
/// Tools discovered from an MCP definition are registered as
/// `<definition id>.<tool name>`, and a definition id cannot contain a dot, so
/// the prefix before the first one names the definition.
fn owning_definition<'a>(name: &'a str, definitions: &HashSet<ProviderKey>) -> Option<&'a str> {
    let (prefix, _) = name.split_once('.')?;
    definitions
        .contains(&ProviderKey { kind: ProviderKind::Tool, id: prefix.to_owned() })
        .then_some(prefix)
}

/// The tools registered from `definition`, by their qualified ids.
///
/// Read from the runtime registry rather than by asking the server: discovery
/// already happened when the definition was registered, and asking again per
/// snapshot is what made a status poll cost one MCP session per tool.
fn discovered_tools(
    providers: Option<&conduit_runtime::Providers>,
    definition: &str,
) -> Vec<String> {
    let prefix = format!("{definition}.");
    providers
        .map(|providers| {
            providers
                .tools()
                .names()
                .filter(|name| name.starts_with(&prefix))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn push_definition_status(
    definition: &ProviderDefinition,
    descriptor: Option<&conduit_provider::Descriptor>,
    offers_tools: Vec<String>,
    health: Option<&Health>,
    references: &HashMap<ProviderKey, BTreeSet<String>>,
    proven: &HashMap<ProviderKey, TurnId>,
    failed: &HashMap<ProviderKey, TurnId>,
    statuses: &mut Vec<ProviderStatus>,
    seen: &mut HashSet<ProviderKey>,
) {
    let key = ProviderKey {
        kind: provider_kind_for_capability(definition.capability()),
        id: definition.id.clone(),
    };
    seen.insert(key.clone());
    let (reachable, state, message) = match health {
        Some(Health::Healthy) => (true, ProviderStatusState::Reachable, None),
        Some(Health::Degraded { reason }) => {
            (true, ProviderStatusState::Reachable, Some(reason.clone()))
        }
        Some(Health::Unhealthy { reason }) => {
            (false, ProviderStatusState::Configured, Some(reason.clone()))
        }
        None => (
            false,
            ProviderStatusState::Configured,
            Some("no successful reachability check yet".to_owned()),
        ),
    };
    let (state, proven_by_turn, message) = apply_proof(&key, state, message, proven, failed);
    statuses.push(ProviderStatus {
        id: key.id.clone(),
        kind: key.kind,
        provider: descriptor.map(|descriptor| descriptor.id.clone()),
        label: descriptor.map(|descriptor| descriptor.label.clone()),
        version: descriptor.map(|descriptor| descriptor.version.clone()),
        state,
        configured: true,
        reachable,
        proven_by_turn,
        message,
        affects_pipelines: references
            .get(&key)
            .map(|pipelines| pipelines.iter().cloned().collect())
            .unwrap_or_default(),
        offers_tools,
    });
}

#[allow(clippy::too_many_arguments)]
fn push_registered_status(
    key: ProviderKey,
    descriptor: &conduit_provider::Descriptor,
    health: Health,
    references: &HashMap<ProviderKey, BTreeSet<String>>,
    proven: &HashMap<ProviderKey, TurnId>,
    failed: &HashMap<ProviderKey, TurnId>,
    statuses: &mut Vec<ProviderStatus>,
    seen: &mut HashSet<ProviderKey>,
) {
    seen.insert(key.clone());
    let reachable = health.is_usable();
    let (state, message) = match health {
        Health::Healthy => (ProviderStatusState::Reachable, None),
        Health::Degraded { reason } => (ProviderStatusState::Reachable, Some(reason)),
        Health::Unhealthy { reason } => (ProviderStatusState::Configured, Some(reason)),
    };
    let (state, proven_by_turn, message) = apply_proof(&key, state, message, proven, failed);
    statuses.push(ProviderStatus {
        id: key.id.clone(),
        kind: key.kind,
        provider: Some(descriptor.id.clone()),
        label: Some(descriptor.label.clone()),
        version: Some(descriptor.version.clone()),
        state,
        configured: true,
        reachable,
        proven_by_turn,
        message,
        affects_pipelines: references
            .get(&key)
            .map(|pipelines| pipelines.iter().cloned().collect())
            .unwrap_or_default(),
        offers_tools: Vec::new(),
    });
}

/// Settles whether real turns have proven this provider, or unproven it.
///
/// A success proves a provider and a later failure takes that back, which is
/// the whole point of the distinction: `reachable` says a health check
/// answered, and `proven` says the provider did the job inside a real
/// pipeline. A provider that answered a health check while failing every turn
/// must not read as proven on the strength of the health check — the failure
/// is the more recent and more expensive evidence, and it stands until a later
/// success proves recovery.
fn apply_proof(
    key: &ProviderKey,
    state: ProviderStatusState,
    message: Option<String>,
    proven: &HashMap<ProviderKey, TurnId>,
    failed: &HashMap<ProviderKey, TurnId>,
) -> (ProviderStatusState, Option<TurnId>, Option<String>) {
    if let Some(turn) = failed.get(key) {
        let message = message.or_else(|| {
            Some(format!("turn {turn} failed at this provider and no later turn has proven it"))
        });
        return (state, None, message);
    }
    match proven.get(key) {
        Some(turn) => (ProviderStatusState::Proven, Some(*turn), message),
        None => (state, None, message),
    }
}

fn unavailable_provider(
    key: ProviderKey,
    message: impl Into<String>,
    pipelines: BTreeSet<String>,
) -> ProviderStatus {
    ProviderStatus {
        id: key.id,
        kind: key.kind,
        // A slot nothing fills, or a selector naming a provider that was never
        // built: there is no implementation to state an identity or a version.
        provider: None,
        label: None,
        version: None,
        state: ProviderStatusState::Unavailable,
        configured: false,
        reachable: false,
        proven_by_turn: None,
        message: Some(message.into()),
        affects_pipelines: pipelines.into_iter().collect(),
        offers_tools: Vec::new(),
    }
}

fn provider_references(
    graphs: &[(String, PipelineGraph)],
) -> HashMap<ProviderKey, BTreeSet<String>> {
    let mut references = HashMap::<ProviderKey, BTreeSet<String>>::new();
    for (pipeline, graph) in graphs {
        for node in graph.topological_order().unwrap_or_default() {
            let Some(kind) = provider_kind_for_node(node.kind()) else {
                continue;
            };
            references
                .entry(ProviderKey { kind, id: node.provider().to_owned() })
                .or_default()
                .insert(pipeline.clone());
        }
    }
    references
}

/// Providers a real turn has proven, and the turn that proved them.
///
/// Read per component rather than off the pipeline's `last_successful_turn`,
/// which is what a provider's proof is *about*: a turn that failed at synthesis
/// says nothing about the model that answered in it, and clearing the whole
/// pipeline's success marker used to un-prove every provider in the graph on
/// any one provider's failure. A component that last completed successfully
/// keeps its proof; the component that failed loses it, through
/// [`failed_providers`].
fn proven_providers(
    graphs: &[(String, PipelineGraph)],
    projection: &Projection,
) -> HashMap<ProviderKey, TurnId> {
    let mut proven = HashMap::new();
    for (pipeline, graph) in graphs {
        let Some(runtime) = projection.pipelines.get(pipeline) else {
            continue;
        };
        for node in graph.topological_order().unwrap_or_default() {
            let Some(component) = component_for_node_kind(node.kind()) else {
                continue;
            };
            let Some(kind) = provider_kind_for_node(node.kind()) else {
                continue;
            };
            let Some(recorded) = runtime.components.get(&component) else {
                continue;
            };
            if recorded.state != ComponentHealthState::Healthy {
                continue;
            }
            let Some(turn) = recorded.last_turn else {
                continue;
            };
            proven.insert(ProviderKey { kind, id: node.provider().to_owned() }, turn);
        }
    }
    proven
}

/// Providers a turn failed at, and the turn that failed.
///
/// The other half of proof: a successful turn proves a provider, and a failure
/// that no later success has cleared takes that back. Read from the same
/// per-component runtime `proven_providers` reads, so the two answers cannot
/// disagree — a component is in `unresolved_failures` exactly while its last
/// word was a failure, and the successful turn that clears it is what removes
/// it from that set.
///
/// This is why an unreachable or unproven provider can name the pipelines it
/// affects: the failure is discovered through a pipeline's components, so the
/// pipeline is known at the moment the provider is marked.
fn failed_providers(
    graphs: &[(String, PipelineGraph)],
    projection: &Projection,
) -> HashMap<ProviderKey, TurnId> {
    let mut failed = HashMap::new();
    for (pipeline, graph) in graphs {
        let Some(runtime) = projection.pipelines.get(pipeline) else {
            continue;
        };
        for node in graph.topological_order().unwrap_or_default() {
            let Some(component) = component_for_node_kind(node.kind()) else {
                continue;
            };
            if !runtime.unresolved_failures.contains(&component) {
                continue;
            }
            let Some(kind) = provider_kind_for_node(node.kind()) else {
                continue;
            };
            // The turn the component last failed in, falling back to the
            // pipeline's last failed turn: either way the operator is pointed
            // at a turn they can look up.
            let Some(turn) = runtime
                .components
                .get(&component)
                .and_then(|recorded| recorded.last_turn)
                .or(runtime.last_failed_turn)
            else {
                continue;
            };
            failed.insert(ProviderKey { kind, id: node.provider().to_owned() }, turn);
        }
    }
    failed
}

fn pipeline_provider_ids(graph: &PipelineGraph) -> Vec<String> {
    graph
        .topological_order()
        .unwrap_or_default()
        .into_iter()
        .filter(|node| provider_kind_for_node(node.kind()).is_some())
        .map(|node| node.provider().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn provider_kind_for_node(kind: NodeKind) -> Option<ProviderKind> {
    match kind {
        NodeKind::Stt => Some(ProviderKind::Stt),
        // A core's provider is the model it binds; its tools and memory are
        // bindings rather than nodes, and are reported from the core plan.
        NodeKind::Core => Some(ProviderKind::Llm),
        NodeKind::Tts => Some(ProviderKind::Tts),
        NodeKind::Transform => Some(ProviderKind::Transform),
        NodeKind::WakeWord => Some(ProviderKind::Wake),
        NodeKind::SpeakerId => Some(ProviderKind::SpeakerId),
        _ => None,
    }
}

/// The status vocabulary's name for a runtime capability.
///
/// [`ProviderCapability`] describes what an operator *configured* and
/// [`Capability`](conduit_provider::Capability) describes what the runtime
/// *registered*; both are reported through one [`ProviderKind`], so a provider
/// reads the same whether it came from a stored definition or from the bundle.
/// Memory has no stored definition variant today, which is why only this
/// direction mentions it.
const fn provider_kind_for(capability: conduit_provider::Capability) -> ProviderKind {
    match capability {
        conduit_provider::Capability::Stt => ProviderKind::Stt,
        conduit_provider::Capability::Llm => ProviderKind::Llm,
        conduit_provider::Capability::Tts => ProviderKind::Tts,
        conduit_provider::Capability::Transform => ProviderKind::Transform,
        conduit_provider::Capability::Tool => ProviderKind::Tool,
        conduit_provider::Capability::Memory => ProviderKind::Memory,
        conduit_provider::Capability::Wake => ProviderKind::Wake,
        conduit_provider::Capability::SpeakerId => ProviderKind::SpeakerId,
    }
}

fn provider_kind_for_capability(capability: ProviderCapability) -> ProviderKind {
    match capability {
        ProviderCapability::Stt => ProviderKind::Stt,
        ProviderCapability::Llm => ProviderKind::Llm,
        ProviderCapability::Tool => ProviderKind::Tool,
        ProviderCapability::Tts => ProviderKind::Tts,
        ProviderCapability::Transform => ProviderKind::Transform,
        ProviderCapability::Wake => ProviderKind::Wake,
        ProviderCapability::SpeakerId => ProviderKind::SpeakerId,
    }
}

fn provider_kind_missing(kind: ProviderKind, seen: &HashSet<ProviderKey>) -> bool {
    !seen.iter().any(|key| key.kind == kind)
}

fn unavailable_slot_id(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Stt => "stt",
        ProviderKind::Llm => "llm",
        ProviderKind::Tool => "tool",
        ProviderKind::Tts => "tts",
        ProviderKind::Transform => "transform",
        ProviderKind::Wake => "wake",
        ProviderKind::SpeakerId => "speaker_id",
        ProviderKind::Memory => "memory",
    }
}

fn unavailable_message(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Stt => "no speech-to-text provider is registered",
        ProviderKind::Llm => "no language-model provider is registered",
        ProviderKind::Tool => "no tool provider is registered",
        ProviderKind::Tts => "no text-to-speech provider is registered",
        ProviderKind::Transform => "no utterance transform provider is registered",
        ProviderKind::Wake => "no wake word provider is registered",
        ProviderKind::SpeakerId => "no speaker identification provider is registered",
        ProviderKind::Memory => "no memory provider is registered",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProviderKey {
    kind: ProviderKind,
    id: String,
}

fn project_components(
    graph: &PipelineGraph,
    runtime: Option<&PipelineRuntime>,
    usable: bool,
) -> Vec<ComponentHealth> {
    graph
        .topological_order()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|node| {
            let kind = component_for_node_kind(node.kind())?;
            let recorded = runtime.and_then(|runtime| runtime.components.get(&kind));
            let state = recorded.map_or(
                if usable {
                    ComponentHealthState::Unproven
                } else {
                    ComponentHealthState::NotConfigured
                },
                |component| component.state,
            );
            Some(ComponentHealth {
                kind,
                provider: Some(node.provider().to_owned()),
                state,
                detail: recorded.and_then(|component| component.detail.clone()),
                last_turn: recorded.and_then(|component| component.last_turn),
            })
        })
        .collect()
}

fn component_for_node_kind(kind: NodeKind) -> Option<ComponentKind> {
    match kind {
        NodeKind::Stt => Some(ComponentKind::Transcription),
        NodeKind::Core => Some(ComponentKind::Reasoning),
        NodeKind::Tts => Some(ComponentKind::Synthesis),
        _ => None,
    }
}

fn default_event_stream_contract() -> EventStreamContract {
    EventStreamContract {
        route: "/v1/events".to_owned(),
        stale_state_on_disconnect: StaleState::Stale,
        refresh_snapshot_after_reconnect: true,
        bindings: vec![
            SnapshotEventBinding {
                resource: SnapshotResource::PipelineHealth,
                events: vec![
                    "TurnStarted".to_owned(),
                    "StageFailed".to_owned(),
                    "ConversationCompleted".to_owned(),
                    "ConversationCancelled".to_owned(),
                ],
            },
            SnapshotEventBinding {
                resource: SnapshotResource::ActiveTurns,
                events: vec![
                    "TurnStarted".to_owned(),
                    "ConversationCompleted".to_owned(),
                    "ConversationCancelled".to_owned(),
                ],
            },
            SnapshotEventBinding {
                resource: SnapshotResource::RecentFailures,
                events: vec!["StageFailed".to_owned(), "ConversationCompleted".to_owned()],
            },
            SnapshotEventBinding {
                resource: SnapshotResource::ProviderStatus,
                events: vec![
                    "SpeechFinal".to_owned(),
                    "LlmFinished".to_owned(),
                    "ToolCompleted".to_owned(),
                    "TtsFinished".to_owned(),
                    "ConversationCompleted".to_owned(),
                ],
            },
            SnapshotEventBinding {
                resource: SnapshotResource::SatelliteStatus,
                events: vec![
                    "ConversationStarted".to_owned(),
                    "AudioStarted".to_owned(),
                    "ConversationCompleted".to_owned(),
                    "ConversationCancelled".to_owned(),
                ],
            },
        ],
    }
}

#[derive(Debug, Clone, Default)]
struct Projection {
    pipelines: HashMap<String, PipelineRuntime>,
    active_turns: HashMap<TurnId, ActiveTurnRecord>,
    turns_by_conversation: HashMap<ConversationId, TurnId>,
    connected_satellites: HashMap<DeviceId, ConnectedSatellite>,
    recent_satellites: HashMap<DeviceId, RecentlyActiveSatellite>,
}

impl Projection {
    fn connect_satellite(
        &mut self,
        device: DeviceId,
        name: String,
        pipeline: String,
        conversation: ConversationId,
    ) {
        self.connected_satellites.insert(
            device,
            ConnectedSatellite {
                device,
                name: name.clone(),
                connected_since: Utc::now(),
                conversation: Some(conversation),
                pipeline,
            },
        );
        self.recent_satellites.insert(
            device,
            RecentlyActiveSatellite {
                device,
                name,
                last_seen_at: Utc::now(),
                last_event: "ConversationStarted".to_owned(),
            },
        );
    }

    fn disconnect_satellite(&mut self, device: DeviceId) {
        self.connected_satellites.remove(&device);
    }

    fn record(&mut self, envelope: &Envelope) {
        self.record_satellite_activity(envelope);

        let Some(pipeline) = envelope.pipeline.as_ref() else {
            return;
        };
        match &envelope.event {
            Event::TurnStarted { turn } => {
                let Some(conversation) = envelope.conversation else {
                    return;
                };
                let record = ActiveTurnRecord {
                    pipeline: pipeline.clone(),
                    conversation,
                    turn: *turn,
                    trace: envelope.trace,
                    started_at: envelope.at,
                    invoked_components: BTreeSet::new(),
                    failed_components: BTreeSet::new(),
                    failure: None,
                };
                self.active_turns.insert(*turn, record);
                self.turns_by_conversation.insert(conversation, *turn);
                self.pipelines.entry(pipeline.clone()).or_default();
            }
            Event::SpeechFinal { .. } => {
                self.mark_component(envelope, ComponentKind::Transcription, None);
            }
            Event::LlmRequestStarted { .. } => {
                self.mark_invoked(envelope, ComponentKind::Reasoning);
            }
            Event::LlmFinished { .. } => {
                self.mark_component(envelope, ComponentKind::Reasoning, None);
            }
            Event::ToolRequested { .. } | Event::ToolStarted { .. } => {
                self.mark_invoked(envelope, ComponentKind::Tools);
            }
            Event::ToolCompleted { .. } => {
                self.mark_component(envelope, ComponentKind::Tools, None);
            }
            Event::ToolFailed { error, .. } => {
                self.mark_component(envelope, ComponentKind::Tools, Some(error.clone()));
            }
            Event::TtsStarted { .. } => {
                self.mark_invoked(envelope, ComponentKind::Synthesis);
            }
            Event::TtsFinished { .. } => {
                self.mark_component(envelope, ComponentKind::Synthesis, None);
            }
            Event::StageFailed { node, error, recovered } => {
                if !recovered {
                    let component = component_for_node_name(node);
                    self.mark_component(envelope, component, Some(error.clone()));
                }
            }
            Event::ConversationCancelled { reason } => {
                self.finish_cancelled(envelope, *reason);
            }
            Event::ConversationCompleted => {
                self.finish_completed(envelope);
            }
            _ => {}
        }
    }

    fn record_satellite_activity(&mut self, envelope: &Envelope) {
        let Some(device) = envelope.device else {
            return;
        };
        let name = self
            .connected_satellites
            .get(&device)
            .map_or_else(|| device.to_string(), |satellite| satellite.name.clone());
        self.recent_satellites.insert(
            device,
            RecentlyActiveSatellite {
                device,
                name,
                last_seen_at: envelope.at,
                last_event: event_variant_name(&envelope.event),
            },
        );
    }

    fn active_turn_mut(&mut self, envelope: &Envelope) -> Option<&mut ActiveTurnRecord> {
        let conversation = envelope.conversation?;
        let turn = self.turns_by_conversation.get(&conversation)?;
        self.active_turns.get_mut(turn)
    }

    fn mark_invoked(&mut self, envelope: &Envelope, component: ComponentKind) {
        if let Some(turn) = self.active_turn_mut(envelope) {
            turn.invoked_components.insert(component);
        }
    }

    fn mark_component(
        &mut self,
        envelope: &Envelope,
        component: ComponentKind,
        error: Option<String>,
    ) {
        let Some(pipeline) = envelope.pipeline.clone() else {
            return;
        };
        if let Some(error) = error {
            let turn_id = envelope.conversation.and_then(|conversation| {
                self.turns_by_conversation.get(&conversation).copied()
            });
            {
                let runtime = self.pipelines.entry(pipeline.clone()).or_default();
                let entry = runtime.components.entry(component).or_default();
                entry.last_turn = turn_id;
                entry.state = ComponentHealthState::Unhealthy;
                entry.detail = Some(error.clone());
                runtime.unresolved_failures.insert(component);
            }
            if let Some(turn) = turn_id.and_then(|turn| self.active_turns.get_mut(&turn)) {
                turn.invoked_components.insert(component);
                turn.failed_components.insert(component);
                turn.failure = Some(FailureRecord {
                    pipeline,
                    turn: Some(turn.turn),
                    component,
                    provider: None,
                    message: error,
                    at: envelope.at,
                });
            }
        } else {
            let turn_id = envelope.conversation.and_then(|conversation| {
                self.turns_by_conversation.get(&conversation).copied()
            });
            let unresolved = self
                .pipelines
                .get(&pipeline)
                .is_some_and(|runtime| runtime.unresolved_failures.contains(&component));
            {
                let runtime = self.pipelines.entry(pipeline).or_default();
                let entry = runtime.components.entry(component).or_default();
                entry.last_turn = turn_id;
                if !unresolved {
                    entry.state = ComponentHealthState::Healthy;
                    entry.detail = Some("last invoked turn completed".to_owned());
                }
            }
            if let Some(turn) = turn_id.and_then(|turn| self.active_turns.get_mut(&turn)) {
                turn.invoked_components.insert(component);
            }
        }
    }

    fn finish_cancelled(&mut self, envelope: &Envelope, reason: CancelReason) {
        let Some(turn) = self.take_active_turn(envelope) else {
            return;
        };
        if let Some(failure) = turn.failure {
            let runtime = self.pipelines.entry(turn.pipeline.clone()).or_default();
            runtime.last_failed_turn = failure.turn;
            runtime.last_successful_turn = None;
            runtime.recent_failures.insert(0, failure.into_runtime_failure());
        } else if reason == CancelReason::Error {
            let runtime = self.pipelines.entry(turn.pipeline.clone()).or_default();
            runtime.last_failed_turn = Some(turn.turn);
            runtime.last_successful_turn = None;
        }
    }

    fn finish_completed(&mut self, envelope: &Envelope) {
        let Some(turn) = self.take_active_turn(envelope) else {
            return;
        };
        let runtime = self.pipelines.entry(turn.pipeline.clone()).or_default();
        if turn.failed_components.is_empty() {
            for component in &turn.invoked_components {
                runtime.unresolved_failures.remove(component);
                let entry = runtime.components.entry(*component).or_default();
                entry.state = ComponentHealthState::Healthy;
                entry.detail = Some("last invoked turn completed".to_owned());
                entry.last_turn = Some(turn.turn);
            }
            if runtime.unresolved_failures.is_empty() {
                runtime.last_successful_turn = Some(turn.turn);
                runtime.last_failed_turn = None;
                runtime.recent_failures.clear();
            }
        } else {
            runtime.last_failed_turn = Some(turn.turn);
            runtime.last_successful_turn = None;
        }
    }

    fn take_active_turn(&mut self, envelope: &Envelope) -> Option<ActiveTurnRecord> {
        let conversation = envelope.conversation?;
        let turn = self.turns_by_conversation.remove(&conversation)?;
        self.active_turns.remove(&turn)
    }
}

fn component_for_node_name(node: &str) -> ComponentKind {
    let lower = node.to_ascii_lowercase();
    if lower.contains("stt") || lower.contains("transcri") {
        ComponentKind::Transcription
    } else if lower.contains("tool") {
        ComponentKind::Tools
    } else if lower.contains("tts") || lower.contains("synth") {
        ComponentKind::Synthesis
    } else {
        ComponentKind::Reasoning
    }
}

fn event_variant_name(event: &Event) -> String {
    serde_json::to_value(event)
        .ok()
        .and_then(|value| value.get("type").and_then(|value| value.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "Unknown".to_owned())
}

#[derive(Debug, Clone, Default)]
struct PipelineRuntime {
    last_successful_turn: Option<TurnId>,
    last_failed_turn: Option<TurnId>,
    unresolved_failures: BTreeSet<ComponentKind>,
    components: HashMap<ComponentKind, ComponentRuntime>,
    recent_failures: Vec<RuntimeFailure>,
}

#[derive(Debug, Clone)]
struct ComponentRuntime {
    state: ComponentHealthState,
    detail: Option<String>,
    last_turn: Option<TurnId>,
}

impl Default for ComponentRuntime {
    fn default() -> Self {
        Self { state: ComponentHealthState::Unproven, detail: None, last_turn: None }
    }
}

#[derive(Debug, Clone)]
struct ActiveTurnRecord {
    pipeline: String,
    conversation: ConversationId,
    turn: TurnId,
    trace: TraceId,
    started_at: DateTime<Utc>,
    invoked_components: BTreeSet<ComponentKind>,
    failed_components: BTreeSet<ComponentKind>,
    failure: Option<FailureRecord>,
}

#[derive(Debug, Clone)]
struct FailureRecord {
    pipeline: String,
    turn: Option<TurnId>,
    component: ComponentKind,
    provider: Option<String>,
    message: String,
    at: DateTime<Utc>,
}

impl FailureRecord {
    fn into_runtime_failure(self) -> RuntimeFailure {
        RuntimeFailure {
            pipeline: self.pipeline,
            turn: self.turn,
            component: self.component,
            provider: self.provider,
            message: self.message,
            at: self.at,
        }
    }
}
