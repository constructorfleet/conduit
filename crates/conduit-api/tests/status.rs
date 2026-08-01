//! End-to-end checks of the operator runtime status endpoint.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use conduit_api::auth::{Access, Tokens};
use conduit_api::status::StatusCollector;
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_core::event::{CancelReason, Envelope, Event, FinishReason};
use conduit_core::graph::{Edge, Node, NodeKind, PipelineGraph};
use conduit_core::id::{ConversationId, DeviceId, TraceId, TurnId};
use conduit_provider::llm::{Completion, CompletionRequest, LanguageModel};
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions, Transcript};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{ChunkStream, Health, Provider};
use conduit_runtime::Providers;
use futures_util::{stream, SinkExt, StreamExt};
use http_body_util::BodyExt;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

const DEVICE_TOKEN: &str = "device-token-000000000000000000000000";
const MANAGEMENT_TOKEN: &str = "management-token-00000000000000000000";

fn token_file() -> String {
    format!(
        r#"{{
          "devices": [
            {{ "token": "{DEVICE_TOKEN}", "device": "kitchen" }}
          ],
          "management": [
            {{ "token": "{MANAGEMENT_TOKEN}", "name": "ui" }}
          ]
        }}"#
    )
}

fn guarded() -> AppState {
    let tokens = Tokens::parse(&token_file()).expect("the token file parses");
    with_status(AppState::new(EventBus::default()).with_access(Access::Tokens(tokens)))
}

fn open() -> AppState {
    with_status(AppState::new(EventBus::default()).with_access(Access::anonymous()))
}

fn with_status(state: AppState) -> AppState {
    StatusCollector::spawn(state.status(), &state.bus);
    state
}

fn with_providers(state: AppState) -> AppState {
    state.with_providers(
        Providers::new()
            .with_stt(conduit_provider::testing::EchoStt)
            .with_llm(conduit_provider::testing::EchoLlm)
            .with_tts(conduit_provider::testing::EchoTts),
    )
}

async fn server(state: AppState) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let app = router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    address
}

fn ws_request(
    address: std::net::SocketAddr,
    path: &str,
    token: &str,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let mut request =
        format!("ws://{address}{path}").into_client_request().expect("a websocket request");
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().expect("a header value"));
    request
}

fn with_status_providers(state: AppState, health: Health) -> AppState {
    state.with_providers(
        Providers::new()
            .with_stt(StatusStt::new("configured-stt", health.clone()))
            .with_llm(StatusLlm::new("configured-llm", health.clone()))
            .with_tts(StatusTts::new("configured-tts", health)),
    )
}

#[derive(Debug, Clone)]
struct StatusStt {
    name: &'static str,
    health: Health,
}

impl StatusStt {
    fn new(name: &'static str, health: Health) -> Self {
        Self { name, health }
    }
}

#[async_trait::async_trait]
impl Provider for StatusStt {
    fn name(&self) -> &str {
        self.name
    }

    async fn health(&self) -> Health {
        self.health.clone()
    }
}

#[async_trait::async_trait]
impl SpeechToText for StatusStt {
    async fn transcribe(
        &self,
        _audio: ChunkStream<AudioChunk>,
        _options: TranscribeOptions,
    ) -> conduit_core::Result<ChunkStream<Transcript>> {
        Ok(Box::pin(stream::empty()))
    }
}

#[derive(Debug, Clone)]
struct StatusLlm {
    name: &'static str,
    health: Health,
}

impl StatusLlm {
    fn new(name: &'static str, health: Health) -> Self {
        Self { name, health }
    }
}

#[async_trait::async_trait]
impl Provider for StatusLlm {
    fn name(&self) -> &str {
        self.name
    }

    async fn health(&self) -> Health {
        self.health.clone()
    }
}

#[async_trait::async_trait]
impl LanguageModel for StatusLlm {
    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> conduit_core::Result<ChunkStream<Completion>> {
        Ok(Box::pin(stream::empty()))
    }
}

