//! Server-owned turn reconstruction read model and routes.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::Json;
use chrono::{DateTime, Utc};
use conduit_core::bus::EventBus;
use conduit_core::event::{CancelReason, Envelope, Event, UtteranceSegmentRole};
use conduit_core::graph::Modality;
use conduit_core::id::{ConversationId, EventId, ToolCallId, TurnId};
use futures_util::stream::{Stream, StreamExt};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::auth::ManagementCaller;
use crate::{ApiError, AppState};

/// Count and age bounds for the in-memory turn-history read model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnHistoryRetention {
    /// Maximum terminal turns retained. `None` removes the count bound.
    pub max_turns: Option<usize>,
    /// Maximum terminal-turn age retained. `None` removes the age bound.
    pub max_age: Option<Duration>,
}

impl Default for TurnHistoryRetention {
    fn default() -> Self {
        Self { max_turns: Some(500), max_age: Some(Duration::from_secs(86_400)) }
    }
}

/// A live reconstruction update.
#[derive(Debug, Clone, Serialize)]
pub struct TurnReconstructionUpdate {
    /// Turn this update changes.
    pub turn_id: TurnId,
    /// Conversation that owns the turn.
    pub conversation_id: ConversationId,
    /// Pipeline that handled the turn.
    pub pipeline_name: String,
    /// Monotonic reconstruction sequence for this turn.
    pub sequence: u64,
    /// Kind of update being sent.
    pub update: TurnUpdateKind,
}

/// What changed in a reconstructed turn.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnUpdateKind {
    /// The reconstructed snapshot changed and can be refetched if needed.
    SnapshotChanged,
}

/// In-memory projection of raw events into reconstructed turns.
#[derive(Debug)]
pub struct TurnHistory {
    inner: RwLock<Inner>,
    updates: broadcast::Sender<TurnReconstructionUpdate>,
}

#[derive(Debug)]
struct Inner {
    retention: TurnHistoryRetention,
    turns: HashMap<TurnId, TurnSnapshot>,
    conversations: HashMap<ConversationId, TurnId>,
    pending: HashMap<ConversationId, Vec<Envelope>>,
}

impl TurnHistory {
    /// Creates a history projection and starts feeding it from `bus`.
    #[must_use]
    pub fn spawn(bus: &EventBus, retention: TurnHistoryRetention) -> Arc<Self> {
        let history = Arc::new(Self {
            inner: RwLock::new(Inner {
                retention,
                turns: HashMap::new(),
                conversations: HashMap::new(),
                pending: HashMap::new(),
            }),
            updates: broadcast::channel(1024).0,
        });
        let mut subscription = bus.subscribe();
        let task_history = Arc::clone(&history);
        tokio::spawn(async move {
            while let Some(envelope) = subscription.recv().await {
                task_history.observe(envelope.as_ref().clone()).await;
            }
        });
        history
    }

    /// Replaces retention settings for future pruning.
    pub fn set_retention(&self, retention: TurnHistoryRetention) {
        let mut inner = self.inner.write().expect("turn history lock poisoned");
        inner.retention = retention;
        inner.prune(Utc::now());
    }

    /// Returns recent turns, newest first.
    pub async fn list(&self) -> Vec<TurnSummary> {
        let inner = self.inner.read().expect("turn history lock poisoned");
        let mut turns = inner.turns.values().map(TurnSnapshot::summary).collect::<Vec<_>>();
        turns.sort_by_key(|turn| std::cmp::Reverse(turn.started_at));
        turns
    }

    /// Fetches one reconstructed turn.
    pub async fn get(&self, turn: TurnId) -> Option<TurnSnapshot> {
        self.inner.read().expect("turn history lock poisoned").turns.get(&turn).cloned()
    }

    /// Fetches the raw evidence retained for one turn.
    pub async fn evidence(&self, turn: TurnId) -> Option<RawTurnEvents> {
        let snapshot =
            self.inner.read().expect("turn history lock poisoned").turns.get(&turn)?.clone();
        Some(RawTurnEvents { turn_id: turn, events: snapshot.raw_events })
    }

    /// Subscribes to live reconstruction updates.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<TurnReconstructionUpdate> {
        self.updates.subscribe()
    }

    async fn observe(&self, envelope: Envelope) {
        let mut inner = self.inner.write().expect("turn history lock poisoned");
        let update = inner.observe(envelope);
        inner.prune(Utc::now());
        drop(inner);
        if let Some(update) = update {
            let _ = self.updates.send(update);
        }
    }
}

