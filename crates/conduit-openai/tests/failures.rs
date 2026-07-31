//! How this provider behaves when the server is slow, overloaded, or wrong.
//!
//! Two properties are under test. A server that accepts a request and then
//! goes quiet must not hang the caller, and a caller must be able to tell a
//! transient failure from a permanent one — a `429` and a `400` cannot look
//! the same, or nothing can ever decide to retry.

mod server;

use std::time::Duration;

use bytes::Bytes;
use conduit_openai::{Failure, OpenAi, OpenAiConfig, OpenAiStt, OpenAiTts};
use conduit_provider::llm::{CompletionRequest, LanguageModel, Message};
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions};
use conduit_provider::tts::{SynthesisRequest, TextToSpeech};
use conduit_provider::{ChunkStream, Health, Provider};
use futures_util::StreamExt;
use server::MockServer;

/// A configuration that gives up quickly, so a stall is a fast test rather
/// than a slow one.
fn impatient(server: &MockServer) -> OpenAiConfig {
    OpenAiConfig {
        base_url: server.url(),
        read_timeout: Some(Duration::from_millis(150)),
        ..OpenAiConfig::default()
    }
}

fn config(server: &MockServer) -> OpenAiConfig {
    OpenAiConfig { base_url: server.url(), ..OpenAiConfig::default() }
}

fn request() -> CompletionRequest {
    CompletionRequest::new("gpt-test", vec![Message::user("hello")])
}

fn utterance() -> ChunkStream<AudioChunk> {
    Box::pin(futures_util::stream::once(async {
        Ok(AudioChunk { sequence: 0, data: Bytes::from_static(b"\0\0\0\0") })
    }))
}

/// The failure this crate recorded, which must survive being boxed into a
/// [`conduit_core::Error`] or a caller cannot classify anything.
fn failure(error: &conduit_core::Error) -> &Failure {
    Failure::of(error).unwrap_or_else(|| panic!("not a classified provider failure: {error}"))
}

