//! Publishing events on behalf of one turn.

use conduit_core::audio::AudioFormat;
use conduit_core::bus::EventBus;
use conduit_core::event::{Envelope, Event};
use conduit_core::id::{ConversationId, DeviceId, TraceId};
use conduit_provider::stt::AudioChunk;
use conduit_provider::ChunkStream;
use futures_util::StreamExt;

/// Publishes events stamped with one turn's correlation ids.
///
/// Cloning yields a handle to the same turn, so work that runs concurrently —
/// tools, synthesis — still reports under one trace.
#[derive(Debug, Clone)]
pub struct Emitter {
    bus: EventBus,
    trace: TraceId,
    conversation: ConversationId,
    /// Which satellite this turn belongs to, when the caller knew.
    device: Option<DeviceId>,
    /// The format captured audio arrives in, reported by the capture events.
    format: AudioFormat,
}

impl Emitter {
    /// Creates an emitter for a fresh turn capturing audio in `format`.
    pub fn new(bus: EventBus, format: AudioFormat) -> Self {
        Self {
            bus,
            trace: TraceId::new(),
            conversation: ConversationId::new(),
            device: None,
            format,
        }
    }

    /// Tags every event from this turn with the device it came from.
    ///
    /// This is what makes `/v1/events?device=` select anything. The identity has
    /// to come from an authenticated device token, not from a hostname or a
    /// client-supplied field, or the filter would select whatever a caller
    /// claimed.
    #[must_use]
    pub const fn with_device(mut self, device: DeviceId) -> Self {
        self.device = Some(device);
        self
    }

    /// Publishes `event` for this turn.
    pub fn emit(&self, event: Event) {
        let mut envelope =
            Envelope::new(self.trace, event).with_conversation(self.conversation);
        if let Some(device) = self.device {
            envelope = envelope.with_device(device);
        }
        self.bus.publish(envelope);
    }

    /// The conversation every event in this turn belongs to.
    pub const fn conversation(&self) -> ConversationId {
        self.conversation
    }

    /// Wraps `audio` in a stream that reports the capture stage as it flows.
    ///
    /// Publishes `AudioStarted` before the first chunk, `AudioChunkReceived`
    /// per chunk, and `AudioFinished` when the stream ends — including when it
    /// ends because the utterance was abandoned, since a capture that stopped
    /// early is exactly what an operator watching this wants to see.
    ///
    /// The audio itself is passed straight through and never buffered: this is
    /// the live path to the recognizer, and holding a chunk back to describe it
    /// would delay the transcript to produce a log line. Events carry the size
    /// of each chunk, not its samples, so nothing here copies audio onto the
    /// bus.
    ///
    /// `AudioStarted` is published on the first chunk rather than eagerly,
    /// because a device that connects and sends nothing has not started
    /// capturing anything.
    pub fn observe_capture(&self, audio: ChunkStream<AudioChunk>) -> ChunkStream<AudioChunk> {
        let emitter = self.clone();
        let format = self.format;

        Box::pin(futures_util::stream::unfold(
            (audio, emitter, format, State::default()),
            |(mut audio, emitter, format, mut state)| async move {
                match audio.next().await {
                    Some(item) => {
                        if let Ok(chunk) = &item {
                            if !state.started {
                                state.started = true;
                                emitter.emit(Event::AudioStarted { format });
                            }
                            state.bytes += chunk.data.len();
                            emitter.emit(Event::AudioChunkReceived {
                                sequence: chunk.sequence,
                                bytes: chunk.data.len(),
                            });
                        }
                        Some((item, (audio, emitter, format, state)))
                    }
                    None => {
                        // Only when something was captured: a stream that was
                        // always empty has no capture to have finished.
                        if state.started {
                            emitter.emit(Event::AudioFinished {
                                // Unknown for compressed encodings, whose
                                // bitrate is variable. Zero is honest there:
                                // the alternative is a duration derived from
                                // the wrong bytes-per-sample.
                                duration_ms: format.duration_ms(state.bytes).unwrap_or(0),
                            });
                        }
                        None
                    }
                }
            },
        ))
    }
}

/// What [`Emitter::observe_capture`] has to remember between chunks.
#[derive(Debug, Default)]
struct State {
    /// Whether `AudioStarted` has been published.
    started: bool,
    /// Bytes seen so far, for the duration reported at the end.
    bytes: usize,
}
