//! What the service writes about a request that carried a credential.
//!
//! This lives in its own test binary, and installs its subscriber as the
//! *global* default rather than a thread-local one, because `tracing` caches
//! each callsite's interest globally the first time that callsite is hit. A
//! thread-local subscriber alongside sibling tests loses a race: whichever test
//! drives the trace layer first decides, for the whole process, that nobody is
//! listening — and the span this test needs to inspect is never recorded.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use conduit_api::auth::{Access, Tokens};
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Long enough to pass the entropy floor. The value is irrelevant; only that
/// it is a credential the service accepts, and so must never write down.
const DEVICE_TOKEN: &str = "logging-device-token-00000000000000000";
const MANAGEMENT_TOKEN: &str = "logging-management-token-000000000000";
const UNKNOWN_TOKEN: &str = "nobody-holds-this-token-000000000000";

/// State that authenticates against the tokens above.
fn guarded() -> AppState {
    let tokens = Tokens::parse(&format!(
        r#"{{
          "devices": [{{ "token": "{DEVICE_TOKEN}", "device": "kitchen" }}],
          "management": [{{ "token": "{MANAGEMENT_TOKEN}", "name": "ui" }}]
        }}"#
    ))
    .expect("the token file parses");
    AppState::new(EventBus::default()).with_access(Access::Tokens(tokens))
}

/// Sends a GET carrying `token` as a bearer credential, and drains the body so
/// the request is fully handled before the recording is read back.
async fn call(state: &AppState, uri: &str, token: &str) -> StatusCode {
    let request = Request::builder()
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request");
    let response = router(state.clone()).oneshot(request).await.expect("router responds");
    let status = response.status();
    response.into_body().collect().await.expect("body");
    status
}

/// Collects everything the tracing layer writes, so a test can read it back.
#[derive(Clone, Default)]
struct Recorder(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

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

#[tokio::test]
async fn a_token_never_reaches_a_log_line_or_a_span() {
    // Spans are exported to a collector and logs are shipped off the host, so a
    // token in either is a token in a system nobody thought was holding
    // credentials. This is why the request span is built with
    // `include_headers(false)`.
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

    let state = guarded();
    // Every outcome, because each one takes a different path through the code
    // that might mention what it was given: accepted, wrong audience, unknown.
    assert_eq!(call(&state, "/v1/pipelines", MANAGEMENT_TOKEN).await, StatusCode::OK);
    assert_eq!(call(&state, "/v1/events", DEVICE_TOKEN).await, StatusCode::FORBIDDEN);
    assert_eq!(call(&state, "/v1/pipelines", UNKNOWN_TOKEN).await, StatusCode::UNAUTHORIZED);

    let recorded = recorder.written();
    assert!(!recorded.is_empty(), "the request span must have been recorded at all");
    for token in [DEVICE_TOKEN, MANAGEMENT_TOKEN, UNKNOWN_TOKEN] {
        assert!(!recorded.contains(token), "a token was recorded: {recorded}");
    }
    assert!(
        !recorded.to_lowercase().contains("authorization"),
        "the header itself must not be recorded either: {recorded}"
    );
}
