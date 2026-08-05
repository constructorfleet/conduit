//! What happens when the vendor, the network, or the caller stops cooperating.
//!
//! The cases here are the ones a voice pipeline notices: a turn that goes quiet
//! mid-sentence must not look like a turn that finished, a caller who interrupts
//! must actually stop the synthesis rather than merely stop listening to it, and
//! a rejection must say whether trying again is worth anything.

mod server;

use std::time::Duration;

use bytes::Bytes;
use conduit_core::Error;
use conduit_elevenlabs::{
    ElevenLabsConfig, ElevenLabsStt, ElevenLabsTts, Failure, FailureKind,
};
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions};
use conduit_provider::tts::{SynthesisRequest, TextToSpeech};
use conduit_provider::{ChunkStream, Provider};
use futures_util::StreamExt;
use server::MockServer;

const VOICE: &str = "21m00Tcm4TlvDq8ikWAM";

fn config(server: &MockServer) -> ElevenLabsConfig {
    ElevenLabsConfig {
        base_url: server.url(),
        api_key: Some("sk_test".to_owned()),
        voice_id: Some(VOICE.to_owned()),
        // Short enough that a stalled server fails the test rather than hanging
        // it, long enough that a loaded machine does not fail a healthy one.
        read_timeout: Some(Duration::from_millis(400)),
        ..ElevenLabsConfig::default()
    }
}

fn synthesizer(server: &MockServer) -> ElevenLabsTts {
    ElevenLabsTts::new(&config(server)).expect("builds")
}

fn recognizer(server: &MockServer) -> ElevenLabsStt {
    ElevenLabsStt::new(&config(server)).expect("builds")
}

fn utterance() -> ChunkStream<AudioChunk> {
    Box::pin(futures_util::stream::once(async {
        Ok(AudioChunk { sequence: 0, data: Bytes::from_static(b"samples") })
    }))
}

/// The classification the shared HTTP layer put on `error`.
///
/// Every failure this crate reports carries one, so a caller can tell a retry
/// from a failover. An error without one would be a failure that escaped the
/// shared plumbing.
fn failure(error: &Error) -> &Failure {
    Failure::of(error).unwrap_or_else(|| panic!("expected a classified failure: {error}"))
}

// ------------------------------------------------- a turn that loses its voice

#[tokio::test]
async fn audio_that_stops_arriving_mid_turn_is_an_error_item_not_a_finished_stream() {
    // The failure this test exists for: a body that delivers some audio and then
    // goes quiet. If that ended the stream cleanly, the pipeline would believe
    // the assistant finished speaking, mark the turn done, and wait for the user
    // to reply to half a sentence. The turn has to be *lost*, visibly.
    let server = MockServer::start_stalled_after(&["first", "second"]).await;
    let tts = synthesizer(&server);

    let items: Vec<_> = tts
        .synthesize(SynthesisRequest::new("a long sentence"))
        .await
        .expect("starts")
        .collect()
        .await;

    let (audio, errors): (Vec<_>, Vec<_>) = items.into_iter().partition(Result::is_ok);
    assert_eq!(audio.len(), 2, "the audio that did arrive is still delivered");
    assert_eq!(errors.len(), 1, "and the silence that followed is reported");

    let error = errors.into_iter().next().expect("one error").expect_err("an error");
    assert!(
        matches!(failure(&error).kind(), FailureKind::Timeout),
        "a body that went quiet is a timeout, which is retryable: {error}"
    );
    assert!(failure(&error).is_retryable(), "{error}");
}

#[tokio::test]
async fn a_lost_turn_reports_its_failure_once_and_then_ends() {
    // A failed `reqwest` body reports the same failure on every poll, so a
    // provider that forwarded it verbatim would produce an endless stream of
    // identical errors — a caller draining a lost turn would spin instead of
    // giving up, which is a hang rather than a failure.
    let server = MockServer::start_stalled_after(&["first"]).await;
    let tts = synthesizer(&server);

    let mut speech = tts.synthesize(SynthesisRequest::new("hello")).await.expect("starts");
    assert!(speech.next().await.expect("audio").is_ok(), "the audio that arrived");
    assert!(speech.next().await.expect("a failure").is_err(), "the silence that followed");
    assert!(speech.next().await.is_none(), "and then the stream must be over");
}

