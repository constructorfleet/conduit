//! The JSON shapes of the Messages API.
//!
//! Kept separate from the provider so the mapping between Conduit's vocabulary
//! and Anthropic's is in one place and readable on its own.

use conduit_core::event::FinishReason;
use conduit_provider::llm::{CompletionRequest, Message, Role, ToolSpec, Usage};
use serde::{Deserialize, Serialize};

/// Cap on generated tokens when a request names none.
///
/// The field is required, so there is no "leave it to the server" to pass
/// through. This is the streaming guidance from Anthropic's own
/// documentation — generous enough not to truncate a spoken answer, and every
/// response Conduit asks for is streamed.
pub const DEFAULT_MAX_TOKENS: u32 = 64_000;

/// A streaming Messages request.
#[derive(Debug, Serialize)]
pub struct Request {
    /// Model identifier, passed through untouched.
    pub model: String,
    /// Conversation history, oldest first, never ending on an assistant turn.
    pub messages: Vec<WireMessage>,
    /// Instructions framing the conversation.
    ///
    /// A top-level field rather than a message, which is the Messages API's
    /// own shape and happens to match what `LanguageModel::system_prompt`
    /// already describes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Always true: Conduit only ever streams.
    pub stream: bool,
    /// Tools the model may call.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireTool>,
    /// Cap on generated tokens. Required by the API, so never absent.
    pub max_tokens: u32,
    /// Provider-specific settings, sent alongside the documented fields.
    ///
    /// Whatever the provider's declared settings schema admitted, which is why
    /// it can be flattened in without a second look.
    #[serde(flatten)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

impl Request {
    /// Translates a Conduit request into the Messages API's shape.
    ///
    /// `defaults` are the Configured Provider's stored settings; the request's
    /// own settings override them, so a pipeline can still overrule a default.
    ///
    /// `system` is the provider's configured prompt, used when the history
    /// carries no system message of its own.
    ///
    /// Note what is *not* forwarded: [`CompletionRequest::temperature`].
    /// Current models reject `temperature` outright — a 400, not a warning —
    /// so passing a pipeline's setting through would turn every turn into an
    /// error. A caller who needs sampling control on a model that still takes
    /// it can name it in `settings`, which is checked against the schema.
    pub fn from_completion(
        request: CompletionRequest,
        defaults: &serde_json::Map<String, serde_json::Value>,
        system: Option<&str>,
    ) -> Self {
        let (history, from_messages) = split_system(request.messages);
        Self {
            model: request.model,
            messages: history,
            // A system message in the history is the runtime's per-turn
            // framing — the graph's prompt and anything recalled from memory —
            // and it is more specific than the provider's standing prompt.
            system: from_messages.or_else(|| system.map(str::to_owned)),
            stream: true,
            tools: request.tools.into_iter().map(WireTool::from_spec).collect(),
            max_tokens: request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            settings: crate::layered_settings(defaults, request.settings.as_map()),
        }
    }
}

/// Separates system framing from the conversation.
///
/// Every system message is lifted out and joined, because the API takes one
/// `system` field and the runtime may contribute several — the graph's prompt
/// and a memory recall arrive as separate messages.
fn split_system(messages: Vec<Message>) -> (Vec<WireMessage>, Option<String>) {
    let mut system = Vec::new();
    let mut history = Vec::new();
    for message in messages {
        match message.role {
            Role::System => system.push(message.content),
            _ => history.push(WireMessage::from_message(message)),
        }
    }

    (trim_trailing_assistant(history), (!system.is_empty()).then(|| system.join("\n\n")))
}

/// Drops a trailing assistant turn.
///
/// Prefilling the assistant's reply was removed from the API and is now a 400,
/// so a history that ends on an assistant message cannot be sent as it stands.
/// It is dropped rather than kept, because the alternative is failing a turn
/// over context the model is about to regenerate anyway.
fn trim_trailing_assistant(mut history: Vec<WireMessage>) -> Vec<WireMessage> {
    while history.last().is_some_and(|message| message.role == "assistant") {
        tracing::debug!("dropping a trailing assistant turn, which the API reads as a prefill");
        history.pop();
    }
    history
}

/// One message in the conversation history.
#[derive(Debug, Serialize)]
pub struct WireMessage {
    /// Vendor role name. The API has only `user` and `assistant`.
    pub role: &'static str,
    /// Message text.
    pub content: String,
}

impl WireMessage {
    fn from_message(message: Message) -> Self {
        Self {
            role: match message.role {
                Role::Assistant => "assistant",
                // A tool result belongs to the user turn in this API, and it
                // would ideally be a `tool_result` block naming the
                // `tool_use` it answers. It cannot be: Conduit's history keeps
                // the result and the id but not the `tool_use` block that
                // requested it, and a `tool_result` without its matching
                // `tool_use` is rejected. Sent as the text it is, the model
                // reads the answer without the pairing.
                Role::User | Role::Tool | Role::System => "user",
            },
            content: message.content,
        }
    }
}

/// A tool offered to the model.
#[derive(Debug, Serialize)]
pub struct WireTool {
    /// Name the model calls the tool by.
    pub name: String,
    /// What the tool does, written for the model.
    pub description: String,
    /// JSON Schema for the arguments. Named `input_schema` here, where the
    /// chat completions API calls it `parameters`.
    pub input_schema: serde_json::Value,
}

impl WireTool {
    fn from_spec(spec: ToolSpec) -> Self {
        Self { name: spec.name, description: spec.description, input_schema: spec.parameters }
    }
}

/// One streamed event of a response.
///
/// The API frames a response as a sequence of typed events rather than
/// uniform chunks: blocks open, deltas accumulate into whichever block is
/// open, blocks close, and the message reports why it stopped.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The response is starting. Carries the input token count.
    MessageStart {
        /// The message being started.
        message: StartedMessage,
    },
    /// A content block is opening at `index`.
    ContentBlockStart {
        /// Which block, for tying deltas to it.
        index: usize,
        /// What kind of block, and its non-streamed fields.
        content_block: BlockStart,
    },
    /// A content block at `index` is growing.
    ContentBlockDelta {
        /// Which block this adds to.
        index: usize,
        /// What it adds.
        delta: Delta,
    },
    /// A content block is complete.
    ContentBlockStop {
        /// Which block finished.
        index: usize,
    },
    /// The response is ending, with the reason and output token count.
    MessageDelta {
        /// Why it stopped.
        delta: MessageDelta,
        /// Output tokens, reported here rather than at the start.
        #[serde(default)]
        usage: Option<WireUsage>,
    },
    /// The response has ended.
    MessageStop,
    /// A keepalive. Carries nothing and means nothing but "still here".
    Ping,
    /// The server reporting a failure mid-stream.
    Error {
        /// What went wrong.
        error: WireError,
    },
}

