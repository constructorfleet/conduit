//! Executing one conversation turn.
//!
//! A turn walks audio through recognition, reasoning, and synthesis, emitting
//! an event at every transition. Nothing is buffered that could be forwarded:
//! partial transcripts are published as they arrive, a sentence is spoken as
//! soon as it is complete rather than when the model finishes, and a preamble
//! before a tool call is spoken *while* that tool runs.

use std::sync::Arc;
use std::time::Duration;

use conduit_core::audio::AudioFormat;
use conduit_core::bus::EventBus;
use conduit_core::event::{CancelReason, Event, FinishReason, SpokenSegmentRole, Stage};
use conduit_core::id::{ConversationId, SpeakerId, TurnId};
use conduit_core::resample::Resampler;
use conduit_core::{Error, Result};
use conduit_provider::llm::{Completion, CompletionRequest, Message};
use conduit_provider::stt::{AudioChunk, TranscribeOptions};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest};
use conduit_provider::ChunkStream;
use futures_util::StreamExt;
use tokio::sync::mpsc::Sender;

use crate::deadline::{until_idle, Progress};
use crate::emit::Emitter;
use crate::plan::Plan;
use crate::sentences;
use crate::stop::{until_stopped, Stop};
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
    /// Who is speaking, when anything has identified them.
    ///
    /// Always `None` in production today: no speaker identification provider
    /// exists and nothing runs one, so the turn has nothing to learn an
    /// identity from. It is a field rather than a hardcoded argument at the
    /// call site so that wiring identification up is a matter of setting it,
    /// and so that the path from here to a tool's permission check is tested
    /// rather than discovered later.
    speaker: Option<SpeakerId>,
    /// How a client asks this turn to stop talking.
    stop: Stop,
    /// How long this turn may publish nothing before it is abandoned, and the
    /// marker every publication reports through. `None` removes the bound.
    idle: Option<Duration>,
    progress: Progress,
    /// This turn within its conversation.
    ///
    /// One per `Turn` because the runtime holds one turn per conversation
    /// today: a socket carries a single utterance and its reply. When a
    /// conversation spans several, the id changes per turn while the
    /// conversation id does not — which is the distinction `TurnStarted`
    /// exists to report.
    turn: TurnId,
    /// Chunk counter, so the caller sees one monotonic stream even though each
    /// synthesis request numbers its chunks from zero.
    sequence: u64,
    /// Whether `TtsStarted` has been published for this turn.
    speaking: bool,
    /// Audio emitted so far, reported when the turn ends.
    spoken_ms: u64,
    /// Stable identity source for spoken-segment reconstruction items.
    spoken_segments: u64,
}

impl Turn {
    /// Prepares a turn that will publish to `bus` and write audio to `output`.
    ///
    /// `idle` bounds how long the turn may publish nothing before it gives up;
    /// `None` removes the bound.
    pub fn new(
        plan: Arc<Plan>,
        bus: EventBus,
        format: AudioFormat,
        output: Sender<Result<SpeechChunk>>,
        stop: Stop,
        idle: Option<Duration>,
    ) -> Self {
        let progress = Progress::default();
        let pipeline = plan.pipeline.clone();
        Self {
            plan,
            emitter: Emitter::new(bus, pipeline, format, progress.clone()),
            format,
            output,
            speaker: None,
            stop,
            idle,
            progress,
            turn: TurnId::new(),
            sequence: 0,
            speaking: false,
            spoken_ms: 0,
            spoken_segments: 0,
        }
    }

    /// Attributes this turn to an identified speaker.
    ///
    /// The identity must come from a voice, not from a device, a token, or a
    /// pipeline: those name which satellite is connected, and a per-speaker
    /// tool policy satisfied by the wrong identity is worse than one satisfied
    /// by none.
    #[must_use]
    pub const fn with_speaker(mut self, speaker: SpeakerId) -> Self {
        self.speaker = Some(speaker);
        self
    }

    /// Tags this turn's events with the device holding the conversation.
    ///
    /// Deliberately separate from [`Turn::with_speaker`]: this says which
    /// satellite is connected and is never evidence of who is talking.
    #[must_use]
    pub fn with_device(mut self, device: conduit_core::id::DeviceId) -> Self {
        self.emitter = self.emitter.with_device(device);
        self
    }

    /// The conversation this turn's events are filed under.
    pub fn conversation(&self) -> ConversationId {
        self.emitter.conversation()
    }

