//! Synthesis against a stand-in MaryTTS server.
//!
//! The unit tests cover the pieces; these cover what actually goes on the wire
//! and what comes back off it.

mod server;

use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::Error;
use conduit_http::{Failure, FailureKind};
use conduit_marytts::{MaryTts, MaryTtsConfig};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech};
use conduit_provider::{Health, Provider};
use futures_util::StreamExt;
use server::MockServer;

fn config(server: &MockServer) -> MaryTtsConfig {
    MaryTtsConfig { base_url: server.url(), ..MaryTtsConfig::default() }
}

/// Collects a synthesis stream into its chunks, failing on the first error.
async fn speak(provider: &MaryTts, request: SynthesisRequest) -> Vec<SpeechChunk> {
    provider
        .synthesize(request)
        .await
        .expect("accepted")
        .map(|item| item.expect("a chunk"))
        .collect()
        .await
}

/// The error from a request that must be refused outright.
///
/// A [`ChunkStream`](conduit_provider::ChunkStream) is a boxed trait object and
/// so is not `Debug`, which `expect_err` requires.
async fn refusal(provider: &MaryTts, request: SynthesisRequest) -> Error {
    match provider.synthesize(request).await {
        Ok(_) => panic!("the request should have been refused"),
        Err(error) => error,
    }
}

/// The items a synthesis stream yields, errors included.
async fn items(
    provider: &MaryTts,
    request: SynthesisRequest,
) -> Vec<conduit_core::Result<SpeechChunk>> {
    provider.synthesize(request).await.expect("accepted").collect().await
}

#[tokio::test]
async fn an_utterance_comes_back_as_playable_samples_in_the_interchange_format() {
    let server = MockServer::start(server::wav_file(AudioFormat::DEFAULT, 160)).await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    let chunks = speak(&provider, SynthesisRequest::new("turn on the light")).await;

    assert_eq!(chunks.len(), 1, "one round trip, one chunk");
    assert_eq!(chunks[0].sequence, 0);
    assert_eq!(chunks[0].format, AudioFormat::DEFAULT);
    assert_eq!(chunks[0].data.len(), 320, "the samples, not the 44-byte header");
    assert!(!chunks[0].data.starts_with(b"RIFF"), "the container never reaches playback");
}

#[tokio::test]
async fn the_text_travels_in_a_post_body_rather_than_a_url() {
    // A transform-heavy pipeline produces long utterances, and a URL is capped
    // somewhere around 4-8 KB by servers and proxies. MaryTTS parses a POST
    // entity as URL-encoded parameters, so the text belongs there.
    let server = MockServer::start(server::wav_file(AudioFormat::DEFAULT, 16)).await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    let long = "the kitchen light is on. ".repeat(500);
    assert!(long.len() > 8_192, "long enough that a query string would be refused");
    let _ = speak(&provider, SynthesisRequest::new(long.clone())).await;

    let received = server.received().await;
    assert_eq!(received.method.as_deref(), Some("POST"));
    assert_eq!(
        received.content_type.as_deref(),
        Some("application/x-www-form-urlencoded"),
        "the shape MaryTTS parses a POST entity as"
    );
    // MaryTTS only reads the body when the URI carries no query of its own.
    assert_eq!(received.query, None, "a query string would stop the body being parsed");
    assert_eq!(received.form.get("INPUT_TEXT"), Some(&long));
}

#[tokio::test]
async fn the_request_asks_for_the_one_thing_this_provider_can_decode() {
    let server = MockServer::start(server::wav_file(AudioFormat::DEFAULT, 16)).await;
    let provider = MaryTts::new(MaryTtsConfig {
        voice: Some("cmu-slt-hsmm".to_owned()),
        ..config(&server)
    })
    .expect("builds");

    let _ = speak(&provider, SynthesisRequest::new("hello")).await;

    let form = server.received().await.form;
    assert_eq!(form.get("INPUT_TYPE").map(String::as_str), Some("TEXT"));
    assert_eq!(form.get("OUTPUT_TYPE").map(String::as_str), Some("AUDIO"));
    // `WAVE_FILE`, not `WAVE_STREAM`: MaryTTS offers `_STREAM` for MP3 and
    // Vorbis only, and neither is samples.
    assert_eq!(form.get("AUDIO").map(String::as_str), Some("WAVE_FILE"));
    assert_eq!(form.get("LOCALE").map(String::as_str), Some("en_US"));
    assert_eq!(form.get("VOICE").map(String::as_str), Some("cmu-slt-hsmm"));
}

#[tokio::test]
async fn no_configured_voice_means_the_server_chooses_its_own() {
    // MaryTTS ships no voices, so a name invented here would be wrong on some
    // installs. Omitting `VOICE` asks the server for its default.
    let server = MockServer::start(server::wav_file(AudioFormat::DEFAULT, 16)).await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    let _ = speak(&provider, SynthesisRequest::new("hello")).await;

    assert_eq!(server.received().await.form.get("VOICE"), None);
}

