//! Holding audio back until the wake word fires.
//!
//! A wake stage sits between the microphone and the recognizer. Everything the
//! device sends reaches the detector; nothing reaches the recognizer until a
//! phrase is accepted. That is the whole of what the stage does — and it is why
//! it cannot be a provider call in sequence like recognition is, because the
//! detector and the recognizer want the same stream at different times.

use std::sync::Arc;
use std::time::Duration;

use conduit_core::audio::AudioFormat;
use conduit_core::event::Event;
use conduit_provider::stt::AudioChunk;
use conduit_provider::wake::{WakePhrase, WakeWordDetector};
use conduit_provider::ChunkStream;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::emit::Emitter;

/// How much audio before the detection is kept and forwarded.
///
/// A detector reports an activation some way *after* the phrase ended —
/// openWakeWord scores a sliding window, so the report lands a few hundred
/// milliseconds late. Opening the gate at that instant clips the beginning of
/// what was actually asked, which reaches an operator as a recognizer that
/// drops the first word of every command.
///
/// Half a second covers that lag without replaying the wake phrase itself into
/// the recognizer, which would put "hey jarvis" at the front of every
/// transcript.
const PRE_ROLL: Duration = Duration::from_millis(500);

/// How many chunks may be queued for the detector before capture waits.
///
/// One, so that capture can never run far ahead of scoring. With a deeper
/// queue a source faster than real time — a file, a test, a device catching up
/// after a stall — hands over the whole utterance before the detector has
/// scored any of it, and the activation then lands after the audio it was
/// supposed to open the gate for has already gone by.
const DETECTOR_DEPTH: usize = 1;

/// How many gated chunks may be queued for the recognizer before capture waits.
const OUTPUT_DEPTH: usize = 32;

/// How long capture waits, once the audio has ended, for a detection that is
/// still being scored.
///
/// Without it an activation reported microseconds after the last chunk arrives
/// is a wake word that fired and a turn that heard nothing.
const SETTLE: Duration = Duration::from_millis(250);

/// Runs `audio` past `detector`, forwarding only what follows an accepted
/// activation.
///
/// Both accepted and rejected activations are published — a near miss is how an
/// operator discovers their threshold is too high — and the returned stream
/// carries the audio the recognizer should hear.
///
/// A detector that fails mid-stream surfaces the failure on the returned
/// stream, so the turn reports it against the wake node rather than silently
/// never waking.
pub fn gate(
    detector: Arc<dyn WakeWordDetector>,
    phrases: Vec<WakePhrase>,
    node: String,
    emitter: Emitter,
    format: AudioFormat,
    mut audio: ChunkStream<AudioChunk>,
) -> ChunkStream<AudioChunk> {
    let (to_detector, from_capture) =
        mpsc::channel::<conduit_core::Result<AudioChunk>>(DETECTOR_DEPTH);
    let (to_recognizer, gated) =
        mpsc::channel::<conduit_core::Result<AudioChunk>>(OUTPUT_DEPTH);
    // Flipped by the detector task the moment a phrase is accepted, and read by
    // the capture pump on every chunk. A channel rather than a shared flag
    // because the two run concurrently and the pump must see the change on the
    // very next chunk.
    let (opened, is_open) = tokio::sync::watch::channel(false);

    let detection_emitter = emitter.clone();
    let detection_node = node.clone();
    let detection_output = to_recognizer.clone();
    tokio::spawn(async move {
        let scored =
            detector.detect(Box::pin(ReceiverStream::new(from_capture)), phrases).await;
        let mut scored = match scored {
            Ok(scored) => scored,
            Err(error) => {
                tracing::warn!(node = detection_node, %error, "wake detector failed to start");
                detection_emitter.emit(Event::StageFailed {
                    node: detection_node,
                    error: error.to_string(),
                    recovered: false,
                });
                let _ = detection_output.send(Err(error)).await;
                return;
            }
        };

        while let Some(item) = scored.next().await {
            match item {
                Ok(detection) if detection.accepted => {
                    detection_emitter.emit(Event::WakeWordDetected {
                        phrase: detection.phrase.clone(),
                        confidence: detection.confidence,
                    });
                    // Sending fails once the turn has stopped listening, which
                    // is not a detector problem — the audio simply has nowhere
                    // left to go.
                    if opened.send(true).is_err() {
                        return;
                    }
                }
                Ok(detection) => {
                    // Reported rather than swallowed: a near miss is the only
                    // evidence an operator has that their threshold is wrong.
                    detection_emitter.emit(Event::WakeWordRejected {
                        phrase: detection.phrase,
                        confidence: detection.confidence,
                    });
                }
                Err(error) => {
                    tracing::warn!(node = detection_node, %error, "wake detection failed");
                    detection_emitter.emit(Event::StageFailed {
                        node: detection_node.clone(),
                        error: error.to_string(),
                        recovered: false,
                    });
                    let _ = detection_output.send(Err(error)).await;
                    return;
                }
            }
        }
    });

    tokio::spawn(async move {
        let mut pre_roll = PreRoll::new(format);
        let mut open = false;
        while let Some(chunk) = audio.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    let _ = to_recognizer.send(Err(error)).await;
                    return;
                }
            };

            // Every chunk reaches the detector, awake or not: the microphone
            // keeps listening after the assistant answers, and the next
            // activation is scored on the same session.
            if to_detector.send(Ok(chunk.clone())).await.is_err() {
                tracing::debug!("wake detector stopped reading audio");
            }

            if !open {
                open = *is_open.borrow();
                if !open {
                    pre_roll.push(chunk);
                    continue;
                }
                // The activation was reported after the phrase ended, so the
                // first moments of what was actually asked are already behind
                // us. Forward what was kept before forwarding anything new.
                for held in pre_roll.take() {
                    if to_recognizer.send(Ok(held)).await.is_err() {
                        return;
                    }
                }
            }

            if to_recognizer.send(Ok(chunk)).await.is_err() {
                return;
            }
        }

        if open {
            return;
        }
        // The audio ended while the last of it was still being scored. Closing
        // the detector's input is what lets it finish, and what it decides in
        // the moment after is still an activation: a wake word that fired and
        // a turn that heard nothing is the worst of both.
        drop(to_detector);
        let mut is_open = is_open;
        if tokio::time::timeout(SETTLE, is_open.changed()).await.is_err() {
            return;
        }
        if *is_open.borrow() {
            for held in pre_roll.take() {
                if to_recognizer.send(Ok(held)).await.is_err() {
                    return;
                }
            }
        }
    });

    Box::pin(ReceiverStream::new(gated))
}

