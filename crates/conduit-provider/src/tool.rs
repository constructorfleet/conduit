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
    ///
    /// Always `None` today: nothing identifies a voice yet, so a tool with a
    /// per-speaker policy must decide what an unknown speaker may do. Never
    /// substitute the device or the conversation for it — those say which
    /// satellite is connected, not who is talking, and a policy satisfied by
    /// the wrong identity is worse than one that has none.
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
    /// Refuse until the speaker confirms — which, today, means refuse.
    ///
    /// Conduit has no way to put a question to a speaker mid-turn and collect
    /// the answer, so the runtime treats this as a denial: the tool does not
    /// run, and the model is told plainly that nothing happened and why. That
    /// is the whole point of the name. A variant that promised to ask would be
    /// more dangerous than [`Permission::Deny`] for exactly the tools that
    /// need it most, because an unasked question reads to a model like a
    /// granted one, and it will cheerfully report a lock opened or a purchase
    /// made.
    ///
    /// Use it anyway for anything that genuinely needs a human in the loop.
    /// It is refused now and asks later, once a mid-turn control channel and a
    /// bounded wait exist; a tool marked [`Permission::Allow`] to work around
    /// this would then run unconfirmed forever.
    DenyUntilConfirmed {
        /// The question a speaker would have to answer, published on the bus
        /// and reported to the model so it can say what was blocked.
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
    ///
    /// Deliberately not read off the
    /// [`Descriptor`](crate::Descriptor): the audiences differ. The descriptor
    /// says what Conduit calls this tool — the selector a graph names, the
    /// label a screen shows — while this says what the *model* must write to
    /// call it, under the tool's real name on its server, which a definition
    /// may have aliased.
    ///
    /// The argument schema is the one part they share, and an implementation
    /// should declare it once as its descriptor's settings and return it here,
    /// so an operator rendering a tool's arguments and a model filling them in
    /// read the same document.
    fn spec(&self) -> ToolSpec;

    /// Decides whether this invocation may run.
    ///
    /// Checked before [`Tool::invoke`], so a denial costs nothing. The
    /// default allows everything; tools with side effects should override.
    ///
    /// Anything but [`Permission::Allow`] means the tool is not invoked, and
    /// the model is told what was refused and why.
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