/// The opening of a response.
#[derive(Debug, Deserialize)]
pub struct StartedMessage {
    /// Input token counts, known before any output exists.
    #[serde(default)]
    pub usage: Option<WireUsage>,
}

/// What kind of content block is opening.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockStart {
    /// Spoken text.
    Text {
        /// Text already present, usually empty.
        #[serde(default)]
        text: String,
    },
    /// The model's reasoning, which is never spoken.
    Thinking {
        /// Reasoning already present, usually empty.
        #[serde(default)]
        thinking: String,
    },
    /// A tool call. The name and id arrive here; the arguments stream in as
    /// `input_json_delta` fragments.
    ToolUse {
        /// The id a result must quote.
        id: String,
        /// Which tool.
        name: String,
    },
    /// A block type this provider has no mapping for, ignored rather than
    /// treated as a failure: the API grows block types, and one Conduit does
    /// not speak is not a reason to fail a turn.
    #[serde(other)]
    Other,
}

/// What a `content_block_delta` adds to its block.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    /// Spoken text.
    TextDelta {
        /// The text.
        text: String,
    },
    /// Reasoning text, kept apart from spoken text so it is never said aloud.
    ThinkingDelta {
        /// The reasoning.
        thinking: String,
    },
    /// A fragment of a tool call's arguments, as JSON *text*: valid only once
    /// every fragment for the block has been concatenated.
    InputJsonDelta {
        /// The fragment.
        partial_json: String,
    },
    /// A delta type this provider has no mapping for. Signature deltas on
    /// thinking blocks arrive this way, and they are not content.
    #[serde(other)]
    Other,
}

