//! End-to-end checks of the pipeline endpoints against the real router.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_core::graph::{Edge, Node, NodeKind, PipelineGraph};
use conduit_core::Result;
use conduit_provider::storage::PipelineStore;
use conduit_provider::testing::{EchoLlm, EchoStt, EchoTts};
use conduit_runtime::Providers;
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

fn echo_graph() -> PipelineGraph {
    PipelineGraph::new("echo")
        .with_node(Node::new("stt", NodeKind::Stt, "echo-stt"))
        .with_node(Node::new("llm", NodeKind::Llm, "echo-llm"))
        .with_node(Node::new("tts", NodeKind::Tts, "echo-tts"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
}

fn providers() -> Providers {
    Providers::new().with_stt(EchoStt).with_llm(EchoLlm).with_tts(EchoTts)
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

async fn ops_call(state: &AppState, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response =
        conduit_api::ops_router(state.clone()).oneshot(request).await.expect("router responds");
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
        .uri(format!("/v1/pipelines/{}", graph.name))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(graph).expect("serialize")))
        .expect("request")
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).expect("request")
}

fn put_json(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request")
}

fn delete(uri: &str) -> Request<Body> {
    Request::builder().method("DELETE").uri(uri).body(Body::empty()).expect("request")
}

#[tokio::test]
async fn health_reports_ok() {
    // On the ops router, not this one: a liveness probe cannot present a
    // credential, so `/health` lives on the unauthenticated listener.
    let state = AppState::new(EventBus::default());
    let response =
        conduit_api::ops_router(state).oneshot(get("/health")).await.expect("router responds");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn readiness_reports_store_health() {
    let state = AppState::new(EventBus::default());

    let (status, body) = ops_call(&state, get("/ready")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ready");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn readiness_fails_when_the_store_cannot_be_read() {
    let state = AppState::with_store(EventBus::default(), std::sync::Arc::new(BrokenStore));

    let (status, body) = ops_call(&state, get("/ready")).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "unavailable");
    assert!(body["detail"].as_str().expect("detail").contains("pipeline store"));
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
async fn replacing_a_pipeline_refuses_node_configuration_fields() {
    let state = AppState::new(EventBus::default());
    let original = valid_graph();
    let (status, _) = call(&state, put(&original)).await;
    assert_eq!(status, StatusCode::CREATED);
    let mut invalid = serde_json::to_value(valid_graph()).expect("serialize");
    invalid["nodes"][1]["config"] = serde_json::json!({ "url": "tcp://whisper.local:10300" });
    let request = Request::builder()
        .method("PUT")
        .uri("/v1/pipelines/kitchen")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&invalid).expect("serialize")))
        .expect("request");

    let (status, body) = call(&state, request).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "bad_request");
    assert!(body["detail"].as_str().expect("detail").contains("config"));
    let (status, body) = call(&state, get("/v1/pipelines/kitchen")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["graph"]["nodes"][1]["provider"], original.nodes[1].provider);
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
async fn malformed_json_uses_the_api_error_shape() {
    let state = AppState::new(EventBus::default());
    let request = Request::builder()
        .method("PUT")
        .uri("/v1/pipelines/kitchen")
        .header("content-type", "application/json")
        .body(Body::from("{"))
        .expect("request");

    let (status, body) = call(&state, request).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "bad_request");
    assert!(body["detail"].as_str().is_some_and(|detail| detail.contains("JSON")));
}

