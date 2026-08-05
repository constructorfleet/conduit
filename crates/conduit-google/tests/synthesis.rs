//! What Cloud Text-to-Speech is actually sent, and what comes back out.

mod server;

use conduit_core::audio::{AudioFormat, Encoding};
use conduit_google::{Credentials, GoogleConfig, GoogleTts};
use conduit_provider::tts::{SynthesisRequest, TextToSpeech};
use conduit_provider::{Descriptor, Provider};
use futures_util::StreamExt;
use server::MockGoogle;

/// A provider pointed at `server`, with a token so nothing goes looking for ADC.
async fn synthesizer(server: &MockGoogle, config: GoogleConfig) -> GoogleTts {
    GoogleTts::new(&GoogleConfig {
        credentials: Credentials::Token("t0ken".to_owned()),
        tts_base_url: server.url(),
        ..config
    })
    .await
    .expect("a synthesizer")
}

/// Every chunk of a synthesis, in order.
async fn speak(
    provider: &GoogleTts,
    request: SynthesisRequest,
) -> Vec<conduit_provider::tts::SpeechChunk> {
    let stream = provider.synthesize(request).await.expect("synthesis accepted");
    stream.map(|chunk| chunk.expect("a chunk")).collect().await
}

/// Why a synthesis was refused. A [`conduit_provider::ChunkStream`] is not
/// `Debug`, so `expect_err` cannot be used on one directly.
async fn refusal(provider: &GoogleTts, request: SynthesisRequest) -> conduit_core::Error {
    match provider.synthesize(request).await {
        Ok(_) => panic!("expected the request to be refused"),
        Err(error) => error,
    }
}

/// A request validated against the provider's own declared schema, which is the
/// only way settings reach a provider in production.
fn settings(descriptor: &Descriptor, values: serde_json::Value) -> conduit_provider::Settings {
    descriptor.validate_settings(&values).expect("valid settings")
}

/// A synthesis response carrying `payload` verbatim, however unlikely a shape it
/// is — which is the point when testing what happens to a bad one.
fn audio_content(payload: &[u8]) -> serde_json::Value {
    use base64::Engine as _;
    serde_json::json!({
        "audioContent": base64::engine::general_purpose::STANDARD.encode(payload),
    })
}

#[tokio::test]
async fn a_synthesis_request_carries_the_documented_fields() {
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, vec![7; 64]).await;
    let provider = synthesizer(
        &server,
        GoogleConfig { voice: Some("en-US-Neural2-F".to_owned()), ..GoogleConfig::default() },
    )
    .await;

    speak(
        &provider,
        SynthesisRequest { rate: Some(1.25), ..SynthesisRequest::new("hello there") },
    )
    .await;

    assert_eq!(server.last_path().await.as_deref(), Some("/v1/text:synthesize"));
    let body = server.last_body().await.expect("a JSON body");
    assert_eq!(body["input"]["text"], "hello there");
    assert!(body["input"].get("ssml").is_none(), "text and ssml are mutually exclusive");
    assert_eq!(body["voice"]["languageCode"], "en-US");
    assert_eq!(body["voice"]["name"], "en-US-Neural2-F");
    assert_eq!(body["audioConfig"]["audioEncoding"], "LINEAR16");
    assert_eq!(body["audioConfig"]["sampleRateHertz"], 16_000);
    assert_eq!(body["audioConfig"]["speakingRate"], 1.25);
}

#[tokio::test]
async fn the_request_is_authenticated_with_a_bearer_token() {
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, vec![0; 32]).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    speak(&provider, SynthesisRequest::new("hello")).await;

    assert_eq!(server.last_authorization().await.as_deref(), Some("Bearer t0ken"));
}

#[tokio::test]
async fn a_voice_carries_its_own_language_code() {
    // Google rejects a request whose `languageCode` and `name` disagree, so a
    // German voice must travel with `de-DE` even though the provider is `en-US`.
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, vec![0; 32]).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    speak(
        &provider,
        SynthesisRequest {
            voice: Some("de-DE-Neural2-B".to_owned()),
            ..SynthesisRequest::new("hallo")
        },
    )
    .await;

    let body = server.last_body().await.expect("a body");
    assert_eq!(body["voice"]["languageCode"], "de-DE");
    assert_eq!(body["voice"]["name"], "de-DE-Neural2-B");
}

