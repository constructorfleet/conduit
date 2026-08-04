//! The speaker roster endpoints against the real router.
//!
//! The interesting cases are the ones where the roster and the identification
//! service can disagree: an entry that says it is enrolled while the service
//! has never heard the voice, or a voice print left behind by an entry that
//! was deleted. Both produce a house that answers to the wrong person, so both
//! are checked here rather than left to the console.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use conduit_api::{router, AppState};
use conduit_core::audio::AudioFormat;
use conduit_core::bus::EventBus;
use conduit_core::id::SpeakerId;
use conduit_core::Result;
use conduit_provider::descriptor::Descriptor;
use conduit_provider::registry::Capability;
use conduit_provider::speaker::{Identification, SpeakerIdentifier};
use conduit_provider::stt::AudioChunk;
use conduit_provider::testing::{EchoLlm, EchoStt, EchoTts};
use conduit_provider::{ChunkStream, Provider};
use conduit_runtime::Providers;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// One enrollment: who it was for, and the samples that were sent.
type Enrollment = (SpeakerId, Vec<u8>);

/// An identification service that records what it was asked to remember.
#[derive(Debug, Clone, Default)]
struct RecordingIdentifier {
    name: &'static str,
    /// Every enrolled speaker and the samples that were sent for them.
    enrolled: Arc<Mutex<Vec<Enrollment>>>,
    forgotten: Arc<Mutex<Vec<SpeakerId>>>,
    /// What the service says when it refuses a sample, if it does.
    refuses_enroll: Option<&'static str>,
    /// What it says when it refuses to forget one, if it does.
    ///
    /// Separate from the above because the interesting failure is a service
    /// that took the print and will not give it back.
    refuses_forget: Option<&'static str>,
}

impl RecordingIdentifier {
    fn new(name: &'static str) -> Self {
        Self { name, ..Self::default() }
    }

    fn refusing_enrollment(name: &'static str, reason: &'static str) -> Self {
        Self { name, refuses_enroll: Some(reason), ..Self::default() }
    }

    fn refusing_to_forget(name: &'static str, reason: &'static str) -> Self {
        Self { name, refuses_forget: Some(reason), ..Self::default() }
    }

    fn enrolled(&self) -> Vec<Enrollment> {
        self.enrolled.lock().expect("lock").clone()
    }

    fn forgotten(&self) -> Vec<SpeakerId> {
        self.forgotten.lock().expect("lock").clone()
    }
}

impl Provider for RecordingIdentifier {
    fn descriptor(&self) -> &Descriptor {
        // Leaked so the descriptor can borrow for the provider's life; a test
        // binary builds a handful of these and then exits.
        Box::leak(Box::new(Descriptor::new(self.name, Capability::SpeakerId)))
    }
}

#[async_trait::async_trait]
impl SpeakerIdentifier for RecordingIdentifier {
    async fn identify(&self, _audio: ChunkStream<AudioChunk>) -> Result<Identification> {
        Ok(Identification::unknown(0.0))
    }

    async fn enroll(
        &self,
        speaker: SpeakerId,
        mut samples: ChunkStream<AudioChunk>,
    ) -> Result<()> {
        let mut audio = Vec::new();
        while let Some(chunk) = samples.next().await {
            audio.extend_from_slice(&chunk?.data);
        }
        if let Some(reason) = self.refuses_enroll {
            return Err(conduit_core::Error::Config(reason.to_owned()));
        }
        self.enrolled.lock().expect("lock").push((speaker, audio));
        Ok(())
    }

    async fn forget(&self, speaker: SpeakerId) -> Result<()> {
        if let Some(reason) = self.refuses_forget {
            return Err(conduit_core::Error::Config(reason.to_owned()));
        }
        self.forgotten.lock().expect("lock").push(speaker);
        Ok(())
    }
}