#[tokio::test]
async fn pipeline_writes_have_a_request_body_limit() {
    let state = AppState::new(EventBus::default());
    let request = Request::builder()
        .method("PUT")
        .uri("/v1/pipelines/kitchen")
        .header("content-type", "application/json")
        .body(Body::from(vec![b' '; conduit_api::REQUEST_BODY_LIMIT_BYTES + 1]))
        .expect("request");

    let (status, body) = call(&state, request).await;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["error"], "payload_too_large");
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
async fn test_turn_runs_the_stored_pipeline_through_real_providers() {
    let state = AppState::new(EventBus::default()).with_providers(providers());
    let (status, _) = call(&state, put(&echo_graph())).await;
    assert_eq!(status, StatusCode::CREATED);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/pipelines/echo/test-turn")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"utterance":"hello conduit"}"#))
        .expect("request");

    let (status, body) = call(&state, request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pipeline"], "echo");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["reply_text"], "You said: hello conduit.");
    assert!(body["conversation"].as_str().is_some());
    assert!(body["audio_bytes"].as_u64().is_some_and(|bytes| bytes > 0));
}

#[tokio::test]
async fn test_turn_refuses_to_pretend_when_no_runtime_providers_are_configured() {
    let state = AppState::new(EventBus::default());
    let (status, _) = call(&state, put(&echo_graph())).await;
    assert_eq!(status, StatusCode::CREATED);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/pipelines/echo/test-turn")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .expect("request");

    let (status, body) = call(&state, request).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "invalid");
    assert!(body["detail"].as_str().expect("detail").contains("no providers"));
}

#[tokio::test]
async fn component_catalog_includes_openai_audio_and_mcp_tool_providers() {
    let state = AppState::new(EventBus::default());

    let (status, body) = call(&state, get("/v1/catalog/providers")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["components"][0]["id"], "openai.responses");
    assert_eq!(body["components"][0]["kind"], "llm");
    assert_eq!(body["components"][0]["definition_variant"], "openai_llm");
    assert_eq!(
        body["components"][0]["schema"]["properties"]["base_url"],
        serde_json::json!({ "type": "string", "format": "url" })
    );
    assert_eq!(
        body["components"][0]["schema"]["required"],
        serde_json::json!(["base_url", "model"])
    );
    assert_eq!(
        body["components"][1]["schema"]["properties"]["streaming"],
        serde_json::json!({ "type": "boolean" })
    );
    assert_eq!(body["components"][2]["id"], "wyoming");
    assert_eq!(
        body["components"][2]["schema"]["properties"]["url"],
        serde_json::json!({ "type": "string", "format": "url" })
    );
    let components = body["components"].as_array().expect("component list");
    assert_component(components, "openai.speech", "tts", &["base_url", "model"], &["model"]);
    assert_component(
        components,
        "wyoming.tts",
        "tts",
        &["url", "voice", "model", "mode", "streaming"],
        &["url"],
    );
    assert_component(
        components,
        "openai.transcription",
        "stt",
        &["base_url", "model", "stream"],
        &["model"],
    );
    assert_component(components, "mcp.sse", "tool", &["url"], &["url"]);
    assert_component(components, "mcp.streamable_http", "tool", &["url"], &["url"]);
    assert_component(components, "mcp.stdio", "tool", &["command"], &["command"]);
}

#[tokio::test]
async fn old_pipeline_component_catalog_route_is_gone() {
    let state = AppState::new(EventBus::default());

    let (status, _) = call(&state, get("/v1/pipeline-components")).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn provider_definition_crud_round_trips_typed_variants_and_redacts_secrets() {
    let state = AppState::new(EventBus::default());
    let definition = serde_json::json!({
        "id": "openai-primary",
        "label": "OpenAI Primary",
        "variant": {
            "type": "openai_llm",
            "base_url": "https://api.openai.com/v1",
            "api_key": { "type": "inline", "value": "sk-test" },
            "models": ["gpt-4.1"],
            "streaming": true,
            "system_prompt": "Be useful."
        }
    });

    let (status, body) =
        call(&state, put_json("/v1/providers/openai-primary", definition)).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["id"], "openai-primary");
    assert_eq!(body["kind"], "llm");
    assert_eq!(body["variant"]["type"], "openai_llm");
    assert_eq!(body["variant"]["api_key"], serde_json::json!({ "type": "redacted" }));

    let (status, body) = call(&state, get("/v1/providers/openai-primary")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["variant"]["api_key"], serde_json::json!({ "type": "redacted" }));

    let (status, body) = call(&state, get("/v1/providers")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!(["openai-primary"]));
}

