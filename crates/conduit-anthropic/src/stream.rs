//! Turning a streamed Messages response into [`Completion`] items.
//!
//! The API frames a response as blocks that open, accumulate deltas, and close.
//! Two things make this more than a parse: a tool call's arguments arrive as
//! JSON *text* split across any number of fragments and mean nothing until
//! they are concatenated, and the stream must end with exactly one `Finished`
//! even when the server simply stops talking.

use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;

use conduit_core::id::ToolCallId;
use conduit_core::{Error, Result};
use conduit_http::sse::Decoder;
use conduit_http::Failure;
use conduit_provider::llm::{Completion, Usage};
use conduit_provider::ChunkStream;
use futures_util::{Stream, StreamExt};

use crate::wire;

/// A tool call being assembled from one block's fragments.
#[derive(Debug)]
struct Partial {
    id: String,
    name: String,
    /// Arguments as JSON text, appended to across fragments.
    arguments: String,
}

/// The state one response stream carries between polls.
struct State {
    bytes: Pin<Box<dyn Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    decoder: Decoder,
    /// Items decoded but not yet handed to the caller.
    ready: VecDeque<Result<Completion>>,
    /// Tool calls under construction, keyed by the block index that groups
    /// their fragments.
    calls: BTreeMap<usize, Partial>,
    usage: Usage,
    /// Whether a `Finished` has been emitted.
    finished: bool,
    /// Whether the response body is exhausted.
    ended: bool,
    provider: String,
}

impl State {
    /// Queues a failure and stops reading.
    fn fail(&mut self, failure: Failure) {
        self.ready.push_back(Err(Error::provider(&self.provider, failure)));
        self.ended = true;
    }
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
                    // A reply that stops arriving mid-sentence is a stall, and
                    // the classification is what lets a caller retry it.
                    state.fail(Failure::transport(&error));
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
    let event: wire::Event = match serde_json::from_str(payload) {
        Ok(event) => event,
        Err(error) => {
            // An event we cannot read means the rest of the response is not
            // trustworthy either; skipping it would silently truncate a reply.
            tracing::error!(%error, payload, "unreadable message event");
            state.fail(Failure::malformed(format!("unreadable message event: {error}")));
            return;
        }
    };

    match event {
        wire::Event::MessageStart { message } => {
            if let Some(usage) = message.usage {
                usage.fold_into(&mut state.usage);
            }
        }
        wire::Event::ContentBlockStart { index, content_block } => {
            open_block(state, index, content_block);
        }
        wire::Event::ContentBlockDelta { index, delta } => apply_delta(state, index, delta),
        // A block closing needs no action: a tool call is held until the
        // response ends, because the runtime wants every call in one batch.
        wire::Event::ContentBlockStop { .. } | wire::Event::MessageStop | wire::Event::Ping => {
        }
        wire::Event::MessageDelta { delta, usage } => {
            if let Some(usage) = usage {
                usage.fold_into(&mut state.usage);
            }
            if let Some(reason) = delta.stop_reason {
                finish(state, Some(wire::finish_reason(&reason)));
            }
        }
        wire::Event::Error { error } => {
            // The server gave up mid-stream. Its own words are the most useful
            // thing to report, and the turn is over either way.
            tracing::warn!(provider = %state.provider, %error, "the model server failed mid-stream");
            state.fail(Failure::malformed(error.to_string()));
        }
    }
}

/// Records a block that is opening, and any content it arrived with.
fn open_block(state: &mut State, index: usize, block: wire::BlockStart) {
    match block {
        wire::BlockStart::Text { text } => {
            if !text.is_empty() {
                state.ready.push_back(Ok(Completion::Token { delta: text }));
            }
        }
        wire::BlockStart::Thinking { thinking } => {
            if !thinking.is_empty() {
                state.ready.push_back(Ok(Completion::Reasoning { delta: thinking }));
            }
        }
        wire::BlockStart::ToolUse { id, name } => {
            state.calls.insert(index, Partial { id, name, arguments: String::new() });
        }
        // A block type this provider does not speak. The API grows them, and
        // one Conduit has no mapping for is not a reason to fail a turn.
        wire::BlockStart::Other => {
            tracing::debug!(index, "ignoring a content block of an unfamiliar type");
        }
    }
}

