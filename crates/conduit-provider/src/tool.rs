//! Tool execution interface.
//!
//! Tools are the pipeline's side effects: turning on a light, querying a
//! calendar, calling an HTTP API. They are the only place where a model's
//! output reaches the outside world, which is why permissions live here.

use conduit_core::id::{ConversationId, SpeakerId};
use conduit_core::Result;
use serde::{Deserialize, Serialize};

use crate::llm::ToolSpec;
use crate::Provider;

/// Who a tool is running on behalf of, and where.
///
/// Passed to every invocation so tools can enforce per-speaker policy rather
/// than trusting the model's arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolContext {
    /// The conversation that requested the tool.
    pub conversation: ConversationId,
    /// The identified speaker, or `None` when the voice is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<SpeakerId>,
}

/// What a tool produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Structured result returned to the model.
    pub value: serde_json::Value,
    /// Optional phrasing for the assistant to speak instead of summarizing
    /// `value` itself — useful for short confirmations like "done".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoken: Option<String>,
}

impl ToolOutput {
    /// A result with no dedicated spoken form.
    #[must_use]
    pub fn new(value: serde_json::Value) -> Self {
        Self { value, spoken: None }
    }

    /// Sets the phrasing the assistant should speak.
    #[must_use]
    pub fn with_spoken(mut self, spoken: impl Into<String>) -> Self {
        self.spoken = Some(spoken.into());
        self
    }
}

/// Whether an invocation may proceed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Permission {
    /// Run without asking.
    Allow,
    /// Ask the speaker for confirmation first.
    Confirm {
        /// Question to put to the speaker.
        prompt: String,
    },
    /// Refuse.
    Deny {
        /// Why the invocation was refused, spoken back to the speaker.
        reason: String,
    },
}

/// A callable side effect.
#[async_trait::async_trait]
pub trait Tool: Provider {
    /// The schema advertised to the model.
    fn spec(&self) -> ToolSpec;

    /// Decides whether this invocation may run.
    ///
    /// Checked before [`Tool::invoke`], so a denial costs nothing. The
    /// default allows everything; tools with side effects should override.
    async fn permission(
        &self,
        _arguments: &serde_json::Value,
        _context: &ToolContext,
    ) -> Permission {
        Permission::Allow
    }

    /// Runs the tool.
    ///
    /// Implementations must be safe to abandon: the caller drops the future
    /// on timeout or barge-in, so any in-flight work should be cancellation
    /// safe or idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error if the tool fails. The error is reported to the model
    /// so it can recover or explain the failure.
    async fn invoke(
        &self,
        arguments: serde_json::Value,
        context: ToolContext,
    ) -> Result<ToolOutput>;
}
