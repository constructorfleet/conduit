//! A stand-in for the Messages API.
//!
//! Real enough to be worth testing against: it records the headers and body it
//! received, and can reply in packets that do not line up with event
//! boundaries — which is what the network actually does, and the reason event
//! reassembly exists.

// Shared by several test binaries, not all of which inspect every field.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use tokio::sync::Mutex;

/// What the server should reply with.
#[derive(Clone)]
enum Reply {
    /// A body delivered in the given packets.
    Chunks(Vec<String>),
    /// An error status and message.
    Status(u16, String),
    /// An error status, message, and `Retry-After` header value.
    StatusRetryAfter(u16, String, String),
}

/// What the server received.
#[derive(Default)]
struct Received {
    body: Option<serde_json::Value>,
    api_key: Option<String>,
    version: Option<String>,
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
    /// Serves `events` as one SSE response, each event sent whole.
    pub async fn start(events: &[&str]) -> Self {
        Self::spawn(Reply::Chunks(
            events.iter().map(|event| format!("data: {event}\n\n")).collect(),
        ))
        .await
    }

    /// Serves the response in arbitrary packets, to exercise reassembly.
    pub async fn start_chunked(packets: Vec<String>) -> Self {
        Self::spawn(Reply::Chunks(packets)).await
    }

    /// Rejects requests with `status`.
    pub async fn start_status(status: u16, message: &str) -> Self {
        Self::spawn(Reply::Status(status, message.to_owned())).await
    }

    /// Rejects requests with `status` and a `Retry-After` header.
    pub async fn start_retry_after(status: u16, message: &str, retry_after: &str) -> Self {
        Self::spawn(Reply::StatusRetryAfter(status, message.to_owned(), retry_after.to_owned()))
            .await
    }

    async fn spawn(reply: Reply) -> Self {
        let received = Arc::new(Mutex::new(Received::default()));
        let state = AppState { reply, received: Arc::clone(&received) };

        let app = Router::new()
            .route("/messages", post(messages))
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

    /// The `x-api-key` header of the most recent request.
    pub async fn last_api_key(&self) -> Option<String> {
        self.received.lock().await.api_key.clone()
    }

    /// The `anthropic-version` header of the most recent request.
    pub async fn last_version(&self) -> Option<String> {
        self.received.lock().await.version.clone()
    }

    /// The `Authorization` header of the most recent request, which this API
    /// does not use and so should never see.
    pub async fn last_authorization(&self) -> Option<String> {
        self.received.lock().await.authorization.clone()
    }
}

/// Records the request and replies as configured.
async fn messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, (StatusCode, String)> {
    {
        let mut received = state.received.lock().await;
        received.body = serde_json::from_str(&body).ok();
        received.api_key = header(&headers, "x-api-key");
        received.version = header(&headers, "anthropic-version");
        received.authorization = header(&headers, "authorization");
    }

    match state.reply {
        Reply::Chunks(chunks) => {
            let stream =
                futures_util::stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>));
            Ok(Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from_stream(stream))
                .expect("response"))
        }
        Reply::Status(status, message) => {
            Err((StatusCode::from_u16(status).expect("valid status"), message))
        }
        Reply::StatusRetryAfter(status, message, retry_after) => Ok(Response::builder()
            .status(StatusCode::from_u16(status).expect("valid status"))
            .header("retry-after", retry_after)
            .body(Body::from(message))
            .expect("response")),
    }
}

/// The route the health check reaches.
async fn models(State(state): State<AppState>) -> Result<Response, (StatusCode, String)> {
    match state.reply {
        Reply::Status(status, message) | Reply::StatusRetryAfter(status, message, _) => {
            Err((StatusCode::from_u16(status).expect("valid status"), message))
        }
        Reply::Chunks(_) => Ok(Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(r#"{"data":[]}"#))
            .expect("response")),
    }
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_owned)
}
