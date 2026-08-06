//! Rendering the Conduit part of a satellite's ESPHome configuration.
//!
//! Driven through the real router, because what matters is what a request gets
//! back: a fragment that includes cleanly into a hand-written board file, names
//! no credential, and refuses rather than renders when a phrase has no model.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_core::graph::{Edge, Modality, Node, PipelineGraph};
use conduit_core::testing::voice_graph;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The parameters a Satellite1 board declares, as the two YAMLs spell them.
const SAT1_PARAMS: &str = "microphone=sat1_mics&speaker=announcement_resampling_speaker\
                           &mute_switch=master_mute_switch&gain_factor=6&server=192.168.1.10:8080";

async fn call(state: &AppState, uri: &str) -> (StatusCode, String) {
    let request = Request::builder().uri(uri).body(Body::empty()).expect("request");
    let response = router(state.clone()).oneshot(request).await.expect("router responds");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8(bytes.to_vec()).expect("utf-8"))
}

/// A pipeline whose wake stage detects on the device, with `phrases` flashed.
fn on_device_wake(phrases: &[&str], models: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "satellite",
        "label": "Satellite",
        "variant": {
            "type": "wake",
            "variant": {
                "type": "microwakeword",
                "runtime": { "where": "device" },
                "phrases": phrases,
                "models": models,
            }
        }
    })
}

async fn store(state: &AppState, uri: &str, body: serde_json::Value) {
    let request = Request::builder()
        .method("PUT")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");
    let response = router(state.clone()).oneshot(request).await.expect("router responds");
    assert!(response.status().is_success(), "storing `{uri}` succeeds: {}", response.status());
}

/// A voice pipeline with a wake stage ahead of recognition.
fn waking_graph() -> PipelineGraph {
    PipelineGraph::new("kitchen")
        .with_node(Node::source("source", "websocket", Modality::Audio))
        .with_node(Node::wake_word("wake", "satellite"))
        .with_node(Node::stt("stt", "whisper"))
        .with_node(Node::core("core", "ollama"))
        .with_node(Node::tts("tts", "piper"))
        .with_node(Node::sink("sink", "websocket", Modality::Audio))
        .with_edge(Edge::new("source", "wake"))
        .with_edge(Edge::new("wake", "stt"))
        .with_edge(Edge::new("stt", "core"))
        .with_edge(Edge::new("core", "tts"))
        .with_edge(Edge::new("tts", "sink"))
}

/// State holding a waking pipeline whose detector flashes `phrases`.
async fn waking_state(phrases: &[&str], models: serde_json::Value) -> AppState {
    let state = AppState::new(EventBus::default());
    store(&state, "/v1/providers/satellite", on_device_wake(phrases, models)).await;
    state.put_pipeline("kitchen", waking_graph()).await.expect("stores");
    state
}

#[tokio::test]
async fn a_rendered_fragment_carries_both_blocks_and_no_credential() {
    let state = waking_state(&["hey jarvis"], serde_json::json!({})).await;

    let (status, yaml) =
        call(&state, &format!("/v1/devices/kitchen/firmware?pipeline=kitchen&{SAT1_PARAMS}"))
            .await;

    assert_eq!(status, StatusCode::OK, "{yaml}");
    assert!(yaml.contains("conduit_voice:"), "{yaml}");
    assert!(yaml.contains("micro_wake_word:"), "{yaml}");
    assert!(yaml.contains("- model: hey_jarvis"), "the phrase became a model: {yaml}");
    // The security property of the whole feature: a fragment is safe to commit.
    assert!(yaml.contains("token: !secret conduit_token"), "{yaml}");
    assert!(yaml.contains("debug_wake_event_url: !secret wake_debug_event_url"), "{yaml}");
}

#[tokio::test]
async fn a_rendered_fragment_is_yaml_not_json_wrapped_yaml() {
    // The artifact is a file an operator saves beside a board file. A YAML
    // document inside a JSON string is a worse version of that.
    let state = waking_state(&["stop"], serde_json::json!({})).await;

    let request = Request::builder()
        .uri(format!("/v1/devices/kitchen/firmware?pipeline=kitchen&{SAT1_PARAMS}"))
        .body(Body::empty())
        .expect("request");
    let response = router(state.clone()).oneshot(request).await.expect("router responds");

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.starts_with("application/yaml"), "content type was `{content_type}`");
}