#[tokio::test]
async fn a_voice_at_the_wrong_rate_is_resampled_before_it_reaches_the_pipeline() {
    // The pitch bug, end to end: a 22.05 kHz voice played as 16 kHz is a fifth
    // low. One second in must be about one second out.
    let format = AudioFormat { sample_rate: 22_050, ..AudioFormat::DEFAULT };
    let server = MockServer::start(server::wav_file(format, 22_050)).await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    let chunks = speak(&provider, SynthesisRequest::new("hello")).await;

    assert_eq!(chunks[0].format, AudioFormat::DEFAULT, "reported as what it now is");
    let frames = chunks[0].data.len() / 2;
    assert!(
        (15_500..=17_500).contains(&frames),
        "a second of 22.05 kHz should be about 16 000 frames, got {frames}"
    );
}

#[tokio::test]
async fn a_stereo_voice_is_mixed_down_rather_than_played_at_double_speed() {
    let format = AudioFormat { channels: 2, sample_rate: 44_100, ..AudioFormat::DEFAULT };
    let server = MockServer::start(server::wav_file(format, 44_100 * 2)).await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    let chunks = speak(&provider, SynthesisRequest::new("hello")).await;

    let frames = chunks[0].data.len() / 2;
    assert!((15_500..=17_500).contains(&frames), "got {frames} frames");
}

#[tokio::test]
async fn audio_that_stops_partway_through_surfaces_as_an_error_item_not_a_lost_turn() {
    // The case that matters for a single-chunk provider: the server accepted
    // the request, started sending a WAV, and stopped. A caller that received
    // an empty stream would render the turn as the assistant choosing silence,
    // so the failure has to arrive *on* the stream.
    let server = MockServer::start_truncated(server::wav_file(AudioFormat::DEFAULT, 160)).await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    // `synthesize` itself must succeed: the status and headers arrived, so the
    // request was accepted. If this returned an error the test would be proving
    // something else — that a rejected request fails early, which it does
    // elsewhere.
    let stream = provider.synthesize(SynthesisRequest::new("hello")).await;
    let yielded: Vec<_> =
        stream.expect("accepted, because the server answered").collect().await;

    assert_eq!(yielded.len(), 1, "the failure is an item, not an empty stream");
    let error = yielded.into_iter().next().expect("an item").expect_err("failed");
    assert!(error.to_string().contains("marytts"), "names the provider: {error}");
    assert!(Failure::of(&error).is_some(), "classified, so a caller can decide: {error}");
}

#[tokio::test]
async fn a_response_that_is_not_audio_is_an_error_rather_than_noise_sent_to_a_speaker() {
    // A reverse proxy answering 200 with an error page. Playing it is noise and
    // retrying it gets the same page.
    let server = MockServer::start_not_audio("<html>Bad Gateway</html>").await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    let yielded = items(&provider, SynthesisRequest::new("hello")).await;

    let error = yielded.into_iter().next().expect("an item").expect_err("failed");
    let failure = Failure::of(&error).expect("classified");
    assert_eq!(failure.kind(), FailureKind::Malformed);
    assert!(!failure.is_retryable(), "the same bytes come back next time");
}

#[tokio::test]
async fn a_rejected_request_fails_before_a_stream_is_returned() {
    // The server said no. That is not a mid-stream failure, so the caller finds
    // out from `synthesize` itself.
    let server = MockServer::start_status(400, "unknown voice").await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    let error = refusal(&provider, SynthesisRequest::new("hello")).await;
    let failure = Failure::of(&error).expect("classified");
    assert_eq!(failure.status(), Some(400));
    assert!(!failure.is_retryable(), "the request itself is wrong");
    assert!(error.to_string().contains("unknown voice"), "quotes the server: {error}");
}

#[tokio::test]
async fn a_server_that_is_overloaded_is_worth_trying_again() {
    let server = MockServer::start_status(503, "synthesis queue full").await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    let error = refusal(&provider, SynthesisRequest::new("hello")).await;
    assert!(Failure::of(&error).expect("classified").is_retryable());
    assert!(error.is_retryable(), "and the core error agrees");
}

#[tokio::test]
async fn an_empty_utterance_is_refused_without_a_round_trip() {
    let server = MockServer::start(server::wav_file(AudioFormat::DEFAULT, 16)).await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    let error = refusal(&provider, SynthesisRequest::new("   ")).await;
    assert!(matches!(error, Error::Config(_)), "{error}");
    assert!(server.received().await.method.is_none(), "nothing was sent");
}

