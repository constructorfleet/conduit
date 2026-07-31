//! End-to-end checks of the pipeline endpoints against the real router.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_core::graph::{Edge, Node, NodeKind, PipelineGraph};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn valid_graph() -> PipelineGraph {
    PipelineGraph::new("kitchen")
        .with_node(Node::new("mic", NodeKind::Source, "websocket"))
        .with_node(Node::new("stt", NodeKind::Stt, "whisper"))
        .with_node(Node::new("llm", NodeKind::Llm, "ollama"))
        .with_node(Node::new("tts", NodeKind::Tts, "piper"))
        .with_edge(Edge::new("mic", "stt"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
}

async fn call(state: &AppState, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = router(state.clone()).oneshot(request).await.expect("router responds");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json body")
    };
    (status, json)
}

fn put(graph: &PipelineGraph) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri("/v1/pipelines/kitchen")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(graph).expect("serialize")))
        .expect("request")
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).expect("request")
}

#[tokio::test]
async fn health_reports_ok() {
    let state = AppState::new(EventBus::default());
    let (status, body) = call(&state, get("/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn storing_then_reading_a_pipeline_round_trips() {
    let state = AppState::new(EventBus::default());

    let (status, body) = call(&state, put(&valid_graph())).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["order"], serde_json::json!(["mic", "stt", "llm", "tts"]));

    let (status, body) = call(&state, get("/v1/pipelines/kitchen")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["graph"]["name"], "kitchen");

    let (status, body) = call(&state, get("/v1/pipelines")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!(["kitchen"]));
}

#[tokio::test]
async fn replacing_a_pipeline_returns_ok_not_created() {
    let state = AppState::new(EventBus::default());
    call(&state, put(&valid_graph())).await;

    let (status, _) = call(&state, put(&valid_graph())).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn invalid_graphs_are_rejected_and_not_stored() {
    let state = AppState::new(EventBus::default());
    let broken = valid_graph().with_edge(Edge::new("tts", "nowhere"));

    let (status, body) = call(&state, put(&broken)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "invalid");
    assert!(body["detail"].as_str().expect("detail").contains("nowhere"));

    let (status, _) = call(&state, get("/v1/pipelines/kitchen")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn validate_checks_without_storing() {
    let state = AppState::new(EventBus::default());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/pipelines/validate")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&valid_graph()).expect("serialize")))
        .expect("request");

    let (status, _) = call(&state, request).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(&state, get("/v1/pipelines")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));
}

#[tokio::test]
async fn deleting_a_missing_pipeline_is_a_404() {
    let state = AppState::new(EventBus::default());
    let request = Request::builder()
        .method("DELETE")
        .uri("/v1/pipelines/ghost")
        .body(Body::empty())
        .expect("request");

    let (status, body) = call(&state, request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn unknown_stage_filters_are_rejected() {
    let state = AppState::new(EventBus::default());
    let (status, body) = call(&state, get("/v1/events?stages=reasoning,telepathy")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["detail"].as_str().expect("detail").contains("telepathy"));
}
