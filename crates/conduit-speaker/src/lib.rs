//! Speaker identification over HTTP.
//!
//! Recognizing a voice means comparing an embedding against enrolled voice
//! prints, and the models that produce those embeddings — SpeechBrain's
//! ECAPA-TDNN, Resemblyzer's d-vectors, pyannote's — are Python. None of them
//! speaks Wyoming, so Conduit talks to whichever one a deployment runs over a
//! small HTTP contract instead of embedding a model of its own.
//!
//! # The contract
//!
//! A service is anything that answers these three requests. `{base}` is the
//! definition's `base_url`, and `{speaker}` is the UUID Conduit assigned when
//! the voice was enrolled.
//!
//! | Request | Body | Response |
//! | --- | --- | --- |
//! | `POST {base}/identify` | `audio/wav` | `{"speaker": "<uuid>" \| null, "confidence": 0.0..=1.0}` |
//! | `POST {base}/speakers/{speaker}/enroll` | `audio/wav` | any 2xx |
//! | `DELETE {base}/speakers/{speaker}` | — | any 2xx, or 404 |
//!
//! Conduit owns the identifier and the service stores it as an opaque label,
//! so a deployment can swap embedding models without every speaker becoming a
//! stranger to the tools that check who is asking.
//!
//! `services/speaker-id` in this repository implements the contract, and is
//! published as `conduit-speaker-id`. A deployment already running
//! [`Diarization_Server`](https://github.com/CptCamembert/Diarization_Server)
//! can point at that instead: it speaks its own dialect, and
//! [`diarization_server::DiarizationServerSpeakerId`] is the client for it.

pub mod diarization_server;

use std::time::Duration;

use conduit_core::audio::AudioFormat;
use conduit_core::id::SpeakerId;
use conduit_core::{Error, Result};
use conduit_provider::speaker::{Identification, SpeakerIdentifier};
use conduit_provider::stt::AudioChunk;
use conduit_provider::{ChunkStream, Health, Provider};
use futures_util::StreamExt;
use serde::Deserialize;

/// How long a request may take before it is abandoned.
///
/// Identification runs beside recognition on the same utterance, so a service
/// that has stopped answering must not be able to hold up a turn indefinitely:
/// a turn that cannot say *who* asked is still a turn that can answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds the HTTP client every provider in this crate shares.
///
/// # Errors
///
/// Returns [`Error::Provider`] if the client cannot be built.
pub(crate) fn http_client(name: &str, timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| Error::provider(name.to_owned(), error))
}

/// Checks and trims a base URL.
///
/// # Errors
///
/// Returns [`Error::Config`] if it is not an absolute HTTP URL.
pub(crate) fn normalized_base_url(name: &str, base_url: &str) -> Result<String> {
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        return Err(Error::Config(format!(
            "provider `{name}` base_url must use http or https"
        )));
    }
    Ok(base_url.trim_end_matches('/').to_owned())
}

/// Collects a whole utterance into the raw samples it carried.
///
/// Identification is not streamed — a partial answer about who is speaking is
/// not actionable — so the audio is buffered here rather than pretending to be
/// incremental. What each provider wraps those samples in differs: Conduit's
/// own contract takes a container, and Diarization_Server takes the samples
/// themselves.
pub(crate) async fn collect_samples(mut audio: ChunkStream<AudioChunk>) -> Result<Vec<u8>> {
    let mut samples = Vec::new();
    while let Some(chunk) = audio.next().await {
        samples.extend_from_slice(&chunk?.data);
    }
    Ok(samples)
}

/// Fails with the status and body of a response that was not a success.
///
/// # Errors
///
/// Returns [`Error::Provider`] describing what the service said.
pub(crate) async fn checked(
    name: &str,
    response: reqwest::Response,
) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Err(Error::Provider {
        provider: name.to_owned(),
        source: format!("speaker service returned {status}: {body}").into(),
    })
}

/// A speaker identification provider backed by an HTTP service.
#[derive(Debug, Clone)]
pub struct HttpSpeakerId {
    /// Stable registration name, surfaced in health and diagnostics.
    name: String,
    /// Base URL, without a trailing slash.
    base_url: String,
    /// Optional bearer token.
    api_key: Option<String>,
    /// Minimum similarity to call a voice a match, in `0.0..=1.0`.
    ///
    /// Applied here rather than left to the service: the same service serves
    /// several deployments, and how sure Conduit wants to be before it lets a
    /// voice unlock a door is Conduit's decision.
    threshold: f32,
    /// Format the samples handed to [`SpeakerIdentifier::identify`] are in.
    format: AudioFormat,
    client: reqwest::Client,
}

impl HttpSpeakerId {
    /// Builds a provider for the service at `base_url`.
    ///
    /// `threshold` is a similarity in `0.0..=1.0`; a match below it is reported
    /// as an unknown voice.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if `base_url` is not an absolute HTTP URL or
    /// the HTTP client cannot be built.
    pub fn new(
        name: impl Into<String>,
        base_url: &str,
        api_key: Option<String>,
        threshold: f32,
    ) -> Result<Self> {
        let name = name.into();
        Ok(Self {
            client: http_client(&name, REQUEST_TIMEOUT)?,
            base_url: normalized_base_url(&name, base_url)?,
            name,
            api_key,
            threshold,
            format: AudioFormat::DEFAULT,
        })
    }