fn state_with(identifier: RecordingIdentifier) -> AppState {
    let providers = Providers::new()
        .with_stt(EchoStt)
        .with_llm(EchoLlm)
        .with_tts(EchoTts)
        .with_speaker(identifier);
    AppState::new(EventBus::default()).with_providers(providers)
}

/// State with no identification service configured at all.
fn bare_state() -> AppState {
    AppState::new(EventBus::default())
}

async fn call(state: &AppState, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = router(state.clone()).oneshot(request).await.expect("router responds");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json body")
    };
    (status, json)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).expect("request")
}

fn delete(uri: &str) -> Request<Body> {
    Request::builder().method("DELETE").uri(uri).body(Body::empty()).expect("request")
}

fn json_request(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request")
}

/// A WAV file of `seconds` of silence at `rate`, in `channels` channels.
fn wav(seconds: f32, rate: u32, channels: u16) -> Vec<u8> {
    let frames = (seconds * rate as f32) as usize;
    let format = AudioFormat { sample_rate: rate, channels, ..AudioFormat::DEFAULT };
    let samples = vec![0_u8; frames * channels as usize * 2];
    conduit_core::wav::package(format, samples).expect("packages").bytes
}

fn enroll_request(id: &str, body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/v1/speakers/{id}/enroll"))
        .header("content-type", "audio/wav")
        .body(Body::from(body))
        .expect("request")
}

/// Creates a speaker and returns their id.
async fn create_speaker(state: &AppState, name: &str) -> String {
    let (status, body) =
        call(state, json_request("POST", "/v1/speakers", serde_json::json!({ "name": name })))
            .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().expect("an id").to_owned()
}

#[tokio::test]
async fn an_empty_roster_lists_nobody() {
    let state = state_with(RecordingIdentifier::new("voices"));
    let (status, body) = call(&state, get("/v1/speakers")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));
}

#[tokio::test]
async fn somebody_can_be_named_before_they_have_been_recorded() {
    // The two are separate moments: an operator names the household, and
    // records each person when that person is actually there.
    let state = state_with(RecordingIdentifier::new("voices"));
    let (status, body) = call(
        &state,
        json_request("POST", "/v1/speakers", serde_json::json!({ "name": "Ada" })),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["name"], "Ada");
    assert_eq!(body["samples"], 0, "named, not yet heard");
    assert!(body["enrolled_at"].is_null());
    assert!(
        uuid::Uuid::parse_str(body["id"].as_str().expect("an id")).is_ok(),
        "the id is Conduit's, and it is a UUID: {body}"
    );
}

#[tokio::test]
async fn a_speaker_needs_a_name() {
    let state = state_with(RecordingIdentifier::new("voices"));
    let (status, _) =
        call(&state, json_request("POST", "/v1/speakers", serde_json::json!({ "name": "  " })))
            .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_name_may_be_the_kind_of_thing_people_are_actually_called() {
    // A name is never a storage key here — the id is — so the rule that
    // governs pipeline names must not govern this one.
    let state = state_with(RecordingIdentifier::new("voices"));
    let (status, body) = call(
        &state,
        json_request(
            "POST",
            "/v1/speakers",
            serde_json::json!({ "name": "Ada O'Neill-Sørensen" }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], "Ada O'Neill-Sørensen");
}

#[tokio::test]
async fn enrolling_sends_the_audio_and_records_that_it_happened() {
    let identifier = RecordingIdentifier::new("voices");
    let state = state_with(identifier.clone());
    let id = create_speaker(&state, "Ada").await;

    let (status, body) = call(&state, enroll_request(&id, wav(1.0, 16_000, 1))).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["samples"], 1);
    assert_eq!(body["provider"], "voices", "which service holds the print");
    assert!(!body["enrolled_at"].is_null());

    let enrolled = identifier.enrolled();
    assert_eq!(enrolled.len(), 1, "the service was asked exactly once");
    assert_eq!(enrolled[0].0.to_string(), id, "under the id Conduit owns");
    assert_eq!(enrolled[0].1.len(), 32_000, "a second of interchange-format audio");
}

#[tokio::test]
async fn a_second_sample_adds_to_the_first_rather_than_replacing_it() {
    // Enrollment is cumulative on every service Conduit speaks to, and an
    // operator recording a second take is improving the print, not restarting.
    let identifier = RecordingIdentifier::new("voices");
    let state = state_with(identifier.clone());
    let id = create_speaker(&state, "Ada").await;

    call(&state, enroll_request(&id, wav(1.0, 16_000, 1))).await;
    let (status, body) = call(&state, enroll_request(&id, wav(1.0, 16_000, 1))).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["samples"], 2);
    assert_eq!(identifier.enrolled().len(), 2);
}