    /// Runs the turn to completion, or until a client asks it to stop.
    ///
    /// Never returns an error: failures are published as events and forwarded
    /// to the caller as stream items, because by this point there is no one
    /// left to return an error to.
    pub async fn run(mut self, audio: ChunkStream<AudioChunk>) {
        self.emitter.emit(Event::ConversationStarted);
        // After the conversation, before anything a turn does: a subscriber
        // reading in order sees the conversation open, then the turn it is
        // about to spend, and can attribute everything following to that turn.
        self.emitter.emit(Event::TurnStarted { turn: self.turn });

        // Both races wrap the whole turn rather than being checked between
        // stages, so either lands during whichever await is in progress — most
        // usefully mid synthesis for a stop, and mid *anything* for a deadline,
        // since a provider can wedge at any await there is. Providers are
        // documented as safe to abandon for exactly this.
        //
        // The stop is the outer race so an explicit interruption is reported as
        // one even if the turn was also out of time: a client that pressed the
        // button did press it, and `idle_timeout` would misattribute that.
        let stop = self.stop.clone();
        let (idle, progress) = (self.idle, self.progress.clone());
        let finished =
            until_stopped(&stop, until_idle(&progress, idle, self.body(audio))).await;

        match finished {
            Some(Ok(Some(()))) => {
                self.emitter.emit(Event::TtsFinished { duration_ms: self.spoken_ms });
                self.emitter.emit(Event::ConversationCompleted);
            }
            // The turn ended itself, and published why.
            Some(Ok(None)) => {}
            Some(Err(stalled)) => {
                // Logged at warn: unlike a stop or a disconnection, this is
                // nobody's decision — it is a provider that stopped answering,
                // and an operator wants to know which stage it was.
                tracing::warn!(
                    ?stalled,
                    idle_timeout_ms = idle.map(|idle| idle.as_millis()),
                    "abandoning a turn that stopped making progress"
                );
                // Reported to the caller as well as the bus, because a device
                // holding an open socket would otherwise see a reply that simply
                // stopped, with no reason given for it.
                //
                // `elapsed` is the deadline that was exceeded rather than the
                // turn's total age: it is the number an operator can act on,
                // being the one they configured, and the age of a turn that
                // spent most of it working says nothing about the stall.
                let _ = self
                    .output
                    .send(Err(Error::Timeout {
                        operation: stalled_operation(stalled),
                        elapsed: idle.unwrap_or_default(),
                    }))
                    .await;
                self.cancel(CancelReason::IdleTimeout);
            }
            None => {
                tracing::debug!("client asked the turn to stop");
                self.cancel(CancelReason::UserRequested);
            }
        }
    }

    /// The turn itself, minus the stop race and the ending events.
    ///
    /// Returns `None` if it published its own cancellation on the way out.
    async fn body(&mut self, audio: ChunkStream<AudioChunk>) -> Option<()> {
        let transcript = self.listen(audio).await?;
        self.converse(transcript).await
    }

    /// Transcribes the utterance, returning the final text.
    async fn listen(&mut self, audio: ChunkStream<AudioChunk>) -> Option<String> {
        // Reported from here rather than by whoever produced the audio, because
        // this is the last place that sees every chunk regardless of transport:
        // a socket, a file, or a test all reach the recognizer through here.
        let audio = self.emitter.observe_capture(audio);

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
                if !remainder.is_empty()
                    && !self.speak(remainder, SpokenSegmentRole::AssistantResponse).await
                {
                    return None;
                }
                return Some(());
            }

