//! A device holding a conversation over a WebSocket.
//!
//! These drive a real server over a real socket, because the point of the
//! endpoint is what happens on the wire.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use conduit_api::{router, AppState};
use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::bus::EventBus;
use conduit_core::graph::{Edge, Node, NodeKind, PipelineGraph};
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions, Transcript};
use conduit_provider::testing::{EchoLlm, EchoStt, EchoTts};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{ChunkStream, Provider};
use conduit_runtime::Providers;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

/// A synthesizer that speaks the text a syllable at a time, slowly.
///
/// Registered under the echo synthesizer's name so a pipeline can swap it in.
/// Needed because [`EchoTts`] returns instantly: a whole turn finishes before a
/// stop sent from a test could arrive, and the test would prove nothing about
/// interrupting. A real synthesizer takes about as long as the speech lasts.
#[derive(Debug, Clone, Default)]
struct SlowTts;

impl Provider for SlowTts {
    fn name(&self) -> &str {
        "echo-tts"
    }
}

/// A synthesizer that accepts the request and never answers.
///
/// Registered under the echo synthesizer's name, like [`SlowTts`], so a
/// pipeline can swap it in. This is the failure a deadline exists for: nothing
/// errors, no stage fails, and the turn simply waits.
#[derive(Debug, Clone, Default)]
struct SilentTts;

impl Provider for SilentTts {
    fn name(&self) -> &str {
        "echo-tts"
    }
}

#[async_trait::async_trait]
impl TextToSpeech for SilentTts {
    async fn synthesize(
        &self,
        _request: SynthesisRequest,
    ) -> conduit_core::Result<ChunkStream<SpeechChunk>> {
        std::future::pending().await
    }

    async fn voices(&self) -> conduit_core::Result<Vec<Voice>> {
        Ok(Vec::new())
    }
}

/// Records the format the API said the device was streaming.
#[derive(Debug, Clone, Default)]
struct RecordingStt {
    formats: Arc<Mutex<Vec<AudioFormat>>>,
}

impl RecordingStt {
    fn formats(&self) -> Vec<AudioFormat> {
        self.formats.lock().expect("lock").clone()
    }
}

impl Provider for RecordingStt {
    fn name(&self) -> &str {
        "recording-stt"
    }
}

#[async_trait::async_trait]
impl SpeechToText for RecordingStt {
    async fn transcribe(
        &self,
        audio: ChunkStream<AudioChunk>,
        options: TranscribeOptions,
    ) -> conduit_core::Result<ChunkStream<Transcript>> {
        self.formats.lock().expect("lock").push(options.format);
        let heard = audio
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|chunk| chunk.expect("chunk").data)
            .fold(Vec::new(), |mut bytes, chunk| {
                bytes.extend_from_slice(&chunk);
                bytes
            });
        let text = String::from_utf8_lossy(&heard).into_owned();
        Ok(Box::pin(futures_util::stream::iter(vec![Ok(Transcript::final_text(text))])))
    }
}

/// Records the format requested from synthesis.
#[derive(Debug, Clone, Default)]
struct RecordingTts {
    formats: Arc<Mutex<Vec<AudioFormat>>>,
}

impl RecordingTts {
    fn formats(&self) -> Vec<AudioFormat> {
        self.formats.lock().expect("lock").clone()
    }
}

impl Provider for RecordingTts {
    fn name(&self) -> &str {
        "recording-tts"
    }
}

#[async_trait::async_trait]
impl TextToSpeech for RecordingTts {
    async fn synthesize(
        &self,
        request: SynthesisRequest,
    ) -> conduit_core::Result<ChunkStream<SpeechChunk>> {
        self.formats.lock().expect("lock").push(request.format);
        Ok(Box::pin(futures_util::stream::iter(vec![Ok(SpeechChunk {
            sequence: 0,
            format: request.format,
            data: bytes::Bytes::from(request.text.into_bytes()),
        })])))
    }