/// The most recent audio, kept in case the gate opens.
///
/// Bounded by duration rather than by chunk count: a device sending 20 ms
/// chunks and one sending 200 ms chunks should keep the same amount of sound,
/// and only one of those is a number of chunks.
struct PreRoll {
    format: AudioFormat,
    held: std::collections::VecDeque<AudioChunk>,
    bytes: usize,
}

impl PreRoll {
    fn new(format: AudioFormat) -> Self {
        Self { format, held: std::collections::VecDeque::new(), bytes: 0 }
    }

    /// The most audio worth keeping, in bytes, or `None` for a compressed
    /// encoding whose duration cannot be derived from its size.
    fn budget(&self) -> Option<usize> {
        let per_second = self.format.duration_ms(1_000).map(|ms| 1_000_000 / ms.max(1))?;
        usize::try_from(per_second * PRE_ROLL.as_millis() as u64 / 1_000).ok()
    }

    fn push(&mut self, chunk: AudioChunk) {
        self.bytes += chunk.data.len();
        self.held.push_back(chunk);
        // A compressed stream keeps a fixed number of chunks instead: it is
        // still bounded, which is the property that matters, and guessing a
        // bitrate would bound it wrongly rather than not at all.
        let Some(budget) = self.budget() else {
            while self.held.len() > 16 {
                if let Some(dropped) = self.held.pop_front() {
                    self.bytes -= dropped.data.len();
                }
            }
            return;
        };
        while self.bytes > budget && self.held.len() > 1 {
            if let Some(dropped) = self.held.pop_front() {
                self.bytes -= dropped.data.len();
            }
        }
    }