#[tokio::test]
async fn saving_openai_provider_definition_rebuilds_the_runtime_registry_snapshot() {
    let state = AppState::new(EventBus::default());
    let definition = serde_json::json!({
        "id": "openai-primary",
        "label": "OpenAI Primary",
        "variant": {
            "type": "openai_llm",
            "base_url": "http://localhost:11434/v1",
            "models": ["llama3.1"]
        }
    });

    let (status, _) = call(&state, put_json("/v1/providers/openai-primary", definition)).await;

    assert_eq!(status, StatusCode::CREATED);
    let providers = state.providers().expect("snapshot");
    assert_eq!(providers.llm().names().collect::<Vec<_>>(), ["openai-primary"]);
}

#[tokio::test]
async fn redacted_provider_secret_update_keeps_the_existing_secret() {
    let state = AppState::new(EventBus::default());
    let original = serde_json::json!({
        "id": "openai-primary",
        "label": "OpenAI Primary",
        "variant": {
            "type": "openai_llm",
            "base_url": "https://api.openai.com/v1",
            "api_key": { "type": "inline", "value": "sk-test" },
            "models": ["gpt-4.1"]
        }
    });
    let updated = serde_json::json!({
        "id": "openai-primary",
        "label": "OpenAI Primary",
        "variant": {
            "type": "openai_llm",
            "base_url": "https://proxy.local/v1",
            "api_key": { "type": "redacted" },
            "models": ["gpt-4.1-mini"]
        }
    });
    call(&state, put_json("/v1/providers/openai-primary", original)).await;

    let (status, body) = call(&state, put_json("/v1/providers/openai-primary", updated)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["variant"]["base_url"], "https://proxy.local/v1");
    assert_eq!(body["variant"]["api_key"], serde_json::json!({ "type": "redacted" }));
}

#[tokio::test]
async fn provider_delete_is_refused_when_pipelines_still_reference_it() {
    let state = AppState::new(EventBus::default());
    let definition = serde_json::json!({
        "id": "openai-primary",
        "label": "OpenAI Primary",
        "variant": {
            "type": "openai_llm",
            "base_url": "https://api.openai.com/v1",
            "models": ["gpt-4.1"]
        }
    });
    call(&state, put_json("/v1/providers/openai-primary", definition)).await;
    let graph = PipelineGraph::new("kitchen").with_node(Node::new(
        "llm",
        NodeKind::Llm,
        "openai-primary",
    ));
    call(&state, put(&graph)).await;

    let (status, body) = call(&state, delete("/v1/providers/openai-primary")).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "conflict");
    assert_eq!(body["affected_pipelines"], serde_json::json!(["kitchen"]));
}

