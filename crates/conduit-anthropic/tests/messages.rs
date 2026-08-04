//! The Messages provider against a stand-in server.

mod server;

use conduit_anthropic::{Anthropic, AnthropicConfig, Failure, API_VERSION};
use conduit_core::event::FinishReason;
use conduit_provider::llm::{Completion, CompletionRequest, Message, ToolSpec};
use conduit_provider::{Health, Provider};
use futures_util::StreamExt;
use server::MockServer;

fn config(server: &MockServer) -> AnthropicConfig {
    AnthropicConfig {
        base_url: server.url(),
        api_key: Some("sk-ant-test".to_owned()),
        ..AnthropicConfig::default()
    }
}

/// The documented event sequence for a short text reply.
fn text_response() -> Vec<&'static str> {
    vec![
        r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":25}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" there"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}"#,
        r#"{"type":"message_stop"}"#,
    ]
}

async fn complete(provider: &Anthropic, request: CompletionRequest) -> Vec<Completion> {
    use conduit_provider::llm::LanguageModel;

    provider
        .complete(request)
        .await
        .expect("completes")
        .map(|item| item.expect("no failures"))
        .collect()
        .await
}

fn ask() -> CompletionRequest {
    CompletionRequest::new("claude-opus-5", vec![Message::user("hi")])
}

#[tokio::test]
async fn a_reply_arrives_as_tokens_and_one_finish() {
    let server = MockServer::start(&text_response()).await;
    let provider = Anthropic::new(config(&server)).expect("builds");

    let items = complete(&provider, ask()).await;

    assert_eq!(
        items,
        [
            Completion::Token { delta: "Hello".to_owned() },
            Completion::Token { delta: " there".to_owned() },
            Completion::Finished {
                reason: FinishReason::Stop,
                usage: conduit_provider::llm::Usage {
                    prompt_tokens: Some(25),
                    completion_tokens: Some(12),
                },
            },
        ]
    );
}

#[tokio::test]
async fn the_key_and_the_version_travel_as_headers_and_no_bearer_token_is_sent() {
    // The whole reason this is not a base URL under the OpenAI provider: a
    // bearer token is not how this API authenticates.
    let server = MockServer::start(&text_response()).await;
    let provider = Anthropic::new(config(&server)).expect("builds");

    let _ = complete(&provider, ask()).await;

    assert_eq!(server.last_api_key().await.as_deref(), Some("sk-ant-test"));
    assert_eq!(server.last_version().await.as_deref(), Some(API_VERSION));
    assert_eq!(
        server.last_authorization().await,
        None,
        "sending a bearer token as well would leak the key to a second place"
    );
}

#[tokio::test]
async fn the_request_is_written_in_the_apis_own_shape() {
    let server = MockServer::start(&text_response()).await;
    let mut config = config(&server);
    config.system_prompt = Some("Be terse.".to_owned());
    let provider = Anthropic::new(config).expect("builds");

    let _ = complete(
        &provider,
        CompletionRequest {
            tools: vec![ToolSpec {
                name: "lights.turn_on".to_owned(),
                description: "Turns lights on.".to_owned(),
                parameters: serde_json::json!({ "type": "object" }),
            }],
            temperature: Some(0.7),
            ..ask()
        },
    )
    .await;

    let body = server.last_body().await.expect("a request");
    assert_eq!(body["model"], "claude-opus-5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["system"], "Be terse.", "the system prompt is a top-level field");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["tools"][0]["input_schema"], serde_json::json!({ "type": "object" }));
    assert!(body["max_tokens"].is_number(), "the API requires it: {body}");
    assert!(
        body.get("temperature").is_none(),
        "current models reject `temperature`, so a pipeline's value is not forwarded: {body}"
    );
}

#[tokio::test]
async fn events_split_across_packets_are_reassembled() {
    // TCP does not preserve message boundaries; a response split mid-JSON is
    // the normal case, not an edge one.
    let server = MockServer::start_chunked(vec![
        "data: {\"type\":\"content_block_de".to_owned(),
        "lta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",".to_owned(),
        "\"text\":\"split\"}}\n\n".to_owned(),
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n"
            .to_owned(),
    ])
    .await;
    let provider = Anthropic::new(config(&server)).expect("builds");

    let items = complete(&provider, ask()).await;

    assert_eq!(items[0], Completion::Token { delta: "split".to_owned() });
}

#[tokio::test]
async fn a_tool_call_comes_back_assembled_with_the_id_a_result_must_quote() {
    let server = MockServer::start(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_abc","name":"get_weather"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"city\""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":":\"Denver\"}"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
    ])
    .await;
    let provider = Anthropic::new(config(&server)).expect("builds");

    let items = complete(&provider, ask()).await;

    assert_eq!(
        items,
        [
            Completion::ToolCall {
                id: conduit_core::id::ToolCallId::new("toolu_abc"),
                name: "get_weather".to_owned(),
                arguments: serde_json::json!({ "city": "Denver" }),
            },
            Completion::Finished {
                reason: FinishReason::ToolUse,
                usage: conduit_provider::llm::Usage::default(),
            },
        ]
    );
}