#[tokio::test]
async fn a_failure_before_any_audio_is_reported_when_synthesis_starts() {
    // The other half: nothing arrived at all, so there is no stream to put an
    // error item on and the call itself must fail.
    let server = MockServer::start_status(500, "internal error").await;
    let tts = synthesizer(&server);

    let Err(error) = tts.synthesize(SynthesisRequest::new("hello")).await else {
        panic!("a 500 must not become an empty stream of audio");
    };
    assert!(failure(&error).is_retryable(), "a 500 is worth retrying: {error}");
}

// ------------------------------------------------------------------- barge-in

#[tokio::test]
async fn dropping_the_stream_stops_the_synthesis_rather_than_just_ignoring_it() {
    // How barge-in works. When the user interrupts, the pipeline drops the
    // speech stream; that must close the response body and stop the vendor
    // synthesizing, not leave it generating audio into a socket nobody reads.
    // A provider that buffered the whole response first would pass a test that
    // only checked "no more chunks arrive", so this asserts on the *server*
    // observing the hangup.
    let server = MockServer::start_endless().await;
    let tts = synthesizer(&server);

    let mut speech =
        tts.synthesize(SynthesisRequest::new("a very long answer")).await.expect("starts");

    // Take enough to prove audio was flowing, then interrupt.
    let first = speech.next().await.expect("audio").expect("ok");
    assert!(!first.data.is_empty());
    let _ = speech.next().await.expect("audio").expect("ok");
    drop(speech);

    // The hangup travels over a real socket, so it is observed rather than
    // instantaneous.
    let mut waited = Duration::ZERO;
    while !server.client_hung_up() && waited < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        waited += Duration::from_millis(20);
    }
    assert!(server.client_hung_up(), "the server must see the client hang up");

    // And it must have stopped, not merely noticed: no further packets.
    let stopped_at = server.packets_sent();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(server.packets_sent(), stopped_at, "synthesis continued after the interruption");
}

#[tokio::test]
async fn a_stream_dropped_before_it_is_read_at_all_still_stops_the_synthesis() {
    // A pipeline that is interrupted between asking for speech and playing it.
    // The response body must be closed by the drop, not held open by a task
    // still draining it.
    let server = MockServer::start_endless().await;
    let tts = synthesizer(&server);

    let speech = tts.synthesize(SynthesisRequest::new("hello")).await.expect("starts");
    drop(speech);

    let mut waited = Duration::ZERO;
    while !server.client_hung_up() && waited < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        waited += Duration::from_millis(20);
    }
    assert!(server.client_hung_up(), "the server must see the client hang up");
    assert_eq!(server.synthesis_calls(), 1, "and exactly one request was made");
}

// -------------------------------------------------------------- classification

#[tokio::test]
async fn a_rejected_key_is_not_retried() {
    // Retrying a 401 burns the turn's latency budget to be told the same thing.
    let server = MockServer::start_status(401, "invalid api key").await;

    let Err(error) = synthesizer(&server).synthesize(SynthesisRequest::new("hi")).await else {
        panic!("a 401 must fail");
    };
    assert!(matches!(failure(&error).kind(), FailureKind::Status), "{error}");
    assert_eq!(failure(&error).status(), Some(401));
    assert!(!failure(&error).is_retryable(), "{error}");
}

#[tokio::test]
async fn a_rate_limit_is_retryable_and_carries_the_wait_the_vendor_asked_for() {
    let server = MockServer::start_retry_after(429, "too many requests", "3").await;

    let Err(error) = synthesizer(&server).synthesize(SynthesisRequest::new("hi")).await else {
        panic!("a 429 must fail");
    };
    let failure = failure(&error);
    assert!(matches!(failure.kind(), FailureKind::Status), "{error}");
    assert_eq!(failure.status(), Some(429));
    assert!(failure.is_retryable(), "{error}");
    assert_eq!(failure.retry_after(), Some(Duration::from_secs(3)), "the vendor's own advice");
}

