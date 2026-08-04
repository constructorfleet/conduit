//! The roster: who a deployment has enrolled, and enrolling them.
//!
//! Identification is already a pipeline stage — a turn asks a service who is
//! speaking and gets a [`SpeakerId`](conduit_core::id::SpeakerId) back. What
//! was missing is everything
//! around it: somewhere to say that this id is Ada, and a way to give the
//! service a voice to recognize in the first place.
//!
//! Conduit owns the id and the service stores it as an opaque label, so the
//! name lives here and only here. That is deliberate — the service never holds
//! a person's name, and a deployment can change embedding models without every
//! enrolled voice becoming a stranger.

use axum::extract::{Path, Query, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use bytes::Bytes;
use chrono::Utc;
use conduit_provider::speaker::SpeakerIdentifier;
use conduit_provider::storage::EnrolledSpeaker;
use conduit_provider::stt::AudioChunk;
use conduit_provider::ChunkStream;
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::ManagementCaller;
use crate::error::JsonBody;
use crate::{ApiError, AppState};

/// How much audio one enrollment request may carry.
///
/// Larger than the service-wide limit because this route is the one that
/// genuinely carries audio: a 44.1 kHz stereo WAV runs to about 170 kB a
/// second, so the general 1 MiB budget would cut an operator off after six
/// seconds of the take they just recorded.
pub const ENROLLMENT_BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;

/// The longest a speaker's name may be.
///
/// Not [`validate_name`](conduit_provider::storage::validate_name): a name is
/// never a storage key here — the id is — so it may hold apostrophes, accents,
/// and spaces, which is what people's names actually contain. Only the length
/// is bounded, so one request cannot fill the roster's storage.
const MAX_SPEAKER_NAME: usize = 200;

/// What a caller supplies to create or rename someone.
#[derive(Debug, Deserialize)]
pub struct SpeakerNameRequest {
    /// What to call them.
    pub name: String,
}

/// Which identification provider to enroll against.
#[derive(Debug, Default, Deserialize)]
pub struct EnrollQuery {
    /// Provider definition id, or absent to use the deployment's default.
    ///
    /// Worth naming when a deployment runs more than one identification
    /// service: a voice print does not travel between them, so enrolling
    /// against the wrong one produces an entry that looks enrolled and
    /// identifies nobody.
    #[serde(default)]
    pub provider: Option<String>,
}

/// `GET /v1/speakers` — everyone on the roster, in id order.
pub async fn list(
    _caller: ManagementCaller,
    State(state): State<AppState>,
) -> Result<Json<Vec<EnrolledSpeaker>>, ApiError> {
    let ids = state.speaker_ids().await.map_err(store_failure)?;
    let mut speakers = Vec::with_capacity(ids.len());
    for id in ids {
        // A row that will not decode is skipped rather than failing the list:
        // one broken entry must not make the page unopenable, which is where
        // an operator would go to fix it.
        match state.speaker(&id).await {
            Ok(Some(speaker)) => speakers.push(speaker),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(speaker = %id, %error, "skipping an unreadable roster entry");
            }
        }
    }
    Ok(Json(speakers))
}