    /// Sets the format the audio handed to this provider is captured in.
    #[must_use]
    pub const fn with_format(mut self, format: AudioFormat) -> Self {
        self.format = format;
        self
    }

    /// Adds the bearer token, when the definition carries one.
    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => request.bearer_auth(key),
            None => request,
        }
    }

    /// Collects a whole utterance and packages it as an uploadable file.
    ///
    /// Returns the bytes and the media type they are, because those two are
    /// decided together: PCM is wrapped in a WAV container and FLAC passes
    /// through as itself, and a request that announced the wrong one would ask
    /// a service to parse a header that is not there.
    async fn collect(&self, audio: ChunkStream<AudioChunk>) -> Result<(Vec<u8>, &'static str)> {
        let samples = collect_samples(audio).await?;
        let upload = conduit_core::wav::package(self.format, samples)?;
        Ok((upload.bytes, upload.mime))
    }
}

/// What an identification service answers with.
#[derive(Debug, Deserialize)]
struct IdentifyResponse {
    /// The matched speaker, absent or null when the voice matched nobody.
    #[serde(default)]
    speaker: Option<SpeakerId>,
    /// Similarity of the closest voice print, in `0.0..=1.0`.
    #[serde(default)]
    confidence: f32,
}

#[async_trait::async_trait]
impl Provider for HttpSpeakerId {
    fn name(&self) -> &str {
        &self.name
    }