#[tokio::test]
async fn a_voice_carrying_an_injection_never_reaches_the_server() {
    // The guarantee, end to end: a voice that tries to add a request parameter
    // is refused here, so no crafted value is ever sent.
    let server = MockServer::start(server::wav_file(AudioFormat::DEFAULT, 16)).await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    let request = SynthesisRequest {
        voice: Some("cmu-slt-hsmm&OUTPUT_TYPE=TEXT".to_owned()),
        ..SynthesisRequest::new("hello")
    };

    let error = refusal(&provider, request).await;
    assert!(matches!(error, Error::Config(_)), "{error}");
    assert!(error.to_string().contains("`voice`"), "names the field: {error}");
    assert!(server.received().await.method.is_none(), "nothing was sent");
}

#[tokio::test]
async fn the_catalogue_is_read_from_the_server_that_has_the_voices() {
    // Voices are dropped into a MaryTTS install as jars, so the list is
    // per-deployment and cannot be a constant in the crate.
    let server = MockServer::start(server::wav_file(AudioFormat::DEFAULT, 16)).await;
    let mut provider = MaryTts::new(config(&server)).expect("builds");

    assert!(provider.voices().is_empty(), "nothing is known before the server is asked");

    let voices = provider.refresh_catalogue().await.expect("read").to_vec();
    assert_eq!(voices.len(), 2);
    assert_eq!(voices[0].id, "cmu-slt-hsmm");
    assert_eq!(voices[0].language, "en-US", "BCP-47, not Java's en_US");
    assert_eq!(voices[1].id, "dfki-pavoque-neutral");

    let metadata = provider.descriptor().metadata.clone();
    assert_eq!(metadata.languages, ["en-US", "de"], "the descriptor learned them too");
    assert!(metadata.supports_encoding(Encoding::PcmS16Le));
}

#[tokio::test]
async fn a_voice_from_the_catalogue_is_synthesized_in_its_own_locale() {
    // The German voice must not be asked for in `en_US`: MaryTTS rejects a
    // locale that disagrees with the voice.
    let server = MockServer::start(server::wav_file(AudioFormat::DEFAULT, 16)).await;
    let mut provider = MaryTts::new(config(&server)).expect("builds");
    provider.refresh_catalogue().await.expect("read");

    let request = SynthesisRequest {
        voice: Some("dfki-pavoque-neutral".to_owned()),
        ..SynthesisRequest::new("Guten Tag")
    };
    let _ = speak(&provider, request).await;

    let form = server.received().await.form;
    assert_eq!(form.get("VOICE").map(String::as_str), Some("dfki-pavoque-neutral"));
    assert_eq!(form.get("LOCALE").map(String::as_str), Some("de"));
}

#[tokio::test]
async fn the_locales_the_server_speaks_can_be_listed() {
    let server = MockServer::start(server::wav_file(AudioFormat::DEFAULT, 16)).await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    assert_eq!(provider.locales().await.expect("read"), ["en-US", "de"]);
}

#[tokio::test]
async fn a_style_a_voice_understands_is_forwarded() {
    let server = MockServer::start(server::wav_file(AudioFormat::DEFAULT, 16)).await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    let settings = provider
        .descriptor()
        .validate_settings(&serde_json::json!({ "style": "poker" }))
        .expect("valid");
    let request = SynthesisRequest { settings, ..SynthesisRequest::new("hello") };
    let _ = speak(&provider, request).await;

    assert_eq!(server.received().await.form.get("STYLE").map(String::as_str), Some("poker"));
}

#[tokio::test]
async fn a_running_server_reports_healthy() {
    let server = MockServer::start(server::wav_file(AudioFormat::DEFAULT, 16)).await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    assert_eq!(provider.health().await, Health::Healthy);
}

#[tokio::test]
async fn a_server_that_is_up_but_not_ready_reports_unhealthy_with_the_reason() {
    // The point of a real health check: a server still loading its voices looks
    // fine at the socket and cannot synthesize.
    let server = MockServer::start_unhealthy().await;
    let provider = MaryTts::new(config(&server)).expect("builds");

    let Health::Unhealthy { reason } = provider.health().await else {
        panic!("a server answering 503 is not healthy");
    };
    assert!(reason.contains("503"), "says what happened: {reason}");
    assert!(!provider.health().await.is_usable(), "routing must fail over");
}

#[tokio::test]
async fn a_server_that_is_not_there_reports_unhealthy_rather_than_looking_fine() {
    // No server at all. Nothing is bound on this port.
    let provider = MaryTts::new(MaryTtsConfig {
        base_url: "http://127.0.0.1:1".to_owned(),
        ..MaryTtsConfig::default()
    })
    .expect("builds");

    let health = provider.health().await;
    assert!(matches!(health, Health::Unhealthy { .. }), "{health:?}");
    assert!(!health.is_usable());
}