impl Inner {
    fn observe(&mut self, envelope: Envelope) -> Option<TurnReconstructionUpdate> {
        let conversation = envelope.conversation?;

        if let Event::TurnStarted { turn } = envelope.event {
            let snapshot = TurnSnapshot::started(turn, conversation, &envelope);
            self.conversations.insert(conversation, turn);
            self.turns.insert(turn, snapshot);
            if let Some(pending) = self.pending.remove(&conversation) {
                if let Some(snapshot) = self.turns.get_mut(&turn) {
                    for event in pending {
                        snapshot.raw_events.push(event);
                    }
                    snapshot.raw_events.push(envelope);
                    return Some(snapshot.update());
                }
            }
            return self.turns.get_mut(&turn).map(|snapshot| {
                snapshot.raw_events.push(envelope);
                snapshot.update()
            });
        }

        let Some(turn) = self.conversations.get(&conversation).copied() else {
            self.pending.entry(conversation).or_default().push(envelope);
            return None;
        };
        let snapshot = self.turns.get_mut(&turn)?;
        snapshot.observe(envelope);
        Some(snapshot.update())
    }

    fn prune(&mut self, now: DateTime<Utc>) {
        if let Some(max_age) = self.retention.max_age {
            if let Ok(max_age) = chrono::Duration::from_std(max_age) {
                self.turns.retain(|_, turn| {
                    !turn.status.is_terminal()
                        || now.signed_duration_since(turn.ended_at.unwrap_or(turn.started_at))
                            <= max_age
                });
            }
        }

        if let Some(max_turns) = self.retention.max_turns {
            let mut terminal = self
                .turns
                .iter()
                .filter(|(_, turn)| turn.status.is_terminal())
                .map(|(id, turn)| (*id, turn.ended_at.unwrap_or(turn.started_at)))
                .collect::<Vec<_>>();
            terminal.sort_by_key(|(_, ended_at)| std::cmp::Reverse(*ended_at));
            for (id, _) in terminal.into_iter().skip(max_turns) {
                self.turns.remove(&id);
            }
        }

        self.conversations.retain(|_, turn| self.turns.contains_key(turn));
    }
}

/// Recent turn list response.
#[derive(Debug, Clone, Serialize)]
pub struct TurnList {
    /// Recent reconstructed turns, newest first.
    pub turns: Vec<TurnSummary>,
}

/// Summary returned by `GET /v1/turns`.
#[derive(Debug, Clone, Serialize)]
pub struct TurnSummary {
    /// Stable turn id.
    pub turn_id: TurnId,
    /// Conversation that owns the turn.
    pub conversation_id: ConversationId,
    /// Pipeline that handled the turn.
    pub pipeline_name: String,
    /// Coarse current or terminal outcome.
    pub status: TurnStatus,
    /// When the turn started.
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// When the turn reached a terminal state.
    pub ended_at: Option<DateTime<Utc>>,
    /// Latest reconstruction sequence emitted for this turn.
    pub sequence: u64,
}

/// Full turn reconstruction snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct TurnSnapshot {
    /// Stable turn id.
    pub turn_id: TurnId,
    /// Conversation that owns the turn.
    pub conversation_id: ConversationId,
    /// Pipeline that handled the turn.
    pub pipeline_name: String,
    /// Coarse current or terminal outcome.
    pub status: TurnStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Reason for cancellation, when the turn was cancelled.
    pub cancellation_reason: Option<CancelReason>,
    /// When the turn started.
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// When the turn reached a terminal state.
    pub ended_at: Option<DateTime<Utc>>,
    /// Latest reconstruction sequence emitted for this turn.
    pub sequence: u64,
    /// Ordered reconstruction items.
    pub items: Vec<ReconstructionItem>,
    #[serde(skip)]
    raw_events: Vec<Envelope>,
}

impl TurnSnapshot {
    fn started(turn: TurnId, conversation: ConversationId, envelope: &Envelope) -> Self {
        Self {
            turn_id: turn,
            conversation_id: conversation,
            pipeline_name: envelope.pipeline.clone().unwrap_or_else(|| "unknown".to_owned()),
            status: TurnStatus::Running,
            cancellation_reason: None,
            started_at: envelope.at,
            ended_at: None,
            sequence: 0,
            items: Vec::new(),
            raw_events: Vec::new(),
        }
    }

    fn summary(&self) -> TurnSummary {
        TurnSummary {
            turn_id: self.turn_id,
            conversation_id: self.conversation_id,
            pipeline_name: self.pipeline_name.clone(),
            status: self.status,
            started_at: self.started_at,
            ended_at: self.ended_at,
            sequence: self.sequence,
        }
    }

