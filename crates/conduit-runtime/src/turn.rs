//! Executing one conversation turn.
//!
//! A turn walks audio through recognition, reasoning, and synthesis, emitting
//! an event at every transition. Nothing is buffered that could be forwarded:
//! partial transcripts are published as they arrive, and a sentence is spoken
//! as soon as it is complete rather than when the model finishes.

use std::sync::Arc;

use conduit_core::audio::AudioFormat;
use conduit_core::bus::EventBus;
use conduit_core::event::{CancelReason, Envelope, Event, FinishReason};
use conduit_core::id::{ConversationId, TraceId};
use conduit_core::{Error, Result};
use conduit_provider::llm::{Completion, CompletionRequest, Message};
use conduit_provider::stt::{AudioChunk, TranscribeOptions};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest};
use conduit_provider::ChunkStream;
use futures_util::StreamExt;
use tokio::sync::mpsc::Sender;

use crate::plan::Plan;
use crate::sentences;

/// Publishes events stamped with one turn's correlation ids.
struct Emitter {
    bus: EventBus,
    trace: TraceId,
    conversation: ConversationId,
}

impl Emitter {
    /// Publishes `event` for this turn.
    fn emit(&self, event: Event) {
        self.bus.publish(Envelope::new(self.trace, event).with_conversation(self.conversation));
    }
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
            emitter: Emitter {
                bus,
                trace: TraceId::new(),
                conversation: ConversationId::new(),
            },
            format,
            output,
            sequence: 0,
            speaking: false,
            spoken_ms: 0,
        }
    }

    /// Runs the turn to completion.
    ///
    /// Never returns an error: failures are published as events and forwarded
    /// to the caller as stream items, because by this point there is no one
    /// left to return an error to.
    pub async fn run(mut self, audio: ChunkStream<AudioChunk>) {
        self.emitter.emit(Event::ConversationStarted);

        let Some(transcript) = self.listen(audio).await else { return };
        let Some(response) = self.think(transcript).await else { return };

        if !response.is_empty() {
            // Whatever the model produced after the last sentence boundary.
            if !self.speak(response).await {
                return;
            }
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

    /// Streams a model response, speaking each sentence as it completes.
    ///
    /// Returns the trailing fragment that had no sentence boundary.
    async fn think(&mut self, transcript: String) -> Option<String> {
        let mut messages = Vec::new();
        if let Some(system) = &self.plan.system {
            messages.push(Message::system(system.clone()));
        }
        messages.push(Message::user(transcript));

        let request = CompletionRequest::new(self.plan.model.clone(), messages);
        self.emitter.emit(Event::LlmRequestStarted { model: self.plan.model.clone() });

        let mut completions = match self.plan.llm.complete(request).await {
            Ok(completions) => completions,
            Err(error) => return self.fail(&self.plan.llm_node.clone(), error).await,
        };

        let mut buffer = String::new();
        while let Some(item) = completions.next().await {
            match item {
                Ok(Completion::Token { delta }) => {
                    self.emitter.emit(Event::LlmToken { delta: delta.clone() });
                    buffer.push_str(&delta);
                    for sentence in sentences::take_complete(&mut buffer) {
                        if !self.speak(sentence).await {
                            return None;
                        }
                    }
                }
                // Reasoning is surfaced for observability but never spoken.
                Ok(Completion::Reasoning { .. }) => {}
                Ok(Completion::ToolCall { id, name, .. }) => {
                    // Tools are not wired up yet; say so rather than pretend
                    // the request was handled.
                    tracing::warn!(call = %id, tool = %name, "tool calls are not executed yet");
                    self.emitter.emit(Event::ToolFailed {
                        call: id,
                        error: "tool execution is not implemented".to_owned(),
                    });
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

        Some(buffer.trim().to_owned())
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
