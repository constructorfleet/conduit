//! What Cloud Speech-to-Text is actually sent, and what comes back out.

mod server;

use base64::Engine as _;
use bytes::Bytes;
use conduit_core::audio::{AudioFormat, Encoding};
use conduit_google::{Credentials, GoogleConfig, GoogleStt};
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions, Transcript};
use conduit_provider::{ChunkStream, Provider};
use futures_util::StreamExt;
use server::MockGoogle;

/// A provider pointed at `server`, with a token so nothing goes looking for ADC.
async fn recognizer(server: &MockGoogle, config: GoogleConfig) -> GoogleStt {
    GoogleStt::new(&GoogleConfig {
        credentials: Credentials::Token("t0ken".to_owned()),
        stt_base_url: server.url(),
        ..config
    })
    .await
    .expect("a recognizer")
}

/// Captured audio delivered as `chunks` separate chunks.
fn captured(samples: Vec<u8>, chunks: usize) -> ChunkStream<AudioChunk> {
    let size = samples.len().div_ceil(chunks.max(1)).max(1);
    let pieces: Vec<AudioChunk> = samples
        .chunks(size)
        .enumerate()
        .map(|(sequence, data)| AudioChunk {
            sequence: sequence as u64,
            data: Bytes::copy_from_slice(data),
        })
        .collect();
    Box::pin(futures_util::stream::iter(pieces.into_iter().map(Ok)))
}

/// Every transcript of a session, in order.
async fn listen(
    provider: &GoogleStt,
    audio: ChunkStream<AudioChunk>,
    options: TranscribeOptions,
) -> Vec<Transcript> {
    let stream = provider.transcribe(audio, options).await.expect("the session started");
    stream.map(|transcript| transcript.expect("a transcript")).collect().await
}

/// Why a session was refused. A [`ChunkStream`] is not `Debug`, so `expect_err`
/// cannot be used on one directly.
async fn refusal(
    provider: &GoogleStt,
    audio: ChunkStream<AudioChunk>,
    options: TranscribeOptions,
) -> conduit_core::Error {
    match provider.transcribe(audio, options).await {
        Ok(_) => panic!("expected the session to be refused"),
        Err(error) => error,
    }
}

/// A recognition response with one segment.
fn recognized(transcript: &str, confidence: f32) -> serde_json::Value {
    serde_json::json!({
        "results": [{
            "alternatives": [{ "transcript": transcript, "confidence": confidence }],
            "resultEndTime": "1.400s",
            "languageCode": "en-us",
        }],
        "totalBilledTime": "2s",
    })
}

#[tokio::test]
async fn a_recognition_request_carries_the_documented_fields() {
    let server = MockGoogle::json(recognized("turn on the light", 0.94)).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;
    let samples: Vec<u8> = (0..320).map(|byte| byte as u8).collect();

    listen(&provider, captured(samples.clone(), 4), TranscribeOptions::default()).await;

    assert_eq!(server.last_path().await.as_deref(), Some("/v1/speech:recognize"));
    let body = server.last_body().await.expect("a JSON body");
    assert_eq!(body["config"]["encoding"], "LINEAR16");
    assert_eq!(body["config"]["sampleRateHertz"], 16_000);
    assert_eq!(body["config"]["audioChannelCount"], 1);
    assert_eq!(body["config"]["languageCode"], "en-US");
    assert_eq!(
        body["audio"]["content"],
        base64::engine::general_purpose::STANDARD.encode(&samples),
        "the whole utterance, base64-encoded"
    );
}

#[tokio::test]
async fn the_request_is_authenticated_with_a_bearer_token() {
    let server = MockGoogle::json(recognized("hello", 0.9)).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    listen(&provider, captured(vec![0; 32], 1), TranscribeOptions::default()).await;

    assert_eq!(server.last_authorization().await.as_deref(), Some("Bearer t0ken"));
}

#[tokio::test]
async fn every_captured_chunk_reaches_the_recording() {
    // The endpoint takes one recording, so a session that dropped a chunk would
    // transcribe an utterance with a hole in it.
    let samples: Vec<u8> = (0..1_000).map(|byte| byte as u8).collect();
    let server = MockGoogle::json(recognized("a long sentence", 0.9)).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    listen(&provider, captured(samples.clone(), 17), TranscribeOptions::default()).await;

    let body = server.last_body().await.expect("a body");
    let sent = base64::engine::general_purpose::STANDARD
        .decode(body["audio"]["content"].as_str().expect("base64"))
        .expect("valid base64");
    assert_eq!(sent, samples, "17 chunks reassembled to exactly the utterance");
}