/// Why the response stopped.
#[derive(Debug, Deserialize)]
pub struct MessageDelta {
    /// The reason, absent on a delta that only carries usage.
    #[serde(default)]
    pub stop_reason: Option<String>,
}

/// A failure the server reported inside the stream.
#[derive(Debug, Deserialize)]
pub struct WireError {
    /// Machine-readable class, e.g. `overloaded_error`.
    #[serde(default)]
    pub r#type: Option<String>,
    /// Human-readable explanation.
    #[serde(default)]
    pub message: Option<String>,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.r#type, &self.message) {
            (Some(kind), Some(message)) => write!(formatter, "{kind}: {message}"),
            (Some(kind), None) => formatter.write_str(kind),
            (None, Some(message)) => formatter.write_str(message),
            (None, None) => formatter.write_str("the server reported an unspecified error"),
        }
    }
}

/// Token counts, reported in two halves: input at the start, output at the end.
#[derive(Debug, Deserialize)]
pub struct WireUsage {
    /// Tokens consumed by the prompt.
    #[serde(default)]
    pub input_tokens: Option<u32>,
    /// Tokens produced by the model.
    #[serde(default)]
    pub output_tokens: Option<u32>,
}

impl WireUsage {
    /// Folds what this reports into `usage`, leaving the other half alone.
    ///
    /// The two counts arrive in different events, so each must accumulate
    /// rather than replace: taking the last report wholesale would drop the
    /// input count, which only `message_start` carries.
    pub fn fold_into(&self, usage: &mut Usage) {
        if let Some(input) = self.input_tokens {
            usage.prompt_tokens = Some(input);
        }
        if let Some(output) = self.output_tokens {
            usage.completion_tokens = Some(output);
        }
    }
}

