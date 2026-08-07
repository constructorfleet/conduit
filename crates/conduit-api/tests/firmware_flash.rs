//! Handing a rendered fragment to an ESPHome dashboard.
//!
//! Track E of `docs/specs/0003-firmware-fragment-rendering.md`, implementing
//! [ADR-0019]. The properties under test are the ones that ADR names: only the
//! fragment is uploaded, a rejected scheme is refused before any request is
//! made, the configured credential reaches no response body, and an unreachable
//! instance leaves the download path working.
//!
//! [ADR-0019]: ../../../docs/adr/0019-flashing-through-an-esphome-instance-conduit-does-not-own.md

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use conduit_api::esphome::EsphomeDashboard;
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_core::graph::{Edge, Modality, Node, PipelineGraph};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The board ids a flash request carries, matching the sat1 board file.
const PARAMS: &str = "pipeline=kitchen&microphone=sat1_mics\
                      &speaker=announcement_resampling_speaker&mute_switch=master_mute_switch\
                      &gain_factor=6&server=192.168.1.10:8080";

/// What a stub dashboard was asked to write.
#[derive(Debug, Clone)]
struct Upload {
    /// The `configuration` query parameter — the file name.
    configuration: String,
    /// The body, which should be the fragment and nothing else.
    body: String,
    /// The `Authorization` header, if one was sent.
    authorization: Option<String>,
}

/// An ESPHome dashboard that records what it was handed.
#[derive(Clone)]
struct StubDashboard {
    uploads: Arc<Mutex<Vec<Upload>>>,
    url: String,
}

/// What the stub records into, and what it answers with.
#[derive(Clone)]
struct StubState {
    uploads: Arc<Mutex<Vec<Upload>>>,
    /// The status to answer, so a refusal can be tested too.
    status: StatusCode,
    /// The body to answer a refusal with.
    detail: &'static str,
}

impl StubDashboard {
    async fn accepting() -> Self {
        Self::answering(StatusCode::OK, "").await
    }

    async fn answering(status: StatusCode, detail: &'static str) -> Self {
        let uploads = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let state = StubState { uploads: Arc::clone(&uploads), status, detail };
        let app = Router::new().route("/edit", post(record)).with_state(state);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { uploads, url: format!("http://{address}") }
    }

    fn uploads(&self) -> Vec<Upload> {
        self.uploads.lock().expect("not poisoned").clone()
    }
}

async fn record(
    State(stub): State<StubState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> (StatusCode, &'static str) {
    stub.uploads.lock().expect("not poisoned").push(Upload {
        configuration: query.get("configuration").cloned().unwrap_or_default(),
        body,
        authorization: headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    });
    (stub.status, stub.detail)
}

/// A pipeline that wakes on the device, so a fragment has models to render.
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
                "phrases": ["okay_nabu", "stop"],
                "models": {},
            }
        }
    })
}

/// A server with a stored pipeline and detector, and `dashboard` if given.
async fn server(dashboard: Option<EsphomeDashboard>) -> AppState {
    let mut state = AppState::new(EventBus::default());
    if let Some(dashboard) = dashboard {
        state = state.with_esphome(dashboard);
    }

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

/// Posts a flash request for `device`, returning the status and body.
async fn flash(state: &AppState, device: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method("POST")
        .uri(format!("/v1/devices/{device}/firmware/flash?{PARAMS}"))
        .body(Body::empty())
        .expect("request");
    let response = router(state.clone()).oneshot(request).await.expect("router responds");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8(bytes.to_vec()).expect("utf-8"))
}

#[tokio::test]
async fn the_upload_is_the_fragment_and_nothing_else() {
    // ADR-0019: only the fragment is ever uploaded. The board file is placed on
    // the dashboard once by hand, so an upload carrying one would be Conduit
    // claiming to know what a board is made of.
    let dashboard = StubDashboard::accepting().await;
    let state =
        server(Some(EsphomeDashboard::new(&dashboard.url, None).expect("a dashboard"))).await;

    let (status, body) = flash(&state, "kitchen").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let uploads = dashboard.uploads();
    assert_eq!(uploads.len(), 1, "one file, once");
    let upload = &uploads[0];
    assert_eq!(upload.configuration, "conduit-kitchen.conduit.yaml");
    assert!(upload.body.contains("conduit_voice:"), "{}", upload.body);
    assert!(upload.body.contains("micro_wake_word:"), "{}", upload.body);
    // The board's own keys, which only a whole-document upload would carry.
    for board_only in ["esphome:", "esp32:", "wifi:", "i2s_audio:"] {
        assert!(
            !upload.body.contains(board_only),
            "the upload carries board configuration `{board_only}`:\n{}",
            upload.body
        );
    }
}

