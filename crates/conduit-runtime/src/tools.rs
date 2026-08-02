//! Running the tools a model asks for.
//!
//! Every requested call produces a result for the model, whatever happens: a
//! tool that fails, is denied, or does not exist is reported back as a result
//! rather than ending the turn. The model can then explain itself, which is
//! far better than silence from an assistant that was asked a question.

use std::sync::Arc;

use conduit_core::event::Event;
use conduit_core::id::{ConversationId, SpeakerId, ToolCallId};
use conduit_provider::tool::{Permission, ToolContext};
use futures_util::future::join_all;
use tracing::Instrument;

use crate::emit::Emitter;
use conduit_core::graph::ConfirmPolicy;

use crate::confirm::Confirmations;
use crate::plan::Plan;

/// A tool invocation the model asked for.
#[derive(Debug, Clone)]
pub struct Request {
    /// Identifies the call across its lifecycle.
    pub id: ToolCallId,
    /// Name the model used, which may not match any registered tool.
    pub name: String,
    /// Arguments, as the model produced them.
    pub arguments: serde_json::Value,
}

/// What to tell the model about one call.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The call being answered.
    pub id: ToolCallId,
    /// Result text, which may describe a failure.
    pub content: String,
    /// Optional text the assistant should speak directly.
    pub spoken: Option<String>,
}

/// Runs every request, concurrently, and returns one outcome per request.
///
/// Concurrency is the point: two tools the model asked for together should not
/// take turns, and none of them should wait on the assistant's speech.
pub async fn execute(
    plan: Arc<Plan>,
    emitter: Emitter,
    confirmations: Confirmations,
    conversation: ConversationId,
    speaker: Option<SpeakerId>,
    requests: Vec<Request>,
) -> Vec<Outcome> {
    let calls = requests.into_iter().map(|request| {
        let plan = Arc::clone(&plan);
        let emitter = emitter.clone();
        let confirmations = confirmations.clone();
        let span = tracing::info_span!(
            "conduit.tool",
            call = %request.id,
            tool = %request.name
        );
        async move {
            run_one(&plan, &emitter, &confirmations, conversation, speaker, request).await
        }
            .instrument(span)
    });
    join_all(calls).await
}

/// Runs a single call, reporting its lifecycle on the bus.
async fn run_one(
    plan: &Plan,
    emitter: &Emitter,
    confirmations: &Confirmations,
    conversation: ConversationId,
    speaker: Option<SpeakerId>,
    request: Request,
) -> Outcome {
    let Request { id, name, arguments } = request;
    emitter.emit(Event::ToolRequested { call: id.clone(), name: name.clone() });

    let Some(bound) = plan.core.tools.get(&name) else {
        // Models do invent tool names. Say so instead of dropping the call.
        let known: Vec<&str> = plan.core.tools.keys().map(String::as_str).collect();
        let content = format!(
            "there is no tool called `{name}`; available tools: {}",
            if known.is_empty() { "none".to_owned() } else { known.join(", ") }
        );
        tracing::warn!(tool = %name, "model requested an unknown tool");
        emitter.emit(Event::ToolFailed { call: id.clone(), error: content.clone() });
        return Outcome { id, content, spoken: None };
    };

    let tool = &bound.tool;
    let context = ToolContext { conversation, speaker };
    // A pipeline that asked to be consulted is consulted even when the tool
    // itself would have allowed the call: the operator's policy is about this
    // pipeline, and the tool cannot know about it.
    let permission = match bound.confirm {
        ConfirmPolicy::Always => {
            Permission::DenyUntilConfirmed { prompt: format!("Run `{name}`?") }
        }
        ConfirmPolicy::Never => tool.permission(&arguments, &context).await,
    };
    match permission {
        Permission::Allow => {}
        Permission::Deny { reason } => {
            let content = format!("the tool `{name}` was not permitted: {reason}");
            tracing::info!(tool = %name, %reason, "tool call denied");
            emitter.emit(Event::ToolFailed { call: id.clone(), error: content.clone() });
            return Outcome { id, content, spoken: None };
        }
        Permission::DenyUntilConfirmed { prompt } => {
            emitter.emit(Event::ToolConfirmationRequested {
                call: id.clone(),
                prompt: prompt.clone(),
            });

            if !confirmations.answerable() {
                // Refusal-shaped, and deliberately explicit about not having
                // run. Something that merely mentioned a confirmation would
                // read to a model as a granted one, and it would report the
                // action done.
                //
                // The prompt is not spoken either. Asking a question nobody
                // can answer leaves a speaker waiting; the model explains the
                // refusal instead.
                let content = format!(
                    "the tool `{name}` was NOT run: it requires confirmation \
                     (\"{prompt}\"), and this deployment cannot ask for one. Tell the \
                     user the action was not performed and that they must do it \
                     another way."
                );
                tracing::info!(tool = %name, "tool call refused pending confirmation");
                return Outcome { id, content, spoken: None };
            }

            tracing::info!(tool = %name, "waiting for confirmation");
            if !confirmations.wait(&id).await {
                let content = format!(
                    "the tool `{name}` was NOT run: the user refused it. Tell them the \
                     action was not performed."
                );
                tracing::info!(tool = %name, "tool call refused by the user");
                emitter.emit(Event::ToolFailed { call: id.clone(), error: content.clone() });
                return Outcome { id, content, spoken: None };
            }
            tracing::info!(tool = %name, "tool call confirmed");
        }
    }

    emitter.emit(Event::ToolStarted { call: id.clone() });
    let started = std::time::Instant::now();

    match tool.invoke(arguments, context).await {
        Ok(output) => {
            let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            emitter.emit(Event::ToolCompleted { call: id.clone(), duration_ms });
            Outcome { id, content: output.value.to_string(), spoken: output.spoken }
        }
        Err(error) => {
            let content = format!("the tool `{name}` failed: {error}");
            tracing::error!(tool = %name, %error, "tool call failed");
            emitter.emit(Event::ToolFailed { call: id.clone(), error: content.clone() });
            Outcome { id, content, spoken: None }
        }
    }
}