#[tokio::test]
async fn thinking_is_reported_apart_from_speech() {
    let server = MockServer::start(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"weighing it up"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Yes"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
    ])
    .await;
    let provider = Anthropic::new(config(&server)).expect("builds");

    let items = complete(&provider, ask()).await;

    assert_eq!(
        items[0],
        Completion::Reasoning { delta: "weighing it up".to_owned() },
        "reasoning is its own item so it is never spoken aloud"
    );
    assert_eq!(items[1], Completion::Token { delta: "Yes".to_owned() });
}

#[tokio::test]
async fn a_rate_limit_is_classified_as_worth_retrying() {
    use conduit_provider::llm::LanguageModel;

    let server = MockServer::start_retry_after(429, "slow down", "7").await;
    let provider = Anthropic::new(config(&server)).expect("builds");

    let Err(error) = provider.complete(ask()).await else { panic!("a 429 is not a stream") };
    let failure = Failure::of(&error).expect("classified");

    assert_eq!(failure.status(), Some(429));
    assert_eq!(failure.retry_after(), Some(std::time::Duration::from_secs(7)));
    assert!(failure.is_retryable());
    assert!(error.to_string().contains("slow down"), "the server's own words: {error}");
}

#[tokio::test]
async fn a_rejected_key_is_not_worth_retrying() {
    use conduit_provider::llm::LanguageModel;

    let server = MockServer::start_status(401, "invalid x-api-key").await;
    let provider = Anthropic::new(config(&server)).expect("builds");

    let Err(error) = provider.complete(ask()).await else { panic!("a 401 is not a stream") };
    let failure = Failure::of(&error).expect("classified");

    assert_eq!(failure.status(), Some(401));
    assert!(!failure.is_retryable(), "a wrong key is wrong on the second attempt too");
}

#[tokio::test]
async fn an_error_event_mid_stream_reaches_the_caller() {
    use conduit_provider::llm::LanguageModel;

    let server = MockServer::start(&[
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
    ])
    .await;
    let provider = Anthropic::new(config(&server)).expect("builds");

    let items: Vec<_> = provider.complete(ask()).await.expect("completes").collect().await;

    let error = items
        .into_iter()
        .filter_map(Result::err)
        .next()
        .expect("a server that gives up mid-stream is reported, not silently truncated");
    assert!(error.to_string().contains("Overloaded"), "{error}");
}

#[tokio::test]
async fn a_reachable_server_is_healthy_and_an_unreachable_one_says_why() {
    let server = MockServer::start(&text_response()).await;
    let provider = Anthropic::new(config(&server)).expect("builds");
    assert_eq!(provider.health().await, Health::Healthy);

    let rejecting = MockServer::start_status(401, "invalid x-api-key").await;
    let provider = Anthropic::new(config(&rejecting)).expect("builds");
    match provider.health().await {
        Health::Unhealthy { reason } => {
            assert!(reason.contains("401"), "the reason names what happened: {reason}");
        }
        other => panic!("a server rejecting the key is not healthy: {other:?}"),
    }
}
