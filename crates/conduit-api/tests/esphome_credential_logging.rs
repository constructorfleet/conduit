//! What the service writes about a hand-off that carried a dashboard credential.
//!
//! ADR-0019 and track E of `docs/specs/0003-firmware-fragment-rendering.md` both
//! say the ESPHome dashboard credential appears in no response body and no log
//! line. `firmware_flash.rs` covers the response body, and `esphome.rs` proves
//! the hand-written `Debug` redacts. Neither reads what was actually written to a
//! subscriber, which is the assertion that would catch a `tracing` field nobody
//! meant to add — so this is the credential half of what `token_logging.rs` does
//! for device tokens.
//!
//! Its own test binary, and a *global* subscriber rather than a thread-local
//! one, for the reason `token_logging.rs` records: `tracing` caches each
//! callsite's interest globally the first time it is hit, so a thread-local
//! subscriber alongside sibling tests loses a race and the events this needs are
//! never recorded.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use conduit_api::esphome::EsphomeDashboard;
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_core::graph::{Edge, Modality, Node, PipelineGraph};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The secret under test. Distinctive so a substring search cannot pass by
/// accident, and long enough that nothing truncates it into a false negative.
const CREDENTIAL: &str = "Bearer esphome-dashboard-credential-must-not-be-logged";

/// The board ids a flash request carries.
const PARAMS: &str = "pipeline=kitchen&microphone=sat1_mics\
                      &speaker=announcement_resampling_speaker&mute_switch=master_mute_switch\
                      &gain_factor=6&server=192.168.1.10:8080";

/// Collects everything the tracing layer writes, so a test can read it back.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<u8>>>);

impl Recorder {
    fn written(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("not poisoned")).into_owned()
    }
}

impl std::io::Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("not poisoned").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Recorder {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// A dashboard that answers `status`, so both the accepted and refused paths can
/// be driven. A refusal matters here: that path formats the dashboard's own words
/// into an error, which is the most likely place for a credential to be echoed.
async fn stub(status: StatusCode) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let app = Router::new()
        .route(
            "/edit",
            post(|State(status): State<StatusCode>, body: String| async move {
                // Echoes the body back on a refusal, the way a dashboard
                // complaining about a configuration would quote it.
                (status, body)
            }),
        )
        .with_state(status);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

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

fn wake_definition() -> serde_json::Value {
    serde_json::json!({
        "id": "satellite",
        "label": "Satellite",
        "variant": {
            "type": "wake",
            "variant": {
                "type": "microwakeword",
                "runtime": { "where": "device" },
                "phrases": ["okay_nabu"],
                "models": {},
            }
        }
    })
}

/// A server holding `dashboard`, with a pipeline that wakes on the device.
async fn server(dashboard: EsphomeDashboard) -> AppState {
    let state = AppState::new(EventBus::default()).with_esphome(dashboard);

    let stored = Request::builder()
        .method("PUT")
        .uri("/v1/providers/satellite")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&wake_definition()).expect("serialize")))
        .expect("request");
    let response = router(state.clone()).oneshot(stored).await.expect("router responds");
    assert!(response.status().is_success(), "storing the definition");

    state.put_pipeline("kitchen", waking_graph()).await.expect("stores the pipeline");
    state
}

/// Flashes `device`, draining the body so the request is fully handled before
/// the recording is read back.
async fn flash(state: &AppState, device: &str) -> StatusCode {
    let request = Request::builder()
        .method("POST")
        .uri(format!("/v1/devices/{device}/firmware/flash?{PARAMS}"))
        .body(Body::empty())
        .expect("request");
    let response = router(state.clone()).oneshot(request).await.expect("router responds");
    let status = response.status();
    response.into_body().collect().await.expect("body");
    status
}

#[tokio::test]
async fn a_dashboard_credential_never_reaches_a_log_line_or_a_span() {
    // Logs are shipped off the host and spans are exported to a collector, so a
    // dashboard credential in either is a credential in a system nobody thought
    // was holding one. `EsphomeDashboard` is reachable from `AppState`, which
    // derives `Debug`, which is exactly how one gets there by accident.
    use tracing_subscriber::fmt::format::FmtSpan;
    use tracing_subscriber::prelude::*;

    let recorder = Recorder::default();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(recorder.clone())
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                .with_filter(tracing_subscriber::EnvFilter::new("trace")),
        )
        .init();

    // Both outcomes, because they take different paths through the code that
    // might mention what it was given: an accepted upload, and a refusal whose
    // handler formats the dashboard's own reply into an error message.
    let accepted =
        EsphomeDashboard::new(&stub(StatusCode::OK).await, Some(CREDENTIAL.to_owned()))
            .expect("a dashboard");
    assert_eq!(flash(&server(accepted).await, "kitchen").await, StatusCode::OK);

    let refusing = EsphomeDashboard::new(
        &stub(StatusCode::BAD_REQUEST).await,
        Some(CREDENTIAL.to_owned()),
    )
    .expect("a dashboard");
    assert_eq!(flash(&server(refusing).await, "kitchen").await, StatusCode::BAD_GATEWAY);

    // And the path most likely to print the whole struct: something that formats
    // the dashboard itself, which is what a stray `tracing::debug!` would do.
    let dashboard = EsphomeDashboard::new("http://homelab:6052", Some(CREDENTIAL.to_owned()))
        .expect("a dashboard");
    tracing::debug!(?dashboard, "the dashboard as a field, the way a debug line would take it");

    let recorded = recorder.written();
    assert!(!recorded.is_empty(), "something must have been recorded at all");
    assert!(
        !recorded.contains(CREDENTIAL),
        "the dashboard credential was recorded: {recorded}"
    );
    // The distinctive tail on its own, in case a formatter split the header
    // scheme from its value and a whole-string search missed it.
    assert!(
        !recorded.contains("must-not-be-logged"),
        "part of the dashboard credential was recorded: {recorded}"
    );
    assert!(
        recorded.contains("redacted"),
        "the redaction is what makes the absence deliberate rather than lucky: {recorded}"
    );
}