#[tokio::test]
async fn audio_recorded_at_another_rate_is_converted_before_it_is_sent() {
    // The whole reason the body is a WAV: a browser records at whatever its
    // microphone runs at, and samples sent at the wrong rate are a voice
    // pitched wrong, which embeds as a different person.
    let identifier = RecordingIdentifier::new("voices");
    let state = state_with(identifier.clone());
    let id = create_speaker(&state, "Ada").await;

    let (status, body) = call(&state, enroll_request(&id, wav(1.0, 48_000, 2))).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let sent = identifier.enrolled()[0].1.len();
    assert!(
        (31_000..=35_000).contains(&sent),
        "a second of stereo 48 kHz should arrive as about a second of 16 kHz mono, got {sent} bytes"
    );
}

#[tokio::test]
async fn enrollment_carries_more_audio_than_the_service_wide_limit_allows() {
    // The route raises the body limit on purpose: 1 MiB of 44.1 kHz stereo is
    // about six seconds, which would cut an operator off mid-take. This is the
    // check that the per-route limit actually wins over the router's.
    let identifier = RecordingIdentifier::new("voices");
    let state = state_with(identifier.clone());
    let id = create_speaker(&state, "Ada").await;

    // Eight seconds of 44.1 kHz stereo runs to about 1.4 MiB: over the general
    // limit, well under this route's.
    let audio = wav(8.0, 44_100, 2);
    assert!(audio.len() > 1024 * 1024, "the sample must exceed the general limit");

    let (status, body) = call(&state, enroll_request(&id, audio)).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(identifier.enrolled().len(), 1);
}

#[tokio::test]
async fn a_body_that_is_not_a_wav_is_refused_rather_than_enrolled_as_noise() {
    let identifier = RecordingIdentifier::new("voices");
    let state = state_with(identifier.clone());
    let id = create_speaker(&state, "Ada").await;

    let (status, _) = call(&state, enroll_request(&id, b"not audio at all".to_vec())).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(identifier.enrolled().is_empty(), "nothing reached the service");
}

#[tokio::test]
async fn enrolling_nobody_is_a_missing_speaker_rather_than_a_new_one() {
    // A typo in an id must not silently create a voice print under it, which
    // would be a print nothing can ever look up or delete.
    let identifier = RecordingIdentifier::new("voices");
    let state = state_with(identifier.clone());
    let absent = SpeakerId::new().to_string();

    let (status, _) = call(&state, enroll_request(&absent, wav(1.0, 16_000, 1))).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(identifier.enrolled().is_empty());
}