#[tokio::test]
async fn an_unprocessable_request_is_not_retried() {
    // A 422 here means the body was wrong — a mistyped setting, a text the model
    // will not speak. Sending it again produces the same 422.
    let server = MockServer::start_status(422, "voice_settings.stability out of range").await;

    let Err(error) = synthesizer(&server).synthesize(SynthesisRequest::new("hi")).await else {
        panic!("a 422 must fail");
    };
    assert!(!failure(&error).is_retryable(), "{error}");
    assert!(
        error.to_string().contains("stability"),
        "the vendor's explanation must survive: {error}"
    );
}

#[tokio::test]
async fn a_server_that_never_answers_gives_up_rather_than_holding_the_turn_open() {
    // The handshake completes, so a connect timeout cannot save the caller here.
    let server = MockServer::start_stalled().await;

    let Err(error) = synthesizer(&server).synthesize(SynthesisRequest::new("hi")).await else {
        panic!("a stalled server must not look like a successful synthesis");
    };
    assert!(matches!(failure(&error).kind(), FailureKind::Timeout), "{error}");
}

#[tokio::test]
async fn an_unreachable_server_is_a_transport_failure_worth_retrying() {
    // Port 1 on loopback: nothing listens, so the connection is refused rather
    // than timed out.
    let tts = ElevenLabsTts::new(&ElevenLabsConfig {
        base_url: "http://127.0.0.1:1/v1".to_owned(),
        voice_id: Some(VOICE.to_owned()),
        ..ElevenLabsConfig::default()
    })
    .expect("builds");

    let Err(error) = tts.synthesize(SynthesisRequest::new("hi")).await else {
        panic!("an unreachable server must fail");
    };
    assert!(matches!(failure(&error).kind(), FailureKind::Transport), "{error}");
    assert!(failure(&error).is_retryable(), "{error}");
}

// ------------------------------------------------------- transcription failures

