//! A stand-in for the ElevenLabs API.
//!
//! Real enough to be worth testing against. Two things it does that a simpler
//! mock would not, and that this crate's tests depend on:
//!
//! - It records the *path* of every request, because this vendor addresses a
//!   voice in the path and a test that could not see the path could not prove a
//!   traversal attempt never reached the wire.
//! - It can stop sending mid-body, and it reports whether the client hung up, so
//!   a dropped stream is observably a stopped synthesis rather than a wasted one.

// Shared by several test binaries, not all of which inspect every field.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
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
    /// An error status, message, and `Retry-After` header value.
    StatusRetryAfter(u16, String, String),
    /// The request is accepted and never answered: no status, no body.
    Stall,
    /// The given packets are sent and then the body never ends.
    StallAfter(Vec<String>),
    /// Packets are sent slowly and forever, so a client can hang up mid-stream.
    ///
    /// The counterpart to [`Reply::StallAfter`] for the barge-in case: a stalled
    /// body proves a *timeout*, whereas an endless one proves that dropping the
    /// stream is what stops the work.
    Endless,
}

/// What the server received.
#[derive(Default)]
struct Received {
    /// The path of the most recent request, verbatim, before any decoding.
    path: Option<String>,
    /// The full request URI, so a test can see the query string too.
    uri: Option<String>,
    body: Option<serde_json::Value>,
    raw: Option<Vec<u8>>,
    content_type: Option<String>,
    api_key: Option<String>,
    authorization: Option<String>,
    /// The `voice_id` as axum's router extracted it.
    voice_id: Option<String>,
    /// The `output_format` query parameter.
    output_format: Option<String>,
}

#[derive(Clone)]
struct AppState {
    reply: Reply,
    received: Arc<Mutex<Received>>,
    /// How many synthesis requests have arrived.
    synthesis_calls: Arc<AtomicUsize>,
    /// How many packets an endless body managed to send.
    packets_sent: Arc<AtomicUsize>,
    /// Whether an endless body ended because the client hung up.
    client_hung_up: Arc<AtomicBool>,
}

/// A running mock server. Stops when dropped.
pub struct MockServer {
    address: SocketAddr,
    received: Arc<Mutex<Received>>,
    synthesis_calls: Arc<AtomicUsize>,
    packets_sent: Arc<AtomicUsize>,
    client_hung_up: Arc<AtomicBool>,
}

impl MockServer {
    /// Serves `body` as the whole response.
    pub async fn start(body: &str) -> Self {
        Self::spawn(Reply::Body(body.to_owned())).await
    }

