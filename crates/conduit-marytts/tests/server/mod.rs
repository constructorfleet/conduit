//! A stand-in for a MaryTTS server.
//!
//! Real enough to be worth testing against: it serves the plain-text
//! catalogues on the paths MaryTTS serves them on, records the form parameters
//! it received so a test can assert what went on the wire, and can fail in the
//! ways a real server fails — including cutting a WAV body off partway through,
//! which is the case a single-chunk provider is most likely to get wrong.

// Shared by several test binaries, not all of which use every helper.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use conduit_core::audio::AudioFormat;
use futures_util::StreamExt;
use tokio::sync::Mutex;

/// How the server should answer `/process`.
#[derive(Clone)]
pub enum Reply {
    /// A whole WAV file, as a working server sends.
    Wav(Vec<u8>),
    /// A body that declares a length and then stops early, which is what a
    /// server killed mid-response looks like to a client.
    Truncated(Vec<u8>),
    /// A body that is not a WAV file at all.
    NotAudio(String),
    /// An error status and message.
    Status(u16, String),
}

/// What the server received.
#[derive(Default, Clone)]
pub struct Received {
    /// Form parameters of the last `/process` request.
    pub form: HashMap<String, String>,
    /// The raw query string of the last `/process` request, if any.
    pub query: Option<String>,
    /// The `Content-Type` the request carried.
    pub content_type: Option<String>,
    /// The method the request used.
    pub method: Option<String>,
}

#[derive(Clone)]
struct AppState {
    reply: Reply,
    voices: String,
    version: String,
    healthy: bool,
    received: Arc<Mutex<Received>>,
}

/// A running mock server. Stops when dropped.
pub struct MockServer {
    address: SocketAddr,
    received: Arc<Mutex<Received>>,
}

/// A WAV file of `frames` alternating samples in `format`.
///
/// Alternating rather than silent so a test can tell resampled audio from
/// copied audio by its length.
#[must_use]
pub fn wav_file(format: AudioFormat, frames: usize) -> Vec<u8> {
    let samples: Vec<u8> = (0..frames)
        .flat_map(|index| {
            let sample = if index % 2 == 0 { 8_000_i16 } else { -8_000_i16 };
            sample.to_le_bytes()
        })
        .collect();
    conduit_core::wav::package(format, samples).expect("packages").bytes
}

/// The `/voices` body a stock MaryTTS install answers with.
pub const VOICES: &str = "cmu-slt-hsmm en_US female hmm\n\
     dfki-pavoque-neutral de male unitselection general\n";

impl MockServer {
    /// Serves `body` as a complete WAV response.
    pub async fn start(body: Vec<u8>) -> Self {
        Self::spawn(Reply::Wav(body), true).await
    }

    /// Serves a WAV response that stops partway through.
    pub async fn start_truncated(body: Vec<u8>) -> Self {
        Self::spawn(Reply::Truncated(body), true).await
    }

    /// Answers `/process` with something that is not audio.
    pub async fn start_not_audio(body: &str) -> Self {
        Self::spawn(Reply::NotAudio(body.to_owned()), true).await
    }

    /// Rejects `/process` with `status`.
    pub async fn start_status(status: u16, message: &str) -> Self {
        Self::spawn(Reply::Status(status, message.to_owned()), true).await
    }

    /// A server whose informational endpoints all fail, as a server that is up
    /// but broken does.
    pub async fn start_unhealthy() -> Self {
        Self::spawn(Reply::Status(503, "no voices loaded".to_owned()), false).await
    }

    async fn spawn(reply: Reply, healthy: bool) -> Self {
        let received = Arc::new(Mutex::new(Received::default()));
        let state = AppState {
            reply,
            voices: VOICES.to_owned(),
            version: "Mary TTS server 5.2 (impl. 5.2)".to_owned(),
            healthy,
            received: Arc::clone(&received),
        };

        let app = Router::new()
            .route("/process", post(process).get(process))
            .route("/voices", get(voices))
            .route("/locales", get(locales))
            .route("/version", get(version))
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

    /// What the last request carried.
    pub async fn received(&self) -> Received {
        self.received.lock().await.clone()
    }
}

/// Records the request and answers as configured.
async fn process(
    State(state): State<AppState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: String,
) -> Result<Response, (StatusCode, String)> {
    {
        let mut received = state.received.lock().await;
        received.method = Some(method.to_string());
        received.query = uri.query().map(str::to_owned);
        received.content_type = headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        // MaryTTS parses a POST entity as URL-encoded parameters, so the test
        // server reads them the same way the real one does.
        received.form = serde_urlencoded::from_str(&body).unwrap_or_default();
    }

    match state.reply {
        Reply::Wav(body) => Ok(audio(Body::from(body))),
        // Some of the audio, and then the body fails rather than ending. This
        // is what a server killed mid-response looks like to a client: the
        // status and headers already arrived, so `synthesize` has returned and
        // only the stream is left to carry the failure.
        Reply::Truncated(body) => {
            let sent = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(body)]);
            let cut = futures_util::stream::once(async {
                // The headers and the first packet must reach the client before
                // the connection breaks, or the failure lands on `send` instead
                // of on the body and the test proves the wrong thing.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "the server stopped sending",
                ))
            });
            Ok(audio(Body::from_stream(sent.chain(cut))))
        }
        Reply::NotAudio(body) => Ok(Response::builder()
            .header("content-type", "text/html")
            .body(Body::from(body))
            .expect("response")),
        Reply::Status(status, message) => {
            Err((StatusCode::from_u16(status).expect("valid status"), message))
        }
    }
}

fn audio(body: Body) -> Response {
    Response::builder().header("content-type", "audio/wav").body(body).expect("response")
}

/// The catalogue, as `text/plain` one voice per line.
async fn voices(State(state): State<AppState>) -> Result<String, (StatusCode, String)> {
    if state.healthy {
        return Ok(state.voices);
    }
    Err((StatusCode::SERVICE_UNAVAILABLE, "no voices loaded".to_owned()))
}

async fn locales(State(state): State<AppState>) -> Result<String, (StatusCode, String)> {
    if state.healthy {
        return Ok("en_US\nde\n".to_owned());
    }
    Err((StatusCode::SERVICE_UNAVAILABLE, "not ready".to_owned()))
}

/// The version string, which is what a health check reads.
async fn version(State(state): State<AppState>) -> Result<String, (StatusCode, String)> {
    if state.healthy {
        return Ok(state.version);
    }
    Err((StatusCode::SERVICE_UNAVAILABLE, "starting up".to_owned()))
}