#[tokio::test]
async fn an_id_that_is_not_an_id_is_a_bad_request() {
    let state = state_with(RecordingIdentifier::new("voices"));
    let (status, _) = call(&state, get("/v1/speakers/kitchen")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_service_that_refuses_the_sample_does_not_leave_the_roster_claiming_otherwise() {
    // An entry that says "enrolled" when nothing was is the worst outcome
    // here: an operator stops recording, and the voice is never recognized.
    let state = state_with(RecordingIdentifier::refusing_enrollment(
        "voices",
        "the audio is too short",
    ));
    let id = create_speaker(&state, "Ada").await;

    let (status, body) = call(&state, enroll_request(&id, wav(0.1, 16_000, 1))).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body["detail"].as_str().unwrap_or_default().contains("too short"),
        "what the service said is what tells an operator what to do: {body}"
    );

    let (_, entry) = call(&state, get(&format!("/v1/speakers/{id}"))).await;
    assert_eq!(entry["samples"], 0, "still nobody the service has heard");
}

#[tokio::test]
async fn enrolling_with_no_identification_service_says_so() {
    let state = bare_state();
    let id = create_speaker(&state, "Ada").await;

    let (status, body) = call(&state, enroll_request(&id, wav(1.0, 16_000, 1))).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
}

#[tokio::test]
async fn renaming_keeps_the_enrollment_behind_the_name() {
    // A rename is a correction to a label, not a new person. Resetting the
    // samples would report a voice as un-enrolled while the service still
    // recognized it.
    let identifier = RecordingIdentifier::new("voices");
    let state = state_with(identifier);
    let id = create_speaker(&state, "Ada").await;
    call(&state, enroll_request(&id, wav(1.0, 16_000, 1))).await;

    let (status, body) = call(
        &state,
        json_request(
            "PUT",
            &format!("/v1/speakers/{id}"),
            serde_json::json!({ "name": "Ada Lovelace" }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "Ada Lovelace");
    assert_eq!(body["samples"], 1, "the same voice, differently labelled");
    assert_eq!(body["id"], id);
}

#[tokio::test]
async fn deleting_forgets_the_voice_print_before_the_name() {
    let identifier = RecordingIdentifier::new("voices");
    let state = state_with(identifier.clone());
    let id = create_speaker(&state, "Ada").await;
    call(&state, enroll_request(&id, wav(1.0, 16_000, 1))).await;

    let (status, _) = call(&state, delete(&format!("/v1/speakers/{id}"))).await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        identifier.forgotten().iter().map(ToString::to_string).collect::<Vec<_>>(),
        vec![id.clone()],
        "the service was told to forget the print"
    );
    let (status, _) = call(&state, get(&format!("/v1/speakers/{id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_service_that_will_not_forget_keeps_the_name_that_explains_the_print() {
    // Removing the roster entry anyway would leave a voice print that
    // identifies as an id nobody can look up — unnameable and undeletable. So
    // the name survives until the print is actually gone.
    let identifier = RecordingIdentifier::refusing_to_forget("voices", "the service is down");
    let state = state_with(identifier.clone());
    let id = create_speaker(&state, "Ada").await;
    call(&state, enroll_request(&id, wav(1.0, 16_000, 1))).await;

    let (status, body) = call(&state, delete(&format!("/v1/speakers/{id}"))).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body["detail"].as_str().unwrap_or_default().contains("still holds"),
        "the refusal says what is left behind: {body}"
    );
    assert!(identifier.forgotten().is_empty());

    let (status, entry) = call(&state, get(&format!("/v1/speakers/{id}"))).await;
    assert_eq!(status, StatusCode::OK, "the name is still there to explain the print");
    assert_eq!(entry["samples"], 1);
}

#[tokio::test]
async fn an_entry_nobody_enrolled_can_be_removed_without_a_service() {
    // A deployment that has not configured identification yet must still be
    // able to tidy a roster it typed a name into.
    let state = bare_state();
    let id = create_speaker(&state, "Ada").await;

    let (status, _) = call(&state, delete(&format!("/v1/speakers/{id}"))).await;

    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn the_roster_lists_everyone_who_was_created() {
    let state = state_with(RecordingIdentifier::new("voices"));
    create_speaker(&state, "Ada").await;
    create_speaker(&state, "Grace").await;

    let (status, body) = call(&state, get("/v1/speakers")).await;

    assert_eq!(status, StatusCode::OK);
    let mut names: Vec<_> = body
        .as_array()
        .expect("a list")
        .iter()
        .map(|entry| entry["name"].as_str().expect("a name").to_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["Ada", "Grace"]);
}
