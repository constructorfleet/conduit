//! Turning a streamed HTTP response into [`Completion`] items.
//!
//! Two things make this more than a parse. Tool calls arrive in fragments that
//! must be reassembled before they mean anything, and the stream must end with
//! exactly one `Finished` even when the server simply stops talking.

use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;

use conduit_core::id::ToolCallId;
use conduit_core::{Error, Result};
use conduit_provider::llm::{Completion, Usage};
use conduit_provider::ChunkStream;
use futures_util::{Stream, StreamExt};

use crate::wire;
use conduit_http::sse::Decoder;
use conduit_http::Failure;

/// A tool call being assembled from fragments.
#[derive(Debug, Default)]
struct Partial {
    id: Option<String>,
    name: Option<String>,
    /// Arguments as JSON text, appended to across fragments.
    arguments: String,
}

/// The state one response stream carries between polls.
struct State {
    bytes: Pin<Box<dyn Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    decoder: Decoder,
    /// Items decoded but not yet handed to the caller.
    ready: VecDeque<Result<Completion>>,
    /// Tool calls under construction, keyed by the index that groups their
    /// fragments.
    calls: BTreeMap<usize, Partial>,
    usage: Usage,
    /// Whether a `Finished` has been emitted.
    finished: bool,
    /// Whether the response body is exhausted.
    ended: bool,
    provider: String,
}

/// Streams the completions carried by `response`.
pub fn completions(response: reqwest::Response, provider: String) -> ChunkStream<Completion> {
    let state = State {
        bytes: Box::pin(response.bytes_stream()),
        decoder: Decoder::new(),
        ready: VecDeque::new(),
        calls: BTreeMap::new(),
        usage: Usage::default(),
        finished: false,
        ended: false,
        provider,
    };

    Box::pin(futures_util::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(item) = state.ready.pop_front() {
                return Some((item, state));
            }
            if state.ended {
                return None;
            }

            match state.bytes.next().await {
                Some(Ok(packet)) => {
                    for payload in state.decoder.push(&packet) {
                        decode(&mut state, &payload);
                    }
                }
                Some(Err(error)) => {
                    state.ended = true;
                    let provider = state.provider.clone();
                    // A reply that stops arriving mid-sentence is a stall, and
                    // the classification is what lets a caller retry it.
                    state
                        .ready
                        .push_back(Err(Error::provider(provider, Failure::transport(&error))));
                }
                None => {
                    state.ended = true;
                    finish(&mut state, None);
                }
            }
        }
    }))
}

/// Decodes one SSE payload into completions.
fn decode(state: &mut State, payload: &str) {
    let chunk: wire::Chunk = match serde_json::from_str(payload) {
        Ok(chunk) => chunk,
        Err(error) => {
            // A chunk we cannot read means the rest of the response is not
            // trustworthy either; skipping it would silently truncate a reply.
            tracing::error!(%error, payload, "unreadable completion chunk");
            let provider = state.provider.clone();
            state.ready.push_back(Err(Error::provider(
                provider,
                Failure::malformed(format!("unreadable completion chunk: {error}")),
            )));
            state.ended = true;
            return;
        }
    };

    if let Some(usage) = chunk.usage {
        state.usage = usage.into();
    }

    for choice in chunk.choices {
        if let Some(reasoning) = choice.delta.reasoning_content {
            if !reasoning.is_empty() {
                state.ready.push_back(Ok(Completion::Reasoning { delta: reasoning }));
            }
        }
        if let Some(content) = choice.delta.content {
            if !content.is_empty() {
                state.ready.push_back(Ok(Completion::Token { delta: content }));
            }
        }

        for fragment in choice.delta.tool_calls {
            let partial = state.calls.entry(fragment.index).or_default();
            if let Some(id) = fragment.id {
                partial.id = Some(id);
            }
            if let Some(function) = fragment.function {
                if let Some(name) = function.name {
                    partial.name = Some(name);
                }
                if let Some(arguments) = function.arguments {
                    partial.arguments.push_str(&arguments);
                }
            }
        }

        if let Some(reason) = choice.finish_reason {
            finish(state, Some(wire::finish_reason(&reason)));
        }
    }
}

/// Emits any assembled tool calls, then exactly one `Finished`.
///
/// Tool calls come first because the runtime speaks a preamble while they run;
/// a call arriving after the round was declared over would be too late.
fn finish(state: &mut State, reason: Option<conduit_core::event::FinishReason>) {
    if state.finished {
        return;
    }
    state.finished = true;

    for (index, partial) in std::mem::take(&mut state.calls) {
        let Some(name) = partial.name else {
            tracing::warn!(index, "tool call fragment never named a tool; dropping it");
            continue;
        };
        // A model that issues no id still needs one to match the result to.
        let id = partial.id.map_or_else(ToolCallId::generate, ToolCallId::new);
        state.ready.push_back(Ok(Completion::ToolCall {
            id,
            name,
            arguments: arguments(&partial.arguments),
        }));
    }

    state.ready.push_back(Ok(Completion::Finished {
        // A server that stops without saying why has still stopped.
        reason: reason.unwrap_or(conduit_core::event::FinishReason::Stop),
        usage: state.usage,
    }));
}

/// Parses accumulated argument text.
///
/// Models do emit malformed JSON. Passing the raw text through lets the tool
/// reject it and the model try again, which beats failing the whole turn.
fn arguments(text: &str) -> serde_json::Value {
    let text = text.trim();
    if text.is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(text).unwrap_or_else(|error| {
        tracing::warn!(%error, text, "tool arguments were not valid JSON");
        serde_json::Value::String(text.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_arguments_become_an_empty_object() {
        assert_eq!(arguments(""), serde_json::json!({}));
    }

    #[test]
    fn well_formed_arguments_are_parsed() {
        assert_eq!(arguments(r#"{"a":1}"#), serde_json::json!({ "a": 1 }));
    }

    #[test]
    fn malformed_arguments_are_passed_through_for_the_tool_to_reject() {
        assert_eq!(arguments("{oops"), serde_json::Value::String("{oops".to_owned()));
    }
}
