//! Bounded HTTP adapter for a linked Dicta transform peer.

use std::time::Duration;

use conduit_core::Result;
use conduit_provider::transform::{TransformContext, UtteranceTransform};
use conduit_provider::{Capability, Descriptor, Provider};
use serde::{Deserialize, Serialize};

const TOTAL_BUDGET: Duration = Duration::from_millis(500);
const RETRY_DELAY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
/// An authenticated remote Dicta transform with bounded retries and passthrough fallback.
pub struct DictaTransform {
    descriptor: Descriptor,
    url: String,
    peer_id: String,
    token: String,
    client: reqwest::Client,
}

#[derive(Serialize)]
struct TransformRequest<'a> {
    segment: &'a str,
    context: TransformRequestContext,
}

type TransformRequestContext = TransformContext;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransformResponse {
    segment: String,
}

impl DictaTransform {
    /// Creates a transform bound to the linked peer and its advertised endpoint.
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        peer_id: impl Into<String>,
        url: impl Into<String>,
        token: impl Into<String>,
    ) -> Result<Self> {
        let id = id.into();
        let client = reqwest::Client::builder().build().map_err(|error| {
            conduit_core::Error::Config(format!("could not build Dicta HTTP client: {error}"))
        })?;
        Ok(Self {
            descriptor: Descriptor::new(id, Capability::Transform).with_label(label),
            url: url.into(),
            peer_id: peer_id.into(),
            token: token.into(),
            client,
        })
    }

    async fn call(
        &self,
        segment: &str,
        context: &TransformContext,
    ) -> std::result::Result<String, CallFailure> {
        let response = self
            .client
            .post(&self.url)
            .bearer_auth(&self.token)
            .json(&TransformRequest { segment, context: bounded_context(context) })
            .send()
            .await
            .map_err(|error| CallFailure { message: error.to_string(), retryable: true })?;
        if response.status().is_server_error() {
            return Err(CallFailure {
                message: format!("HTTP {}", response.status()),
                retryable: true,
            });
        }
        if !response.status().is_success() {
            return Err(CallFailure {
                message: format!("HTTP {}", response.status()),
                retryable: false,
            });
        }
        response.json::<TransformResponse>().await.map(|body| body.segment).map_err(|error| {
            CallFailure {
                message: format!("invalid transform response: {error}"),
                retryable: false,
            }
        })
    }
}

fn bounded_context(context: &TransformContext) -> TransformContext {
    TransformContext {
        speaker_id: context.speaker_id.clone(),
        session_id: context.session_id.clone(),
        turn_id: context.turn_id.clone(),
        prior_turns: context
            .prior_turns
            .iter()
            .rev()
            .take(4)
            .rev()
            .map(|turn| turn.chars().take(200).collect())
            .collect(),
    }
}

#[derive(Debug)]
struct CallFailure {
    message: String,
    retryable: bool,
}

impl Provider for DictaTransform {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
}

#[async_trait::async_trait]
impl UtteranceTransform for DictaTransform {
    async fn transform(&self, segment: &str) -> Result<String> {
        self.transform_with_context(segment, &TransformContext::default()).await
    }