            let model_round = (round_number + 1).try_into().unwrap_or(u32::MAX);
            let spoke = self.run_tools_while_speaking(&round, model_round).await?;
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
    async fn run_tools_while_speaking(
        &mut self,
        round: &Round,
        model_round: u32,
    ) -> Option<Vec<Message>> {
        let preamble = round.pending.trim().to_owned();
        let batch = format!("{}-tool-batch-{model_round}", self.turn);
        self.emitter.emit(Event::ToolBatchStarted {
            batch,
            calls: round.requests.iter().map(|request| request.id.clone()).collect(),
            model_round,
        });

        // Built before the borrow below so the tool future owns everything it
        // needs and the two halves can run concurrently.
        let running = tools::execute(
            Arc::clone(&self.plan),
            self.emitter.clone(),
            self.emitter.conversation(),
            self.speaker,
            round.requests.clone(),
        );

        let speaking = async {
            if preamble.is_empty() {
                true
            } else {
                self.speak(preamble, SpokenSegmentRole::AssistantPreamble).await
            }
        };

        let (outcomes, spoke) = tokio::join!(running, speaking);
        if !spoke {
            return None;
        }

        for spoken in outcomes.iter().filter_map(|outcome| outcome.spoken.as_ref()) {
            if !self.speak(spoken.trim().to_owned(), SpokenSegmentRole::ToolOutput).await {
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
                        if !self.speak(sentence, SpokenSegmentRole::AssistantResponse).await {
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
    async fn speak(&mut self, sentence: String, role: SpokenSegmentRole) -> bool {
        if !self.speaking {
            let voice = self.plan.voice.clone().unwrap_or_else(|| "default".to_owned());
            self.emitter.emit(Event::TtsStarted { voice });
            self.speaking = true;
        }
        self.spoken_segments = self.spoken_segments.saturating_add(1);
        self.emitter.emit(Event::SpokenSegmentStarted {
            segment: format!("{}-spoken-{}", self.turn, self.spoken_segments),
            role,
            text: sentence.clone(),
        });

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

        // A synthesizer speaks at whatever rate its voice was trained at, and
        // the listener asked for a particular one. Sending the other rate's
        // samples is not an error anyone hears as an error: it plays at the
        // wrong speed, which sounds like the voice slowed down and pitched low.
        let mut resampler: Option<Resampler> = None;

        while let Some(item) = chunks.next().await {
            let mut chunk = match item {
                Ok(chunk) => chunk,
                Err(error) => {
                    self.fail::<()>(&self.plan.tts_node.clone(), error).await;
                    return false;
                }
            };

            if chunk.format != self.format {
                let converter = match resampler.as_mut() {
                    Some(converter) => converter,
                    None => match Resampler::new(chunk.format, self.format) {
                        Ok(converter) => {
                            tracing::info!(
                                node = %self.plan.tts_node,
                                from = chunk.format.sample_rate,
                                to = self.format.sample_rate,
                                "resampling synthesized audio to the requested format"
                            );
                            resampler.insert(converter)
                        }
                        Err(error) => {
                            self.fail::<()>(&self.plan.tts_node.clone(), error).await;
                            return false;
                        }
                    },
                };
                match converter.push(&chunk.data) {
                    // A block-based converter has nothing to emit until it has
                    // a whole block, so an empty result is normal rather than
                    // an end of speech.
                    Ok(data) if data.is_empty() => continue,
                    Ok(data) => {
                        chunk.data = data.into();
                        chunk.format = self.format;
                    }
                    Err(error) => {
                        self.fail::<()>(&self.plan.tts_node.clone(), error).await;
                        return false;
                    }
                }
            }

            chunk.sequence = self.sequence;
            self.sequence += 1;
            self.spoken_ms += chunk.format.duration_ms(chunk.data.len()).unwrap_or(0);
            self.emitter.emit(Event::AudioStreaming {
                sequence: chunk.sequence,
                bytes: chunk.data.len(),
            });

            if self.output.send(Ok(chunk)).await.is_err() {
                // The listener left. Whether they meant to is unknowable from
                // here, so this is not reported as an interruption.
                tracing::debug!("output closed; abandoning turn");
                self.cancel(CancelReason::Disconnected);
                return false;
            }
        }

        // Whatever the converter still holds is the end of this sentence, and
        // it only comes out when asked for.
        if let Some(converter) = resampler.as_mut() {
            let tail = match converter.flush() {
                Ok(tail) => tail,
                Err(error) => {
                    self.fail::<()>(&self.plan.tts_node.clone(), error).await;
                    return false;
                }
            };
            if !tail.is_empty() {
                let chunk = SpeechChunk {
                    sequence: self.sequence,
                    format: self.format,
                    data: tail.into(),
                };
                self.sequence += 1;
                self.spoken_ms += chunk.format.duration_ms(chunk.data.len()).unwrap_or(0);
                self.emitter.emit(Event::AudioStreaming {
                    sequence: chunk.sequence,
                    bytes: chunk.data.len(),
                });
                if self.output.send(Ok(chunk)).await.is_err() {
                    tracing::debug!("output closed; abandoning turn");
                    self.cancel(CancelReason::Disconnected);
                    return false;
                }
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

/// Names the operation a timeout is reported against.
///
/// The stage is named where one is known, because "the reasoning stage stopped
/// answering" tells an operator which provider to look at while "the turn timed
/// out" leaves them to guess between four of them.
fn stalled_operation(stalled: Option<Stage>) -> String {
    match stalled {
        Some(stage) => {
            let name = serde_json::to_value(stage)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".to_owned());
            format!("the {name} stage of this turn")
        }
        // Nothing had been published yet, so there is no stage to blame.
        None => "this turn".to_owned(),
    }
}

/// The assistant's own words, kept in history so the next round has context.
///
/// A model that said "let me look that up" and then sees no such message would
/// be liable to say it again.
fn assistant_message(text: &str) -> Message {
    Message::assistant(text.trim())
}