#[tokio::test]
async fn a_transcription_that_is_not_the_documented_shape_is_not_retried() {
    // A body that is not a transcript will not become one on a second attempt,
    // and reporting it as retryable would make a provider integration bug look
    // like a flaky network.
    let server = MockServer::start(r#"{"unexpected":"shape"}"#).await;

    let Err(error) =
        recognizer(&server).transcribe(utterance(), TranscribeOptions::default()).await
    else {
        panic!("a body with no `text` must not become an empty transcript");
    };
    assert!(matches!(failure(&error).kind(), FailureKind::Malformed), "{error}");
    assert!(!failure(&error).is_retryable(), "{error}");
    assert!(
        error.to_string().contains("transcription"),
        "it must say what it was reading: {error}"
    );
}

#[tokio::test]
async fn a_transcription_body_that_stops_halfway_is_reported_as_the_timeout_it_is() {
    // The same code path as the malformed body above, and it must classify
    // differently: incomplete JSON because the server went quiet is worth
    // retrying, whereas complete JSON of the wrong shape is not.
    let server = MockServer::start_stalled_after(&[r#"{"text":"turn on the"#]).await;

    let Err(error) =
        recognizer(&server).transcribe(utterance(), TranscribeOptions::default()).await
    else {
        panic!("a truncated body must not become a transcript");
    };
    assert!(matches!(failure(&error).kind(), FailureKind::Timeout), "{error}");
    assert!(failure(&error).is_retryable(), "{error}");
}

#[tokio::test]
async fn audio_that_fails_mid_capture_fails_the_transcription() {
    // The recognizer buffers the utterance, so a capture that breaks partway
    // must surface rather than be transcribed as the fragment that arrived.
    let server = MockServer::start(r#"{"text":"unused"}"#).await;
    let stt = recognizer(&server);

    let broken: ChunkStream<AudioChunk> = Box::pin(futures_util::stream::iter(vec![
        Ok(AudioChunk { sequence: 0, data: Bytes::from_static(b"good") }),
        Err(Error::provider("microphone", std::io::Error::other("the microphone went away"))),
    ]));

    let Err(error) = stt.transcribe(broken, TranscribeOptions::default()).await else {
        panic!("a broken capture must not be transcribed as a fragment");
    };
    assert!(
        error.to_string().contains("microphone"),
        "the original cause must survive: {error}"
    );
    assert!(server.last_raw().await.is_none(), "and nothing should have been uploaded");
}

// ---------------------------------------------------------------------- health

#[tokio::test]
async fn a_provider_whose_credential_is_rejected_reports_itself_unhealthy() {
    let server = MockServer::start_status(401, "invalid api key").await;

    for health in [synthesizer(&server).health().await, recognizer(&server).health().await] {
        match health {
            conduit_provider::Health::Unhealthy { reason } => {
                assert!(
                    reason.contains("401") || reason.to_lowercase().contains("auth"),
                    "{reason}"
                );
                assert!(!reason.contains("sk_test"), "a health reason must not leak the key");
            }
            other => panic!("a rejected key is not healthy: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_health_reason_never_contains_the_api_key() {
    // Health reasons are surfaced in the operator console and written to logs,
    // which makes them the likeliest place for a credential to escape.
    let server = MockServer::start_status(500, "internal error").await;
    let tts = ElevenLabsTts::new(&ElevenLabsConfig {
        base_url: server.url(),
        api_key: Some("sk_super_secret_value".to_owned()),
        voice_id: Some(VOICE.to_owned()),
        ..ElevenLabsConfig::default()
    })
    .expect("builds");

    let reason = format!("{:?}", tts.health().await);
    assert!(!reason.contains("sk_super_secret_value"), "{reason}");

    // And neither does the error from a failed request, nor the provider's own
    // `Debug`, which is what a `tracing` field would print.
    let Err(error) = tts.synthesize(SynthesisRequest::new("hi")).await else {
        panic!("a 500 must fail");
    };
    assert!(!error.to_string().contains("sk_super_secret_value"), "{error}");
    assert!(!format!("{tts:?}").contains("sk_super_secret_value"));
}

// ------------------------------------------------------------------- traversal

#[tokio::test]
async fn a_traversal_attempt_never_reaches_the_wire() {
    // The refusal proved end to end rather than only in the validator: the
    // server records every path it is asked for, including ones it does not
    // serve, so "no request arrived" is distinguishable from "a request arrived
    // somewhere else".
    let server = MockServer::start("audio").await;
    let tts = synthesizer(&server);

    for attempt in ["../../v1/user", "..%2f..%2fuser", "/v1/user", "voice/../../user"] {
        let request = SynthesisRequest {
            voice: Some(attempt.to_owned()),
            ..SynthesisRequest::new("hello")
        };
        let Err(error) = tts.synthesize(request).await else {
            panic!("`{attempt}` must be refused");
        };
        assert!(matches!(error, Error::Config(_)), "`{attempt}`: {error}");
        assert!(error.to_string().contains("voice_id"), "`{attempt}`: {error}");
    }

    assert_eq!(server.synthesis_calls(), 0, "no synthesis request should have been sent");
    assert_eq!(server.last_path().await, None, "no request of any kind should have been sent");
}

#[tokio::test]
async fn a_valid_voice_still_reaches_the_endpoint_the_traversal_test_guards() {
    // The other half of the guarantee: the check would be trivially satisfiable
    // by refusing everything, so the same server must confirm a real voice id
    // arrives at the synthesis route rather than at the catch-all.
    let server = MockServer::start("audio").await;
    let tts = synthesizer(&server);

    let _ = tts
        .synthesize(SynthesisRequest::new("hello"))
        .await
        .expect("starts")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(server.synthesis_calls(), 1);
    assert_eq!(server.last_voice_id().await.as_deref(), Some(VOICE));
}