    async fn transform_with_context(
        &self,
        segment: &str,
        context: &TransformContext,
    ) -> Result<String> {
        let call = async {
            match self.call(segment, context).await {
                Ok(transformed) => Ok(transformed),
                Err(first) if first.retryable => {
                    tokio::time::sleep(RETRY_DELAY).await;
                    self.call(segment, context).await.map_err(|second| {
                        format!("{}; retry failed: {}", first.message, second.message)
                    })
                }
                Err(error) => Err(error.message),
            }
        };
        match tokio::time::timeout(TOTAL_BUDGET, call).await {
            Ok(Ok(transformed)) => Ok(transformed),
            Ok(Err(error)) => {
                tracing::warn!(peer = %self.peer_id, capability = "dicta.transform", %error, "linked transform unavailable; passing segment through");
                Ok(segment.to_owned())
            }
            Err(_) => {
                tracing::warn!(peer = %self.peer_id, capability = "dicta.transform", "linked transform exceeded 500 ms; passing segment through");
                Ok(segment.to_owned())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
        Json, Router,
    };
    use serde_json::{json, Value};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    async fn retry_handler(
        State(attempts): State<Arc<AtomicUsize>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        assert_eq!(headers.get("authorization").unwrap(), "Bearer peer-secret");
        assert_eq!(body["context"], json!({}));
        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({})))
        } else {
            (StatusCode::OK, Json(json!({ "segment": "rewritten" })))
        }
    }

    #[tokio::test]
    async fn server_error_retries_once_with_peer_auth_then_returns_rewrite() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let app =
            Router::new().route("/transform", post(retry_handler)).with_state(attempts.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = DictaTransform::new(
            "dicta",
            "Dicta",
            "peer-1",
            format!("http://{address}/transform"),
            "peer-secret",
        )
        .unwrap();
        assert_eq!(provider.transform("original").await.unwrap(), "rewritten");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        server.abort();
    }

    async fn slow_handler() -> (StatusCode, Json<Value>) {
        tokio::time::sleep(Duration::from_secs(2)).await;
        (StatusCode::OK, Json(json!({ "segment": "too late" })))
    }

    #[tokio::test]
    async fn deadline_falls_back_to_original_segment() {
        let app = Router::new().route("/transform", post(slow_handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = DictaTransform::new(
            "dicta",
            "Dicta",
            "peer-1",
            format!("http://{address}/transform"),
            "peer-secret",
        )
        .unwrap();
        assert_eq!(provider.transform("original").await.unwrap(), "original");
        server.abort();
    }

    async fn unavailable_handler(State(attempts): State<Arc<AtomicUsize>>) -> StatusCode {
        attempts.fetch_add(1, Ordering::SeqCst);
        StatusCode::SERVICE_UNAVAILABLE
    }

    #[tokio::test]
    async fn exhausted_server_retry_falls_back_to_original_segment() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/transform", post(unavailable_handler))
            .with_state(attempts.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = DictaTransform::new(
            "dicta",
            "Dicta",
            "peer-1",
            format!("http://{address}/transform"),
            "peer-secret",
        )
        .unwrap();
        assert_eq!(provider.transform("original").await.unwrap(), "original");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        server.abort();
    }

    async fn client_error_handler(State(attempts): State<Arc<AtomicUsize>>) -> StatusCode {
        attempts.fetch_add(1, Ordering::SeqCst);
        StatusCode::BAD_REQUEST
    }

    #[tokio::test]
    async fn client_error_is_not_retried_and_passes_through() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/transform", post(client_error_handler))
            .with_state(attempts.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = DictaTransform::new(
            "dicta",
            "Dicta",
            "peer-1",
            format!("http://{address}/transform"),
            "peer-secret",
        )
        .unwrap();
        assert_eq!(provider.transform("original").await.unwrap(), "original");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        server.abort();
    }

    async fn context_handler(Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(body["context"]["speaker_id"], "speaker-1");
        assert_eq!(body["context"]["session_id"], "conversation-1");
        assert_eq!(body["context"]["turn_id"], "turn-1");
        assert_eq!(body["context"]["prior_turns"].as_array().unwrap().len(), 4);
        assert!(body["context"]["prior_turns"][0].as_str().unwrap().len() <= 200);
        Json(json!({ "segment": "context-aware" }))
    }

    #[tokio::test]
    async fn context_is_serialized_with_bounds() {
        let app = Router::new().route("/transform", post(context_handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = DictaTransform::new(
            "dicta",
            "Dicta",
            "peer-1",
            format!("http://{address}/transform"),
            "peer-secret",
        )
        .unwrap();
        let context = TransformContext {
            speaker_id: Some("speaker-1".into()),
            session_id: Some("conversation-1".into()),
            turn_id: Some("turn-1".into()),
            prior_turns: vec![
                "x".repeat(400),
                "two".into(),
                "three".into(),
                "four".into(),
                "five".into(),
            ],
        };
        assert_eq!(
            provider.transform_with_context("original", &context).await.unwrap(),
            "context-aware"
        );
        server.abort();
    }
}
