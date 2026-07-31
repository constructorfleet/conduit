//! Executing one conversation turn.
//!
//! A turn walks audio through recognition, reasoning, and synthesis, emitting
//! an event at every transition. Nothing is buffered that could be forwarded:
//! partial transcripts are published as they arrive, a sentence is spoken as
//! soon as it is complete rather than when the model finishes, and a preamble
//! before a tool call is spoken *while* that tool runs.

use std::sync::Arc;

use conduit_core::audio::AudioFormat;
use conduit_core::bus::EventBus;
use conduit_core::event::{CancelReason, Event, FinishReason};
use conduit_core::id::ConversationId;
use conduit_core::{Error, Result};
use conduit_provider::llm::{Completion, CompletionRequest, Message};
use conduit_provider::stt::{AudioChunk, TranscribeOptions};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest};
use conduit_provider::ChunkStream;
use futures_util::StreamExt;
use tokio::sync::mpsc::Sender;

use crate::emit::Emitter;
use crate::plan::Plan;
use crate::sentences;
use crate::tools;

/// What one call to the model produced.
struct Round {
    /// Everything the model said, whether or not it has been spoken yet.
    text: String,
    /// Text that has not yet been handed to synthesis.
    pending: String,
    /// Tools the model asked for.
    requests: Vec<tools::Request>,
}

/// Everything one turn needs, owned so it can outlive the call that spawned it.
pub struct Turn {
    plan: Arc<Plan>,
    emitter: Emitter,
    format: AudioFormat,
    output: Sender<Result<SpeechChunk>>,
    /// Chunk counter, so the caller sees one monotonic stream even though each
    /// synthesis request numbers its chunks from zero.
    sequence: u64,
    /// Whether `TtsStarted` has been published for this turn.
    speaking: bool,
    /// Audio emitted so far, reported when the turn ends.
    spoken_ms: u64,
}

impl Turn {
    /// Prepares a turn that will publish to `bus` and write audio to `output`.
    pub fn new(
        plan: Arc<Plan>,
        bus: EventBus,
        format: AudioFormat,
        output: Sender<Result<SpeechChunk>>,
    ) -> Self {
        Self {
            plan,
            emitter: Emitter::new(bus),
            format,
            output,
            sequence: 0,
            speaking: false,
            spoken_ms: 0,
        }
    }

    /// The conversation this turn's events are filed under.
    pub fn conversation(&self) -> ConversationId {
        self.emitter.conversation()
    }

    /// Runs the turn to completion.
    ///
    /// Never returns an error: failures are published as events and forwarded
    /// to the caller as stream items, because by this point there is no one
    /// left to return an error to.
    pub async fn run(mut self, audio: ChunkStream<AudioChunk>) {
        self.emitter.emit(Event::ConversationStarted);

        let Some(transcript) = self.listen(audio).await else { return };
        if self.converse(transcript).await.is_none() {
            return;
        }

        self.emitter.emit(Event::TtsFinished { duration_ms: self.spoken_ms });
        self.emitter.emit(Event::ConversationCompleted);
    }

    /// Transcribes the utterance, returning the final text.
    async fn listen(&mut self, audio: ChunkStream<AudioChunk>) -> Option<String> {
        let options = TranscribeOptions { format: self.format, ..TranscribeOptions::default() };
        let mut transcripts = match self.plan.stt.transcribe(audio, options).await {
            Ok(transcripts) => transcripts,
            Err(error) => return self.fail(&self.plan.stt_node.clone(), error).await,
        };

        let mut final_text = String::new();
        while let Some(item) = transcripts.next().await {
            match item {
                Ok(transcript) if transcript.is_final => {
                    self.emitter.emit(Event::SpeechFinal {
                        text: transcript.text.clone(),
                        confidence: transcript.confidence,
                        language: transcript.language.clone(),
                    });
                    final_text = transcript.text;
                }
                Ok(transcript) => {
                    self.emitter.emit(Event::SpeechPartial { text: transcript.text });
                }
                Err(error) => return self.fail(&self.plan.stt_node.clone(), error).await,
            }
        }

        Some(final_text)
    }

    /// Talks to the model until it stops asking for tools.
    ///
    /// Each pass speaks what the model says, runs any tools it asked for, and
    /// feeds the results back. Speech and tool execution overlap, so "let me
    /// look that up" is heard while the lookup happens rather than after it.
    async fn converse(&mut self, transcript: String) -> Option<()> {
        let mut messages = Vec::new();
        if let Some(system) = &self.plan.system {
            messages.push(Message::system(system.clone()));
        }
        messages.push(Message::user(transcript));

        for round_number in 0..self.plan.max_tool_rounds {
            let round = self.ask(&messages).await?;

            if round.requests.is_empty() {
                // Nothing left to do but finish saying it.
                let remainder = round.pending.trim().to_owned();
                if !remainder.is_empty() && !self.speak(remainder).await {
                    return None;
                }
                return Some(());
            }

            let spoke = self.run_tools_while_speaking(&round).await?;
            messages.push(assistant_message(&round.text));
            messages.extend(spoke);

            tracing::debug!(round = round_number + 1, "continuing after tool results");
        }

        // A model that never stops asking for tools would otherwise loop while
        // someone waits for an answer.
        tracing::warn!(rounds = self.plan.max_tool_rounds, "tool round limit reached");
        self.emitter.emit(Event::StageFailed {
            node: self.plan.llm_node.clone(),
            error: format!(
                "stopped after {} tool rounds without a final answer",
                self.plan.max_tool_rounds
            ),
            recovered: true,
        });
        Some(())
    }