    /// Serves the response in packets, to exercise streaming.
    pub async fn start_chunked(chunks: &[&str]) -> Self {
        Self::spawn(Reply::Chunks(chunks.iter().map(|chunk| (*chunk).to_owned()).collect()))
            .await
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

    /// Accepts the request and never answers it.
    ///
    /// The TCP handshake completes, so `connect_timeout` cannot save a caller
    /// here — only a read timeout can.
    pub async fn start_stalled() -> Self {
        Self::spawn(Reply::Stall).await
    }

    /// Answers with `chunks` and then never ends the body.
    ///
    /// The shape of a provider that starts speaking and then goes quiet
    /// mid-sentence.
    pub async fn start_stalled_after(chunks: &[&str]) -> Self {
        Self::spawn(Reply::StallAfter(chunks.iter().map(|chunk| (*chunk).to_owned()).collect()))
            .await
    }

    /// Streams packets forever, so a client can hang up partway through.
    pub async fn start_endless() -> Self {
        Self::spawn(Reply::Endless).await
    }

    async fn spawn(reply: Reply) -> Self {
        let received = Arc::new(Mutex::new(Received::default()));
        let synthesis_calls = Arc::new(AtomicUsize::new(0));
        let packets_sent = Arc::new(AtomicUsize::new(0));
        let client_hung_up = Arc::new(AtomicBool::new(false));
        let state = AppState {
            reply,
            received: Arc::clone(&received),
            synthesis_calls: Arc::clone(&synthesis_calls),
            packets_sent: Arc::clone(&packets_sent),
            client_hung_up: Arc::clone(&client_hung_up),
        };

        let app = Router::new()
            .route("/v1/text-to-speech/{voice_id}/stream", post(synthesize))
            .route("/v1/speech-to-text", post(transcribe))
            .route("/v1/voices", get(voices))
            // A catch-all, so a request to a path this crate should never send
            // is recorded and answered rather than 404ing anonymously. That is
            // what lets a traversal test assert on where the request *went*.
            .fallback(elsewhere)
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Self { address, received, synthesis_calls, packets_sent, client_hung_up }
    }

    /// Base URL to point a provider at, including the version prefix.
    pub fn url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    /// The path of the most recent request.
    pub async fn last_path(&self) -> Option<String> {
        self.received.lock().await.path.clone()
    }

    /// The full URI of the most recent request, query string included.
    pub async fn last_uri(&self) -> Option<String> {
        self.received.lock().await.uri.clone()
    }

    /// The body of the most recent JSON request.
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

    /// The `xi-api-key` header of the most recent request.
    pub async fn last_api_key(&self) -> Option<String> {
        self.received.lock().await.api_key.clone()
    }

    /// The `Authorization` header of the most recent request.
    pub async fn last_authorization(&self) -> Option<String> {
        self.received.lock().await.authorization.clone()
    }

    /// The `voice_id` the router extracted from the most recent synthesis path.
    pub async fn last_voice_id(&self) -> Option<String> {
        self.received.lock().await.voice_id.clone()
    }

    /// The `output_format` query parameter of the most recent synthesis request.
    pub async fn last_output_format(&self) -> Option<String> {
        self.received.lock().await.output_format.clone()
    }

    /// How many synthesis requests have arrived.
    pub fn synthesis_calls(&self) -> usize {
        self.synthesis_calls.load(Ordering::SeqCst)
    }

    /// How many packets an endless body managed to send.
    pub fn packets_sent(&self) -> usize {
        self.packets_sent.load(Ordering::SeqCst)
    }

    /// Whether an endless body stopped because the client hung up.
    pub fn client_hung_up(&self) -> bool {
        self.client_hung_up.load(Ordering::SeqCst)
    }
}

/// Records a synthesis request and replies as configured.
async fn synthesize(
    State(state): State<AppState>,
    Path(voice_id): Path<String>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Result<Response, (StatusCode, String)> {
    state.synthesis_calls.fetch_add(1, Ordering::SeqCst);
    record(&state, &uri, &headers, |received| {
        received.body = serde_json::from_str(&body).ok();
        received.voice_id = Some(voice_id);
        received.output_format = query.get("output_format").cloned();
    })
    .await;

    // Raw audio, not server-sent events: this endpoint streams PCM with no
    // framing at all.
    respond(&state, "audio/mpeg").await
}

/// Records a transcription upload verbatim and replies as configured.
///
/// The body is multipart, so it is kept as raw bytes for tests to inspect rather
/// than parsed.
async fn transcribe(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, (StatusCode, String)> {
    record(&state, &uri, &headers, |received| {
        received.raw = Some(body.to_vec());
    })
    .await;

    respond(&state, "application/json").await
}

/// The voice catalogue, which is also what a health check reads.
///
/// A server that is refusing requests or has gone quiet does so here too — a
/// health check that answered while every real request stalled would report a
/// broken server as healthy.
async fn voices(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    record(&state, &uri, &headers, |_| {}).await;

    match &state.reply {
        Reply::Stall | Reply::Endless => Err((StatusCode::IM_A_TEAPOT, never().await)),
        Reply::Status(status, message) | Reply::StatusRetryAfter(status, message, _) => {
            Err((StatusCode::from_u16(*status).expect("valid status"), message.clone()))
        }
        // A catalogue rather than the configured reply: every test that needs a
        // *specific* catalogue asks for it with `start`, and the default keeps a
        // health check honest without every test having to supply voices.
        Reply::Body(body) if body.contains("voices") => Ok(json(body.clone())),
        _ => Ok(json(
            r#"{"voices":[{"voice_id":"21m00Tcm4TlvDq8ikWAM","name":"Rachel"}]}"#.to_owned(),
        )),
    }
}

/// Any path this crate should not be sending.
///
/// Answers 418 so a test can tell "the request went somewhere unexpected" from
/// "the request was never sent", which a 404 from an absent route cannot.
async fn elsewhere(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
) -> (StatusCode, String) {
    record(&state, &uri, &headers, |_| {}).await;
    (StatusCode::IM_A_TEAPOT, format!("nothing serves {uri}"))
}

/// Records the parts of a request every handler cares about.
async fn record(
    state: &AppState,
    uri: &Uri,
    headers: &HeaderMap,
    extra: impl FnOnce(&mut Received),
) {
    let mut received = state.received.lock().await;
    received.path = Some(uri.path().to_owned());
    received.uri = Some(uri.to_string());
    received.content_type = header(headers, "content-type");
    received.api_key = header(headers, "xi-api-key");
    received.authorization = header(headers, "authorization");
    extra(&mut received);
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_owned)
}

/// Builds the configured reply, with `content_type` on the success paths.
async fn respond(
    state: &AppState,
    content_type: &str,
) -> Result<Response, (StatusCode, String)> {
    match &state.reply {
        Reply::Body(body) => Ok(typed(content_type, Body::from(body.clone()))),
        Reply::Chunks(chunks) => {
            let stream = futures_util::stream::iter(
                chunks.clone().into_iter().map(Ok::<_, std::io::Error>),
            );
            Ok(typed(content_type, Body::from_stream(stream)))
        }
        Reply::Status(status, message) => {
            Err((StatusCode::from_u16(*status).expect("valid status"), message.clone()))
        }
        Reply::StatusRetryAfter(status, message, retry_after) => Ok(Response::builder()
            .status(StatusCode::from_u16(*status).expect("valid status"))
            .header("retry-after", retry_after)
            .body(Body::from(message.clone()))
            .expect("response")),
        Reply::Stall => Ok(never().await),
        Reply::StallAfter(chunks) => Ok(typed(content_type, unending(chunks.clone()))),
        Reply::Endless => Ok(typed(
            content_type,
            endless(Arc::clone(&state.packets_sent), Arc::clone(&state.client_hung_up)),
        )),
    }
}

/// Never returns, so the caller's request is accepted and left unanswered.
///
/// The handler is dropped when the test's server is dropped, so this leaks
/// nothing beyond the test.
async fn never<T>() -> T {
    std::future::pending().await
}

/// A body that delivers `chunks` and then stays open forever.
fn unending(chunks: Vec<String>) -> Body {
    use futures_util::StreamExt;

    let sent = futures_util::stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>));
    Body::from_stream(sent.chain(futures_util::stream::once(never())))
}

/// A body that keeps sending until the client stops listening.
///
/// The channel send fails once the client hangs up, which is how dropping a
/// synthesis stream becomes observable as *stopped synthesis* rather than just
/// an ignored one.
fn endless(packets_sent: Arc<AtomicUsize>, client_hung_up: Arc<AtomicBool>) -> Body {
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(1);
    tokio::spawn(async move {
        loop {
            if sender.send(Ok(b"audio-packet".to_vec())).await.is_err() {
                client_hung_up.store(true, Ordering::SeqCst);
                return;
            }
            packets_sent.fetch_add(1, Ordering::SeqCst);
            // Slow enough that a test can drop the stream partway through, fast
            // enough not to slow the suite down.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(receiver))
}

fn typed(content_type: &str, body: Body) -> Response {
    Response::builder().header("content-type", content_type).body(body).expect("response")
}

fn json(body: String) -> Response {
    typed("application/json", Body::from(body))
}