/// Maps a vendor stop reason onto Conduit's.
///
/// Unknown reasons become [`FinishReason::Stop`]: the response did end, and
/// inventing a more specific meaning would be a guess.
#[must_use]
pub fn finish_reason(reason: &str) -> FinishReason {
    match reason {
        "tool_use" => FinishReason::ToolUse,
        "max_tokens" => FinishReason::Length,
        "refusal" => FinishReason::Cancelled,
        _ => FinishReason::Stop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::id::ToolCallId;

    fn request(messages: Vec<Message>) -> Request {
        Request::from_completion(
            CompletionRequest::new("claude-opus-5", messages),
            &serde_json::Map::new(),
            None,
        )
    }

    #[test]
    fn a_system_message_becomes_the_top_level_field() {
        // The API has no system role: a system message sent as one is a 400.
        let body = request(vec![Message::system("Be terse."), Message::user("hi")]);

        assert_eq!(body.system.as_deref(), Some("Be terse."));
        assert_eq!(body.messages.len(), 1, "only the user turn is history");
        assert_eq!(body.messages[0].role, "user");
    }

    #[test]
    fn several_system_messages_are_joined_rather_than_dropped() {
        // The runtime contributes the graph's prompt and a memory recall as
        // separate system messages, and one field has to hold both.
        let body = request(vec![
            Message::system("Be terse."),
            Message::system("You remember: the kettle is in the kitchen."),
            Message::user("where is it"),
        ]);

        let system = body.system.expect("both survive");
        assert!(system.contains("Be terse."), "{system}");
        assert!(system.contains("kettle"), "{system}");
    }

    #[test]
    fn a_configured_prompt_applies_only_when_the_history_frames_nothing() {
        let with_history = Request::from_completion(
            CompletionRequest::new("claude-opus-5", vec![Message::system("Per turn.")]),
            &serde_json::Map::new(),
            Some("Configured."),
        );
        assert_eq!(
            with_history.system.as_deref(),
            Some("Per turn."),
            "the turn's own framing is more specific than the provider's standing one"
        );

        let without = Request::from_completion(
            CompletionRequest::new("claude-opus-5", vec![Message::user("hi")]),
            &serde_json::Map::new(),
            Some("Configured."),
        );
        assert_eq!(without.system.as_deref(), Some("Configured."));
    }

    #[test]
    fn a_trailing_assistant_turn_is_dropped_because_prefill_is_a_400() {
        // The runtime appends the assistant's own words before tool results,
        // so a history ending on one is reachable — and it is now rejected as
        // an attempt to prefill the reply.
        let body =
            request(vec![Message::user("hi"), Message::assistant("let me look that up")]);

        assert_eq!(body.messages.len(), 1);
        assert_eq!(body.messages[0].role, "user");
    }

    #[test]
    fn an_assistant_turn_in_the_middle_is_kept() {
        let body = request(vec![
            Message::user("hi"),
            Message::assistant("let me look that up"),
            Message::tool_result(ToolCallId::new("toolu_x"), "42"),
        ]);

        let roles: Vec<&str> = body.messages.iter().map(|message| message.role).collect();
        assert_eq!(roles, ["user", "assistant", "user"], "the tool result is a user turn");
    }

    #[test]
    fn max_tokens_is_always_sent_because_the_api_requires_it() {
        let body = request(vec![Message::user("hi")]);
        assert_eq!(body.max_tokens, DEFAULT_MAX_TOKENS);

        let asked = Request::from_completion(
            CompletionRequest {
                max_tokens: Some(256),
                ..CompletionRequest::new("claude-opus-5", vec![Message::user("hi")])
            },
            &serde_json::Map::new(),
            None,
        );
        assert_eq!(asked.max_tokens, 256, "a request that names one is honoured");
    }

    #[test]
    fn temperature_is_never_forwarded() {
        // Current models reject `temperature` with a 400, so passing a
        // pipeline's setting through would fail every turn rather than sample
        // differently.
        let body = Request::from_completion(
            CompletionRequest {
                temperature: Some(0.7),
                ..CompletionRequest::new("claude-opus-5", vec![Message::user("hi")])
            },
            &serde_json::Map::new(),
            None,
        );

        let written = serde_json::to_value(&body).expect("serialize");
        assert!(
            written.get("temperature").is_none(),
            "temperature is not a field this API accepts: {written}"
        );
    }

    #[test]
    fn a_tool_spec_is_written_with_an_input_schema() {
        let body = Request::from_completion(
            CompletionRequest {
                tools: vec![ToolSpec {
                    name: "lights.turn_on".to_owned(),
                    description: "Turns lights on.".to_owned(),
                    parameters: serde_json::json!({ "type": "object" }),
                }],
                ..CompletionRequest::new("claude-opus-5", vec![Message::user("hi")])
            },
            &serde_json::Map::new(),
            None,
        );

        let written = serde_json::to_value(&body).expect("serialize");
        let tool = &written["tools"][0];
        assert_eq!(tool["name"], "lights.turn_on");
        assert_eq!(tool["input_schema"], serde_json::json!({ "type": "object" }));
        assert!(tool.get("parameters").is_none(), "that is the other API's name for it");
    }

    #[test]
    fn configured_defaults_reach_the_wire_and_a_request_overrides_them() {
        let mut defaults = serde_json::Map::new();
        defaults.insert("top_k".to_owned(), serde_json::json!(10));
        defaults.insert("output_config".to_owned(), serde_json::json!({ "effort": "medium" }));

        let mut completion = CompletionRequest::new("claude-opus-5", vec![Message::user("hi")]);
        completion.settings = serde_json::from_value(serde_json::json!({ "top_k": 40 }))
            .expect("a settings value");

        let body = Request::from_completion(completion, &defaults, None);

        assert_eq!(body.settings.get("top_k"), Some(&serde_json::json!(40)), "request wins");
        assert_eq!(
            body.settings.get("output_config"),
            Some(&serde_json::json!({ "effort": "medium" })),
            "default carries"
        );
    }

    #[test]
    fn the_documented_event_sequence_decodes() {
        // Verbatim from the API reference, so a shape change shows up here
        // rather than as a stream that silently yields nothing.
        let events = [
            r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":25}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}"#,
            r#"{"type":"message_stop"}"#,
            r#"{"type":"ping"}"#,
        ];

        for payload in events {
            serde_json::from_str::<Event>(payload)
                .unwrap_or_else(|error| panic!("{payload} did not decode: {error}"));
        }
    }

    #[test]
    fn a_thinking_delta_is_read_as_reasoning_rather_than_text() {
        let event: Event = serde_json::from_str(
            r#"{"type":"content_block_delta","index":0,
                "delta":{"type":"thinking_delta","thinking":"weighing it up"}}"#,
        )
        .expect("decode");

        let Event::ContentBlockDelta { delta: Delta::ThinkingDelta { thinking }, .. } = event
        else {
            panic!("a thinking delta is its own kind of delta");
        };
        assert_eq!(thinking, "weighing it up");
    }

    #[test]
    fn an_unfamiliar_delta_is_ignored_rather_than_failing_the_turn() {
        // Thinking blocks carry signature deltas, which are not content. A new
        // delta type must not be the thing that breaks a conversation.
        let event: Event = serde_json::from_str(
            r#"{"type":"content_block_delta","index":0,
                "delta":{"type":"signature_delta","signature":"abc"}}"#,
        )
        .expect("decode");

        assert!(matches!(event, Event::ContentBlockDelta { delta: Delta::Other, .. }));
    }

    #[test]
    fn a_tool_use_block_carries_the_id_a_result_must_quote() {
        let event: Event = serde_json::from_str(
            r#"{"type":"content_block_start","index":1,
                "content_block":{"type":"tool_use","id":"toolu_abc","name":"get_weather"}}"#,
        )
        .expect("decode");

        let Event::ContentBlockStart { content_block: BlockStart::ToolUse { id, name }, index } =
            event
        else {
            panic!("a tool_use block opens with its identity");
        };
        assert_eq!(index, 1);
        assert_eq!(id, "toolu_abc");
        assert_eq!(name, "get_weather");
    }

    #[test]
    fn usage_accumulates_rather_than_replacing_the_half_it_does_not_carry() {
        // Input tokens arrive at the start and output tokens at the end, so
        // taking the last report wholesale would report no prompt at all.
        let mut usage = Usage::default();
        WireUsage { input_tokens: Some(25), output_tokens: None }.fold_into(&mut usage);
        WireUsage { input_tokens: None, output_tokens: Some(12) }.fold_into(&mut usage);

        assert_eq!(usage.prompt_tokens, Some(25));
        assert_eq!(usage.completion_tokens, Some(12));
    }

    #[test]
    fn stop_reasons_map_onto_the_vocabulary() {
        assert_eq!(finish_reason("tool_use"), FinishReason::ToolUse);
        assert_eq!(finish_reason("max_tokens"), FinishReason::Length);
        assert_eq!(finish_reason("refusal"), FinishReason::Cancelled);
        assert_eq!(finish_reason("end_turn"), FinishReason::Stop);
        assert_eq!(finish_reason("stop_sequence"), FinishReason::Stop);
        // `pause_turn` and anything added later still mean the response ended.
        assert_eq!(finish_reason("pause_turn"), FinishReason::Stop);
    }

    #[test]
    fn an_error_event_reads_as_the_servers_own_explanation() {
        let event: Event = serde_json::from_str(
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        )
        .expect("decode");

        let Event::Error { error } = event else { panic!("an error event carries an error") };
        assert_eq!(error.to_string(), "overloaded_error: Overloaded");
    }
}