    async fn voices(&self) -> conduit_core::Result<Vec<Voice>> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl TextToSpeech for SlowTts {
    async fn synthesize(
        &self,
        request: SynthesisRequest,
    ) -> conduit_core::Result<ChunkStream<SpeechChunk>> {
        let format = request.format;
        let chunks = request.text.into_bytes();
        Ok(Box::pin(futures_util::stream::unfold(
            chunks.into_iter().enumerate(),
            move |mut bytes| async move {
                let (sequence, byte) = bytes.next()?;
                tokio::time::sleep(Duration::from_millis(20)).await;
                let chunk = SpeechChunk {
                    sequence: sequence as u64,
                    format,
                    data: bytes::Bytes::from(vec![byte]),
                };
                Some((Ok(chunk), bytes))
            },
        )))
    }

    async fn voices(&self) -> conduit_core::Result<Vec<Voice>> {
        Ok(vec![Voice {
            id: "slow".to_owned(),
            name: "Slow".to_owned(),
            language: "en-US".to_owned(),
        }])
    }
}

/// A pipeline built on the in-memory providers: text in, text out.
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

/// The same pipeline, but slow enough to interrupt.
fn slow_providers() -> Providers {
    Providers::new().with_stt(EchoStt).with_llm(EchoLlm).with_tts(SlowTts)
}

/// The same pipeline, but with a synthesizer that never answers.
fn silent_providers() -> Providers {
    Providers::new().with_stt(EchoStt).with_llm(EchoLlm).with_tts(SilentTts)
}

fn recording_graph() -> PipelineGraph {
    PipelineGraph::new("recording")
        .with_node(Node::new("stt", NodeKind::Stt, "recording-stt"))
        .with_node(Node::new("llm", NodeKind::Llm, "echo-llm"))
        .with_node(Node::new("tts", NodeKind::Tts, "recording-tts"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
}

/// Both listeners on ephemeral ports. Stops when the test ends.
///
/// Two of them, as the binary runs them: the service port carries conversations
/// and requires a token, and the ops port carries `/health`, `/ready`, and
/// `/metrics` and requires nothing.
struct Server {
    address: std::net::SocketAddr,
    ops_address: std::net::SocketAddr,
    state: AppState,
}

impl Server {
    async fn start(state: AppState) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let app = router(state.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let ops_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let ops_address = ops_listener.local_addr().expect("address");
        let ops_app = conduit_api::ops_router(state.clone());
        tokio::spawn(async move {
            let _ = axum::serve(ops_listener, ops_app).await;
        });

        Self { address, ops_address, state }
    }

    fn ws_url(&self, path: &str) -> String {
        format!("ws://{}{path}", self.address)
    }

    fn http_url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    /// A URL on the unauthenticated ops listener.
    fn ops_url(&self, path: &str) -> String {
        format!("http://{}{path}", self.ops_address)
    }
}

/// Connects, says `utterance`, and returns the frames the server sent back.
async fn converse(server: &Server, path: &str, utterance: &str) -> Vec<Message> {
    let (mut socket, _) =
        tokio_tungstenite::connect_async(server.ws_url(path)).await.expect("connects");

    socket.send(Message::Binary(utterance.as_bytes().to_vec().into())).await.expect("sends");
    socket
        .send(Message::Text(r#"{"type":"end"}"#.into()))
        .await
        .expect("sends end of utterance");

    let collect = async {
        let mut frames = Vec::new();
        while let Some(frame) = socket.next().await {
            match frame.expect("frame") {
                Message::Close(_) => break,
                message => frames.push(message),
            }
        }
        frames
    };
    tokio::time::timeout(Duration::from_secs(10), collect).await.expect("server replies")
}

/// The audio the server sent, as text — the echo providers speak UTF-8.
fn spoken(frames: &[Message]) -> String {
    frames
        .iter()
        .filter_map(|frame| match frame {
            Message::Binary(data) => Some(String::from_utf8_lossy(data).into_owned()),
            _ => None,
        })
        .collect()
}

/// The JSON control frames the server sent.
fn control(frames: &[Message]) -> Vec<serde_json::Value> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            Message::Text(text) => serde_json::from_str(text).ok(),
            _ => None,
        })
        .collect()
}

/// Stores a pipeline, failing the test if the store refuses.
async fn store(state: &AppState, name: &str, graph: PipelineGraph) {
    state.put_pipeline(name, graph).await.expect("stores");
}

async fn server_with_echo_pipeline() -> Server {
    let state = AppState::new(EventBus::default()).with_providers(providers());
    state.put_pipeline("echo", echo_graph()).await.expect("stores");
    Server::start(state).await
}

#[tokio::test]
async fn a_device_speaks_and_is_answered() {
    let server = server_with_echo_pipeline().await;
    let frames = converse(&server, "/v1/pipelines/echo/converse", "hello there").await;

    assert_eq!(spoken(&frames), "You said: hello there.");
}

#[tokio::test]
async fn the_conversation_is_announced_before_the_audio() {
    // A client needs the id to follow its own turn on /v1/events.
    let server = server_with_echo_pipeline().await;
    let frames = converse(&server, "/v1/pipelines/echo/converse", "hello").await;

    let first = control(&frames).first().cloned().expect("a control frame");
    assert_eq!(first["type"], "started");
    assert!(
        first["conversation"].as_str().is_some_and(|id| !id.is_empty()),
        "expected a conversation id: {first}"
    );
}

#[tokio::test]
async fn the_turn_ends_with_a_done_frame() {
    let server = server_with_echo_pipeline().await;
    let frames = converse(&server, "/v1/pipelines/echo/converse", "hello").await;

    let last = control(&frames).last().cloned().expect("a control frame");
    assert_eq!(last["type"], "done");
}

#[tokio::test]
async fn the_reply_is_streamed_not_delivered_whole() {
    // Sentence by sentence: two sentences must not arrive as one frame.
    let server = server_with_echo_pipeline().await;
    let frames = converse(&server, "/v1/pipelines/echo/converse", "one. two").await;

    let audio_frames =
        frames.iter().filter(|frame| matches!(frame, Message::Binary(_))).count();
    assert!(audio_frames >= 2, "expected streamed audio, got {audio_frames} frame(s)");
}

#[tokio::test]
async fn the_negotiated_audio_format_reaches_the_runtime() {
    let stt = RecordingStt::default();
    let tts = RecordingTts::default();
    let state = AppState::new(EventBus::default()).with_providers(
        Providers::new().with_stt(stt.clone()).with_llm(EchoLlm).with_tts(tts.clone()),
    );
    state.put_pipeline("recording", recording_graph()).await.expect("stores");
    let server = Server::start(state).await;

    let frames = converse(
        &server,
        "/v1/pipelines/recording/converse?encoding=pcm_f32_le&sample_rate=48000&channels=2",
        "hello",
    )
    .await;

    let negotiated =
        AudioFormat { encoding: Encoding::PcmF32Le, sample_rate: 48_000, channels: 2 };
    assert_eq!(spoken(&frames), "You said: hello.");
    assert_eq!(stt.formats(), [negotiated]);
    assert_eq!(tts.formats(), [negotiated]);
}

#[tokio::test]
async fn events_from_the_turn_reach_the_event_stream() {
    let server = server_with_echo_pipeline().await;
    let mut subscription = server.state.bus.subscribe();

    let frames = converse(&server, "/v1/pipelines/echo/converse", "hello").await;
    let id = control(&frames)[0]["conversation"].as_str().expect("id").to_owned();

    let envelope = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
        .await
        .expect("an event")
        .expect("bus open");
    assert_eq!(
        envelope.conversation.map(|conversation| conversation.to_string()),
        Some(id),
        "the announced id must match the events"
    );
}

#[tokio::test]
async fn a_device_can_stop_the_reply_it_asked_for() {
    // The reply is long and slow to speak, so there is something to interrupt.
    let state = AppState::new(EventBus::default()).with_providers(slow_providers());
    state.put_pipeline("echo", echo_graph()).await.expect("stores");
    let server = Server::start(state).await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(server.ws_url("/v1/pipelines/echo/converse"))
            .await
            .expect("connects");

    let utterance = "one. two. three. four. five. six. seven. eight";
    socket.send(Message::Binary(utterance.as_bytes().to_vec().into())).await.expect("sends");
    socket.send(Message::Text(r#"{"type":"end"}"#.into())).await.expect("sends end");

    // Wait for speech to start, so the stop lands mid-reply rather than before
    // the turn has anything to abandon.
    let started = async {
        while let Some(frame) = socket.next().await {
            if matches!(frame.expect("frame"), Message::Binary(_)) {
                return;
            }
        }
        panic!("the server never sent audio");
    };
    tokio::time::timeout(Duration::from_secs(10), started).await.expect("audio starts");

    socket.send(Message::Text(r#"{"type":"stop"}"#.into())).await.expect("sends stop");

    // The socket must terminate cleanly: a client that sees a reset cannot tell
    // an honoured stop from a crashed server.
    let rest = async {
        let mut frames = Vec::new();
        let mut closed = false;
        while let Some(frame) = socket.next().await {
            match frame.expect("frame") {
                Message::Close(_) => {
                    closed = true;
                    break;
                }
                message => frames.push(message),
            }
        }
        (frames, closed)
    };
    let (frames, closed) =
        tokio::time::timeout(Duration::from_secs(10), rest).await.expect("the reply ends");

    assert!(closed, "expected a close frame after a stop, got {frames:?}");
    assert!(
        !spoken(&frames).contains("eight"),
        "the reply must be cut short, got {:?}",
        spoken(&frames)
    );
}

#[tokio::test]
async fn a_stopped_turn_is_recorded_as_asked_for() {
    // The metric is how an operator tells interruptions from dropped clients,
    // so the label has to be the one a stop means.
    let state = AppState::new(EventBus::default()).with_providers(slow_providers());
    state.put_pipeline("echo", echo_graph()).await.expect("stores");
    conduit_metrics::Collector::spawn(state.metrics(), &state.bus);
    let server = Server::start(state).await;

    let (mut socket, _) =
        tokio_tungstenite::connect_async(server.ws_url("/v1/pipelines/echo/converse"))
            .await
            .expect("connects");
    socket
        .send(Message::Binary("one. two. three. four. five".as_bytes().to_vec().into()))
        .await
        .expect("sends");
    socket.send(Message::Text(r#"{"type":"end"}"#.into())).await.expect("sends end");
    socket.send(Message::Text(r#"{"type":"stop"}"#.into())).await.expect("sends stop");

    // Drain until the server is done with the turn.
    let drain = async { while let Some(Ok(_)) = socket.next().await {} };
    let _ = tokio::time::timeout(Duration::from_secs(10), drain).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let body = reqwest::get(server.ops_url("/metrics"))
        .await
        .expect("request")
        .text()
        .await
        .expect("body");
    assert!(
        body.contains("conduit_conversations_total{outcome=\"user_requested\"} 1"),
        "{body}"
    );
}

#[tokio::test]
async fn a_device_is_told_when_a_turn_is_given_up_on() {
    // The socket half of the deadline. A turn that ends without saying why looks
    // exactly like a finished one from the device's side, and a satellite that
    // cannot tell those apart has no idea whether to prompt the person again.
    let state = AppState::new(EventBus::default())
        .with_providers(silent_providers())
        .with_turn_idle_timeout(Some(Duration::from_millis(100)));
    state.put_pipeline("echo", echo_graph()).await.expect("stores");
    let server = Server::start(state).await;

    let frames = tokio::time::timeout(
        Duration::from_secs(10),
        converse(&server, "/v1/pipelines/echo/converse", "hello"),
    )
    .await
    .expect("the socket closes rather than hanging on a wedged provider");

    let last = control(&frames).last().cloned().expect("a control frame");
    assert_eq!(last["type"], "failed", "{last}");
    assert!(
        last["error"].as_str().is_some_and(|error| error.contains("synthesis")),
        "the device is told which stage went quiet: {last}"
    );
    assert_eq!(spoken(&frames), "", "a wedged synthesizer speaks nothing");
}

#[tokio::test]
async fn an_abandoned_turn_is_recorded_as_a_timeout() {
    // The operator's half. `idle_timeout` was a label the collector could
    // produce and nothing ever set, so a scrape could not distinguish a stalled
    // provider from a device that hung up.
    let state = AppState::new(EventBus::default())
        .with_providers(silent_providers())
        .with_turn_idle_timeout(Some(Duration::from_millis(100)));
    state.put_pipeline("echo", echo_graph()).await.expect("stores");
    conduit_metrics::Collector::spawn(state.metrics(), &state.bus);
    let server = Server::start(state).await;

    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        converse(&server, "/v1/pipelines/echo/converse", "hello"),
    )
    .await
    .expect("the socket closes");
    // The collector reads the bus on its own task, so give it the turn's end.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let body = reqwest::get(server.ops_url("/metrics"))
        .await
        .expect("request")
        .text()
        .await
        .expect("body");
    assert!(body.contains("conduit_conversations_total{outcome=\"idle_timeout\"} 1"), "{body}");
}

#[tokio::test]
async fn an_unknown_pipeline_is_refused_before_upgrading() {
    let server = server_with_echo_pipeline().await;
    let result =
        tokio_tungstenite::connect_async(server.ws_url("/v1/pipelines/ghost/converse")).await;
    assert!(result.is_err(), "connecting to a missing pipeline must fail");
}

#[tokio::test]
async fn a_pipeline_the_runtime_cannot_execute_is_refused() {
    // Stored pipelines are only checked as graphs; whether the runtime can
    // execute one is a separate question, answered here rather than mid-turn.
    let state = AppState::new(EventBus::default()).with_providers(providers());
    store(
        &state,
        "unrunnable",
        PipelineGraph::new("unrunnable")
            .with_node(Node::new("stt", NodeKind::Stt, "nonexistent"))
            .with_node(Node::new("llm", NodeKind::Llm, "echo-llm"))
            .with_node(Node::new("tts", NodeKind::Tts, "echo-tts"))
            .with_edge(Edge::new("stt", "llm"))
            .with_edge(Edge::new("llm", "tts")),
    )
    .await;
    let server = Server::start(state).await;

    let result =
        tokio_tungstenite::connect_async(server.ws_url("/v1/pipelines/unrunnable/converse"))
            .await;
    assert!(result.is_err(), "an unrunnable pipeline must be refused");
}

#[tokio::test]
async fn a_server_with_no_providers_still_serves_the_rest_of_the_api() {
    // Providers are optional; a deployment that has not configured any should
    // still be able to store and read pipelines.
    let state = AppState::new(EventBus::default());
    state.put_pipeline("echo", echo_graph()).await.expect("stores");
    let server = Server::start(state).await;

    let response = reqwest::get(server.http_url("/v1/pipelines")).await.expect("request");
    assert!(response.status().is_success());

    let result =
        tokio_tungstenite::connect_async(server.ws_url("/v1/pipelines/echo/converse")).await;
    assert!(result.is_err(), "conversing without providers must be refused");
}

#[tokio::test]
async fn a_conversation_shows_up_in_the_metrics() {
    // The collector is a bus subscriber, so this also proves the server wires
    // one up: without it a scrape would stay empty however much happened.
    let state = AppState::new(EventBus::default()).with_providers(providers());
    state.put_pipeline("echo", echo_graph()).await.expect("stores");
    conduit_metrics::Collector::spawn(state.metrics(), &state.bus);
    let server = Server::start(state).await;

    let _ = converse(&server, "/v1/pipelines/echo/converse", "hello").await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let body = reqwest::get(server.ops_url("/metrics"))
        .await
        .expect("request")
        .text()
        .await
        .expect("body");

    assert!(body.contains("conduit_conversations_total{outcome=\"completed\"} 1"), "{body}");
    assert!(body.contains("conduit_time_to_first_audio_seconds_count 1"), "{body}");
}

#[tokio::test]
async fn the_scrape_endpoint_announces_the_prometheus_format() {
    let server = server_with_echo_pipeline().await;
    let response = reqwest::get(server.ops_url("/metrics")).await.expect("request");

    assert!(response.status().is_success());
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.starts_with("text/plain"), "{content_type}");
    assert!(response.text().await.expect("body").contains("# TYPE conduit_events_total"));
}

/// Long enough to pass the token-entropy floor.
const DEVICE_TOKEN: &str = "converse-device-token-0000000000000000";
const GUEST_TOKEN: &str = "converse-guest-token-00000000000000000";
const MANAGEMENT_TOKEN: &str = "converse-management-token-000000000000";

/// A server whose conversation socket requires a device token.
///
/// `guest` is restricted to a pipeline that is not `echo`, which is what makes
/// the restriction observable rather than merely configured.
async fn guarded_server() -> Server {
    let tokens = conduit_api::auth::Tokens::parse(&format!(
        r#"{{
          "devices": [
            {{ "token": "{DEVICE_TOKEN}", "device": "kitchen" }},
            {{ "token": "{GUEST_TOKEN}", "device": "guest", "pipelines": ["guest-room"] }}
          ],
          "management": [{{ "token": "{MANAGEMENT_TOKEN}", "name": "ui" }}]
        }}"#
    ))
    .expect("the token file parses");

    let state = AppState::new(EventBus::default())
        .with_providers(providers())
        .with_access(conduit_api::auth::Access::Tokens(tokens));
    state.put_pipeline("echo", echo_graph()).await.expect("stores");
    Server::start(state).await
}

/// An upgrade request for `path` carrying `token`, or none at all.
fn upgrade(
    server: &Server,
    path: &str,
    token: Option<&str>,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let mut request = server.ws_url(path).into_client_request().expect("a websocket request");
    if let Some(token) = token {
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {token}").parse().expect("a header value"),
        );
    }
    request
}

#[tokio::test]
async fn a_device_token_opens_a_conversation() {
    let server = guarded_server().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(upgrade(
        &server,
        "/v1/pipelines/echo/converse",
        Some(DEVICE_TOKEN),
    ))
    .await
    .expect("an authenticated device connects");

    socket.send(Message::Binary(b"hello".to_vec().into())).await.expect("sends");
    socket.send(Message::Text(r#"{"type":"end"}"#.into())).await.expect("sends end");

    let collect = async {
        let mut frames = Vec::new();
        while let Some(frame) = socket.next().await {
            match frame.expect("frame") {
                Message::Close(_) => break,
                message => frames.push(message),
            }
        }
        frames
    };
    let frames = tokio::time::timeout(Duration::from_secs(10), collect)
        .await
        .expect("the server replies");
    assert_eq!(spoken(&frames), "You said: hello.");
}

#[tokio::test]
async fn a_conversation_without_a_token_is_refused_before_upgrading() {
    // Refused at the handshake, so a stranger who finds the port never gets as
    // far as talking to the assistant.
    let server = guarded_server().await;
    let result =
        tokio_tungstenite::connect_async(upgrade(&server, "/v1/pipelines/echo/converse", None))
            .await;
    assert!(result.is_err(), "conversing without a token must fail the upgrade");
}

#[tokio::test]
async fn a_conversation_with_an_unknown_token_is_refused() {
    let server = guarded_server().await;
    let result = tokio_tungstenite::connect_async(upgrade(
        &server,
        "/v1/pipelines/echo/converse",
        Some("nobody-holds-this-token-0000000000000"),
    ))
    .await;
    assert!(result.is_err(), "an unknown token must fail the upgrade");
}

#[tokio::test]
async fn a_device_restricted_to_other_pipelines_is_refused() {
    // A satellite in a guest room must not reach the pipeline whose tools
    // unlock the front door.
    let server = guarded_server().await;
    let result = tokio_tungstenite::connect_async(upgrade(
        &server,
        "/v1/pipelines/echo/converse",
        Some(GUEST_TOKEN),
    ))
    .await;

    let error = result.expect_err("a restricted device must be refused").to_string();
    assert!(error.contains("403"), "expected a 403, got {error}");
}

#[tokio::test]
async fn the_events_of_a_conversation_name_the_device_that_started_it() {
    // What makes `/v1/events?device=` select a satellite rather than nothing.
    let server = guarded_server().await;
    let mut subscription = server.state.bus.subscribe();

    let (mut socket, _) = tokio_tungstenite::connect_async(upgrade(
        &server,
        "/v1/pipelines/echo/converse",
        Some(DEVICE_TOKEN),
    ))
    .await
    .expect("connects");
    socket.send(Message::Binary(b"hello".to_vec().into())).await.expect("sends");
    socket.send(Message::Text(r#"{"type":"end"}"#.into())).await.expect("sends end");

    let envelope = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
        .await
        .expect("an event")
        .expect("bus open");
    assert!(
        envelope.device.is_some(),
        "an authenticated conversation must say which device it came from"
    );
}

#[tokio::test]
async fn an_anonymous_server_still_tags_its_events_with_a_device() {
    // Otherwise `?device=` would work only on an authenticated deployment, and
    // the filter would be quietly useless on the one people try first.
    let server = server_with_echo_pipeline().await;
    let mut subscription = server.state.bus.subscribe();

    let _ = converse(&server, "/v1/pipelines/echo/converse", "hello").await;

    let envelope = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
        .await
        .expect("an event")
        .expect("bus open");
    assert!(envelope.device.is_some(), "every conversation belongs to some device");
}
