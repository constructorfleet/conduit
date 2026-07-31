//! The provider driven against a stand-in for an OpenAI-compatible server.
//!
//! The mock replays recorded response bodies byte for byte, including the
//! awkward parts of the real wire format: tool arguments split across deltas,
//! chunks arriving several to a packet, and `[DONE]`.

mod server;

use conduit_core::event::FinishReason;
use conduit_core::id::ToolCallId;
use conduit_openai::{OpenAi, OpenAiConfig};
use conduit_provider::llm::{Completion, CompletionRequest, LanguageModel, Message, ToolSpec};
use conduit_provider::Provider;
use futures_util::StreamExt;
use server::MockServer;

/// Wraps SSE payloads into the framing a real server uses.
fn sse(events: &[&str]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(event);
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn provider(server: &MockServer) -> OpenAi {
    OpenAi::new(OpenAiConfig {
        base_url: server.url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiConfig::default()
    })
    .expect("provider builds")
}

fn request() -> CompletionRequest {
    CompletionRequest::new("gpt-test", vec![Message::user("hello")])
}

/// Collects a completion stream, failing on any error item.
async fn collect(provider: &OpenAi, request: CompletionRequest) -> Vec<Completion> {
    let stream = provider.complete(request).await.expect("request accepted");
    stream.map(|item| item.expect("no stream error")).collect::<Vec<_>>().await
}

#[tokio::test]
async fn streams_text_deltas_as_tokens() {
    let server = MockServer::start(sse(&[
        r#"{"choices":[{"delta":{"content":"Hello"}}]}"#,
        r#"{"choices":[{"delta":{"content":" there"}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
    ]))
    .await;

    let items = collect(&provider(&server), request()).await;

    assert_eq!(
        items,
        [
            Completion::Token { delta: "Hello".to_owned() },
            Completion::Token { delta: " there".to_owned() },
            Completion::Finished { reason: FinishReason::Stop, usage: Default::default() },
        ]
    );
}

#[tokio::test]
async fn reassembles_a_tool_call_split_across_deltas() {
    // This is how the wire format really arrives: the name in one chunk and
    // the arguments as a string built up over several more.
    let server = MockServer::start(sse(&[
        r#"{"choices":[{"delta":{"content":"Let me check. "}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc123","type":"function","function":{"name":"search","arguments":""}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Denver\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ]))
    .await;

    let items = collect(&provider(&server), request()).await;

    assert_eq!(items[0], Completion::Token { delta: "Let me check. ".to_owned() });
    assert_eq!(
        items[1],
        Completion::ToolCall {
            id: ToolCallId::new("call_abc123"),
            name: "search".to_owned(),
            arguments: serde_json::json!({ "city": "Denver" }),
        }
    );
    assert_eq!(
        items[2],
        Completion::Finished { reason: FinishReason::ToolUse, usage: Default::default() }
    );
}

#[tokio::test]
async fn a_tool_call_is_emitted_before_the_finish() {
    // The runtime speaks the preamble while tools run, so the call must not
    // arrive after the round is declared over.
    let server = MockServer::start(sse(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"search","arguments":"{}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ]))
    .await;

    let items = collect(&provider(&server), request()).await;
    let call = items.iter().position(|item| matches!(item, Completion::ToolCall { .. }));
    let finish = items.iter().position(|item| matches!(item, Completion::Finished { .. }));
    assert!(call < finish, "tool call must precede the finish: {items:?}");
}

#[tokio::test]
async fn reassembles_several_tool_calls_by_index() {
    let server = MockServer::start(sse(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"search","arguments":"{}"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"clock","arguments":"{}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ]))
    .await;

    let items = collect(&provider(&server), request()).await;
    let calls: Vec<_> = items
        .iter()
        .filter_map(|item| match item {
            Completion::ToolCall { id, name, .. } => Some((id.as_str(), name.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(calls, [("call_a", "search"), ("call_b", "clock")]);
}

#[tokio::test]
async fn reports_usage_when_the_server_sends_it() {
    let server = MockServer::start(sse(&[
        r#"{"choices":[{"delta":{"content":"hi"}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":2}}"#,
    ]))
    .await;

    let items = collect(&provider(&server), request()).await;
    let Some(Completion::Finished { usage, .. }) = items.last() else {
        panic!("expected a finish, got {items:?}");
    };
    assert_eq!(usage.prompt_tokens, Some(11));
    assert_eq!(usage.completion_tokens, Some(2));
}

#[tokio::test]
async fn reasoning_deltas_are_kept_separate_from_speech() {
    // Reasoning must never reach the synthesizer, so it gets its own variant.
    let server = MockServer::start(sse(&[
        r#"{"choices":[{"delta":{"reasoning_content":"thinking..."}}]}"#,
        r#"{"choices":[{"delta":{"content":"Answer."}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
    ]))
    .await;

    let items = collect(&provider(&server), request()).await;
    assert_eq!(items[0], Completion::Reasoning { delta: "thinking...".to_owned() });
    assert_eq!(items[1], Completion::Token { delta: "Answer.".to_owned() });
}

#[tokio::test]
async fn a_stream_ending_without_a_finish_still_finishes() {
    // Some servers just stop. The runtime relies on exactly one Finished.
    let server =
        MockServer::start(sse(&[r#"{"choices":[{"delta":{"content":"cut off"}}]}"#])).await;

    let items = collect(&provider(&server), request()).await;
    assert_eq!(
        items.last(),
        Some(&Completion::Finished { reason: FinishReason::Stop, usage: Default::default() })
    );
    assert_eq!(
        items.iter().filter(|item| matches!(item, Completion::Finished { .. })).count(),
        1
    );
}

#[tokio::test]
async fn chunks_split_across_packets_are_reassembled() {
    // TCP does not respect message boundaries; a chunk can arrive in pieces.
    let server = MockServer::start_chunked(vec![
        "data: {\"choices\":[{\"delta\":{\"content\":\"He".to_owned(),
        "llo\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_re".to_owned(),
        "ason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_owned(),
    ])
    .await;

    let items = collect(&provider(&server), request()).await;
    assert_eq!(items[0], Completion::Token { delta: "Hello".to_owned() });
}

#[tokio::test]
async fn the_request_carries_the_model_messages_and_tools() {
    let server =
        MockServer::start(sse(&[r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#])).await;

    let request = CompletionRequest {
        tools: vec![ToolSpec {
            name: "search".to_owned(),
            description: "look things up".to_owned(),
            parameters: serde_json::json!({ "type": "object" }),
        }],
        temperature: Some(0.5),
        ..CompletionRequest::new(
            "gpt-test",
            vec![
                Message::system("be brief"),
                Message::user("weather?"),
                Message::assistant("checking"),
                Message::tool_result(ToolCallId::new("call_abc123"), "{\"t\":24}"),
            ],
        )
    };
    let _ = collect(&provider(&server), request).await;

    let body = server.last_body().await.expect("a request was made");
    assert_eq!(body["model"], "gpt-test");
    assert_eq!(body["stream"], true);
    assert_eq!(body["temperature"], 0.5);

    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[2]["role"], "assistant");
    // A tool result must carry the provider's own id back, verbatim.
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call_abc123");

    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "search");
    assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
}

#[tokio::test]
async fn the_api_key_is_sent_as_a_bearer_token() {
    let server =
        MockServer::start(sse(&[r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#])).await;
    let _ = collect(&provider(&server), request()).await;

    assert_eq!(
        server.last_authorization().await.as_deref(),
        Some("Bearer test-key"),
        "the key must be sent"
    );
}

#[tokio::test]
async fn a_rejected_request_becomes_a_provider_error() {
    let server = MockServer::start_status(429, "slow down").await;

    let Err(error) = provider(&server).complete(request()).await else {
        panic!("a 429 must not be reported as success");
    };
    let message = error.to_string();
    assert!(message.contains("openai"), "the provider must name itself: {message}");
    assert!(message.contains("429"), "the status is the actionable part: {message}");
    assert!(error.is_retryable(), "a 429 is worth retrying");
}

#[tokio::test]
async fn a_malformed_chunk_fails_the_stream_rather_than_being_skipped() {
    let server = MockServer::start(sse(&["{not json}"])).await;

    let stream = provider(&server).complete(request()).await.expect("request accepted");
    let items: Vec<_> = stream.collect().await;
    assert!(items.iter().any(Result::is_err), "expected an error item: {items:?}");
}

#[tokio::test]
async fn the_provider_names_itself_for_the_registry() {
    let server = MockServer::start(String::new()).await;
    assert_eq!(provider(&server).name(), "openai");
    assert!(provider(&server).supports_tools());
}

#[tokio::test]
async fn health_follows_the_model_listing() {
    let server = MockServer::start(String::new()).await;
    assert!(provider(&server).health().await.is_usable());
}