/// `GET /v1/speakers/{id}` — one roster entry.
pub async fn get(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<EnrolledSpeaker>, ApiError> {
    Ok(Json(require_speaker(&state, &id).await?))
}

/// `POST /v1/speakers` — names somebody nobody has recorded yet.
///
/// Creating and enrolling are separate requests because they are separate
/// moments: an operator names a household and then records each person, often
/// across several sittings, and a create that demanded audio would make the
/// roster unusable until everyone was in the room.
pub async fn create(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    JsonBody(request): JsonBody<SpeakerNameRequest>,
) -> Result<(StatusCode, Json<EnrolledSpeaker>), ApiError> {
    let name = validate_speaker_name(request.name)?;
    let speaker = EnrolledSpeaker::named(name);
    state.put_speaker(speaker.clone()).await.map_err(store_failure)?;
    Ok((StatusCode::CREATED, Json(speaker)))
}

/// `PUT /v1/speakers/{id}` — renames somebody.
///
/// Only the name: the samples, the provider, and the id are facts about what
/// has happened rather than fields a caller sets, and a rename that reset the
/// sample count would report a voice as un-enrolled while the service still
/// recognized it.
pub async fn rename(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
    JsonBody(request): JsonBody<SpeakerNameRequest>,
) -> Result<Json<EnrolledSpeaker>, ApiError> {
    let name = validate_speaker_name(request.name)?;
    let speaker = EnrolledSpeaker { name, ..require_speaker(&state, &id).await? };
    state.put_speaker(speaker.clone()).await.map_err(store_failure)?;
    Ok(Json(speaker))
}

/// `POST /v1/speakers/{id}/enroll` — teaches the service one voice.
///
/// The body is a WAV file. That is the one format both halves of the console
/// can produce — a recording made in the page and a file an operator already
/// had — and it says its own sample rate, so audio recorded at whatever the
/// machine's microphone runs at arrives correctly rather than at the wrong
/// speed.
///
/// # Errors
///
/// Returns 404 if nobody is on the roster under `id`, 415 if the body is not
/// a WAV file, 422 if it cannot be read as samples, and 503 if no
/// identification provider is configured or the service refuses the audio.
pub async fn enroll(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<EnrollQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<EnrolledSpeaker>, ApiError> {
    let speaker = require_speaker(&state, &id).await?;
    let (provider_id, provider) = identifier(&state, query.provider.as_deref())?;
    let samples = wav_samples(&headers, &body)?;

    provider.enroll(speaker.id, one_utterance(samples)).await.map_err(|error| {
        // The service is what refused, and what it said is the only thing that
        // tells an operator whether to record again or fix a deployment.
        ApiError::unavailable(format!("the identification service refused the sample: {error}"))
    })?;

    let enrolled = EnrolledSpeaker {
        samples: speaker.samples.saturating_add(1),
        provider: Some(provider_id),
        enrolled_at: Some(Utc::now()),
        ..speaker
    };
    state.put_speaker(enrolled.clone()).await.map_err(store_failure)?;
    Ok(Json(enrolled))
}

/// `DELETE /v1/speakers/{id}` — forgets somebody.
///
/// The voice print goes first and the roster entry second. Losing the name
/// while the service still holds the print would leave a voice that identifies
/// as an id nobody can look up — so a service that refuses is reported, and
/// the entry stays until it is gone from both.
///
/// An entry nobody ever enrolled is removed without asking the service
/// anything: there is nothing there to forget, and a deployment with no
/// identification provider configured must still be able to tidy its roster.
pub async fn delete(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let speaker = require_speaker(&state, &id).await?;

    if speaker.is_enrolled() {
        let (_, provider) = identifier(&state, speaker.provider.as_deref())?;
        provider.forget(speaker.id).await.map_err(|error| {
            ApiError::unavailable(format!(
                "the identification service still holds this voice: {error}"
            ))
        })?;
    }

    state.remove_speaker(&id).await.map_err(store_failure)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Fetches a roster entry or reports that there is none.
async fn require_speaker(state: &AppState, id: &str) -> Result<EnrolledSpeaker, ApiError> {
    // Parsed before it is looked up so that a malformed id reads as a bad
    // request rather than as an empty roster.
    Uuid::parse_str(id)
        .map_err(|_| ApiError::bad_request(format!("`{id}` is not a speaker id")))?;
    state
        .speaker(id)
        .await
        .map_err(store_failure)?
        .ok_or_else(|| ApiError::not_found(format!("no speaker `{id}`")))
}

/// Bounds a name without narrowing what a name may contain.
fn validate_speaker_name(name: String) -> Result<String, ApiError> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err(ApiError::unprocessable("a speaker needs a name".to_owned()));
    }
    if name.chars().count() > MAX_SPEAKER_NAME {
        return Err(ApiError::unprocessable(format!(
            "a speaker's name cannot be longer than {MAX_SPEAKER_NAME} characters"
        )));
    }
    Ok(name)
}

/// The identification provider to enroll against, and what it is called.
fn identifier(
    state: &AppState,
    requested: Option<&str>,
) -> Result<(String, Arc<dyn SpeakerIdentifier>), ApiError> {
    let providers = state.providers().ok_or_else(|| {
        ApiError::unavailable(
            "no providers are configured, so there is no service to enroll against".to_owned(),
        )
    })?;
    let registry = providers.speaker();
    let name = match requested {
        Some(name) => name.to_owned(),
        None => registry
            .default_name()
            .ok_or_else(|| {
                ApiError::unavailable(
                    "no speaker identification provider is configured".to_owned(),
                )
            })?
            .to_owned(),
    };
    let provider = registry.get(&name).ok_or_else(|| {
        ApiError::unprocessable(format!(
            "`{name}` is not a registered speaker identification provider"
        ))
    })?;
    Ok((name, provider))
}

/// Reads an uploaded WAV body into interchange-format samples.
fn wav_samples(headers: &HeaderMap, body: &Bytes) -> Result<Vec<u8>, ApiError> {
    // The header is checked but not trusted: browsers and curl disagree about
    // what a WAV is called, and the file itself says so unambiguously.
    let declared =
        headers.get(CONTENT_TYPE).and_then(|value| value.to_str().ok()).unwrap_or("");
    let plausible = declared.is_empty()
        || declared.starts_with("audio/")
        || declared.starts_with("application/octet-stream");
    if !plausible {
        return Err(ApiError::unsupported_media_type(format!(
            "enrollment audio must be a WAV file, not `{declared}`"
        )));
    }
    if body.is_empty() {
        return Err(ApiError::unprocessable("the enrollment request carried no audio"));
    }

    let pcm = conduit_core::wav::parse(body)
        .map_err(|error| ApiError::unprocessable(error.to_string()))?;
    conduit_core::pcm::to_interchange(pcm.format, pcm.samples)
        .map_err(|error| ApiError::unprocessable(error.to_string()))
}

/// Presents a whole utterance as the one-chunk stream enrollment takes.
fn one_utterance(samples: Vec<u8>) -> ChunkStream<AudioChunk> {
    Box::pin(futures_util::stream::iter([Ok(AudioChunk {
        sequence: 0,
        data: Bytes::from(samples),
    })]))
}

fn store_failure(error: conduit_core::Error) -> ApiError {
    match error {
        conduit_core::Error::Config(detail) => ApiError::unprocessable(detail),
        other => ApiError::unavailable(other.to_string()),
    }
}