    fn take(&mut self) -> Vec<AudioChunk> {
        self.bytes = 0;
        self.held.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::bus::EventBus;
    use conduit_core::event::Event;
    use conduit_core::Result;
    use conduit_provider::wake::{Detection, WakePhrase};
    use conduit_provider::{Provider, Registry};
    use std::sync::Arc;

    /// A detector that accepts once it has seen `after` chunks.
    struct AcceptsAfter {
        after: usize,
    }

    #[async_trait::async_trait]
    impl Provider for AcceptsAfter {
        conduit_provider::stub_descriptor!("test-wake", conduit_provider::Capability::Wake);
    }

    #[async_trait::async_trait]
    impl WakeWordDetector for AcceptsAfter {
        async fn detect(
            &self,
            audio: ChunkStream<AudioChunk>,
            _phrases: Vec<WakePhrase>,
        ) -> Result<ChunkStream<Detection>> {
            let after = self.after;
            Ok(Box::pin(audio.enumerate().filter_map(move |(index, _)| async move {
                (index + 1 == after).then(|| {
                    Ok(Detection {
                        phrase: "hey jarvis".to_owned(),
                        confidence: 0.9,
                        accepted: true,
                    })
                })
            })))
        }
    }

    /// A detector that scores every chunk below its threshold.
    struct NeverAccepts;

    #[async_trait::async_trait]
    impl Provider for NeverAccepts {
        conduit_provider::stub_descriptor!("test-wake", conduit_provider::Capability::Wake);
    }

    #[async_trait::async_trait]
    impl WakeWordDetector for NeverAccepts {
        async fn detect(
            &self,
            audio: ChunkStream<AudioChunk>,
            _phrases: Vec<WakePhrase>,
        ) -> Result<ChunkStream<Detection>> {
            Ok(Box::pin(audio.map(|_| {
                Ok(Detection {
                    phrase: "hey jarvis".to_owned(),
                    confidence: 0.2,
                    accepted: false,
                })
            })))
        }
    }

    fn emitter(bus: &EventBus) -> Emitter {
        Emitter::new(
            bus.clone(),
            "test",
            AudioFormat::DEFAULT,
            crate::deadline::Progress::default(),
        )
    }

    /// Chunks of `count` samples each, numbered from zero.
    fn chunks(count: usize) -> ChunkStream<AudioChunk> {
        Box::pin(futures_util::stream::iter((0..count).map(|index| {
            Ok(AudioChunk { sequence: index as u64, data: vec![index as u8; 3_200].into() })
        })))
    }

    #[tokio::test]
    async fn nothing_reaches_the_recognizer_before_the_wake_word() {
        let bus = EventBus::default();
        let gated = gate(
            Arc::new(NeverAccepts),
            vec![WakePhrase::new("hey jarvis")],
            "wake".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(8),
        );

        let heard: Vec<_> = gated.collect().await;
        assert!(heard.is_empty(), "a pipeline that never woke transcribes nothing");
    }

    #[tokio::test]
    async fn audio_after_the_wake_word_reaches_the_recognizer() {
        let bus = EventBus::default();
        let gated = gate(
            Arc::new(AcceptsAfter { after: 4 }),
            vec![WakePhrase::new("hey jarvis")],
            "wake".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(8),
        );

        let heard: Vec<AudioChunk> =
            gated.map(|chunk| chunk.expect("no failures")).collect().await;
        assert!(!heard.is_empty(), "everything after the activation is forwarded");
        let last = heard.last().expect("at least one chunk");
        assert_eq!(last.sequence, 7, "capture continues to the end of the utterance");
    }

    #[tokio::test]
    async fn the_moments_before_the_detection_are_not_lost() {
        // A detector reports late, so opening the gate at the instant of the
        // report clips the start of the command. The kept audio is what stops
        // the recognizer dropping the first word of every request.
        let bus = EventBus::default();
        let gated = gate(
            Arc::new(AcceptsAfter { after: 4 }),
            vec![WakePhrase::new("hey jarvis")],
            "wake".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(8),
        );

        let heard: Vec<AudioChunk> =
            gated.map(|chunk| chunk.expect("no failures")).collect().await;
        let first = heard.first().expect("at least one chunk");
        assert!(
            first.sequence < 3,
            "audio from before the detection is forwarded, got {}",
            first.sequence
        );
    }

    #[tokio::test]
    async fn an_activation_is_published_for_anyone_watching() {
        let bus = EventBus::default();
        let mut events = bus.subscribe();
        let gated = gate(
            Arc::new(AcceptsAfter { after: 2 }),
            vec![WakePhrase::new("hey jarvis")],
            "wake".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(4),
        );
        let _heard: Vec<_> = gated.collect().await;

        let mut detected = None;
        while let Some(envelope) = events.recv().await {
            if let Event::WakeWordDetected { phrase, confidence } = &envelope.event {
                detected = Some((phrase.clone(), *confidence));
                break;
            }
        }
        let (phrase, confidence) = detected.expect("the activation is published");
        assert_eq!(phrase, "hey jarvis");
        assert!((confidence - 0.9).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn a_near_miss_is_published_so_a_threshold_can_be_tuned() {
        let bus = EventBus::default();
        let mut events = bus.subscribe();
        let gated = gate(
            Arc::new(NeverAccepts),
            vec![WakePhrase::new("hey jarvis")],
            "wake".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(2),
        );
        let _heard: Vec<_> = gated.collect().await;

        let mut rejected = false;
        while let Some(envelope) = events.recv().await {
            if matches!(envelope.event, Event::WakeWordRejected { .. }) {
                rejected = true;
                break;
            }
        }
        assert!(rejected, "a scored near miss reaches the event stream");
    }

    #[tokio::test]
    async fn an_on_device_detector_opens_the_gate_immediately() {
        // The satellite already woke, so the very first sample it sends is
        // part of the command. Holding any of it back would clip the request.
        let bus = EventBus::default();
        let detector =
            conduit_provider::wake::DeviceWake::new("okay-nabu", vec!["okay nabu".to_owned()]);
        let mut registry = Registry::<dyn WakeWordDetector>::new();
        registry.insert("okay-nabu", Arc::new(detector) as Arc<dyn WakeWordDetector>);
        let detector = registry.require("okay-nabu").expect("registered");

        let gated = gate(
            detector,
            Vec::new(),
            "wake".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(4),
        );

        let heard: Vec<AudioChunk> =
            gated.map(|chunk| chunk.expect("no failures")).collect().await;
        assert_eq!(heard.len(), 4, "every sample the satellite sent is the command");
    }
}
