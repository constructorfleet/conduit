//! Synthesis against a stand-in Deepgram.
//!
//! The unit tests cover the format matrix; these cover what actually goes on the
//! wire — the auth scheme, the query string, and the body shape, which are the
//! three things this vendor does differently from every other speech API here.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use conduit_core::audio::{AudioFormat, Encoding};
use conduit_deepgram::{DeepgramTts, DeepgramTtsConfig};
use conduit_provider::tts::{SynthesisRequest, TextToSpeech};
use conduit_provider::{Health, Provider};
use futures_util::StreamExt;
use tokio::sync::Mutex;

/// What the last `/speak` request carried.
#[derive(Default, Clone)]
struct Received {
    authorization: Option<String>,
    query: HashMap<String, String>,
    body: String,
}

#[derive(Clone)]
struct AppState {
    /// The samples to answer with, or a status to fail with.
    reply: Result<Vec<u8>, (u16, String)>,
    received: Arc<Mutex<Received>>,
}

struct MockDeepgram {
    address: SocketAddr,
    received: Arc<Mutex<Received>>,
}

impl MockDeepgram {
    async fn start(samples: Vec<u8>) -> Self {
        Self::spawn(Ok(samples)).await
    }

    async fn start_status(status: u16, message: &str) -> Self {
        Self::spawn(Err((status, message.to_owned()))).await
    }

    async fn spawn(reply: Result<Vec<u8>, (u16, String)>) -> Self {
        let received = Arc::new(Mutex::new(Received::default()));
        let state = AppState { reply, received: Arc::clone(&received) };
        let app = Router::new().route("/speak", post(speak)).with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Self { address, received }
    }

    fn config(&self) -> DeepgramTtsConfig {
        DeepgramTtsConfig {
            base_url: format!("http://{}", self.address),
            api_key: Some("dg-test-key".to_owned()),
            ..DeepgramTtsConfig::default()
        }
    }

    async fn received(&self) -> Received {
        self.received.lock().await.clone()
    }
}

