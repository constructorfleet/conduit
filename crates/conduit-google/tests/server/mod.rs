//! A stand-in for the Google Cloud speech APIs.
//!
//! Real enough to be worth testing against: it serves the same paths, records
//! the request bodies and the `Authorization` header, and can reply with
//! Google's own error envelope.

// Shared by several test binaries, not all of which inspect every field.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::sync::Mutex;

/// What the server should reply with.
#[derive(Clone)]
enum Reply {
    /// A JSON body, sent whole.
    Json(serde_json::Value),
    /// Google's error envelope with the given status and message.
    Error(u16, String),
    /// An error status, a message, and a `Retry-After` header value.
    RetryAfter(u16, String, String),
    /// A 200 whose body is not the documented shape.
    Malformed(String),
    /// The request is accepted and never answered.
    Stall,
}

/// What the server received.
#[derive(Default)]
struct Received {
    body: Option<serde_json::Value>,
    authorization: Option<String>,
    query: Option<String>,
    path: Option<String>,
    requests: usize,
}

#[derive(Clone)]
struct AppState {
    reply: Reply,
    received: Arc<Mutex<Received>>,
}

/// A running stand-in. Stops when dropped.
pub struct MockGoogle {
    address: SocketAddr,
    received: Arc<Mutex<Received>>,
}

impl MockGoogle {
    /// Answers every request with `body`.
    pub async fn json(body: serde_json::Value) -> Self {
        Self::spawn(Reply::Json(body)).await
    }

    /// Answers with base64 `audioContent` holding `samples` as a LINEAR16 WAV,
    /// which is the shape Google documents for that encoding.
    pub async fn synthesizing(
        format: conduit_core::audio::AudioFormat,
        samples: Vec<u8>,
    ) -> Self {
        use base64::Engine as _;
        let wav = conduit_core::wav::package(format, samples).expect("a wav").bytes;
        Self::json(serde_json::json!({
            "audioContent": base64::engine::general_purpose::STANDARD.encode(&wav),
        }))
        .await
    }

    /// Rejects requests with Google's error envelope.
    pub async fn error(status: u16, message: &str) -> Self {
        Self::spawn(Reply::Error(status, message.to_owned())).await
    }

    /// Rejects requests with a status and a `Retry-After` header.
    pub async fn retry_after(status: u16, message: &str, retry_after: &str) -> Self {
        Self::spawn(Reply::RetryAfter(status, message.to_owned(), retry_after.to_owned())).await
    }

    /// Answers 200 with a body that is not the documented shape.
    pub async fn malformed(body: &str) -> Self {
        Self::spawn(Reply::Malformed(body.to_owned())).await
    }

    /// Accepts the request and never answers it.
    pub async fn stalled() -> Self {
        Self::spawn(Reply::Stall).await
    }

    async fn spawn(reply: Reply) -> Self {
        let received = Arc::new(Mutex::new(Received::default()));
        let state = AppState { reply, received: Arc::clone(&received) };

        let app = Router::new()
            .route("/v1/text:synthesize", post(handle))
            .route("/v1/speech:recognize", post(handle))
            .route("/v1/voices", get(voices))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Self { address, received }
    }

    /// Base URL to point a provider at.
    pub fn url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    /// The body of the most recent request.
    pub async fn last_body(&self) -> Option<serde_json::Value> {
        self.received.lock().await.body.clone()
    }

    /// The `Authorization` header of the most recent request.
    pub async fn last_authorization(&self) -> Option<String> {
        self.received.lock().await.authorization.clone()
    }

    /// The raw query string of the most recent request.
    pub async fn last_query(&self) -> Option<String> {
        self.received.lock().await.query.clone()
    }

    /// The path of the most recent request.
    pub async fn last_path(&self) -> Option<String> {
        self.received.lock().await.path.clone()
    }

    /// How many requests have arrived.
    pub async fn request_count(&self) -> usize {
        self.received.lock().await.requests
    }
}

/// Records the request and replies as configured.
async fn handle(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    body: String,
) -> Response {
    {
        let mut received = state.received.lock().await;
        received.body = serde_json::from_str(&body).ok();
        received.path = Some(uri.path().to_owned());
        received.query = uri.query().map(str::to_owned);
        received.authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        received.requests += 1;
    }
    respond(&state.reply).await
}

/// The voice catalogue, or whatever failure was configured.
async fn voices(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    Query(_): Query<std::collections::HashMap<String, String>>,
) -> Response {
    {
        let mut received = state.received.lock().await;
        received.path = Some(uri.path().to_owned());
        received.query = uri.query().map(str::to_owned);
        received.authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        received.requests += 1;
    }
    match &state.reply {
        // A synthesis reply says nothing about voices, so the catalogue answers
        // with a plausible one — which is what makes a health check pass.
        Reply::Json(body) if body.get("voices").is_none() => Json(serde_json::json!({
            "voices": [{
                "name": "en-US-Neural2-F",
                "languageCodes": ["en-US"],
                "ssmlGender": "FEMALE",
                "naturalSampleRateHertz": 24_000,
            }],
        }))
        .into_response(),
        reply => respond(reply).await,
    }
}

use axum::response::IntoResponse as _;

async fn respond(reply: &Reply) -> Response {
    match reply {
        Reply::Json(body) => Json(body.clone()).into_response(),
        Reply::Error(status, message) => google_error(*status, message, None),
        Reply::RetryAfter(status, message, retry_after) => {
            google_error(*status, message, Some(retry_after))
        }
        Reply::Malformed(body) => Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(body.clone()))
            .expect("response"),
        // The handler is dropped when the test's server is dropped, so this
        // leaks nothing beyond the test.
        Reply::Stall => std::future::pending().await,
    }
}

/// Google's own error envelope, which is what a real rejection looks like.
fn google_error(status: u16, message: &str, retry_after: Option<&str>) -> Response {
    let body = serde_json::json!({
        "error": {
            "code": status,
            "message": message,
            "status": "INVALID_ARGUMENT",
        },
    });
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(status).expect("valid status"))
        .header("content-type", "application/json");
    if let Some(retry_after) = retry_after {
        builder = builder.header("retry-after", retry_after);
    }
    builder.body(Body::from(body.to_string())).expect("response")
}