    fn observe(&mut self, envelope: Envelope) {
        match &envelope.event {
            Event::UtteranceSegmentStarted { segment, role, modality, text } => {
                self.items.push(ReconstructionItem::UtteranceSegment(UtteranceSegment {
                    id: segment.clone(),
                    sequence: self.sequence + 1,
                    role: *role,
                    modality: *modality,
                    text: text.clone(),
                    started_at: envelope.at,
                    evidence: vec![envelope.id],
                }));
            }
            Event::ToolBatchStarted { batch, calls, model_round } => {
                self.items.push(ReconstructionItem::ToolBatch(ToolBatch {
                    id: batch.clone(),
                    sequence: self.sequence + 1,
                    model_round: *model_round,
                    calls: calls.iter().cloned().map(ToolCall::requested).collect(),
                    started_at: envelope.at,
                    completed_at: None,
                    evidence: vec![envelope.id],
                }));
            }
            Event::ToolRequested { call, name } => {
                let tool = self.tool_call_mut(call);
                tool.name = Some(name.clone());
                tool.status = ToolCallStatus::Requested;
                tool.evidence.push(envelope.id);
            }
            Event::ToolStarted { call } => {
                let tool = self.tool_call_mut(call);
                tool.status = ToolCallStatus::Running;
                tool.evidence.push(envelope.id);
            }
            Event::ToolConfirmationRequested { call, prompt: _ } => {
                let tool = self.tool_call_mut(call);
                tool.status = ToolCallStatus::AwaitingConfirmation;
                tool.evidence.push(envelope.id);
            }
            Event::ToolCompleted { call, duration_ms } => {
                let tool = self.tool_call_mut(call);
                tool.status = ToolCallStatus::Completed;
                tool.duration_ms = Some(*duration_ms);
                tool.evidence.push(envelope.id);
            }
            Event::ToolFailed { call, error } => {
                let tool = self.tool_call_mut(call);
                tool.status = ToolCallStatus::Failed;
                tool.error = Some(error.clone());
                tool.evidence.push(envelope.id);
            }
            Event::StageFailed { recovered, .. } => {
                if *recovered {
                    self.status = TurnStatus::Degraded;
                } else {
                    self.status = TurnStatus::Failed;
                }
            }
            Event::ConversationCompleted => {
                self.ended_at = Some(envelope.at);
                if self.status == TurnStatus::Running {
                    self.status = TurnStatus::Completed;
                }
            }
            Event::ConversationCancelled { reason } => {
                self.status = TurnStatus::Cancelled;
                self.cancellation_reason = Some(*reason);
                self.ended_at = Some(envelope.at);
            }
            _ => {}
        }

        self.raw_events.push(envelope);
    }

    fn tool_call_mut(&mut self, call: &ToolCallId) -> &mut ToolCall {
        let batch_index = self.items.iter().rposition(ReconstructionItem::is_tool_batch);
        let batch_index = match batch_index {
            Some(index) => index,
            None => {
                self.items.push(ReconstructionItem::ToolBatch(ToolBatch {
                    id: format!("{}-tool-batch-implicit", self.turn_id),
                    sequence: self.sequence + 1,
                    model_round: 0,
                    calls: Vec::new(),
                    started_at: Utc::now(),
                    completed_at: None,
                    evidence: Vec::new(),
                }));
                self.items.len() - 1
            }
        };

        match &mut self.items[batch_index] {
            ReconstructionItem::ToolBatch(batch) => {
                let call_index = batch.calls.iter().position(|candidate| &candidate.id == call);
                let call_index = match call_index {
                    Some(index) => index,
                    None => {
                        batch.calls.push(ToolCall::requested(call.clone()));
                        batch.calls.len() - 1
                    }
                };
                &mut batch.calls[call_index]
            }
            ReconstructionItem::UtteranceSegment(_) => unreachable!("selected a tool batch"),
        }
    }

    fn update(&mut self) -> TurnReconstructionUpdate {
        self.sequence = self.sequence.saturating_add(1);
        TurnReconstructionUpdate {
            turn_id: self.turn_id,
            conversation_id: self.conversation_id,
            pipeline_name: self.pipeline_name.clone(),
            sequence: self.sequence,
            update: TurnUpdateKind::SnapshotChanged,
        }
    }
}

/// Coarse status for a reconstructed turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    /// The turn is still in progress.
    Running,
    /// The turn completed normally.
    Completed,
    /// The turn ended early with a cancellation reason.
    Cancelled,
    /// The turn hit an unrecovered failure.
    Failed,
    /// The turn completed with recovered failures.
    Degraded,
}