/// Applies a delta to whichever block is at `index`.
fn apply_delta(state: &mut State, index: usize, delta: wire::Delta) {
    match delta {
        wire::Delta::TextDelta { text } => {
            if !text.is_empty() {
                state.ready.push_back(Ok(Completion::Token { delta: text }));
            }
        }
        wire::Delta::ThinkingDelta { thinking } => {
            if !thinking.is_empty() {
                state.ready.push_back(Ok(Completion::Reasoning { delta: thinking }));
            }
        }
        wire::Delta::InputJsonDelta { partial_json } => match state.calls.get_mut(&index) {
            Some(partial) => partial.arguments.push_str(&partial_json),
            // Arguments for a block that never opened as a tool call. Nothing
            // useful can be done with them, and inventing a call would put a
            // nameless tool in front of the runtime.
            None => {
                tracing::warn!(index, "argument fragment for a block that is not a tool call")
            }
        },
        wire::Delta::Other => {}
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

    for (_, partial) in std::mem::take(&mut state.calls) {
        state.ready.push_back(Ok(Completion::ToolCall {
            id: ToolCallId::new(partial.id),
            name: partial.name,
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
/// A tool call with no arguments streams no fragments at all, so empty is an
/// empty object rather than a failure. Malformed text is passed through for the
/// tool to reject, which beats failing the whole turn.
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

    /// Runs `payloads` through the decoder as one response.
    fn decoded(payloads: &[&str]) -> Vec<Result<Completion>> {
        let mut state = State {
            bytes: Box::pin(futures_util::stream::empty()),
            decoder: Decoder::new(),
            ready: VecDeque::new(),
            calls: BTreeMap::new(),
            usage: Usage::default(),
            finished: false,
            ended: false,
            provider: "anthropic".to_owned(),
        };
        for payload in payloads {
            decode(&mut state, payload);
        }
        if !state.ended {
            finish(&mut state, None);
        }
        state.ready.into_iter().collect()
    }

    #[test]
    fn empty_arguments_become_an_empty_object() {
        assert_eq!(arguments(""), serde_json::json!({}));
    }

    #[test]
    fn malformed_arguments_are_passed_through_for_the_tool_to_reject() {
        assert_eq!(arguments("{oops"), serde_json::Value::String("{oops".to_owned()));
    }

    #[test]
    fn a_text_response_becomes_tokens_and_one_finish() {
        let items = decoded(&[
            r#"{"type":"message_start","message":{"usage":{"input_tokens":25}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}"#,
            r#"{"type":"message_stop"}"#,
        ]);

        let items: Vec<Completion> =
            items.into_iter().map(|item| item.expect("no failures")).collect();
        assert_eq!(
            items,
            [
                Completion::Token { delta: "Hel".to_owned() },
                Completion::Token { delta: "lo".to_owned() },
                Completion::Finished {
                    reason: conduit_core::event::FinishReason::Stop,
                    usage: Usage { prompt_tokens: Some(25), completion_tokens: Some(12) },
                },
            ]
        );
    }

    #[test]
    fn an_empty_opening_block_emits_nothing() {
        // Every text block opens with `"text": ""`, and emitting that would put
        // an empty token in front of the sentence splitter on every response.
        let items = decoded(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ]);

        assert_eq!(items.len(), 1, "only the finish");
    }

    #[test]
    fn thinking_never_arrives_as_speakable_text() {
        // Reasoning that leaked into `Token` would be spoken aloud.
        let items = decoded(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"weighing it"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Yes"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
        ]);

        let items: Vec<Completion> =
            items.into_iter().map(|item| item.expect("no failures")).collect();
        assert!(
            items.contains(&Completion::Reasoning { delta: "weighing it".to_owned() }),
            "{items:?}"
        );
        assert!(items.contains(&Completion::Token { delta: "Yes".to_owned() }), "{items:?}");
        assert!(
            !items.contains(&Completion::Token { delta: "weighing it".to_owned() }),
            "reasoning must never be spoken: {items:?}"
        );
    }

    #[test]
    fn a_tool_calls_arguments_are_assembled_from_their_fragments() {
        // Each fragment is invalid JSON on its own; only the concatenation is
        // a document, which is the whole reason this is not a simple parse.
        let items = decoded(&[
            r#"{"type":"content_block_start","index":0,
                "content_block":{"type":"tool_use","id":"toolu_abc","name":"get_weather"}}"#,
            r#"{"type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta","partial_json":"{\"city\":"}}"#,
            r#"{"type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta","partial_json":"\"Denver\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
        ]);

        let items: Vec<Completion> =
            items.into_iter().map(|item| item.expect("no failures")).collect();
        assert_eq!(
            items,
            [
                Completion::ToolCall {
                    id: ToolCallId::new("toolu_abc"),
                    name: "get_weather".to_owned(),
                    arguments: serde_json::json!({ "city": "Denver" }),
                },
                Completion::Finished {
                    reason: conduit_core::event::FinishReason::ToolUse,
                    usage: Usage::default(),
                },
            ],
            "the call comes before the finish, and carries the id a result must quote"
        );
    }

    #[test]
    fn two_tool_calls_in_one_response_do_not_mix_their_arguments() {
        // The block index is what keeps them apart; interleaved fragments were
        // the failure this guards.
        let items = decoded(&[
            r#"{"type":"content_block_start","index":0,
                "content_block":{"type":"tool_use","id":"toolu_a","name":"first"}}"#,
            r#"{"type":"content_block_start","index":1,
                "content_block":{"type":"tool_use","id":"toolu_b","name":"second"}}"#,
            r#"{"type":"content_block_delta","index":1,
                "delta":{"type":"input_json_delta","partial_json":"{\"b\":2}"}}"#,
            r#"{"type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta","partial_json":"{\"a\":1}"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
        ]);

        let calls: Vec<(String, serde_json::Value)> = items
            .into_iter()
            .filter_map(|item| match item.expect("no failures") {
                Completion::ToolCall { name, arguments, .. } => Some((name, arguments)),
                _ => None,
            })
            .collect();

        assert_eq!(
            calls,
            [
                ("first".to_owned(), serde_json::json!({ "a": 1 })),
                ("second".to_owned(), serde_json::json!({ "b": 2 })),
            ]
        );
    }

    #[test]
    fn a_response_that_just_stops_still_finishes_exactly_once() {
        // The runtime waits for a `Finished`; a stream that ends without one
        // would leave a turn hanging.
        let items = decoded(&[
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        ]);

        let finishes = items
            .iter()
            .filter(|item| {
                matches!(item.as_ref().expect("no failures"), Completion::Finished { .. })
            })
            .count();
        assert_eq!(finishes, 1);
    }

    #[test]
    fn a_stop_reason_and_the_end_of_the_body_do_not_finish_twice() {
        let mut state = State {
            bytes: Box::pin(futures_util::stream::empty()),
            decoder: Decoder::new(),
            ready: VecDeque::new(),
            calls: BTreeMap::new(),
            usage: Usage::default(),
            finished: false,
            ended: false,
            provider: "anthropic".to_owned(),
        };
        decode(&mut state, r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#);
        finish(&mut state, None);

        assert_eq!(state.ready.len(), 1, "the second finish is not emitted");
    }

    #[test]
    fn an_error_event_ends_the_stream_with_the_servers_explanation() {
        let items = decoded(&[
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        ]);

        let error = items
            .into_iter()
            .filter_map(std::result::Result::err)
            .next()
            .expect("the failure reaches the caller");
        assert!(error.to_string().contains("Overloaded"), "{error}");
    }

    #[test]
    fn an_unreadable_event_is_a_failure_rather_than_a_silent_truncation() {
        let items = decoded(&[r#"{"type":"message_delta","delta":"not an object"}"#]);

        assert!(
            items.iter().any(std::result::Result::is_err),
            "a reply that cannot be read must not look like a reply that ended"
        );
    }
}
