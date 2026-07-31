//! Live event stream endpoint.

use axum::extract::{Query, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use conduit_core::bus::Filter;
use conduit_core::event::Stage;
use conduit_core::id::{ConversationId, DeviceId, TraceId};
use futures_util::stream::Stream;
use serde::Deserialize;

use crate::auth::ManagementCaller;
use crate::{ApiError, AppState};

/// Narrows which events a subscriber receives.
///
/// Every field is optional; omitting all of them streams everything.
#[derive(Debug, Default, Deserialize)]
pub struct StreamQuery {
    /// Comma-separated stage names, e.g. `stages=reasoning,tools`.
    #[serde(default)]
    pub stages: Option<String>,
    /// Restrict to one conversation.
    #[serde(default)]
    pub conversation: Option<ConversationId>,
    /// Restrict to one device.
    #[serde(default)]
    pub device: Option<DeviceId>,
    /// Restrict to one trace.
    #[serde(default)]
    pub trace: Option<TraceId>,
}

impl StreamQuery {
    /// Converts the query into a bus filter.
    ///
    /// # Errors
    ///
    /// Returns 422 if `stages` names a stage that does not exist, or one that
    /// exists in the vocabulary but which nothing in this build publishes.
    /// Silently ignoring a typo would look like a quiet pipeline, and so would
    /// accepting a subscription that can never deliver anything.
    fn into_filter(self) -> Result<Filter, ApiError> {
        let mut filter = Filter::all();

        if let Some(stages) = self.stages {
            let parsed = stages
                .split(',')
                .map(str::trim)
                .filter(|stage| !stage.is_empty())
                .map(parse_stage)
                .collect::<Result<Vec<_>, _>>()?;
            if !parsed.is_empty() {
                filter = filter.stages(parsed);
            }
        }
        if let Some(conversation) = self.conversation {
            filter = filter.conversation(conversation);
        }
        if let Some(device) = self.device {
            filter = filter.device(device);
        }
        if let Some(trace) = self.trace {
            filter = filter.trace(trace);
        }
        Ok(filter)
    }
}

/// Parses one stage name from the query string.
///
/// A stage nothing publishes is refused rather than accepted: subscribing to
/// one would hand back a stream that stays open and silent for as long as the
/// client waits, which is indistinguishable from a pipeline that has stopped
/// working. Better to say so at subscribe time.
fn parse_stage(stage: &str) -> Result<Stage, ApiError> {
    let parsed: Stage = serde_json::from_value(serde_json::Value::String(stage.to_owned()))
        .map_err(|_| ApiError::unprocessable(format!("unknown stage `{stage}`")))?;

    if !parsed.has_emitter() {
        return Err(ApiError::unprocessable(format!(
            "nothing publishes `{stage}` events yet, so this subscription would never \
             deliver anything; the stages that carry traffic are {}",
            emitting_stages().join(", ")
        )));
    }
    Ok(parsed)
}

/// The stage names a subscription can usefully ask for, for the error above.
///
/// Derived from [`Stage::has_emitter`] rather than written out, so a stage that
/// gains an emitter starts being suggested without anyone remembering to edit
/// this message.
fn emitting_stages() -> Vec<String> {
    [
        Stage::WakeWord,
        Stage::Capture,
        Stage::Transcription,
        Stage::Identity,
        Stage::Conversation,
        Stage::Reasoning,
        Stage::Tools,
        Stage::Synthesis,
        Stage::Diagnostics,
    ]
    .into_iter()
    .filter(|stage| stage.has_emitter())
    .filter_map(|stage| match serde_json::to_value(stage) {
        Ok(serde_json::Value::String(name)) => Some(format!("`{name}`")),
        _ => None,
    })
    .collect()
}

/// `GET /v1/events` — server-sent events from the bus.
///
/// The stream carries only events published after subscribing; it is a live
/// view, not a history. Each message's SSE `event` field is the variant name,
/// so browser clients can attach per-type listeners.
///
/// # Errors
///
/// Returns 422 if the query names an unknown stage.
pub async fn stream(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Query(query): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, axum::Error>>>, ApiError> {
    let subscription = state.bus.subscribe_filtered(query.into_filter()?);

    let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
        let envelope = subscription.recv().await?;
        let name = envelope_name(&envelope.event);
        let message = SseEvent::default()
            .id(envelope.id.to_string())
            .event(name)
            .json_data(envelope.as_ref());
        Some((message, subscription))
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// The variant name used as the SSE event type.
fn envelope_name(event: &conduit_core::event::Event) -> String {
    serde_json::to_value(event)
        .ok()
        .and_then(|value| value.get("type").and_then(|tag| tag.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "Event".to_owned())
}
