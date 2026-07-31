//! Language model provider interface.

use conduit_core::event::FinishReason;
use conduit_core::id::ToolCallId;
use conduit_core::Result;
use serde::{Deserialize, Serialize};

use crate::{ChunkStream, Provider};

/// Who produced a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Instructions that frame the whole conversation.
    System,
    /// Input from the person speaking.
    User,
    /// A previous model response.
    Assistant,
    /// The result of a tool the model requested.
    Tool,
}

/// One entry in the conversation history sent to a model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Who produced this message.
    pub role: Role,
    /// The message text.
    pub content: String,
    /// For [`Role::Tool`], the invocation this message answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCallId>,
}

impl Message {
    /// A system message.
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: Role::System, content: content.into(), tool_call: None }
    }

    /// A user message.
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into(), tool_call: None }
    }

    /// An assistant message.
    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: content.into(), tool_call: None }
    }

    /// A tool result answering `call`.
    #[must_use]
    pub fn tool_result(call: ToolCallId, content: impl Into<String>) -> Self {
        Self { role: Role::Tool, content: content.into(), tool_call: Some(call) }
    }
}

/// A tool offered to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Name the model uses to call the tool.
    pub name: String,
    /// What the tool does, written for the model.
    pub description: String,
    /// JSON Schema describing the tool's arguments.
    pub parameters: serde_json::Value,
}

/// A request for model output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionRequest {
    /// Model identifier, e.g. `"claude-opus-5"` or `"llama3.1:8b"`.
    pub model: String,
    /// Conversation history, oldest first.
    pub messages: Vec<Message>,
    /// Tools the model may call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    /// Sampling temperature, when the provider supports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Cap on generated tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Provider-specific settings.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub extra: serde_json::Value,
}

impl CompletionRequest {
    /// A request with no tools and provider defaults for sampling.
    #[must_use]
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            extra: serde_json::Value::Null,
        }
    }
}

/// Token counts reported by a provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens consumed by the prompt.
    pub prompt_tokens: Option<u32>,
    /// Tokens produced by the model.
    pub completion_tokens: Option<u32>,
}

/// One item in a model's streamed response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Completion {
    /// A text delta. Deltas append; concatenating them yields the response.
    Token {
        /// The appended text.
        delta: String,
    },
    /// Reasoning output from models that expose it, kept separate so it is
    /// never spoken aloud.
    Reasoning {
        /// The appended reasoning text.
        delta: String,
    },
    /// The model requested a tool.
    ToolCall {
        /// Identifies this invocation for the rest of its lifecycle.
        id: ToolCallId,
        /// Which tool to run.
        name: String,
        /// Arguments, matching the tool's declared schema.
        arguments: serde_json::Value,
    },
    /// Generation ended. Always the final item of a successful stream.
    Finished {
        /// Why generation stopped.
        reason: FinishReason,
        /// Token counts, when reported.
        usage: Usage,
    },
}

/// Generates text, optionally calling tools.
#[async_trait::async_trait]
pub trait LanguageModel: Provider {
    /// Streams a response to `request`.
    ///
    /// The stream ends with exactly one [`Completion::Finished`] on success.
    /// Dropping it cancels generation, which is how barge-in is implemented.
    ///
    /// # Errors
    ///
    /// Returns an error if the request is rejected outright. Mid-stream
    /// failures surface as error items on the returned stream.
    async fn complete(&self, request: CompletionRequest) -> Result<ChunkStream<Completion>>;

    /// Models this provider can serve. Empty means any model name is passed
    /// through untouched, as with OpenAI-compatible local endpoints.
    fn models(&self) -> &[String] {
        &[]
    }

    /// Whether this provider can execute tool calls.
    fn supports_tools(&self) -> bool {
        false
    }
}
