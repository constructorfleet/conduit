//! The JSON shapes of the chat completions API.
//!
//! Kept separate from the provider so the mapping between Conduit's vocabulary
//! and the vendor's is in one place and readable on its own.

use conduit_core::event::FinishReason;
use conduit_provider::llm::{CompletionRequest, Message, Role, ToolSpec, Usage};
use serde::{Deserialize, Serialize};

/// A streaming chat completions request.
#[derive(Debug, Serialize)]
pub struct Request {
    /// Model identifier, passed through untouched.
    pub model: String,
    /// Conversation history, oldest first.
    pub messages: Vec<WireMessage>,
    /// Always true: Conduit only ever streams.
    pub stream: bool,
    /// Tools the model may call.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireTool>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Cap on generated tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Provider-specific settings, sent alongside the documented fields.
    ///
    /// These reached the vendor untouched as an untyped blob before; they are
    /// now whatever the provider's declared settings schema admitted, which is
    /// why they can be flattened in without a second look.
    #[serde(flatten)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

impl Request {
    /// Translates a Conduit request into the vendor's shape.
    pub fn from_completion(request: CompletionRequest) -> Self {
        Self {
            model: request.model,
            messages: request.messages.into_iter().map(WireMessage::from_message).collect(),
            stream: true,
            tools: request.tools.into_iter().map(WireTool::from_spec).collect(),
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            settings: request.settings.as_map().clone(),
        }
    }
}

/// One message in the conversation history.
#[derive(Debug, Serialize)]
pub struct WireMessage {
    /// Vendor role name.
    pub role: &'static str,
    /// Message text.
    pub content: String,
    /// Present only on tool results, carrying the id the model issued.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl WireMessage {
    fn from_message(message: Message) -> Self {
        Self {
            role: match message.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            },
            content: message.content,
            tool_call_id: message.tool_call.map(|id| id.as_str().to_owned()),
        }
    }
}

/// A tool offered to the model.
#[derive(Debug, Serialize)]
pub struct WireTool {
    /// Always `"function"`; the API has no other tool type.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// The callable definition.
    pub function: WireFunction,
}

impl WireTool {
    fn from_spec(spec: ToolSpec) -> Self {
        Self {
            kind: "function",
            function: WireFunction {
                name: spec.name,
                description: spec.description,
                parameters: spec.parameters,
            },
        }
    }
}

/// The callable half of a tool definition.
#[derive(Debug, Serialize)]
pub struct WireFunction {
    /// Name the model calls the tool by.
    pub name: String,
    /// What the tool does, written for the model.
    pub description: String,
    /// JSON Schema for the arguments.
    pub parameters: serde_json::Value,
}

/// One streamed chunk of a response.
#[derive(Debug, Deserialize)]
pub struct Chunk {
    /// Choices carried by this chunk.
    #[serde(default)]
    pub choices: Vec<Choice>,
    /// Token counts, usually only on the final chunk.
    #[serde(default)]
    pub usage: Option<WireUsage>,
}

/// One choice within a chunk. Conduit only ever asks for one.
#[derive(Debug, Deserialize)]
pub struct Choice {
    /// What this chunk adds.
    #[serde(default)]
    pub delta: Delta,
    /// Present on the last chunk of a choice.
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// The incremental content of a choice.
#[derive(Debug, Default, Deserialize)]
pub struct Delta {
    /// Spoken text.
    #[serde(default)]
    pub content: Option<String>,
    /// Reasoning text, which several compatible servers expose under this
    /// name. Kept apart from `content` so it is never spoken.
    #[serde(default, alias = "reasoning")]
    pub reasoning_content: Option<String>,
    /// Tool call fragments.
    #[serde(default)]
    pub tool_calls: Vec<ToolCallDelta>,
}

/// A fragment of a tool call.
///
/// The model sends these in pieces: an id and name once, then the arguments as
/// a string built up over any number of later fragments. `index` is what ties
/// the fragments of one call together.
#[derive(Debug, Deserialize)]
pub struct ToolCallDelta {
    /// Groups the fragments belonging to one call.
    #[serde(default)]
    pub index: usize,
    /// The call id, sent once.
    #[serde(default)]
    pub id: Option<String>,
    /// Name and argument fragments.
    #[serde(default)]
    pub function: Option<FunctionDelta>,
}

/// The name and argument fragments of a tool call.
#[derive(Debug, Deserialize)]
pub struct FunctionDelta {
    /// Tool name, sent once.
    #[serde(default)]
    pub name: Option<String>,
    /// A fragment of the JSON argument text.
    #[serde(default)]
    pub arguments: Option<String>,
}

/// Token counts, when the server reports them.
#[derive(Debug, Deserialize)]
pub struct WireUsage {
    /// Tokens consumed by the prompt.
    #[serde(default)]
    pub prompt_tokens: Option<u32>,
    /// Tokens produced by the model.
    #[serde(default)]
    pub completion_tokens: Option<u32>,
}

impl From<WireUsage> for Usage {
    fn from(usage: WireUsage) -> Self {
        Self { prompt_tokens: usage.prompt_tokens, completion_tokens: usage.completion_tokens }
    }
}

/// Maps a vendor finish reason onto Conduit's.
///
/// Unknown reasons become [`FinishReason::Stop`]: the response did end, and
/// inventing a more specific meaning would be a guess.
#[must_use]
pub fn finish_reason(reason: &str) -> FinishReason {
    match reason {
        "tool_calls" | "function_call" => FinishReason::ToolUse,
        "length" | "max_tokens" => FinishReason::Length,
        "content_filter" => FinishReason::Cancelled,
        _ => FinishReason::Stop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::id::ToolCallId;

    #[test]
    fn tool_results_carry_the_models_own_id() {
        let message =
            WireMessage::from_message(Message::tool_result(ToolCallId::new("call_x"), "42"));
        assert_eq!(message.role, "tool");
        assert_eq!(message.tool_call_id.as_deref(), Some("call_x"));
    }

    #[test]
    fn ordinary_messages_carry_no_tool_id() {
        let message = WireMessage::from_message(Message::user("hi"));
        assert_eq!(message.tool_call_id, None);
    }

    #[test]
    fn finish_reasons_map_onto_the_vocabulary() {
        assert_eq!(finish_reason("tool_calls"), FinishReason::ToolUse);
        assert_eq!(finish_reason("length"), FinishReason::Length);
        assert_eq!(finish_reason("stop"), FinishReason::Stop);
        // An unfamiliar reason still means the response ended.
        assert_eq!(finish_reason("something_new"), FinishReason::Stop);
    }
}
