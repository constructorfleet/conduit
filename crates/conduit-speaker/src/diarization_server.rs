//! A client for [`Diarization_Server`], for deployments already running one.
//!
//! Despite its name that project does speaker *recognition* against enrolled
//! embeddings rather than diarization, which is the thing a Conduit pipeline
//! needs: its README describes identifying speakers and learning new ones. Its
//! four routes line up with the [`SpeakerIdentifier`] trait, so pointing at one
//! needs no adapter — only this client.
//!
//! It differs from Conduit's own contract in three ways worth knowing:
//!
//! - The body is **raw little-endian 16-bit PCM at 16 kHz**, not a container.
//!   The server reads it with `np.frombuffer(body, dtype=np.int16)` and builds
//!   its recognizer at a fixed 16 kHz, so there is nothing in the request that
//!   could describe any other format, and a pipeline capturing one is refused
//!   here rather than sent samples the server would misread as speech.
//! - The speaker is a free-text `name` query parameter. Conduit sends the
//!   speaker's UUID, which passes through the server's sanitizer unchanged.
//! - Identification returns a ranked list, and this asks for the top entry.
//!
//! [`Diarization_Server`]: https://github.com/CptCamembert/Diarization_Server

use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::id::SpeakerId;
use conduit_core::{Error, Result};
use conduit_provider::speaker::{Identification, SpeakerIdentifier};
use conduit_provider::stt::AudioChunk;
use conduit_provider::{ChunkStream, Health, Provider};
use serde::Deserialize;

use crate::{collect_samples, http_client, REQUEST_TIMEOUT};

/// The only format the server can read.
///
/// Not a preference: the request carries no format description at all, so
/// anything else is silently misinterpreted rather than rejected by the server.
const REQUIRED_FORMAT: AudioFormat =
    AudioFormat { encoding: Encoding::PcmS16Le, sample_rate: 16_000, channels: 1 };

/// A speaker identifier backed by a Diarization_Server instance.
#[derive(Debug, Clone)]
pub struct DiarizationServerSpeakerId {
    name: String,
    base_url: String,
    threshold: f32,
    format: AudioFormat,
    client: reqwest::Client,
}