impl TurnStatus {
    const fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// One stable item in the reconstructed turn story.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReconstructionItem {
    /// Text that was intentionally sent to speech synthesis.
    UtteranceSegment(UtteranceSegment),
    /// Tool calls requested by one model round.
    ToolBatch(ToolBatch),
}

impl ReconstructionItem {
    const fn is_tool_batch(&self) -> bool {
        matches!(self, Self::ToolBatch(_))
    }
}

/// Text a turn intentionally emitted as one piece.
#[derive(Debug, Clone, Serialize)]
pub struct UtteranceSegment {
    /// Stable reconstruction item id.
    pub id: String,
    /// Canonical sequence within the turn.
    pub sequence: u64,
    /// Why this text was emitted.
    pub role: UtteranceSegmentRole,
    /// How it was rendered. A text pipeline's segments were never spoken, and
    /// a reader showing a playback control for one would be describing audio
    /// that does not exist.
    pub modality: Modality,
    /// The text of the span.
    pub text: String,
    /// When the segment started.
    pub started_at: DateTime<Utc>,
    /// Raw event ids supporting this item.
    pub evidence: Vec<EventId>,
}

/// A set of tool calls requested by one model round.
#[derive(Debug, Clone, Serialize)]
pub struct ToolBatch {
    /// Stable reconstruction item id.
    pub id: String,
    /// Canonical sequence within the turn.
    pub sequence: u64,
    /// One-based model round that requested the calls.
    pub model_round: u32,
    /// Tool calls in this batch.
    pub calls: Vec<ToolCall>,
    /// When the batch started.
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// When every call in the batch reached a terminal status.
    pub completed_at: Option<DateTime<Utc>>,
    /// Raw event ids supporting this item.
    pub evidence: Vec<EventId>,
}

/// One tool call inside a batch.
#[derive(Debug, Clone, Serialize)]
pub struct ToolCall {
    /// Provider-issued tool call id.
    pub id: ToolCallId,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Tool name requested by the model.
    pub name: Option<String>,
    /// Current or terminal lifecycle status.
    pub status: ToolCallStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Wall-clock execution duration when completed.
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Failure or denial detail when applicable.
    pub error: Option<String>,
    /// Raw event ids supporting this call.
    pub evidence: Vec<EventId>,
}

impl ToolCall {
    fn requested(id: ToolCallId) -> Self {
        Self {
            id,
            name: None,
            status: ToolCallStatus::Requested,
            duration_ms: None,
            error: None,
            evidence: Vec::new(),
        }
    }
}

/// Lifecycle status for a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    /// The model requested the call.
    Requested,
    /// The runtime started invoking the call.
    Running,
    /// The call completed successfully.
    Completed,
    /// The call failed.
    Failed,
    /// The call was denied by policy.
    Denied,
    /// The call required confirmation the runtime could not collect.
    AwaitingConfirmation,
}

/// Raw event evidence response.
#[derive(Debug, Clone, Serialize)]
pub struct RawTurnEvents {
    /// Turn whose evidence is returned.
    pub turn_id: TurnId,
    /// Raw event envelopes retained for the turn.
    pub events: Vec<Envelope>,
}

/// `GET /v1/turns` — recent reconstructed turns.
pub async fn list(_caller: ManagementCaller, State(state): State<AppState>) -> Json<TurnList> {
    Json(TurnList { turns: state.turns().list().await })
}

/// `GET /v1/turns/{turn_id}` — one reconstructed turn snapshot.
pub async fn get(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(turn): Path<TurnId>,
) -> Result<Json<TurnSnapshot>, ApiError> {
    state
        .turns()
        .get(turn)
        .await
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("turn `{turn}` was not found")))
}

/// `GET /v1/turns/{turn_id}/events` — raw event evidence for a turn.
pub async fn events(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(turn): Path<TurnId>,
) -> Result<Json<RawTurnEvents>, ApiError> {
    state
        .turns()
        .evidence(turn)
        .await
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("turn `{turn}` was not found")))
}

/// `GET /v1/turns/live` — live reconstruction updates.
pub async fn live(
    _caller: ManagementCaller,
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<SseEvent, axum::Error>>> {
    let updates = state.turns().subscribe();
    let stream =
        tokio_stream::wrappers::BroadcastStream::new(updates).filter_map(|update| async move {
            let update = update.ok()?;
            Some(Ok(SseEvent::default()
                .id(format!("{}:{}", update.turn_id, update.sequence))
                .event("turn_reconstruction")
                .json_data(update)
                .expect("turn update serializes")))
        });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