#[tokio::test]
async fn naming_no_voice_sends_only_a_language() {
    // `languageCode` is the only required part of `voice`; inventing a name
    // would override a choice the operator deliberately left to Google.
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, vec![0; 32]).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    speak(&provider, SynthesisRequest::new("hello")).await;

    let body = server.last_body().await.expect("a body");
    assert_eq!(body["voice"]["languageCode"], "en-US");
    assert!(body["voice"].get("name").is_none(), "no voice was named");
}

#[tokio::test]
async fn declared_settings_reach_the_audio_config() {
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, vec![0; 32]).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;
    let settings = settings(
        provider.descriptor(),
        serde_json::json!({ "pitch": -3.5, "volumeGainDb": 6.0 }),
    );

    speak(&provider, SynthesisRequest { settings, ..SynthesisRequest::new("hello") }).await;

    let body = server.last_body().await.expect("a body");
    assert_eq!(body["audioConfig"]["pitch"], -3.5);
    assert_eq!(body["audioConfig"]["volumeGainDb"], 6.0);
}

#[tokio::test]
async fn a_stored_default_applies_and_a_request_setting_overrides_it() {
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, vec![0; 32]).await;
    let mut defaults = serde_json::Map::new();
    defaults.insert("pitch".to_owned(), serde_json::json!(-2.0));
    defaults.insert("volumeGainDb".to_owned(), serde_json::json!(4.0));
    let provider = synthesizer(
        &server,
        GoogleConfig { default_settings: defaults, ..GoogleConfig::default() },
    )
    .await;
    let settings = settings(provider.descriptor(), serde_json::json!({ "pitch": 8.0 }));

    speak(&provider, SynthesisRequest { settings, ..SynthesisRequest::new("hello") }).await;

    let body = server.last_body().await.expect("a body");
    assert_eq!(body["audioConfig"]["pitch"], 8.0, "the request wins");
    assert_eq!(body["audioConfig"]["volumeGainDb"], 4.0, "the untouched default stands");
}

#[tokio::test]
async fn ssml_is_sent_as_ssml_rather_than_as_text() {
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, vec![0; 32]).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;
    let settings = settings(provider.descriptor(), serde_json::json!({ "ssml": true }));

    let markup = "<speak>hello <break time=\"200ms\"/>there</speak>";
    speak(&provider, SynthesisRequest { settings, ..SynthesisRequest::new(markup) }).await;

    let body = server.last_body().await.expect("a body");
    assert_eq!(body["input"]["ssml"], markup);
    assert!(body["input"].get("text").is_none(), "Google refuses a request carrying both");
}

#[tokio::test]
async fn a_gender_preference_is_dropped_when_a_voice_is_named() {
    // `ssmlGender` selects a voice; sending it beside a name it cannot change is
    // noise at best and a contradiction at worst.
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, vec![0; 32]).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;
    let settings =
        settings(provider.descriptor(), serde_json::json!({ "ssmlGender": "FEMALE" }));

    speak(
        &provider,
        SynthesisRequest { settings: settings.clone(), ..SynthesisRequest::new("hi") },
    )
    .await;
    assert_eq!(
        server.last_body().await.expect("a body")["voice"]["ssmlGender"],
        "FEMALE",
        "with no voice named, the preference is the only guidance Google gets"
    );

    speak(
        &provider,
        SynthesisRequest {
            settings,
            voice: Some("en-US-Neural2-D".to_owned()),
            ..SynthesisRequest::new("hi")
        },
    )
    .await;
    let body = server.last_body().await.expect("a body");
    assert!(body["voice"].get("ssmlGender").is_none(), "the name already decided");
}

#[tokio::test]
async fn the_wav_header_google_sends_never_reaches_the_pipeline() {
    // Google documents LINEAR16 `audioContent` as including a WAV header.
    // Passing those 44 bytes on as samples plays as a click.
    let samples: Vec<u8> = (0..128).map(|byte| byte as u8).collect();
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, samples.clone()).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    let chunks = speak(&provider, SynthesisRequest::new("hello")).await;

    let audio: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.data.to_vec()).collect();
    assert_eq!(audio, samples, "exactly the samples, with no header");
    assert!(!audio.starts_with(b"RIFF"), "the header was stripped");
}