    async fn health(&self) -> Health {
        let request = self.authorized(self.client.get(format!("{}/health", self.base_url)));
        match request.send().await {
            Ok(response) if response.status().is_success() => Health::Healthy,
            Ok(response) => {
                Health::Unhealthy { reason: format!("health returned {}", response.status()) }
            }
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl SpeakerIdentifier for HttpSpeakerId {
    async fn identify(&self, audio: ChunkStream<AudioChunk>) -> Result<Identification> {
        let (body, mime) = self.collect(audio).await?;
        let request = self
            .authorized(self.client.post(format!("{}/identify", self.base_url)))
            .header("content-type", mime)
            .body(body);
        let response =
            request.send().await.map_err(|error| Error::provider(self.name.clone(), error))?;
        let response = checked(&self.name, response).await?;
        let identified: IdentifyResponse =
            response.json().await.map_err(|error| Error::provider(self.name.clone(), error))?;

        // A match the deployment is not sure enough about is an unknown voice,
        // not a wrong one. Reporting the name anyway would let a per-speaker
        // tool policy be satisfied by whoever sounds closest.
        if identified.confidence < self.threshold {
            tracing::debug!(
                provider = self.name,
                confidence = identified.confidence,
                threshold = self.threshold,
                "closest voice print fell below the configured threshold"
            );
            return Ok(Identification::unknown(identified.confidence));
        }
        Ok(Identification { speaker: identified.speaker, confidence: identified.confidence })
    }

    async fn enroll(&self, speaker: SpeakerId, samples: ChunkStream<AudioChunk>) -> Result<()> {
        let (body, mime) = self.collect(samples).await?;
        let request = self
            .authorized(
                self.client.post(format!("{}/speakers/{speaker}/enroll", self.base_url)),
            )
            .header("content-type", mime)
            .body(body);
        let response =
            request.send().await.map_err(|error| Error::provider(self.name.clone(), error))?;
        checked(&self.name, response).await.map(|_| ())
    }

    async fn forget(&self, speaker: SpeakerId) -> Result<()> {
        let request = self
            .authorized(self.client.delete(format!("{}/speakers/{speaker}", self.base_url)));
        let response =
            request.send().await.map_err(|error| Error::provider(self.name.clone(), error))?;
        // Removing a speaker who is not there succeeds: the caller asked for
        // that voice print to be gone, and it is.
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        checked(&self.name, response).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Path;
    use axum::http::StatusCode;
    use axum::routing::{delete, get, post};
    use axum::{Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Serves `router` on a local port and returns its base URL.
    ///
    /// Shared with the Diarization_Server tests: both clients are checked
    /// against a real HTTP server rather than a mocked transport, because the
    /// thing most likely to be wrong is the shape of the request.
    pub(crate) async fn serve(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve");
        });
        format!("http://{address}")
    }

    fn utterance() -> ChunkStream<AudioChunk> {
        Box::pin(futures_util::stream::iter([Ok(AudioChunk {
            sequence: 0,
            data: vec![0, 1, 2, 3].into(),
        })]))
    }

    #[tokio::test]
    async fn a_confident_match_names_the_speaker() {
        let speaker = SpeakerId::new();
        let base = serve(Router::new().route(
            "/identify",
            post(move || async move {
                Json(serde_json::json!({ "speaker": speaker, "confidence": 0.93 }))
            }),
        ))
        .await;

        let provider = HttpSpeakerId::new("voices", &base, None, 0.5).expect("built");
        let identified = provider.identify(utterance()).await.expect("identified");

        assert_eq!(identified.speaker, Some(speaker));
        assert!((identified.confidence - 0.93).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn a_match_below_the_threshold_is_an_unknown_voice() {
        // The service found its closest voice print and Conduit is not sure
        // enough. Reporting the name anyway is how a per-speaker tool policy
        // ends up satisfied by whoever happens to sound similar.
        let speaker = SpeakerId::new();
        let base = serve(Router::new().route(
            "/identify",
            post(move || async move {
                Json(serde_json::json!({ "speaker": speaker, "confidence": 0.4 }))
            }),
        ))
        .await;

        let provider = HttpSpeakerId::new("voices", &base, None, 0.8).expect("built");
        let identified = provider.identify(utterance()).await.expect("identified");

        assert_eq!(identified.speaker, None, "a doubtful match names nobody");
        assert!((identified.confidence - 0.4).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn a_voice_that_matched_nobody_is_not_a_failure() {
        let base = serve(Router::new().route(
            "/identify",
            post(|| async { Json(serde_json::json!({ "speaker": null, "confidence": 0.1 })) }),
        ))
        .await;

        let provider = HttpSpeakerId::new("voices", &base, None, 0.0).expect("built");
        let identified = provider.identify(utterance()).await.expect("an unknown voice is Ok");
        assert_eq!(identified.speaker, None);
    }

    #[tokio::test]
    async fn the_utterance_reaches_the_service_as_a_wav_file() {
        // The service runs a Python model that opens a file. Raw PCM with no
        // container is not one, and every embedding would be garbage.
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let base = serve(Router::new().route(
            "/identify",
            post(move |body: axum::body::Bytes| async move {
                recorder.lock().await.extend_from_slice(&body);
                Json(serde_json::json!({ "speaker": null, "confidence": 0.0 }))
            }),
        ))
        .await;

        let provider = HttpSpeakerId::new("voices", &base, None, 0.0).expect("built");
        provider.identify(utterance()).await.expect("identified");

        let body = seen.lock().await;
        assert_eq!(&body[..4], b"RIFF", "the samples arrive in a container");
        assert_eq!(&body[8..12], b"WAVE");
    }

    #[tokio::test]
    async fn forgetting_a_speaker_nobody_enrolled_succeeds() {
        // The caller asked for that voice print to be gone, and it is.
        let base = serve(Router::new().route(
            "/speakers/{speaker}",
            delete(|Path(_): Path<String>| async { StatusCode::NOT_FOUND }),
        ))
        .await;

        let provider = HttpSpeakerId::new("voices", &base, None, 0.5).expect("built");
        provider.forget(SpeakerId::new()).await.expect("removing an unknown speaker succeeds");
    }

    #[tokio::test]
    async fn a_service_that_refuses_reports_its_status_and_body() {
        let base = serve(Router::new().route(
            "/identify",
            post(|| async { (StatusCode::BAD_GATEWAY, "model not loaded") }),
        ))
        .await;

        let provider = HttpSpeakerId::new("voices", &base, None, 0.5).expect("built");
        let error = provider.identify(utterance()).await.expect_err("502 is a failure");
        let message = error.to_string();
        assert!(message.contains("502"), "{message}");
        assert!(message.contains("model not loaded"), "{message}");
    }

    #[tokio::test]
    async fn enrollment_posts_the_samples_under_the_speakers_own_id() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let speaker = SpeakerId::new();
        let expected = speaker.to_string();
        let base = serve(Router::new().route(
            "/speakers/{speaker}/enroll",
            post(move |Path(id): Path<String>| {
                let counter = Arc::clone(&counter);
                let expected = expected.clone();
                async move {
                    assert_eq!(id, expected, "Conduit owns the identifier");
                    counter.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            }),
        ))
        .await;

        let provider = HttpSpeakerId::new("voices", &base, None, 0.5).expect("built");
        provider.enroll(speaker, utterance()).await.expect("enrolled");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn health_follows_the_services_own_answer() {
        let base =
            serve(Router::new().route("/health", get(|| async { StatusCode::OK }))).await;
        let provider = HttpSpeakerId::new("voices", &base, None, 0.5).expect("built");
        assert_eq!(provider.health().await, Health::Healthy);

        let down = serve(Router::new()).await;
        let provider = HttpSpeakerId::new("voices", &down, None, 0.5).expect("built");
        assert!(!provider.health().await.is_usable());
    }

    #[test]
    fn new_rejects_a_url_that_is_not_http() {
        let error =
            HttpSpeakerId::new("voices", "tcp://localhost:9000", None, 0.5).unwrap_err();
        assert!(matches!(error, Error::Config(_)));
    }

    #[test]
    fn a_trailing_slash_does_not_double_up_in_request_paths() {
        let provider =
            HttpSpeakerId::new("voices", "https://voices.example/", None, 0.5).expect("built");
        assert_eq!(provider.base_url, "https://voices.example");
    }
}