async fn speak(
    State(state): State<AppState>,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: String,
) -> Result<Response, (StatusCode, String)> {
    {
        let mut received = state.received.lock().await;
        received.authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        received.query = uri
            .query()
            .map(|query| {
                query
                    .split('&')
                    .filter_map(|pair| pair.split_once('='))
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        received.body = body;
    }

    match state.reply {
        Ok(samples) => Ok(Response::builder()
            .header("content-type", "audio/wav")
            .body(Body::from(samples))
            .expect("a response")),
        Err((status, message)) => Err((
            StatusCode::from_u16(status).expect("a status"),
            // The shape Deepgram actually answers errors with.
            format!(r#"{{"err_code":"INVALID_AUTH","err_msg":"{message}"}}"#),
        )),
    }
}

/// Half a second of alternating samples, so a test can tell audio from padding.
fn samples(frames: usize) -> Vec<u8> {
    (0..frames)
        .flat_map(|index| {
            let sample = if index % 2 == 0 { 8_000_i16 } else { -8_000_i16 };
            sample.to_le_bytes()
        })
        .collect()
}

#[tokio::test]
async fn the_key_is_sent_as_token_rather_than_bearer() {
    // The whole reason this is a crate and not a `base_url` on the `openai`
    // variant. A `Bearer` key yields a 401 that reads as a wrong key, so an
    // operator checks their key against the dashboard and finds nothing wrong.
    let server = MockDeepgram::start(samples(80)).await;
    let tts = DeepgramTts::new(server.config()).expect("builds");

    let mut stream = tts.synthesize(SynthesisRequest::new("hello")).await.expect("accepted");
    while stream.next().await.is_some() {}

    let received = server.received().await;
    assert_eq!(received.authorization.as_deref(), Some("Token dg-test-key"));
}

#[tokio::test]
async fn samples_are_asked_for_raw_with_the_rate_the_pipeline_wants() {
    let server = MockDeepgram::start(samples(80)).await;
    let tts = DeepgramTts::new(server.config()).expect("builds");

    let mut stream = tts.synthesize(SynthesisRequest::new("hello")).await.expect("accepted");
    while stream.next().await.is_some() {}

    let query = server.received().await.query;
    assert_eq!(query.get("encoding").map(String::as_str), Some("linear16"));
    assert_eq!(
        query.get("container").map(String::as_str),
        Some("none"),
        "the parameter defaults to `wav`; unset, a RIFF header rides into a \
         stream of samples and every utterance starts with a click"
    );
    assert_eq!(query.get("sample_rate").map(String::as_str), Some("16000"));
}

#[tokio::test]
async fn the_model_travels_in_the_query_and_the_text_in_the_body() {
    // OpenAI puts both in the body. Deepgram splits them, and a `model` sent in
    // the body is silently ignored in favour of the default voice — which sounds
    // like a provider that will not honour a voice choice.
    let server = MockDeepgram::start(samples(80)).await;
    let tts = DeepgramTts::new(DeepgramTtsConfig {
        model: Some("aura-2-thalia-en".to_owned()),
        ..server.config()
    })
    .expect("builds");

    let mut stream =
        tts.synthesize(SynthesisRequest::new("turn on the light")).await.expect("accepted");
    while stream.next().await.is_some() {}

    let received = server.received().await;
    assert_eq!(received.query.get("model").map(String::as_str), Some("aura-2-thalia-en"));
    assert_eq!(received.body, r#"{"text":"turn on the light"}"#);
}

#[tokio::test]
async fn a_request_voice_overrides_the_configured_one() {
    let server = MockDeepgram::start(samples(80)).await;
    let tts = DeepgramTts::new(DeepgramTtsConfig {
        model: Some("aura-2-thalia-en".to_owned()),
        ..server.config()
    })
    .expect("builds");

    let request = SynthesisRequest {
        voice: Some("aura-2-apollo-en".to_owned()),
        ..SynthesisRequest::new("hi")
    };
    let mut stream = tts.synthesize(request).await.expect("accepted");
    while stream.next().await.is_some() {}

    let query = server.received().await.query;
    assert_eq!(query.get("model").map(String::as_str), Some("aura-2-apollo-en"));
}

#[tokio::test]
async fn the_samples_arrive_as_chunks_in_the_format_that_was_asked_for() {
    let server = MockDeepgram::start(samples(80)).await;
    let tts = DeepgramTts::new(server.config()).expect("builds");

    let chunks: Vec<_> = tts
        .synthesize(SynthesisRequest::new("hello"))
        .await
        .expect("accepted")
        .map(|item| item.expect("a chunk"))
        .collect()
        .await;

    assert!(!chunks.is_empty(), "the body carried audio");
    assert_eq!(chunks[0].sequence, 0);
    assert_eq!(chunks[0].format, AudioFormat::DEFAULT);
    let total: usize = chunks.iter().map(|chunk| chunk.data.len()).sum();
    assert_eq!(total, 160, "every byte of the body, and no container header");
}

#[tokio::test]
async fn an_utterance_over_the_character_cap_is_refused_with_both_numbers() {
    // Refused here rather than passed through, so the message names the limit
    // and the actual length instead of relaying a vendor 400.
    let server = MockDeepgram::start(samples(80)).await;
    let tts = DeepgramTts::new(server.config()).expect("builds");

    let error = match tts.synthesize(SynthesisRequest::new("x".repeat(2_001))).await {
        Ok(_) => panic!("2001 characters should have been refused"),
        Err(error) => error.to_string(),
    };

    assert!(error.contains("2000"), "the message names the limit: {error}");
    assert!(error.contains("2001"), "and what was asked for: {error}");
}

#[tokio::test]
async fn an_utterance_exactly_at_the_cap_is_accepted() {
    // The boundary belongs to the vendor, not to a guess: 2000 is allowed.
    let server = MockDeepgram::start(samples(80)).await;
    let tts = DeepgramTts::new(server.config()).expect("builds");

    let accepted = tts.synthesize(SynthesisRequest::new("x".repeat(2_000))).await.is_ok();

    assert!(accepted, "the limit is inclusive");
}

#[tokio::test]
async fn the_cap_counts_characters_rather_than_bytes() {
    // A multi-byte character is one character to the vendor's counter. Counting
    // bytes would refuse a legitimate utterance a third of the way in — and it
    // would do so only for non-English speech.
    let server = MockDeepgram::start(samples(80)).await;
    let tts = DeepgramTts::new(server.config()).expect("builds");

    // 1000 three-byte characters: 3000 bytes, 1000 characters.
    let accepted = tts.synthesize(SynthesisRequest::new("あ".repeat(1_000))).await.is_ok();

    assert!(accepted, "1000 characters is under the cap whatever they encode to");
}

#[tokio::test]
async fn a_format_deepgram_will_not_produce_is_refused_before_the_request() {
    let server = MockDeepgram::start(samples(80)).await;
    let tts = DeepgramTts::new(server.config()).expect("builds");

    let request = SynthesisRequest {
        format: AudioFormat { encoding: Encoding::PcmF32Le, ..AudioFormat::DEFAULT },
        ..SynthesisRequest::new("hello")
    };
    let refused = tts.synthesize(request).await.is_err();

    assert!(refused);
    let received = server.received().await;
    assert!(received.body.is_empty(), "nothing was billed for a request that cannot work");
}

#[tokio::test]
async fn a_refused_key_reports_what_the_vendor_said() {
    let server = MockDeepgram::start_status(401, "project does not have access").await;
    let tts = DeepgramTts::new(server.config()).expect("builds");

    let error = match tts.synthesize(SynthesisRequest::new("hello")).await {
        Ok(_) => panic!("a 401 should not produce a stream"),
        Err(error) => error.to_string(),
    };

    assert!(
        error.contains("project does not have access") || error.contains("401"),
        "the operator needs the vendor's own words: {error}"
    );
}

#[tokio::test]
async fn health_is_unhealthy_when_the_key_is_refused() {
    let server = MockDeepgram::start_status(401, "invalid credentials").await;
    let tts = DeepgramTts::new(server.config()).expect("builds");

    assert!(matches!(tts.health().await, Health::Unhealthy { .. }));
}

#[tokio::test]
async fn health_proves_the_key_the_scheme_and_the_model_together() {
    // Deepgram publishes no unauthenticated ping, so the probe is the shortest
    // billable utterance — which is also the only thing that proves all three.
    let server = MockDeepgram::start(samples(2)).await;
    let tts = DeepgramTts::new(server.config()).expect("builds");

    assert!(matches!(tts.health().await, Health::Healthy));

    let received = server.received().await;
    assert_eq!(received.authorization.as_deref(), Some("Token dg-test-key"));
    assert!(received.query.contains_key("model"), "and the model id is exercised");
}