#[tokio::test]
async fn the_declared_sample_rate_is_what_google_is_told() {
    // The single most consequential field: audio read at the wrong rate produces
    // a confident, wrong transcript rather than an error.
    let server = MockGoogle::json(recognized("hello", 0.9)).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    let format = AudioFormat { sample_rate: 48_000, channels: 2, ..AudioFormat::DEFAULT };
    listen(
        &provider,
        captured(vec![0; 64], 1),
        TranscribeOptions { format, ..TranscribeOptions::default() },
    )
    .await;

    let body = server.last_body().await.expect("a body");
    assert_eq!(body["config"]["sampleRateHertz"], 48_000, "not the interchange default");
    assert_eq!(body["config"]["audioChannelCount"], 2);
}

#[tokio::test]
async fn a_nonsense_format_is_refused_before_anything_is_sent() {
    // Zero Hz cannot be recognized, and Google's rejection of it would read as an
    // opaque INVALID_ARGUMENT.
    let server = MockGoogle::json(recognized("hello", 0.9)).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    let format = AudioFormat { sample_rate: 0, ..AudioFormat::DEFAULT };
    let error = refusal(
        &provider,
        captured(vec![0; 32], 1),
        TranscribeOptions { format, ..TranscribeOptions::default() },
    )
    .await;

    assert!(error.to_string().contains('0'), "{error}");
    assert_eq!(server.request_count().await, 0, "nothing was sent");
}

#[tokio::test]
async fn a_session_language_overrides_the_configured_one() {
    let server = MockGoogle::json(recognized("guten tag", 0.9)).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    listen(
        &provider,
        captured(vec![0; 32], 1),
        TranscribeOptions {
            language: Some("de-DE".to_owned()),
            ..TranscribeOptions::default()
        },
    )
    .await;

    assert_eq!(server.last_body().await.expect("a body")["config"]["languageCode"], "de-DE");
}

#[tokio::test]
async fn a_configured_model_travels_with_every_request() {
    let server = MockGoogle::json(recognized("hello", 0.9)).await;
    let provider = recognizer(
        &server,
        GoogleConfig { model: Some("latest_long".to_owned()), ..GoogleConfig::default() },
    )
    .await;

    listen(&provider, captured(vec![0; 32], 1), TranscribeOptions::default()).await;

    assert_eq!(server.last_body().await.expect("a body")["config"]["model"], "latest_long");
}

#[tokio::test]
async fn no_model_is_sent_when_none_is_configured() {
    // An omitted `model` is Google's own default; sending `null` or a guess would
    // override a choice deliberately left open.
    let server = MockGoogle::json(recognized("hello", 0.9)).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    listen(&provider, captured(vec![0; 32], 1), TranscribeOptions::default()).await;

    assert!(server.last_body().await.expect("a body")["config"].get("model").is_none());
}

#[tokio::test]
async fn declared_settings_reach_the_recognition_config() {
    let server = MockGoogle::json(recognized("Hello, there.", 0.9)).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;
    let settings = provider
        .descriptor()
        .validate_settings(&serde_json::json!({
            "enableAutomaticPunctuation": true,
            "maxAlternatives": 3,
            "profanityFilter": false,
        }))
        .expect("valid settings");

    listen(
        &provider,
        captured(vec![0; 32], 1),
        TranscribeOptions { settings, ..TranscribeOptions::default() },
    )
    .await;

    let body = server.last_body().await.expect("a body");
    assert_eq!(body["config"]["enableAutomaticPunctuation"], true);
    assert_eq!(body["config"]["maxAlternatives"], 3);
    assert_eq!(body["config"]["profanityFilter"], false);
}

#[tokio::test]
async fn a_stored_default_applies_and_a_request_setting_overrides_it() {
    let server = MockGoogle::json(recognized("hello", 0.9)).await;
    let mut defaults = serde_json::Map::new();
    defaults.insert("enableAutomaticPunctuation".to_owned(), serde_json::json!(true));
    defaults.insert("profanityFilter".to_owned(), serde_json::json!(true));
    let provider = recognizer(
        &server,
        GoogleConfig { default_settings: defaults, ..GoogleConfig::default() },
    )
    .await;
    let settings = provider
        .descriptor()
        .validate_overrides(&serde_json::json!({ "profanityFilter": false }))
        .expect("valid override");

    listen(
        &provider,
        captured(vec![0; 32], 1),
        TranscribeOptions { settings, ..TranscribeOptions::default() },
    )
    .await;

    let body = server.last_body().await.expect("a body");
    assert_eq!(body["config"]["profanityFilter"], false, "the request wins");
    assert_eq!(body["config"]["enableAutomaticPunctuation"], true, "the default stands");
}

