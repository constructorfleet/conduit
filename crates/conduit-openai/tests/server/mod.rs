//! A stand-in for an OpenAI-compatible server.
//!
//! Real enough to be worth testing against: it records what it received and
//! can reply in packets that do not line up with message boundaries.

// Shared by several test binaries, not all of which inspect every field.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::sync::Mutex;

/// What the server should reply with.
#[derive(Clone)]
enum Reply {
    /// One body, sent whole.
    Body(String),
    /// A body delivered in the given packets.
    Chunks(Vec<String>),
    /// An error status and message.
    Status(u16, String),
}

/// What the server received.
#[derive(Default)]
struct Received {
    body: Option<serde_json::Value>,
    raw: Option<Vec<u8>>,
    content_type: Option<String>,
    authorization: Option<String>,
}

#[derive(Clone)]
struct AppState {
    reply: Reply,
    received: Arc<Mutex<Received>>,
}

/// A running mock server. Stops when dropped.
pub struct MockServer {
    address: SocketAddr,
    received: Arc<Mutex<Received>>,
}

impl MockServer {
    /// Serves `body` as the streamed response.
    pub async fn start(body: String) -> Self {
        Self::spawn(Reply::Body(body)).await
    }

    /// Serves the response in packets, to exercise reassembly.
    pub async fn start_chunked(chunks: Vec<String>) -> Self {
        Self::spawn(Reply::Chunks(chunks)).await
    }

    /// Rejects requests with `status`.
    pub async fn start_status(status: u16, message: &str) -> Self {
        Self::spawn(Reply::Status(status, message.to_owned())).await
    }

    async fn spawn(reply: Reply) -> Self {
        let received = Arc::new(Mutex::new(Received::default()));
        let state = AppState { reply, received: Arc::clone(&received) };

        let app = Router::new()
            .route("/chat/completions", post(chat))
            .route("/audio/transcriptions", post(transcriptions))
            .route("/audio/speech", post(chat))
            .route("/models", get(models))
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
        format!("http://{}", self.address)
    }

    /// The body of the most recent request.
    pub async fn last_body(&self) -> Option<serde_json::Value> {
        self.received.lock().await.body.clone()
    }

    /// The raw body of the most recent upload.
    pub async fn last_raw(&self) -> Option<Vec<u8>> {
        self.received.lock().await.raw.clone()
    }

    /// The `Content-Type` of the most recent upload.
    pub async fn last_content_type(&self) -> Option<String> {
        self.received.lock().await.content_type.clone()
    }

    /// The `Authorization` header of the most recent request.
    pub async fn last_authorization(&self) -> Option<String> {
        self.received.lock().await.authorization.clone()
    }
}

/// Records the request and replies as configured.
async fn chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, (StatusCode, String)> {
    {
        let mut received = state.received.lock().await;
        received.body = serde_json::from_str(&body).ok();
        received.authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
    }

    match state.reply {
        Reply::Body(body) => Ok(sse_response(Body::from(body))),
        Reply::Chunks(chunks) => {
            let stream =
                futures_util::stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>));
            Ok(sse_response(Body::from_stream(stream)))
        }
        Reply::Status(status, message) => {
            Err((StatusCode::from_u16(status).expect("valid status"), message))
        }
    }
}

/// Records the uploaded body verbatim and replies as configured.
///
/// The body is multipart, so it is kept as raw text for tests to inspect
/// rather than parsed into JSON.
async fn transcriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, (StatusCode, String)> {
    {
        let mut received = state.received.lock().await;
        received.raw = Some(body.to_vec());
        received.content_type = headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        received.authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
    }

    match state.reply {
        Reply::Body(body) => Ok(Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("response")),
        Reply::Chunks(chunks) => {
            let stream =
                futures_util::stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>));
            Ok(Response::builder()
                .header("content-type", "application/octet-stream")
                .body(Body::from_stream(stream))
                .expect("response"))
        }
        Reply::Status(status, message) => {
            Err((StatusCode::from_u16(status).expect("valid status"), message))
        }
    }
}

/// Minimal model listing, enough for a health check.
async fn models() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "data": [{ "id": "gpt-test" }] }))
}

fn sse_response(body: Body) -> Response {
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(body)
        .expect("response")
}