#[tokio::test]
async fn a_provider_reports_the_rate_it_actually_produced() {
    // Getting this wrong pitches the audio. Google resamples to what is asked
    // for where it can, and answers at the voice's own rate where it cannot, so
    // the header it sends is the only trustworthy claim about what arrived.
    let produced = AudioFormat { sample_rate: 24_000, ..AudioFormat::DEFAULT };
    let server = MockGoogle::synthesizing(produced, vec![3; 64]).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    let chunks = speak(&provider, SynthesisRequest::new("hello")).await;

    assert!(!chunks.is_empty());
    assert_eq!(
        chunks[0].format.sample_rate, 24_000,
        "the first chunk reports what was produced, not what was requested"
    );
    assert!(
        chunks.iter().all(|chunk| chunk.format == chunks[0].format),
        "the format is constant for the lifetime of one stream"
    );
}

#[tokio::test]
async fn chunks_are_numbered_from_zero_and_reassemble_to_the_utterance() {
    let samples: Vec<u8> = (0..1_000).map(|byte| byte as u8).collect();
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, samples.clone()).await;
    let provider =
        synthesizer(&server, GoogleConfig { chunk_bytes: 256, ..GoogleConfig::default() })
            .await;

    let chunks = speak(&provider, SynthesisRequest::new("a longer utterance")).await;

    assert_eq!(chunks.len(), 4, "1000 bytes in 256-byte pieces");
    assert_eq!(chunks.iter().map(|chunk| chunk.sequence).collect::<Vec<_>>(), [0, 1, 2, 3]);
    let audio: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.data.to_vec()).collect();
    assert_eq!(audio, samples);
}

#[tokio::test]
async fn silence_synthesizes_to_no_chunks_rather_than_an_empty_one() {
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, Vec::new()).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    assert!(speak(&provider, SynthesisRequest::new("")).await.is_empty());
}

#[tokio::test]
async fn an_opus_request_asks_for_ogg_opus_and_passes_the_payload_through() {
    let payload = b"OggS\x00\x02 not really opus".to_vec();
    let server = MockGoogle::json(audio_content(&payload)).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    let format = AudioFormat { encoding: Encoding::Opus, sample_rate: 48_000, channels: 1 };
    let chunks =
        speak(&provider, SynthesisRequest { format, ..SynthesisRequest::new("hello") }).await;

    assert_eq!(
        server.last_body().await.expect("a body")["audioConfig"]["audioEncoding"],
        "OGG_OPUS"
    );
    let audio: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.data.to_vec()).collect();
    assert_eq!(audio, payload, "a container Google built is not unwrapped here");
    assert_eq!(chunks[0].format, format);
}

#[tokio::test]
async fn an_encoding_google_cannot_produce_is_refused_before_anything_is_sent() {
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, vec![0; 32]).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    let format = AudioFormat { encoding: Encoding::Flac, ..AudioFormat::DEFAULT };
    let error =
        refusal(&provider, SynthesisRequest { format, ..SynthesisRequest::new("hello") }).await;

    assert!(error.to_string().contains("FLAC"), "{error}");
    assert_eq!(server.request_count().await, 0, "nothing was sent");
}

#[tokio::test]
async fn a_rejection_carries_googles_own_message_and_its_classification() {
    let server = MockGoogle::error(400, "Invalid voice name: en-US-Nonexistent").await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    let error = refusal(&provider, SynthesisRequest::new("hello")).await;

    assert!(error.to_string().contains("Invalid voice name"), "{error}");
    assert!(!error.to_string().contains("\"code\""), "the envelope is unwrapped: {error}");
    let failure = conduit_google::Failure::of(&error).expect("classified");
    assert_eq!(failure.status(), Some(400));
    assert!(!failure.is_retryable(), "a bad voice name will be bad next time too");
}

#[tokio::test]
async fn a_rate_limit_is_retryable_and_carries_the_wait() {
    let server = MockGoogle::retry_after(429, "Quota exceeded", "12").await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    let error = refusal(&provider, SynthesisRequest::new("hello")).await;

    let failure = conduit_google::Failure::of(&error).expect("classified");
    assert!(failure.is_retryable());
    assert_eq!(failure.retry_after(), Some(std::time::Duration::from_secs(12)));
}

#[tokio::test]
async fn a_body_that_is_not_the_documented_shape_is_malformed_and_not_retryable() {
    let server = MockGoogle::malformed("{\"unexpected\": true}").await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    let error = refusal(&provider, SynthesisRequest::new("hello")).await;

    let failure = conduit_google::Failure::of(&error).expect("classified");
    assert_eq!(failure.kind(), conduit_google::FailureKind::Malformed);
    assert!(!failure.is_retryable());
}