#[tokio::test]
async fn a_setting_the_provider_never_declared_is_refused_rather_than_forwarded() {
    let server = MockGoogle::json(recognized("hello", 0.9)).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    let error = provider
        .descriptor()
        .validate_settings(&serde_json::json!({ "enableAutomaticPunctuaton": true }))
        .expect_err("a typo is not silently dropped");

    assert!(error.to_string().contains("enableAutomaticPunctuaton"), "{error}");
}

#[tokio::test]
async fn one_final_transcript_is_emitted_and_no_partials() {
    // This endpoint has no partials to give. Inventing them would make the
    // pipeline look more responsive than it is.
    let server = MockGoogle::json(recognized("turn on the kitchen light", 0.94)).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    let transcripts = listen(
        &provider,
        captured(vec![0; 64], 1),
        TranscribeOptions { partials: true, ..TranscribeOptions::default() },
    )
    .await;

    assert_eq!(transcripts.len(), 1, "one transcript, however many were asked for");
    assert_eq!(transcripts[0].text, "turn on the kitchen light");
    assert!(transcripts[0].is_final);
    assert_eq!(transcripts[0].confidence, Some(0.94));
    assert_eq!(transcripts[0].language.as_deref(), Some("en-us"));
}

#[tokio::test]
async fn consecutive_segments_are_joined_into_one_transcript() {
    let server = MockGoogle::json(serde_json::json!({
        "results": [
            { "alternatives": [{ "transcript": "turn on", "confidence": 0.95 }] },
            { "alternatives": [{ "transcript": " the kitchen light", "confidence": 0.88 }] },
        ],
    }))
    .await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    let transcripts =
        listen(&provider, captured(vec![0; 64], 1), TranscribeOptions::default()).await;

    assert_eq!(transcripts.len(), 1);
    assert_eq!(transcripts[0].text, "turn on the kitchen light");
    assert_eq!(transcripts[0].confidence, Some(0.88), "as confident as the shakiest segment");
}

#[tokio::test]
async fn silence_recognizes_to_an_empty_final_rather_than_an_error() {
    // Google answers a recording with no speech in it with `{}`. The pipeline
    // needs the final to know the turn is over.
    let server = MockGoogle::json(serde_json::json!({})).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    let transcripts =
        listen(&provider, captured(vec![0; 320], 1), TranscribeOptions::default()).await;

    assert_eq!(transcripts.len(), 1);
    assert_eq!(transcripts[0].text, "");
    assert!(transcripts[0].is_final);
}

#[tokio::test]
async fn a_flac_recording_is_described_as_flac() {
    let server = MockGoogle::json(recognized("hello", 0.9)).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    let format = AudioFormat { encoding: Encoding::Flac, ..AudioFormat::DEFAULT };
    listen(
        &provider,
        captured(b"fLaC not really".to_vec(), 1),
        TranscribeOptions { format, ..TranscribeOptions::default() },
    )
    .await;

    assert_eq!(server.last_body().await.expect("a body")["config"]["encoding"], "FLAC");
}

#[tokio::test]
async fn an_encoding_google_cannot_read_is_refused_before_anything_is_sent() {
    let server = MockGoogle::json(recognized("hello", 0.9)).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    for encoding in [Encoding::Opus, Encoding::PcmF32Le] {
        let format = AudioFormat { encoding, ..AudioFormat::DEFAULT };
        let error = refusal(
            &provider,
            captured(vec![0; 32], 1),
            TranscribeOptions { format, ..TranscribeOptions::default() },
        )
        .await;
        assert!(error.to_string().contains("PcmS16Le"), "{error}");
    }
    assert_eq!(server.request_count().await, 0, "nothing was sent");
}