#[tokio::test]
async fn a_phrase_with_no_known_model_is_refused_rather_than_omitted() {
    // The failure rendering exists to prevent: a device flashed without the
    // model for a phrase the server believes it detects.
    let state = waking_state(&["open the pod bay doors"], serde_json::json!({})).await;

    let (status, body) =
        call(&state, &format!("/v1/devices/kitchen/firmware?pipeline=kitchen&{SAT1_PARAMS}"))
            .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("open the pod bay doors"), "names the phrase: {body}");
}

#[tokio::test]
async fn an_explicit_model_url_reaches_the_rendered_fragment() {
    // The escape hatch, end to end: a phrase upstream does not ship, rendered
    // from the URL an operator stored on the definition.
    let url = "https://fph-firmware-assets.s3.us-east-1.amazonaws.com/wake-word/custom.json";
    let state = waking_state(&["custom"], serde_json::json!({ "custom": url })).await;

    let (status, yaml) =
        call(&state, &format!("/v1/devices/kitchen/firmware?pipeline=kitchen&{SAT1_PARAMS}"))
            .await;

    assert_eq!(status, StatusCode::OK, "{yaml}");
    assert!(yaml.contains(&format!("- model: {url}")), "{yaml}");
}

#[tokio::test]
async fn a_pipeline_that_does_not_wake_on_the_device_renders_no_wake_block() {
    // Not an error: the device converses, it just does not listen for a phrase
    // itself, so there is nothing to flash.
    let state = AppState::new(EventBus::default());
    let graph = voice_graph("kitchen")
        .source("websocket")
        .stt("whisper")
        .core("ollama")
        .tts("piper")
        .build();
    state.put_pipeline("kitchen", graph).await.expect("stores");

    let (status, yaml) =
        call(&state, &format!("/v1/devices/kitchen/firmware?pipeline=kitchen&{SAT1_PARAMS}"))
            .await;

    assert_eq!(status, StatusCode::OK, "{yaml}");
    assert!(yaml.contains("conduit_voice:"), "{yaml}");
    assert!(!yaml.contains("micro_wake_word:"), "nothing to flash, so no block: {yaml}");
}

#[tokio::test]
async fn a_pipeline_that_does_not_exist_is_a_404() {
    let state = AppState::new(EventBus::default());

    let (status, body) =
        call(&state, &format!("/v1/devices/kitchen/firmware?pipeline=missing&{SAT1_PARAMS}"))
            .await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_parameter_that_would_change_the_documents_shape_is_refused() {
    // Every rendered field is an injection site into a config format, so the
    // endpoint refuses before emitting rather than letting ESPHome discover it.
    let state = waking_state(&["stop"], serde_json::json!({})).await;

    let injected = "sat1_mics%0Atoken%3A%20stolen";
    let (status, body) = call(
        &state,
        &format!(
            "/v1/devices/kitchen/firmware?pipeline=kitchen&microphone={injected}\
             &speaker=spk&mute_switch=mute&gain_factor=6&server=192.168.1.10:8080"
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("microphone"), "names the field: {body}");
}

#[tokio::test]
async fn a_missing_board_id_is_refused_rather_than_defaulted() {
    // A default microphone ID would render a fragment that compiles against
    // some other board, which is worse than refusing.
    let state = waking_state(&["stop"], serde_json::json!({})).await;

    let (status, body) = call(&state, "/v1/devices/kitchen/firmware?pipeline=kitchen").await;

    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
        "a board ID has no sensible default: {status} {body}"
    );
}

#[tokio::test]
async fn rendering_twice_returns_the_same_bytes() {
    // A fragment that churned would make every re-render look like a change
    // worth flashing.
    let state = waking_state(&["hey jarvis", "okay nabu"], serde_json::json!({})).await;
    let uri = format!("/v1/devices/kitchen/firmware?pipeline=kitchen&{SAT1_PARAMS}");

    let (_, first) = call(&state, &uri).await;
    let (_, second) = call(&state, &uri).await;

    assert_eq!(first, second);
}
