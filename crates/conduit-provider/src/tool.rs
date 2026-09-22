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
    /// Ask the speaker before running, if anything can hear the answer.
    ///
    /// The runtime publishes `prompt` and waits for a device to answer it. A
    /// deployment nothing is listening to — no device has called
    /// `Confirmations::listen` — cannot ask anyone, so the call is refused
    /// immediately rather than left waiting on a question nobody will ever
    /// see. A question that *is* heard but never answered is different: there
    /// is no tool-specific refusal for it. The call simply waits until the
    /// turn's overall idle deadline gives up and abandons the whole turn — not
    /// just this call. Either way a lock or a purchase never goes through on
    /// the strength of silence.
    ///
    /// This is the whole reason the variant is not called `Allow`: an unasked
    /// or unanswered question that ran anyway would be more dangerous than
    /// [`Permission::Deny`] for exactly the tools that need it most, because a
    /// model told nothing happened will not report a lock opened or a
    /// purchase made — one told it ran will.
    Confirm {
        /// The question a speaker must answer, published on the bus, spoken
        /// to whichever device can answer it, and reported to the model so it
        /// can say what was blocked if nothing does.
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