#[tokio::test]
async fn a_rejection_carries_googles_own_message_and_its_classification() {
    let server = MockGoogle::error(400, "Invalid recognition config: bad sample rate").await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    let error =
        refusal(&provider, captured(vec![0; 32], 1), TranscribeOptions::default()).await;

    assert!(error.to_string().contains("bad sample rate"), "{error}");
    assert!(!error.to_string().contains("\"code\""), "the envelope is unwrapped: {error}");
    let failure = conduit_google::Failure::of(&error).expect("classified");
    assert_eq!(failure.status(), Some(400));
    assert!(!failure.is_retryable());
}

#[tokio::test]
async fn a_rate_limit_is_retryable_and_carries_the_wait() {
    let server = MockGoogle::retry_after(429, "Quota exceeded", "5").await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    let error =
        refusal(&provider, captured(vec![0; 32], 1), TranscribeOptions::default()).await;

    let failure = conduit_google::Failure::of(&error).expect("classified");
    assert!(failure.is_retryable());
    assert_eq!(failure.retry_after(), Some(std::time::Duration::from_secs(5)));
}

#[tokio::test]
async fn a_body_that_is_not_json_is_malformed_and_not_retryable() {
    let server = MockGoogle::malformed("not json at all").await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    let error =
        refusal(&provider, captured(vec![0; 32], 1), TranscribeOptions::default()).await;

    let failure = conduit_google::Failure::of(&error).expect("classified");
    assert_eq!(failure.kind(), conduit_google::FailureKind::Malformed);
    assert!(!failure.is_retryable(), "the same bytes arrive next time");
}

#[tokio::test]
async fn a_stalled_service_times_out_rather_than_hanging_the_turn() {
    let server = MockGoogle::stalled().await;
    let provider = recognizer(
        &server,
        GoogleConfig {
            read_timeout: Some(std::time::Duration::from_millis(150)),
            ..GoogleConfig::default()
        },
    )
    .await;

    let error =
        refusal(&provider, captured(vec![0; 32], 1), TranscribeOptions::default()).await;

    assert!(conduit_google::Failure::of(&error).expect("classified").is_timeout(), "{error}");
}

#[tokio::test]
async fn a_failure_capturing_audio_stops_the_session_before_anything_is_sent() {
    // Recognizing a truncated recording would produce a transcript of half an
    // utterance and present it as the whole one.
    let server = MockGoogle::json(recognized("hello", 0.9)).await;
    let provider = recognizer(&server, GoogleConfig::default()).await;

    let audio: ChunkStream<AudioChunk> = Box::pin(futures_util::stream::iter(vec![
        Ok(AudioChunk { sequence: 0, data: Bytes::from_static(b"good") }),
        Err(conduit_core::Error::Cancelled),
    ]));
    let error = refusal(&provider, audio, TranscribeOptions::default()).await;

    assert!(matches!(error, conduit_core::Error::Cancelled), "{error}");
    assert_eq!(server.request_count().await, 0, "a partial recording is not transcribed");
}

#[tokio::test]
async fn health_probes_the_endpoint_and_reports_why_when_it_fails() {
    let healthy = MockGoogle::json(serde_json::json!({})).await;
    let provider = recognizer(&healthy, GoogleConfig::default()).await;
    assert_eq!(provider.health().await, conduit_provider::Health::Healthy);
    assert_eq!(healthy.request_count().await, 1, "the probe is a real request");

    let broken = MockGoogle::error(403, "Cloud Speech-to-Text API has not been used").await;
    let provider = recognizer(&broken, GoogleConfig::default()).await;
    match provider.health().await {
        conduit_provider::Health::Unhealthy { reason } => {
            assert!(reason.contains("has not been used"), "{reason}");
        }
        other => panic!("expected unhealthy, got {other:?}"),
    }
}

#[tokio::test]
async fn the_descriptor_describes_the_provider_without_a_request() {
    let server = MockGoogle::json(recognized("hello", 0.9)).await;
    let provider = recognizer(
        &server,
        GoogleConfig {
            name: "google-stt".to_owned(),
            label: Some("Google (telephony)".to_owned()),
            model: Some("telephony".to_owned()),
            ..GoogleConfig::default()
        },
    )
    .await;

    let descriptor = provider.descriptor();
    assert_eq!(descriptor.id, "google-stt");
    assert_eq!(descriptor.label, "Google (telephony)");
    assert_eq!(descriptor.capability, conduit_provider::Capability::Stt);
    assert!(descriptor.metadata.serves_model("telephony"));
    assert!(descriptor.metadata.supports_encoding(Encoding::PcmS16Le));
    assert!(!descriptor.metadata.supports_encoding(Encoding::Opus));
}