#[tokio::test]
async fn audio_content_that_is_not_base64_is_a_malformed_response() {
    let server =
        MockGoogle::json(serde_json::json!({ "audioContent": "not!valid!base64!" })).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    let error = refusal(&provider, SynthesisRequest::new("hello")).await;

    assert!(error.to_string().contains("base64"), "{error}");
    assert_eq!(
        conduit_google::Failure::of(&error).map(conduit_google::Failure::kind),
        Some(conduit_google::FailureKind::Malformed)
    );
}

#[tokio::test]
async fn linear16_that_is_not_a_wav_file_is_a_malformed_response() {
    let server = MockGoogle::json(audio_content(b"raw samples, no header")).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    let error = refusal(&provider, SynthesisRequest::new("hello")).await;

    assert!(error.to_string().contains("WAV"), "{error}");
}

#[tokio::test]
async fn a_stalled_service_times_out_rather_than_hanging_the_turn() {
    let server = MockGoogle::stalled().await;
    let provider = synthesizer(
        &server,
        GoogleConfig {
            read_timeout: Some(std::time::Duration::from_millis(150)),
            ..GoogleConfig::default()
        },
    )
    .await;

    let error = refusal(&provider, SynthesisRequest::new("hello")).await;

    let failure = conduit_google::Failure::of(&error).expect("classified");
    assert!(failure.is_timeout(), "{error}");
    assert!(failure.is_retryable(), "a service that went quiet may not be next time");
}

#[tokio::test]
async fn the_voice_catalogue_is_fetched_for_the_configured_language() {
    let server = MockGoogle::json(serde_json::json!({
        "voices": [
            { "name": "en-GB-Neural2-A", "languageCodes": ["en-GB"] },
            { "name": "en-GB-Wavenet-B", "languageCodes": ["en-GB"] },
        ],
    }))
    .await;
    let mut provider = synthesizer(
        &server,
        GoogleConfig { language: "en-GB".to_owned(), ..GoogleConfig::default() },
    )
    .await;

    let voices = provider.refresh_voices().await.expect("a catalogue").to_vec();

    assert_eq!(server.last_path().await.as_deref(), Some("/v1/voices"));
    assert_eq!(server.last_query().await.as_deref(), Some("languageCode=en-GB"));
    assert_eq!(voices.len(), 2);
    assert_eq!(voices[0].id, "en-GB-Neural2-A");
    assert_eq!(voices[0].language, "en-GB");
    assert_eq!(
        provider.descriptor().metadata.voices.len(),
        2,
        "the descriptor is what a status screen reads"
    );
}

#[tokio::test]
async fn health_is_the_catalogue_and_reports_why_when_it_fails() {
    let healthy = MockGoogle::json(serde_json::json!({ "voices": [] })).await;
    let provider = synthesizer(&healthy, GoogleConfig::default()).await;
    assert_eq!(provider.health().await, conduit_provider::Health::Healthy);

    let broken = MockGoogle::error(403, "Cloud Text-to-Speech API has not been used").await;
    let provider = synthesizer(&broken, GoogleConfig::default()).await;
    match provider.health().await {
        conduit_provider::Health::Unhealthy { reason } => {
            assert!(reason.contains("has not been used"), "{reason}");
        }
        other => panic!("expected unhealthy, got {other:?}"),
    }
}

#[tokio::test]
async fn a_setting_the_provider_never_declared_is_refused_rather_than_forwarded() {
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, vec![0; 32]).await;
    let provider = synthesizer(&server, GoogleConfig::default()).await;

    let error = provider
        .descriptor()
        .validate_settings(&serde_json::json!({ "speakingRat": 2.0 }))
        .expect_err("a typo is not silently dropped");

    assert!(error.to_string().contains("speakingRat"), "{error}");
}

#[tokio::test]
async fn the_descriptor_describes_the_provider_without_a_request() {
    let server = MockGoogle::synthesizing(AudioFormat::DEFAULT, vec![0; 32]).await;
    let provider = synthesizer(
        &server,
        GoogleConfig {
            name: "google-tts".to_owned(),
            label: Some("Google (studio voices)".to_owned()),
            ..GoogleConfig::default()
        },
    )
    .await;

    let descriptor = provider.descriptor();
    assert_eq!(descriptor.id, "google-tts");
    assert_eq!(descriptor.label, "Google (studio voices)");
    assert_eq!(descriptor.capability, conduit_provider::Capability::Tts);
    assert_eq!(provider.name(), "google-tts");
    assert!(!descriptor.settings.is_empty(), "the settings a form would render");
}