    /// Runs the round's tools and speaks its pending text at the same time.
    ///
    /// Returns the tool results as messages for the next round.
    async fn run_tools_while_speaking(&mut self, round: &Round) -> Option<Vec<Message>> {
        let preamble = round.pending.trim().to_owned();

        // Built before the borrow below so the tool future owns everything it
        // needs and the two halves can run concurrently.
        let running = tools::execute(
            Arc::clone(&self.plan),
            self.emitter.clone(),
            self.emitter.conversation(),
            None,
            round.requests.clone(),
        );

        let speaking = async {
            if preamble.is_empty() {
                true
            } else {
                self.speak(preamble).await
            }
        };

        let (outcomes, spoke) = tokio::join!(running, speaking);
        if !spoke {
            return None;
        }

        for spoken in outcomes.iter().filter_map(|outcome| outcome.spoken.as_ref()) {
            if !self.speak(spoken.trim().to_owned()).await {
                return None;
            }
        }

        Some(
            outcomes
                .into_iter()
                .map(|outcome| Message::tool_result(outcome.id, outcome.content))
                .collect(),
        )
    }

    /// Streams one model response, speaking sentences as they complete.
    async fn ask(&mut self, messages: &[Message]) -> Option<Round> {
        let request = CompletionRequest {
            tools: self.plan.tool_specs(),
            ..CompletionRequest::new(self.plan.model.clone(), messages.to_vec())
        };
        self.emitter.emit(Event::LlmRequestStarted { model: self.plan.model.clone() });

        let mut completions = match self.plan.llm.complete(request).await {
            Ok(completions) => completions,
            Err(error) => return self.fail(&self.plan.llm_node.clone(), error).await,
        };

        let mut round =
            Round { text: String::new(), pending: String::new(), requests: Vec::new() };

        while let Some(item) = completions.next().await {
            match item {
                Ok(Completion::Token { delta }) => {
                    self.emitter.emit(Event::LlmToken { delta: delta.clone() });
                    round.text.push_str(&delta);
                    round.pending.push_str(&delta);
                    for sentence in sentences::take_complete(&mut round.pending) {
                        if !self.speak(sentence).await {
                            return None;
                        }
                    }
                }
                // Reasoning is surfaced for observability but never spoken.
                Ok(Completion::Reasoning { .. }) => {}
                Ok(Completion::ToolCall { id, name, arguments }) => {
                    round.requests.push(tools::Request { id, name, arguments });
                }
                Ok(Completion::Finished { reason, usage }) => {
                    self.emitter.emit(Event::LlmFinished {
                        reason,
                        prompt_tokens: usage.prompt_tokens,
                        completion_tokens: usage.completion_tokens,
                    });
                    if reason == FinishReason::Cancelled {
                        self.cancel(CancelReason::UserRequested);
                        return None;
                    }
                }
                // `Completion` is non-exhaustive: a provider built against a
                // newer version may send something this runtime predates.
                Ok(unknown) => {
                    tracing::debug!(?unknown, "ignoring unrecognized completion item");
                }
                Err(error) => return self.fail(&self.plan.llm_node.clone(), error).await,
            }
        }

        Some(round)
    }

    /// Synthesizes one sentence and forwards its audio.
    ///
    /// Returns `false` when the turn should stop, either because synthesis
    /// failed or because the caller stopped listening.
    async fn speak(&mut self, sentence: String) -> bool {
        if !self.speaking {
            let voice = self.plan.voice.clone().unwrap_or_else(|| "default".to_owned());
            self.emitter.emit(Event::TtsStarted { voice });
            self.speaking = true;
        }

        let request = SynthesisRequest {
            voice: self.plan.voice.clone(),
            format: self.format,
            ..SynthesisRequest::new(sentence)
        };

        let mut chunks = match self.plan.tts.synthesize(request).await {
            Ok(chunks) => chunks,
            Err(error) => {
                self.fail::<()>(&self.plan.tts_node.clone(), error).await;
                return false;
            }
        };

        while let Some(item) = chunks.next().await {
            let mut chunk = match item {
                Ok(chunk) => chunk,
                Err(error) => {
                    self.fail::<()>(&self.plan.tts_node.clone(), error).await;
                    return false;
                }
            };

            chunk.sequence = self.sequence;
            self.sequence += 1;
            self.spoken_ms += chunk.format.duration_ms(chunk.data.len()).unwrap_or(0);
            self.emitter.emit(Event::AudioStreaming {
                sequence: chunk.sequence,
                bytes: chunk.data.len(),
            });

            if self.output.send(Ok(chunk)).await.is_err() {
                // The caller hung up — barge-in, or a client that went away.
                tracing::debug!("output closed; abandoning turn");
                self.cancel(CancelReason::BargeIn);
                return false;
            }
        }

        true
    }

    /// Reports a stage failure to the bus and the caller, ending the turn.
    async fn fail<T>(&self, node: &str, error: Error) -> Option<T> {
        tracing::error!(node, %error, "pipeline stage failed");
        self.emitter.emit(Event::StageFailed {
            node: node.to_owned(),
            error: error.to_string(),
            recovered: false,
        });
        let _ = self.output.send(Err(error)).await;
        self.cancel(CancelReason::Error);
        None
    }

    /// Publishes the end of a turn that did not complete.
    fn cancel(&self, reason: CancelReason) {
        self.emitter.emit(Event::ConversationCancelled { reason });
    }
}

/// The assistant's own words, kept in history so the next round has context.
///
/// A model that said "let me look that up" and then sees no such message would
/// be liable to say it again.
fn assistant_message(text: &str) -> Message {
    Message::assistant(text.trim())
}