#[tokio::test]
async fn what_is_flashed_is_byte_identical_to_what_can_be_downloaded() {
    // Two render paths would be two chances to disagree about what a device is
    // configured to do, and the download is the fallback for this route — so a
    // fragment applied by hand after a failed flash has to be the same file.
    let dashboard = StubDashboard::accepting().await;
    let state =
        server(Some(EsphomeDashboard::new(&dashboard.url, None).expect("a dashboard"))).await;

    let (status, _) = flash(&state, "kitchen").await;
    assert_eq!(status, StatusCode::OK);

    let request = Request::builder()
        .uri(format!("/v1/devices/kitchen/firmware?{PARAMS}"))
        .body(Body::empty())
        .expect("request");
    let response = router(state.clone()).oneshot(request).await.expect("router responds");
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let downloaded = String::from_utf8(bytes.to_vec()).expect("utf-8");

    assert_eq!(dashboard.uploads()[0].body, downloaded);
}

#[tokio::test]
async fn the_configured_credential_reaches_the_dashboard_and_no_response() {
    // The credential is a secret Conduit holds. It goes in the request to the
    // instance that needs it and nowhere else — not into a response body, which
    // is the rule `auth.rs` already follows for device tokens.
    let dashboard = StubDashboard::accepting().await;
    let state = server(Some(
        EsphomeDashboard::new(&dashboard.url, Some("Bearer dashboard-secret".to_owned()))
            .expect("a dashboard"),
    ))
    .await;

    let (status, body) = flash(&state, "kitchen").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(
        dashboard.uploads()[0].authorization.as_deref(),
        Some("Bearer dashboard-secret"),
        "the dashboard needs it"
    );
    assert!(!body.contains("dashboard-secret"), "and the response does not: {body}");
    assert!(!dashboard.uploads()[0].body.contains("dashboard-secret"), "nor the fragment");
}

#[tokio::test]
async fn a_dashboard_that_refuses_says_what_it_said() {
    // The operator's next move is to apply the fragment by hand, so the
    // dashboard's own words are the useful part of the failure.
    let dashboard =
        StubDashboard::answering(StatusCode::BAD_REQUEST, "no such configuration").await;
    let state =
        server(Some(EsphomeDashboard::new(&dashboard.url, None).expect("a dashboard"))).await;

    let (status, body) = flash(&state, "kitchen").await;

    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.contains("no such configuration"), "{body}");
}

#[tokio::test]
async fn an_unreachable_dashboard_leaves_the_download_working() {
    // ADR-0019: a broken upload degrades to "here is your fragment, apply it
    // yourself" rather than to a dead page. The fallback is the point.
    let address = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        listener.local_addr().expect("address")
    };
    let state = server(Some(
        EsphomeDashboard::new(&format!("http://{address}"), None).expect("a dashboard"),
    ))
    .await;

    let (status, body) = flash(&state, "kitchen").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.contains("download"), "points at the fallback: {body}");

    let request = Request::builder()
        .uri(format!("/v1/devices/kitchen/firmware?{PARAMS}"))
        .body(Body::empty())
        .expect("request");
    let response = router(state).oneshot(request).await.expect("router responds");
    assert_eq!(response.status(), StatusCode::OK, "the download still works");
}

#[tokio::test]
async fn a_server_with_no_dashboard_says_so_rather_than_dialing_anything() {
    // No default and no localhost guess: dialing an address nobody named is how
    // a convenience becomes a scan.
    let state = server(None).await;

    let (status, body) = flash(&state, "kitchen").await;

    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(body.contains("CONDUIT_ESPHOME_URL"), "says how to fix it: {body}");
    assert!(body.contains("download"), "and what to do instead: {body}");
}

#[tokio::test]
async fn a_rejected_scheme_is_refused_before_any_request_is_made() {
    // The SSRF check, asserted where it happens: constructing the dashboard, at
    // startup, not on the request path. A `file://` base URL never becomes a
    // client at all.
    for base in ["file:///etc/passwd", "unix:///var/run/docker.sock"] {
        assert!(EsphomeDashboard::new(base, None).is_err(), "`{base}` must not be dialable");
    }
}

#[tokio::test]
async fn a_device_name_cannot_walk_out_of_the_config_directory() {
    // The device name becomes a file name on somebody else's filesystem, so a
    // name with a slash in it is a path traversal waiting to happen. Sent
    // percent-encoded so it arrives as one path segment and reaches the handler
    // decoded — routing would otherwise reject it before the interesting part.
    let dashboard = StubDashboard::accepting().await;
    let state =
        server(Some(EsphomeDashboard::new(&dashboard.url, None).expect("a dashboard"))).await;

    let (status, body) = flash(&state, "%2E%2E%2F%2E%2E%2Fetc%2Fpasswd").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let written = &dashboard.uploads()[0].configuration;
    assert!(!written.contains('/'), "no separator survives: {written}");
    assert!(!written.contains(".."), "and no parent reference: {written}");
    assert_eq!(written, "conduit-etc-passwd.conduit.yaml");
}