impl DiarizationServerSpeakerId {
    /// Builds a client for the server at `base_url`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if `base_url` is not an absolute HTTP URL or
    /// the HTTP client cannot be built.
    pub fn new(name: impl Into<String>, base_url: &str, threshold: f32) -> Result<Self> {
        let name = name.into();
        Ok(Self {
            client: http_client(&name, REQUEST_TIMEOUT)?,
            base_url: crate::normalized_base_url(&name, base_url)?,
            name,
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

    /// The samples to send, or an error naming the format that cannot be sent.
    ///
    /// Checked before the audio is collected rather than after: a pipeline
    /// capturing FLAC would otherwise buffer a whole utterance to discover
    /// something knowable from its configuration.
    fn checked_format(&self) -> Result<()> {
        if self.format == REQUIRED_FORMAT {
            return Ok(());
        }
        Err(Error::Config(format!(
            "provider `{}` speaks to a Diarization_Server, which reads raw 16 kHz mono \
             16-bit PCM and cannot be told otherwise; this pipeline captures {:?} at \
             {} Hz in {} channel(s)",
            self.name, self.format.encoding, self.format.sample_rate, self.format.channels
        )))
    }
}

/// One ranked candidate from `/diarize`.
#[derive(Debug, Deserialize)]
struct RankedSpeaker {
    /// The name the voice was enrolled under, which for Conduit is a UUID.
    speaker: String,
    /// Similarity, which the server does not bound but reports as a float.
    score: f32,
}

/// What `/diarize` answers with.
#[derive(Debug, Deserialize)]
struct DiarizeResponse {
    #[serde(default)]
    speakers: Vec<RankedSpeaker>,
}

#[async_trait::async_trait]
impl Provider for DiarizationServerSpeakerId {
    fn name(&self) -> &str {
        &self.name
    }

    async fn health(&self) -> Health {
        match self.client.get(format!("{}/health", self.base_url)).send().await {
            Ok(response) if response.status().is_success() => Health::Healthy,
            Ok(response) => {
                Health::Unhealthy { reason: format!("health returned {}", response.status()) }
            }
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl SpeakerIdentifier for DiarizationServerSpeakerId {
    async fn identify(&self, audio: ChunkStream<AudioChunk>) -> Result<Identification> {
        self.checked_format()?;
        let samples = collect_samples(audio).await?;
        let response = self
            .client
            .post(format!("{}/diarize?top_n=1", self.base_url))
            .body(samples)
            .send()
            .await
            .map_err(|error| Error::provider(self.name.clone(), error))?;
        let response = crate::checked(&self.name, response).await?;
        let ranked: DiarizeResponse =
            response.json().await.map_err(|error| Error::provider(self.name.clone(), error))?;

        let Some(best) = ranked.speakers.into_iter().next() else {
            // Nothing enrolled, or nothing close enough for the server to
            // rank. Either way nobody was recognized, which is an answer.
            return Ok(Identification::unknown(0.0));
        };
        // The server's score is unbounded in principle; a confidence is not.
        let confidence = best.score.clamp(0.0, 1.0);

        if confidence < self.threshold {
            tracing::debug!(
                provider = self.name,
                confidence,
                threshold = self.threshold,
                "closest voice print fell below the configured threshold"
            );
            return Ok(Identification::unknown(confidence));
        }

        // A server shared with something other than Conduit may hold voices
        // enrolled under a human name. That is a real match to a speaker this
        // pipeline cannot address, so it is reported as an unknown voice with
        // its confidence intact rather than as an error or as nobody.
        match best.speaker.parse::<uuid::Uuid>() {
            Ok(speaker) => {
                Ok(Identification { speaker: Some(SpeakerId::from_uuid(speaker)), confidence })
            }
            Err(_) => {
                tracing::debug!(
                    provider = self.name,
                    matched = %best.speaker,
                    "voice matched a speaker that Conduit did not enroll"
                );
                Ok(Identification::unknown(confidence))
            }
        }
    }

    async fn enroll(&self, speaker: SpeakerId, samples: ChunkStream<AudioChunk>) -> Result<()> {
        self.checked_format()?;
        let body = collect_samples(samples).await?;
        let response = self
            .client
            .post(format!("{}/diarize_teach?name={speaker}", self.base_url))
            .body(body)
            .send()
            .await
            .map_err(|error| Error::provider(self.name.clone(), error))?;
        crate::checked(&self.name, response).await.map(|_| ())
    }

    async fn forget(&self, speaker: SpeakerId) -> Result<()> {
        let response = self
            .client
            .post(format!("{}/diarize_teach_delete?name={speaker}", self.base_url))
            .send()
            .await
            .map_err(|error| Error::provider(self.name.clone(), error))?;
        // The server answers 200 whether or not the speaker was there, which
        // matches what the trait asks for: removing an unknown speaker
        // succeeds.
        crate::checked(&self.name, response).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use std::sync::Arc;

    fn utterance() -> ChunkStream<AudioChunk> {
        Box::pin(futures_util::stream::iter([Ok(AudioChunk {
            sequence: 0,
            data: vec![1, 0, 2, 0].into(),
        })]))
    }

    #[tokio::test]
    async fn a_ranked_match_becomes_an_identification() {
        let speaker = SpeakerId::new();
        let base = crate::tests::serve(Router::new().route(
            "/diarize",
            post(move || async move {
                Json(serde_json::json!({
                    "speakers": [{ "speaker": speaker.to_string(), "score": 0.88 }]
                }))
            }),
        ))
        .await;

        let provider = DiarizationServerSpeakerId::new("voices", &base, 0.5).expect("built");
        let identified = provider.identify(utterance()).await.expect("identified");

        assert_eq!(identified.speaker, Some(speaker));
        assert!((identified.confidence - 0.88).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn the_audio_arrives_as_raw_samples_rather_than_a_container() {
        // The server reads the body with `np.frombuffer(dtype=int16)`. A WAV
        // header would be read as forty-four bytes of speech.
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let base = crate::tests::serve(Router::new().route(
            "/diarize",
            post(move |body: axum::body::Bytes| async move {
                recorder.lock().await.extend_from_slice(&body);
                Json(serde_json::json!({ "speakers": [] }))
            }),
        ))
        .await;

        let provider = DiarizationServerSpeakerId::new("voices", &base, 0.5).expect("built");
        provider.identify(utterance()).await.expect("identified");

        assert_eq!(&*seen.lock().await, &[1, 0, 2, 0], "the samples, and nothing else");
    }

    #[tokio::test]
    async fn a_pipeline_in_another_format_is_refused_rather_than_misread() {
        // The request cannot describe a format, so the server would read
        // 32-bit floats as pairs of tiny integers and score noise.
        let base = crate::tests::serve(Router::new()).await;
        let provider =
            DiarizationServerSpeakerId::new("voices", &base, 0.5).expect("built").with_format(
                AudioFormat { encoding: Encoding::PcmF32Le, sample_rate: 16_000, channels: 1 },
            );

        let error = provider.identify(utterance()).await.expect_err("refused");
        let message = error.to_string();
        assert!(message.contains("PcmF32Le"), "{message}");
        assert!(message.contains("raw 16 kHz mono"), "{message}");
    }

    #[tokio::test]
    async fn a_sample_rate_the_server_cannot_be_told_about_is_refused() {
        let base = crate::tests::serve(Router::new()).await;
        let provider = DiarizationServerSpeakerId::new("voices", &base, 0.5)
            .expect("built")
            .with_format(AudioFormat { sample_rate: 48_000, ..AudioFormat::DEFAULT });

        let error = provider.identify(utterance()).await.expect_err("refused");
        assert!(error.to_string().contains("48000"), "{error}");
    }

    #[tokio::test]
    async fn a_match_below_the_threshold_is_an_unknown_voice() {
        let speaker = SpeakerId::new();
        let base = crate::tests::serve(Router::new().route(
            "/diarize",
            post(move || async move {
                Json(serde_json::json!({
                    "speakers": [{ "speaker": speaker.to_string(), "score": 0.3 }]
                }))
            }),
        ))
        .await;

        let provider = DiarizationServerSpeakerId::new("voices", &base, 0.8).expect("built");
        let identified = provider.identify(utterance()).await.expect("identified");

        assert_eq!(identified.speaker, None);
        assert!((identified.confidence - 0.3).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn an_empty_ranking_matches_nobody() {
        let base =
            crate::tests::serve(Router::new().route(
                "/diarize",
                post(|| async { Json(serde_json::json!({ "speakers": [] })) }),
            ))
            .await;

        let provider = DiarizationServerSpeakerId::new("voices", &base, 0.5).expect("built");
        let identified = provider.identify(utterance()).await.expect("nobody is not an error");
        assert_eq!(identified.speaker, None);
    }

    #[tokio::test]
    async fn a_voice_enrolled_outside_conduit_is_reported_as_unknown() {
        // A server shared with something else may hold a voice under a human
        // name. It is a real match to somebody Conduit cannot address, so the
        // confidence survives and the identity does not.
        let base = crate::tests::serve(Router::new().route(
            "/diarize",
            post(|| async {
                Json(serde_json::json!({
                    "speakers": [{ "speaker": "maximilian", "score": 0.95 }]
                }))
            }),
        ))
        .await;

        let provider = DiarizationServerSpeakerId::new("voices", &base, 0.5).expect("built");
        let identified = provider.identify(utterance()).await.expect("identified");

        assert_eq!(identified.speaker, None);
        assert!((identified.confidence - 0.95).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn enrollment_names_the_speaker_in_the_query_string() {
        let speaker = SpeakerId::new();
        let expected = speaker.to_string();
        let base = crate::tests::serve(Router::new().route(
            "/diarize_teach",
            post(move |request: axum::extract::Request| {
                let expected = expected.clone();
                async move {
                    let query = request.uri().query().unwrap_or_default().to_owned();
                    assert!(query.contains(&expected), "got `{query}`");
                    Json(serde_json::json!({ "success": true }))
                }
            }),
        ))
        .await;

        let provider = DiarizationServerSpeakerId::new("voices", &base, 0.5).expect("built");
        provider.enroll(speaker, utterance()).await.expect("enrolled");
    }

    #[tokio::test]
    async fn forgetting_posts_to_the_servers_own_delete_route() {
        // It is a POST, not a DELETE. Sending the verb the trait is named for
        // would 405 against every real instance.
        let base = crate::tests::serve(Router::new().route(
            "/diarize_teach_delete",
            post(|| async { Json(serde_json::json!({ "success": true })) }),
        ))
        .await;

        let provider = DiarizationServerSpeakerId::new("voices", &base, 0.5).expect("built");
        provider.forget(SpeakerId::new()).await.expect("forgotten");
    }

    #[tokio::test]
    async fn health_follows_the_servers_own_answer() {
        let base = crate::tests::serve(
            Router::new()
                .route("/health", get(|| async { Json(serde_json::json!({"status":"ok"})) })),
        )
        .await;
        let provider = DiarizationServerSpeakerId::new("voices", &base, 0.5).expect("built");
        assert_eq!(provider.health().await, Health::Healthy);
    }
}
