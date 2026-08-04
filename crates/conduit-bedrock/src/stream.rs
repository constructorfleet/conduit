//! Turning a `ConverseStream` event stream into [`Completion`] items.
//!
//! The shape is the Messages API's — blocks open, accumulate deltas, and close —
//! but the frames arrive already decoded, because the SDK owns the event stream
//! parser. What is left is the part no decoder can do for us: a tool call's
//! arguments arrive as JSON *text* split across any number of fragments and mean
//! nothing until they are concatenated, and the stream must end with exactly one
//! `Finished` even when the server simply stops talking.
//!
//! One ordering detail drives the design. Converse sends `messageStop` and *then*
//! a `metadata` event carrying the token counts, so finishing the moment a stop
//! reason arrives would report every turn as having used no tokens. The reason is
//! held instead, and the `Finished` is emitted once the counts arrive or the
//! stream ends.

use std::collections::{BTreeMap, VecDeque};

use aws_sdk_bedrockruntime::primitives::event_stream::EventReceiver;
use aws_sdk_bedrockruntime::types::error::ConverseStreamOutputError;
use aws_sdk_bedrockruntime::types::{
    ContentBlockDelta, ContentBlockStart, ConverseStreamOutput as Event,
    ReasoningContentBlockDelta, TokenUsage,
};
use conduit_core::event::FinishReason;
use conduit_core::id::ToolCallId;
use conduit_core::{Error, Result};
use conduit_http::Failure;
use conduit_provider::llm::{Completion, Usage};
use conduit_provider::ChunkStream;

use crate::{failure, wire};

/// The receiver the SDK hands back for a streamed Converse response.
type Events = EventReceiver<Event, ConverseStreamOutputError>;

/// A tool call being assembled from one block's fragments.
#[derive(Debug)]
struct Partial {
    id: String,
    name: String,
    /// Arguments as JSON text, appended to across fragments.
    arguments: String,
}

/// What one response stream carries between polls.
#[derive(Debug)]
struct State {
    /// Items decoded but not yet handed to the caller.
    ready: VecDeque<Result<Completion>>,
    /// Tool calls under construction, keyed by the block index that groups their
    /// fragments. `i32` because that is what the API numbers them with.
    calls: BTreeMap<i32, Partial>,
    usage: Usage,
    /// Why the model stopped, once it has said.
    ///
    /// Held rather than acted on, because the token counts arrive after it.
    stop: Option<FinishReason>,
    /// Whether a `Finished` has been emitted.
    finished: bool,
    /// Whether the stream is exhausted.
    ended: bool,
    provider: String,
}

impl State {
    /// A fresh stream for `provider`.
    fn new(provider: String) -> Self {
        Self {
            ready: VecDeque::new(),
            calls: BTreeMap::new(),
            usage: Usage::default(),
            stop: None,
            finished: false,
            ended: false,
            provider,
        }
    }

    /// Queues a failure and stops reading.
    fn fail(&mut self, failure: Failure) {
        self.ready.push_back(Err(Error::provider(&self.provider, failure)));
        self.ended = true;
    }
}

/// Streams the completions carried by `events`.
pub fn completions(events: Events, provider: String) -> ChunkStream<Completion> {
    Box::pin(futures_util::stream::unfold(
        (events, State::new(provider)),
        |(mut events, mut state)| async move {
            loop {
                if let Some(item) = state.ready.pop_front() {
                    return Some((item, (events, state)));
                }
                if state.ended {
                    return None;
                }

                match events.recv().await {
                    Ok(Some(event)) => decode(&mut state, event),
                    Ok(None) => {
                        state.ended = true;
                        finish(&mut state);
                    }
                    Err(error) => {
                        // A reply that stops arriving mid-sentence is worth
                        // reporting as what it was — a throttle is worth waiting
                        // out, a validation error is not.
                        let failure = failure::of_stream(&error);
                        tracing::warn!(
                            provider = %state.provider,
                            %failure,
                            "the model stream failed part way through a response"
                        );
                        state.fail(failure);
                    }
                }
            }
        },
    ))
}