fn assert_component(
    components: &[serde_json::Value],
    id: &str,
    kind: &str,
    properties: &[&str],
    required: &[&str],
) {
    let component = components
        .iter()
        .find(|component| component["id"] == id)
        .unwrap_or_else(|| panic!("missing component {id}"));
    assert_eq!(component["kind"], kind);

    let actual_properties = component["schema"]["properties"].as_object().expect("properties");
    for property in properties {
        assert!(actual_properties.contains_key(*property), "{id} should accept {property}");
    }
    assert_eq!(component["schema"]["required"], serde_json::json!(required));
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

#[tokio::test]
async fn subscribing_to_a_stage_nothing_publishes_is_refused() {
    // `wake_word` is a real stage name that parses, and nothing emits it — so
    // this used to be a 200 followed by silence for as long as the client was
    // willing to wait, which is indistinguishable from a broken pipeline.
    let state = AppState::new(EventBus::default());
    let (status, body) = call(&state, get("/v1/events?stages=wake_word")).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let detail = body["detail"].as_str().expect("detail");
    assert!(detail.contains("wake_word"), "the message must name the stage: {detail}");
    assert!(
        detail.contains("reasoning") && detail.contains("capture"),
        "and say what does carry traffic, including the newly emitting capture: {detail}"
    );
}

#[tokio::test]
async fn one_silent_stage_refuses_the_whole_subscription() {
    // Dropping just the silent stage would hand back a stream that quietly
    // means something narrower than what was asked for.
    let state = AppState::new(EventBus::default());
    let (status, body) = call(&state, get("/v1/events?stages=reasoning,identity")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["detail"].as_str().expect("detail").contains("identity"));
}

#[tokio::test]
async fn the_capture_stage_can_now_be_subscribed_to() {
    // The other half of the fix: capture has an emitter, so asking for it must
    // succeed rather than be refused along with the genuinely silent stages.
    let state = AppState::new(EventBus::default());
    let response = router(state)
        .oneshot(get("/v1/events?stages=capture,conversation"))
        .await
        .expect("router responds");
    assert_eq!(response.status(), StatusCode::OK);
}

/// A directory that cleans itself up.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "conduit-api-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn a_stored_pipeline_survives_a_restart() {
    // The whole reason for a store: a fresh server on the same directory.
    let directory = TempDir::new();
    let store =
        std::sync::Arc::new(conduit_store::FileStore::open(&directory.0).await.expect("opens"));

    let before = AppState::with_store(EventBus::default(), store.clone());
    let (status, _) = call(&before, put(&valid_graph())).await;
    assert_eq!(status, StatusCode::CREATED);

    let after = AppState::with_store(EventBus::default(), store);
    let (status, body) = call(&after, get("/v1/pipelines/kitchen")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["graph"]["name"], "kitchen");
}

#[tokio::test]
async fn a_provider_definition_survives_a_restart() {
    let directory = TempDir::new();
    let provider_store =
        std::sync::Arc::new(conduit_store::FileStore::open(&directory.0).await.expect("opens"));
    let pipeline_store = std::sync::Arc::new(conduit_store::MemoryStore::new());
    let definition = serde_json::json!({
        "id": "openai-primary",
        "label": "OpenAI Primary",
        "variant": {
            "type": "openai_llm",
            "base_url": "http://localhost:11434/v1",
            "models": ["llama3.1"]
        }
    });

    let before = AppState::with_stores(
        EventBus::default(),
        pipeline_store.clone(),
        provider_store.clone(),
    );
    let (status, _) = call(&before, put_json("/v1/providers/openai-primary", definition)).await;
    assert_eq!(status, StatusCode::CREATED);

    let after = AppState::with_stores(EventBus::default(), pipeline_store, provider_store);
    after.reload_provider_definitions().await.expect("reloads runtime providers");
    let (status, body) = call(&after, get("/v1/providers/openai-primary")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "openai-primary");
    assert_eq!(body["kind"], "llm");
    assert_eq!(
        after.providers().expect("snapshot").llm().names().collect::<Vec<_>>(),
        ["openai-primary"]
    );
}

#[tokio::test]
async fn a_name_the_store_cannot_use_is_rejected() {
    let state = AppState::new(EventBus::default());
    let request = Request::builder()
        .method("PUT")
        .uri("/v1/pipelines/kitchen%20light")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&valid_graph()).expect("serialize")))
        .expect("request");

    let (status, body) = call(&state, request).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "invalid");
}

#[derive(Debug)]
struct BrokenStore;

#[async_trait::async_trait]
impl PipelineStore for BrokenStore {
    async fn list(&self) -> Result<Vec<String>> {
        Err(conduit_core::Error::Config("pipeline store is offline".to_owned()))
    }

    async fn get(&self, _name: &str) -> Result<Option<PipelineGraph>> {
        unreachable!("readiness only lists names")
    }

    async fn put(&self, _name: &str, _graph: PipelineGraph) -> Result<bool> {
        unreachable!("readiness only lists names")
    }

    async fn remove(&self, _name: &str) -> Result<bool> {
        unreachable!("readiness only lists names")
    }
}