#[derive(Debug, Clone)]
struct StatusTts {
    name: &'static str,
    health: Health,
}

impl StatusTts {
    fn new(name: &'static str, health: Health) -> Self {
        Self { name, health }
    }
}

#[async_trait::async_trait]
impl Provider for StatusTts {
    fn name(&self) -> &str {
        self.name
    }

    async fn health(&self) -> Health {
        self.health.clone()
    }
}

#[async_trait::async_trait]
impl TextToSpeech for StatusTts {
    async fn synthesize(
        &self,
        _request: SynthesisRequest,
    ) -> conduit_core::Result<ChunkStream<SpeechChunk>> {
        Ok(Box::pin(stream::empty()))
    }

    async fn voices(&self) -> conduit_core::Result<Vec<Voice>> {
        Ok(Vec::new())
    }
}

fn valid_graph() -> PipelineGraph {
    PipelineGraph::new("kitchen")
        .with_node(Node::new("mic", NodeKind::Source, "websocket"))
        .with_node(Node::new("stt", NodeKind::Stt, "echo-stt"))
        .with_node(
            Node::new("llm", NodeKind::Llm, "echo-llm")
                .with_config(serde_json::json!({ "model": "echo" })),
        )
        .with_node(Node::new("tts", NodeKind::Tts, "echo-tts"))
        .with_edge(Edge::new("mic", "stt"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
}

fn provider_status_graph() -> PipelineGraph {
    PipelineGraph::new("kitchen")
        .with_node(Node::new("mic", NodeKind::Source, "websocket"))
        .with_node(Node::new("stt", NodeKind::Stt, "configured-stt"))
        .with_node(
            Node::new("llm", NodeKind::Llm, "configured-llm")
                .with_config(serde_json::json!({ "model": "echo" })),
        )
        .with_node(Node::new("tts", NodeKind::Tts, "configured-tts"))
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

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).expect("request")
}

fn bearer(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request")
}

fn put(graph: &PipelineGraph) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri("/v1/pipelines/kitchen")
        .header("authorization", format!("Bearer {MANAGEMENT_TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(graph).expect("serialize")))
        .expect("request")
}

async fn wait_for_status(
    state: &AppState,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let started = tokio::time::Instant::now();
    loop {
        let (status, body) = call(state, bearer("/v1/status", MANAGEMENT_TOKEN)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if predicate(&body) {
            return body;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "status never reached expected state: {body}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_http_status(
    address: std::net::SocketAddr,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let client = reqwest::Client::new();
    let started = tokio::time::Instant::now();
    loop {
        let body: serde_json::Value = client
            .get(format!("http://{address}/v1/status"))
            .bearer_auth(MANAGEMENT_TOKEN)
            .send()
            .await
            .expect("status request")
            .json()
            .await
            .expect("json status");
        if predicate(&body) {
            return body;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "status never reached expected state: {body}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn publish(state: &AppState, pipeline: &str, conversation: ConversationId, event: Event) {
    state.bus.publish(
        Envelope::new(TraceId::new(), event)
            .with_conversation(conversation)
            .with_pipeline(pipeline),
    );
}

#[tokio::test]
async fn status_requires_management_access() {
    let state = guarded();

    let (status, _) = call(&state, get("/v1/status")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = call(&state, bearer("/v1/status", DEVICE_TOKEN)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = call(&state, bearer("/v1/status", MANAGEMENT_TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = call(&open(), get("/v1/status")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn status_opens_first_run_setup_when_no_usable_pipeline_exists() {
    let state = guarded();

    let (status, body) = call(&state, bearer("/v1/status", MANAGEMENT_TOKEN)).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["runtime"]["launch_state"], "first_run_setup");
    assert_eq!(body["runtime"]["stale_state"], "fresh");
    assert_eq!(body["pipelines"], serde_json::json!([]));
    assert_eq!(body["event_stream"]["route"], "/v1/events");
    assert_eq!(body["event_stream"]["refresh_snapshot_after_reconnect"], true);
}

#[tokio::test]
async fn usable_pipeline_without_real_turns_is_unproven() {
    let state = with_providers(guarded());
    let (status, body) = call(&state, put(&valid_graph())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = call(&state, bearer("/v1/status", MANAGEMENT_TOKEN)).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["runtime"]["launch_state"], "operations_workspace");
    assert_eq!(body["pipelines"][0]["name"], "kitchen");
    assert_eq!(body["pipelines"][0]["usable"], true);
    assert_eq!(body["pipelines"][0]["health"]["state"], "unproven");
    assert_eq!(body["pipelines"][0]["components"][0]["state"], "unproven");
}

#[tokio::test]
async fn status_reports_unavailable_provider_slots_without_a_runtime_registry() {
    let state = guarded();

    let (status, body) = call(&state, bearer("/v1/status", MANAGEMENT_TOKEN)).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["providers"],
        serde_json::json!([
            {
                "id": "llm",
                "kind": "llm",
                "state": "unavailable",
                "configured": false,
                "reachable": false,
                "proven_by_turn": null,
                "message": "no language-model provider is registered",
                "affects_pipelines": []
            },
            {
                "id": "stt",
                "kind": "stt",
                "state": "unavailable",
                "configured": false,
                "reachable": false,
                "proven_by_turn": null,
                "message": "no speech-to-text provider is registered",
                "affects_pipelines": []
            },
            {
                "id": "tts",
                "kind": "tts",
                "state": "unavailable",
                "configured": false,
                "reachable": false,
                "proven_by_turn": null,
                "message": "no text-to-speech provider is registered",
                "affects_pipelines": []
            }
        ])
    );
}

#[tokio::test]
async fn configured_provider_does_not_become_reachable_from_saved_settings() {
    let state = with_status_providers(
        guarded(),
        Health::Unhealthy { reason: "upstream refused credentials".to_owned() },
    );
    let (status, body) = call(&state, put(&provider_status_graph())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = call(&state, bearer("/v1/status", MANAGEMENT_TOKEN)).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let providers = body["providers"].as_array().expect("providers");
    let llm = providers.iter().find(|provider| provider["id"] == "configured-llm").unwrap();
    assert_eq!(llm["kind"], "llm");
    assert_eq!(llm["state"], "configured");
    assert_eq!(llm["configured"], true);
    assert_eq!(llm["reachable"], false);
    assert_eq!(llm["proven_by_turn"], serde_json::Value::Null);
    assert_eq!(llm["message"], "upstream refused credentials");
    assert_eq!(llm["affects_pipelines"], serde_json::json!(["kitchen"]));
}

#[tokio::test]
async fn reachable_provider_is_not_proven_until_a_real_turn_uses_it() {
    let state = with_status_providers(guarded(), Health::Healthy);
    let (status, body) = call(&state, put(&provider_status_graph())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = call(&state, bearer("/v1/status", MANAGEMENT_TOKEN)).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let providers = body["providers"].as_array().expect("providers");
    let tts = providers.iter().find(|provider| provider["id"] == "configured-tts").unwrap();
    assert_eq!(tts["kind"], "tts");
    assert_eq!(tts["state"], "reachable");
    assert_eq!(tts["configured"], true);
    assert_eq!(tts["reachable"], true);
    assert_eq!(tts["proven_by_turn"], serde_json::Value::Null);
    assert_eq!(tts["affects_pipelines"], serde_json::json!(["kitchen"]));
}

#[tokio::test]
async fn successful_turn_marks_invoked_providers_as_proven() {
    let state = with_status_providers(guarded(), Health::Healthy);
    let (status, body) = call(&state, put(&provider_status_graph())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let conversation = ConversationId::new();
    let turn = TurnId::new();

    publish(&state, "kitchen", conversation, Event::TurnStarted { turn });
    publish(
        &state,
        "kitchen",
        conversation,
        Event::SpeechFinal { text: "hello".to_owned(), confidence: None, language: None },
    );
    publish(
        &state,
        "kitchen",
        conversation,
        Event::LlmRequestStarted { model: "echo".to_owned() },
    );
    publish(
        &state,
        "kitchen",
        conversation,
        Event::LlmFinished {
            reason: FinishReason::Stop,
            prompt_tokens: None,
            completion_tokens: None,
        },
    );
    publish(&state, "kitchen", conversation, Event::TtsStarted { voice: "echo".to_owned() });
    publish(&state, "kitchen", conversation, Event::TtsFinished { duration_ms: 20 });
    publish(&state, "kitchen", conversation, Event::ConversationCompleted);

    let body = wait_for_status(&state, |body| {
        body["providers"].as_array().is_some_and(|providers| {
            providers.iter().any(|provider| {
                provider["id"] == "configured-llm" && provider["state"] == "proven"
            })
        })
    })
    .await;
    let providers = body["providers"].as_array().expect("providers");
    for id in ["configured-stt", "configured-llm", "configured-tts"] {
        let provider = providers.iter().find(|provider| provider["id"] == id).unwrap();
        assert_eq!(provider["state"], "proven");
        assert_eq!(provider["proven_by_turn"], turn.to_string());
    }
}

#[tokio::test]
async fn failed_synthesis_keeps_pipeline_unhealthy_until_later_success() {
    let state = with_providers(guarded());
    let (status, body) = call(&state, put(&valid_graph())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let conversation = ConversationId::new();
    let failed = TurnId::new();

    publish(&state, "kitchen", conversation, Event::TurnStarted { turn: failed });
    publish(
        &state,
        "kitchen",
        conversation,
        Event::LlmRequestStarted { model: "echo".to_owned() },
    );
    publish(
        &state,
        "kitchen",
        conversation,
        Event::LlmFinished {
            reason: FinishReason::Stop,
            prompt_tokens: None,
            completion_tokens: None,
        },
    );
    publish(&state, "kitchen", conversation, Event::TtsStarted { voice: "echo".to_owned() });
    publish(
        &state,
        "kitchen",
        conversation,
        Event::StageFailed {
            node: "tts".to_owned(),
            error: "connection refused".to_owned(),
            recovered: false,
        },
    );
    publish(
        &state,
        "kitchen",
        conversation,
        Event::ConversationCancelled { reason: CancelReason::Error },
    );

    let body =
        wait_for_status(&state, |body| body["pipelines"][0]["health"]["state"] == "unhealthy")
            .await;
    assert_eq!(body["pipelines"][0]["health"]["last_failed_turn"], failed.to_string());
    assert_eq!(body["pipelines"][0]["components"][2]["kind"], "synthesis");
    assert_eq!(body["pipelines"][0]["components"][2]["state"], "unhealthy");
    assert_eq!(body["recent_failures"][0]["message"], "connection refused");

    let recovered_conversation = ConversationId::new();
    let recovered = TurnId::new();
    publish(&state, "kitchen", recovered_conversation, Event::TurnStarted { turn: recovered });
    publish(
        &state,
        "kitchen",
        recovered_conversation,
        Event::SpeechFinal { text: "hello".to_owned(), confidence: None, language: None },
    );
    publish(
        &state,
        "kitchen",
        recovered_conversation,
        Event::LlmRequestStarted { model: "echo".to_owned() },
    );
    publish(
        &state,
        "kitchen",
        recovered_conversation,
        Event::LlmFinished {
            reason: FinishReason::Stop,
            prompt_tokens: None,
            completion_tokens: None,
        },
    );
    publish(
        &state,
        "kitchen",
        recovered_conversation,
        Event::TtsStarted { voice: "echo".to_owned() },
    );
    publish(&state, "kitchen", recovered_conversation, Event::TtsFinished { duration_ms: 20 });
    publish(&state, "kitchen", recovered_conversation, Event::ConversationCompleted);

    let body =
        wait_for_status(&state, |body| body["pipelines"][0]["health"]["state"] == "healthy")
            .await;
    assert_eq!(body["pipelines"][0]["health"]["last_successful_turn"], recovered.to_string());
    assert_eq!(body["pipelines"][0]["health"]["last_failed_turn"], serde_json::Value::Null);
    assert_eq!(body["recent_failures"], serde_json::json!([]));
}

#[tokio::test]
async fn conversation_socket_tracks_connected_and_recent_satellite_separately() {
    let state = with_providers(guarded());
    let (status, body) = call(&state, put(&valid_graph())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let address = server(state).await;

    let (mut socket, _) = tokio_tungstenite::connect_async(ws_request(
        address,
        "/v1/pipelines/kitchen/converse",
        DEVICE_TOKEN,
    ))
    .await
    .expect("device connects");

    let connected = wait_for_http_status(address, |body| {
        body["satellites"]["connected"].as_array().is_some_and(|satellites| {
            satellites.iter().any(|satellite| satellite["name"] == "kitchen")
        })
    })
    .await;
    assert_eq!(connected["satellites"]["connected"][0]["name"], "kitchen");
    assert_eq!(connected["satellites"]["connected"][0]["pipeline"], "kitchen");
    assert!(
        connected["satellites"]["connected"][0]["conversation"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "{connected}"
    );

    socket.send(Message::Binary(b"hello".to_vec().into())).await.expect("sends audio");
    socket.send(Message::Text(r#"{"type":"end"}"#.into())).await.expect("sends end");
    let drain = async {
        while let Some(frame) = socket.next().await {
            if matches!(frame.expect("frame"), Message::Close(_)) {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), drain).await.expect("conversation ends");

    let inactive_but_recent = wait_for_http_status(address, |body| {
        body["satellites"]["connected"].as_array().is_some_and(Vec::is_empty)
            && body["satellites"]["recently_active"].as_array().is_some_and(|satellites| {
                satellites.iter().any(|satellite| satellite["name"] == "kitchen")
            })
    })
    .await;
    assert_eq!(
        inactive_but_recent["satellites"]["recently_active"][0]["last_event"],
        "ConversationCompleted"
    );
}

#[tokio::test]
async fn recent_satellite_activity_survives_without_a_connected_socket() {
    let state = with_providers(guarded());
    let (status, body) = call(&state, put(&valid_graph())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let conversation = ConversationId::new();
    let device = DeviceId::new();

    state.bus.publish(
        Envelope::new(
            TraceId::new(),
            Event::AudioStarted { format: conduit_core::audio::AudioFormat::DEFAULT },
        )
        .with_conversation(conversation)
        .with_device(device)
        .with_pipeline("kitchen"),
    );

    let body = wait_for_status(&state, |body| {
        body["satellites"]["connected"].as_array().is_some_and(Vec::is_empty)
            && body["satellites"]["recently_active"]
                .as_array()
                .is_some_and(|satellites| !satellites.is_empty())
    })
    .await;
    assert_eq!(body["satellites"]["recently_active"][0]["name"], device.to_string());
    assert_eq!(body["satellites"]["recently_active"][0]["last_event"], "AudioStarted");
}

#[tokio::test]
async fn stale_satellite_activity_ages_out_of_recent_status() {
    let state = with_providers(guarded());
    let (status, body) = call(&state, put(&valid_graph())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let old = chrono::Duration::seconds(
        conduit_api::status::RECENT_SATELLITE_WINDOW_SECONDS as i64 + 1,
    );
    let envelope = Envelope {
        id: conduit_core::id::EventId::new(),
        trace: TraceId::new(),
        at: Utc::now() - old,
        device: Some(DeviceId::new()),
        conversation: Some(ConversationId::new()),
        pipeline: Some("kitchen".to_owned()),
        event: Event::AudioStarted { format: conduit_core::audio::AudioFormat::DEFAULT },
    };

    state.status().record(&envelope).await;

    let (status, body) = call(&state, bearer("/v1/status", MANAGEMENT_TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["satellites"]["recently_active"], serde_json::json!([]));
}
