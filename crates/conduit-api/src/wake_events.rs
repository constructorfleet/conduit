//! Peer-authenticated Excita wake-event ingestion.

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use chrono::{DateTime, Utc};
use conduit_core::event::{Envelope, Event};
use conduit_core::id::{DeviceId, TraceId};
use conduit_link::LinkedServiceKind;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::JsonBody;
use crate::{ApiError, AppState};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WakeEventRequest {
    event_type: WakeEventType,
    peer_id: String,
    phrase: String,
    confidence: f32,
    detected_at: DateTime<Utc>,
    #[serde(default)]
    source_device: Option<String>,
    #[serde(default)]
    audio_clip_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WakeEventType {
    Detected,
    Rejected,
}

/// Authenticates, validates, and publishes one linked Excita wake event.
pub(crate) async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    JsonBody(request): JsonBody<WakeEventRequest>,
) -> Result<StatusCode, ApiError> {
    if request.phrase.trim().is_empty() || request.phrase.len() > 200 {
        return Err(ApiError::unprocessable("phrase must contain 1 to 200 bytes"));
    }
    if !request.confidence.is_finite() || !(0.0..=1.0).contains(&request.confidence) {
        return Err(ApiError::unprocessable("confidence must be between 0 and 1"));
    }
    let device = request
        .source_device
        .as_deref()
        .and_then(|value| Uuid::parse_str(value).ok())
        .map(DeviceId::from_uuid);
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, token)| {
            scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty()
        })
        .map(|(_, token)| token.trim())
        .ok_or_else(ApiError::unauthorized)?;
    let presented_hash = Sha256::digest(bearer.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut authenticated_peer = None;
    for peer_id in state.linked_service_ids().await.map_err(|error| {
        ApiError::unavailable(format!("could not list linked services: {error}"))
    })? {
        let link = state.linked_service(&peer_id).await.map_err(|error| {
            ApiError::unavailable(format!("could not load linked service `{peer_id}`: {error}"))
        })?;
        if let Some(link) = link.filter(|link| {
            link.service_kind == LinkedServiceKind::Excita
                && link.capabilities.iter().any(|capability| capability == "excita.wake-events")
                && link.sync_token_hash == presented_hash
        }) {
            authenticated_peer = Some(link.peer_id);
            break;
        }
    }
    let peer_id = authenticated_peer.ok_or_else(ApiError::unauthorized)?;
    if request.peer_id != peer_id {
        return Err(ApiError::forbidden(
            "peer_id does not match the authenticated Excita link",
        ));
    }
    let idempotency_key = headers.get("idempotency-key").and_then(|value| value.to_str().ok());
    if let Some(key) = idempotency_key {
        if key.is_empty() || key.len() > 200 {
            return Err(ApiError::unprocessable("Idempotency-Key must contain 1 to 200 bytes"));
        }
        if state.remember_wake_event(&peer_id, key) {
            return Ok(StatusCode::ACCEPTED);
        }
    }
    let event = match request.event_type {
        WakeEventType::Detected => Event::WakeWordDetected {
            phrase: request.phrase,
            confidence: request.confidence,
            source_device: request.source_device.clone(),
            detected_at: Some(request.detected_at),
            audio_clip_ref: request.audio_clip_ref.clone(),
        },
        WakeEventType::Rejected => Event::WakeWordRejected {
            phrase: request.phrase,
            confidence: request.confidence,
            source_device: request.source_device.clone(),
            detected_at: Some(request.detected_at),
            audio_clip_ref: request.audio_clip_ref.clone(),
        },
    };
    let mut envelope = Envelope::new(TraceId::new(), event);
    envelope.device = device;
    state.bus.publish(envelope);
    tracing::info!(peer = %peer_id, capability = "excita.wake-events", detected_at = %request.detected_at,
        source_device = ?request.source_device, audio_clip_ref = ?request.audio_clip_ref, "received linked wake event");
    Ok(StatusCode::ACCEPTED)
}