/// Decodes one event into completions.
fn decode(state: &mut State, event: Event) {
    match event {
        Event::ContentBlockStart(start) => {
            let index = start.content_block_index();
            if let Some(block) = start.start() {
                open_block(state, index, block);
            }
        }
        Event::ContentBlockDelta(delta) => {
            let index = delta.content_block_index();
            if let Some(delta) = delta.delta() {
                apply_delta(state, index, delta);
            }
        }
        // A block closing needs no action: a tool call is held until the response
        // ends, because the runtime wants every call in one batch.
        Event::ContentBlockStop(_) | Event::MessageStart(_) => {}
        Event::MessageStop(stop) => {
            // Recorded rather than acted on. The token counts are still to come,
            // and finishing here would report every turn as free.
            state.stop = Some(wire::finish_reason(stop.stop_reason()));
        }
        Event::Metadata(metadata) => {
            if let Some(usage) = metadata.usage() {
                fold_usage(&mut state.usage, usage);
            }
            // The last event of a well-behaved response, and the one that makes
            // the finish complete.
            finish(state);
        }
        // An event type this build has no mapping for. The API grows them, and
        // one Conduit does not know is not a reason to fail a turn.
        _ => tracing::debug!("ignoring an event of an unfamiliar type"),
    }
}

/// Records a block that is opening.
fn open_block(state: &mut State, index: i32, block: &ContentBlockStart) {
    match block {
        ContentBlockStart::ToolUse(call) => {
            state.calls.insert(
                index,
                Partial {
                    id: call.tool_use_id().to_owned(),
                    name: call.name().to_owned(),
                    arguments: String::new(),
                },
            );
        }
        // Images and tool results open blocks this provider does not read: it
        // asks for text, and a model that sends something else is sending it
        // unprompted.
        _ => tracing::debug!(index, "ignoring a content block of an unfamiliar type"),
    }
}

/// Applies a delta to whichever block is at `index`.
fn apply_delta(state: &mut State, index: i32, delta: &ContentBlockDelta) {
    match delta {
        ContentBlockDelta::Text(text) => {
            if !text.is_empty() {
                state.ready.push_back(Ok(Completion::Token { delta: text.clone() }));
            }
        }
        ContentBlockDelta::ReasoningContent(reasoning) => match reasoning {
            ReasoningContentBlockDelta::Text(text) if !text.is_empty() => {
                state.ready.push_back(Ok(Completion::Reasoning { delta: text.clone() }));
            }
            // A signature and a redacted block are how the API lets a caller
            // hand reasoning back on a later turn. Conduit does not, and neither
            // is anything a person should hear.
            _ => {}
        },
        ContentBlockDelta::ToolUse(fragment) => match state.calls.get_mut(&index) {
            Some(partial) => partial.arguments.push_str(fragment.input()),
            // Arguments for a block that never opened as a tool call. Inventing
            // one would put a nameless tool in front of the runtime.
            None => {
                tracing::warn!(index, "argument fragment for a block that is not a tool call");
            }
        },
        _ => {}
    }
}

/// Emits any assembled tool calls, then exactly one `Finished`.
///
/// Tool calls come first because the runtime speaks a preamble while they run; a
/// call arriving after the round was declared over would be too late.
fn finish(state: &mut State) {
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
        reason: state.stop.unwrap_or(FinishReason::Stop),
        usage: state.usage,
    }));
}