/// Asks for a completion and returns the error, failing if one does not come.
async fn completion_error(config: OpenAiConfig) -> conduit_core::Error {
    let provider = OpenAi::new(config).expect("provider builds");
    match provider.complete(request()).await {
        Ok(_) => panic!("the request should not have succeeded"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn a_server_that_never_answers_ends_the_request_instead_of_hanging() {
    // The handshake completes and then nothing arrives, which is precisely
    // what `connect_timeout` alone does not bound.
    let server = MockServer::start_stalled().await;

    let error = completion_error(impatient(&server)).await;
    let failure = failure(&error);
    assert!(failure.is_timeout(), "a stall must be reported as a timeout: {error}");
    assert!(failure.is_retryable(), "a stall is worth retrying: {error}");
    assert_eq!(failure.status(), None, "no status was ever sent");
}

#[tokio::test]
async fn a_reply_that_goes_quiet_mid_sentence_fails_the_stream() {
    // A model that streams half an answer and then stops must not leave the
    // turn waiting on a body that will never end.
    let server = MockServer::start_stalled_after(vec![
        "data: {\"choices\":[{\"delta\":{\"content\":\"half\"}}]}\n\n".to_owned(),
    ])
    .await;

    let provider = OpenAi::new(impatient(&server)).expect("provider builds");
    let items: Vec<_> =
        provider.complete(request()).await.expect("the head arrives").collect::<Vec<_>>().await;

    assert!(items.first().is_some_and(Result::is_ok), "the first token arrived: {items:?}");
    let error = items
        .iter()
        .find_map(|item| item.as_ref().err())
        .unwrap_or_else(|| panic!("expected the stalled body to fail: {items:?}"));
    assert!(failure(error).is_retryable(), "a truncated reply is worth retrying: {error}");
}

#[tokio::test]
async fn a_stalled_transcription_ends_the_request() {
    let server = MockServer::start_stalled().await;
    let stt = OpenAiStt::new(&impatient(&server), "whisper-1").expect("builds");

    let Err(error) = stt.transcribe(utterance(), TranscribeOptions::default()).await else {
        panic!("a stalled upload must not be reported as success");
    };
    assert!(failure(&error).is_timeout(), "{error}");
}

#[tokio::test]
async fn stalled_synthesis_ends_the_request() {
    let server = MockServer::start_stalled().await;
    let tts = OpenAiTts::new(&impatient(&server), "tts-1").expect("builds");

    let Err(error) = tts.synthesize(SynthesisRequest::new("hello")).await else {
        panic!("a stalled synthesis must not be reported as success");
    };
    assert!(failure(&error).is_timeout(), "{error}");
}

#[tokio::test]
async fn a_stalled_server_is_unhealthy_rather_than_unanswered() {
    // Health is what failover consults; a health check that hangs is worse
    // than one that fails.
    let server = MockServer::start_stalled().await;
    let provider = OpenAi::new(impatient(&server)).expect("provider builds");

    assert!(matches!(provider.health().await, Health::Unhealthy { .. }));
}

#[tokio::test]
async fn an_overloaded_server_is_retryable() {
    for status in [408, 429, 500, 502, 503, 504] {
        let server = MockServer::start_status(status, "try later").await;
        let error = completion_error(config(&server)).await;
        let failure = failure(&error);

        assert_eq!(failure.status(), Some(status), "{error}");
        assert!(failure.is_retryable(), "HTTP {status} is transient: {error}");
    }
}

#[tokio::test]
async fn a_request_the_server_refuses_is_not_retryable() {
    // Sending the same bad request again gets the same answer, so a caller
    // that retried would only slow down the failure.
    for status in [400, 401, 403, 404, 422, 501] {
        let server = MockServer::start_status(status, "no").await;
        let error = completion_error(config(&server)).await;
        let failure = failure(&error);

        assert_eq!(failure.status(), Some(status), "{error}");
        assert!(!failure.is_retryable(), "HTTP {status} is permanent: {error}");
    }
}

#[tokio::test]
async fn the_status_and_body_stay_in_the_message() {
    let server = MockServer::start_status(429, "slow down").await;
    let error = completion_error(config(&server)).await;
    let message = error.to_string();

    assert!(message.contains("openai"), "the provider must name itself: {message}");
    assert!(message.contains("429"), "the status is the actionable part: {message}");
    assert!(message.contains("slow down"), "the server said something useful: {message}");
}

#[tokio::test]
async fn a_retry_after_header_is_surfaced_to_the_caller() {
    // A server that says how long to wait should be believed rather than
    // guessed at.
    let server = MockServer::start_retry_after(429, "slow down", "12").await;
    let error = completion_error(config(&server)).await;

    assert_eq!(failure(&error).retry_after(), Some(Duration::from_secs(12)));
}

#[tokio::test]
async fn an_absent_or_unparseable_retry_after_is_simply_absent() {
    let server =
        MockServer::start_retry_after(503, "later", "Wed, 21 Oct 2015 07:28:00 GMT").await;
    let error = completion_error(config(&server)).await;

    let failure = failure(&error);
    assert_eq!(failure.retry_after(), None, "an HTTP-date is not a delay this crate reads");
    assert!(failure.is_retryable(), "the status still decides retryability");
}

#[tokio::test]
async fn an_unreachable_server_is_a_retryable_transport_failure() {
    // Port 1 on the loopback interface refuses connections, so this exercises
    // the transport path rather than a status.
    let error = completion_error(OpenAiConfig {
        base_url: "http://127.0.0.1:1/v1".to_owned(),
        connect_timeout: Duration::from_millis(150),
        ..OpenAiConfig::default()
    })
    .await;

    let failure = failure(&error);
    assert_eq!(failure.status(), None);
    assert!(failure.is_retryable(), "a refused connection may succeed later: {error}");
}

#[tokio::test]
async fn a_response_the_provider_cannot_read_is_not_retryable() {
    // Retrying a server that answers with something other than the documented
    // shape just produces the same unreadable answer.
    let server = MockServer::start("not json at all".to_owned()).await;
    let stt = OpenAiStt::new(&config(&server), "whisper-1").expect("builds");

    let Err(error) = stt.transcribe(utterance(), TranscribeOptions::default()).await else {
        panic!("an unreadable body must not be reported as success");
    };
    assert!(!failure(&error).is_retryable(), "{error}");
}