/// Folds the API's token counts into Conduit's.
///
/// The counts are `i32` on the wire and unsigned here. A negative count is not a
/// number of tokens, so it is dropped rather than wrapped into an enormous one.
fn fold_usage(usage: &mut Usage, reported: &TokenUsage) {
    if let Ok(prompt) = u32::try_from(reported.input_tokens()) {
        usage.prompt_tokens = Some(prompt);
    }
    if let Ok(completion) = u32::try_from(reported.output_tokens()) {
        usage.completion_tokens = Some(completion);
    }
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
    use aws_sdk_bedrockruntime::types::{
        ContentBlockDeltaEvent, ContentBlockStartEvent, ConverseStreamMetadataEvent,
        MessageStopEvent, StopReason, ToolUseBlockDelta, ToolUseBlockStart,
    };

    /// Runs `events` through the decoder as one response.
    fn decoded(events: Vec<Event>) -> Vec<Result<Completion>> {
        let mut state = State::new("bedrock".to_owned());
        for event in events {
            decode(&mut state, event);
        }
        if !state.ended {
            finish(&mut state);
        }
        state.ready.into_iter().collect()
    }

    /// The completions of a response that did not fail.
    fn completions(events: Vec<Event>) -> Vec<Completion> {
        decoded(events).into_iter().map(|item| item.expect("no failures")).collect()
    }

    fn text(index: i32, delta: &str) -> Event {
        Event::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .content_block_index(index)
                .delta(ContentBlockDelta::Text(delta.to_owned()))
                .build()
                .expect("an index is all it needs"),
        )
    }

    fn reasoning(index: i32, delta: &str) -> Event {
        Event::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .content_block_index(index)
                .delta(ContentBlockDelta::ReasoningContent(ReasoningContentBlockDelta::Text(
                    delta.to_owned(),
                )))
                .build()
                .expect("an index is all it needs"),
        )
    }

    fn tool_use(index: i32, id: &str, name: &str) -> Event {
        Event::ContentBlockStart(
            ContentBlockStartEvent::builder()
                .content_block_index(index)
                .start(ContentBlockStart::ToolUse(
                    ToolUseBlockStart::builder()
                        .tool_use_id(id)
                        .name(name)
                        .build()
                        .expect("an id and a name"),
                ))
                .build()
                .expect("an index is all it needs"),
        )
    }

    fn tool_arguments(index: i32, fragment: &str) -> Event {
        Event::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .content_block_index(index)
                .delta(ContentBlockDelta::ToolUse(
                    ToolUseBlockDelta::builder().input(fragment).build().expect("input"),
                ))
                .build()
                .expect("an index is all it needs"),
        )
    }

    fn message_stop(reason: StopReason) -> Event {
        Event::MessageStop(
            MessageStopEvent::builder().stop_reason(reason).build().expect("a reason"),
        )
    }

    fn metadata(prompt: i32, completion: i32) -> Event {
        Event::Metadata(
            ConverseStreamMetadataEvent::builder()
                .usage(
                    TokenUsage::builder()
                        .input_tokens(prompt)
                        .output_tokens(completion)
                        .total_tokens(prompt + completion)
                        .build()
                        .expect("three counts"),
                )
                .build(),
        )
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
        let items = completions(vec![
            text(0, "Hel"),
            text(0, "lo"),
            Event::ContentBlockStop(
                aws_sdk_bedrockruntime::types::ContentBlockStopEvent::builder()
                    .content_block_index(0)
                    .build()
                    .expect("an index"),
            ),
            message_stop(StopReason::EndTurn),
            metadata(25, 12),
        ]);

        assert_eq!(
            items,
            [
                Completion::Token { delta: "Hel".to_owned() },
                Completion::Token { delta: "lo".to_owned() },
                Completion::Finished {
                    reason: FinishReason::Stop,
                    usage: Usage { prompt_tokens: Some(25), completion_tokens: Some(12) },
                },
            ]
        );
    }

    #[test]
    fn token_counts_reach_the_finish_even_though_they_arrive_after_the_stop() {
        // The ordering this module exists to handle: Converse says why it
        // stopped and only then says what it cost. Finishing on the stop would
        // report every turn as free.
        let items =
            completions(vec![text(0, "Hi"), message_stop(StopReason::EndTurn), metadata(9, 3)]);

        assert_eq!(
            items.last(),
            Some(&Completion::Finished {
                reason: FinishReason::Stop,
                usage: Usage { prompt_tokens: Some(9), completion_tokens: Some(3) },
            })
        );
    }

    #[test]
    fn an_empty_delta_emits_nothing() {
        // Blocks routinely open with an empty text delta, and emitting that
        // would put an empty token in front of the sentence splitter.
        assert_eq!(completions(vec![text(0, "")]).len(), 1, "only the finish");
    }

    #[test]
    fn reasoning_never_arrives_as_speakable_text() {
        // Reasoning that leaked into `Token` would be spoken aloud.
        let items = completions(vec![
            reasoning(0, "weighing it"),
            Event::ContentBlockDelta(
                ContentBlockDeltaEvent::builder()
                    .content_block_index(0)
                    .delta(ContentBlockDelta::ReasoningContent(
                        ReasoningContentBlockDelta::Signature("sig".to_owned()),
                    ))
                    .build()
                    .expect("an index"),
            ),
            text(1, "Yes"),
            message_stop(StopReason::EndTurn),
        ]);

        assert!(
            items.contains(&Completion::Reasoning { delta: "weighing it".to_owned() }),
            "{items:?}"
        );
        assert!(items.contains(&Completion::Token { delta: "Yes".to_owned() }), "{items:?}");
        assert!(
            !items.contains(&Completion::Token { delta: "weighing it".to_owned() }),
            "reasoning must never be spoken: {items:?}"
        );
        assert!(
            !items.contains(&Completion::Reasoning { delta: "sig".to_owned() }),
            "a signature is bookkeeping, not thought: {items:?}"
        );
    }

    #[test]
    fn a_tool_calls_arguments_are_assembled_from_their_fragments() {
        // Each fragment is invalid JSON on its own; only the concatenation is a
        // document, which is the whole reason this is not a simple parse.
        let items = completions(vec![
            tool_use(0, "tooluse_abc", "get_weather"),
            tool_arguments(0, "{\"city\":"),
            tool_arguments(0, "\"Denver\"}"),
            message_stop(StopReason::ToolUse),
        ]);

        assert_eq!(
            items,
            [
                Completion::ToolCall {
                    id: ToolCallId::new("tooluse_abc"),
                    name: "get_weather".to_owned(),
                    arguments: serde_json::json!({ "city": "Denver" }),
                },
                Completion::Finished { reason: FinishReason::ToolUse, usage: Usage::default() },
            ],
            "the call comes before the finish, and carries the id a result must quote"
        );
    }

    #[test]
    fn two_tool_calls_in_one_response_do_not_mix_their_arguments() {
        // The block index is what keeps them apart; interleaved fragments were
        // the failure this guards.
        let calls: Vec<(String, serde_json::Value)> = completions(vec![
            tool_use(0, "tooluse_a", "first"),
            tool_use(1, "tooluse_b", "second"),
            tool_arguments(1, "{\"b\":2}"),
            tool_arguments(0, "{\"a\":1}"),
            message_stop(StopReason::ToolUse),
        ])
        .into_iter()
        .filter_map(|item| match item {
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
        let finishes = completions(vec![text(0, "Hi")])
            .into_iter()
            .filter(|item| matches!(item, Completion::Finished { .. }))
            .count();

        assert_eq!(finishes, 1);
    }

    #[test]
    fn the_metadata_event_and_the_end_of_the_stream_do_not_finish_twice() {
        let mut state = State::new("bedrock".to_owned());
        decode(&mut state, message_stop(StopReason::EndTurn));
        decode(&mut state, metadata(1, 1));
        finish(&mut state);

        assert_eq!(state.ready.len(), 1, "the second finish is not emitted");
    }

    #[test]
    fn a_stream_that_reports_no_usage_still_finishes() {
        // Not every response ends in a metadata event — a stream cut short by a
        // dropped connection does not — and the turn still has to end.
        let items = completions(vec![text(0, "Hi"), message_stop(StopReason::MaxTokens)]);

        assert_eq!(
            items.last(),
            Some(&Completion::Finished {
                reason: FinishReason::Length,
                usage: Usage::default(),
            })
        );
    }

    #[test]
    fn an_impossible_token_count_is_dropped_rather_than_wrapped() {
        // The counts are signed on the wire. A negative one read as unsigned
        // would become billions of tokens on an operator's screen.
        let mut usage = Usage::default();
        fold_usage(
            &mut usage,
            &TokenUsage::builder()
                .input_tokens(-1)
                .output_tokens(7)
                .total_tokens(7)
                .build()
                .expect("three counts"),
        );

        assert_eq!(usage, Usage { prompt_tokens: None, completion_tokens: Some(7) });
    }
}
