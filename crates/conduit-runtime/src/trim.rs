//! Taking the silence off either end of what was said.
//!
//! A trimming stage sits between the microphone and the recognizer, the same
//! position the wake gate occupies, and it has the same problem: a detector
//! reports about a frame some way after that frame went by, so forwarding audio
//! only from the moment speech is reported clips the first word.
//!
//! The wake gate solves that with a fixed half-second pre-roll, because it opens
//! once and never closes. A trimmer cannot: it opens and closes repeatedly
//! within one utterance, and a fixed pre-roll re-emitted at every reopening
//! would duplicate audio. So this stage holds each chunk until it has the verdict
//! for it — [`VoiceActivityDetector::detect`] promises one verdict per chunk in
//! order, which is exactly what makes that possible — and pairs them positionally
//! rather than guessing from a clock.
//!
//! What it forwards is the chunk it received, unmodified. Nothing here rewrites
//! samples: trimming is a decision about *which* chunks the recognizer hears, not
//! about their contents, because a recognizer handed re-cut audio would be
//! transcribing something no microphone produced.

use std::sync::Arc;
use std::time::Duration;

use conduit_core::audio::AudioFormat;
use conduit_core::event::Event;
use conduit_provider::stt::AudioChunk;
use conduit_provider::vad::{VadOptions, VoiceActivityDetector};
use conduit_provider::ChunkStream;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::emit::Emitter;

/// How much audio before the first speech is kept and forwarded.
///
/// The lead-in problem the wake gate's `PRE_ROLL` exists for, at the scale a
/// trimmer needs it: a detector confirms speech a frame or two after it started,
/// and a recognizer given a word with its first consonant sliced off transcribes
/// a different word. Shorter than the wake gate's half second because there is
/// no wake phrase to avoid replaying — everything held here is audio the
/// recognizer should have.
const LEAD_IN: Duration = Duration::from_millis(200);

/// The least trailing silence forwarded after speech, whatever a definition says.
///
/// A recognizer needs some trailing silence to know a word ended; cutting at the
/// last speech frame runs the next sound directly into it. So an operator who
/// configured an aggressively short pause still gets this much — the floor is
/// about the recognizer's needs, and the setting above it is about how long to
/// wait through a pause in the middle of a sentence.
const MIN_TAIL: Duration = Duration::from_millis(300);

/// How many chunks may be queued in either direction before capture waits.
///
/// One toward the detector, so capture can never run ahead of scoring: with a
/// deeper queue a source faster than real time hands over the whole utterance
/// before a single verdict comes back, and the pairing this stage depends on
/// would be assembled from decisions that arrived all at once at the end. The
/// same reasoning as the wake gate's `DETECTOR_DEPTH`, and the same value.
const DETECTOR_DEPTH: usize = 1;

/// How many trimmed chunks may be queued for the recognizer before capture waits.
const OUTPUT_DEPTH: usize = 32;

/// Runs `audio` past `detector`, forwarding the chunks that carry speech.
///
/// The returned stream carries the audio the recognizer should hear: every chunk
/// the detector called speech, the [`LEAD_IN`] before the first of them, and the
/// [`TAIL`] after the last.
///
/// **A detector that fails does not end the turn.** Whatever it had not yet
/// scored is forwarded untrimmed and the stream continues, because untrimmed
/// audio is precisely how every pipeline behaved before this stage existed. This
/// is the identification precedent rather than the wake one: a gate that cannot
/// tell whether it was addressed must not guess, but a trimmer that cannot tell
/// speech from silence can simply forward both. The failure is published as a
/// recovered [`Event::StageFailed`] so an operator sees a detector to fix rather
/// than a pipeline that quietly stopped trimming.
///
/// A detector that cannot score `format`'s sample rate is the same case, decided
/// before any audio moves: the mismatch is reported once, and the stage forwards
/// everything.
pub fn trim(
    detector: Arc<dyn VoiceActivityDetector>,
    options: VadOptions,
    node: String,
    emitter: Emitter,
    format: AudioFormat,
    audio: ChunkStream<AudioChunk>,
) -> ChunkStream<AudioChunk> {
    if let Err(error) = conduit_provider::vad::accepts_rate(
        detector.name(),
        &detector.descriptor().metadata,
        format.sample_rate,
    ) {
        // Refused rather than resampled — but not fatal, because the turn can
        // still be heard. What an operator has to fix is the definition, and
        // this is what tells them so.
        tracing::warn!(node, %error, "detector cannot score this audio; forwarding it untrimmed");
        emitter.emit(Event::StageFailed { node, error: error.to_string(), recovered: true });
        return audio;
    }

    // The node's setting, the detector's, then the floor — so a pipeline can tune
    // how long a mid-sentence pause may run without being able to cut the
    // trailing silence a recognizer needs below what it needs.
    let tail = Duration::from_millis(u64::from(
        options.silence_ms.unwrap_or_else(|| detector.silence_ms()),
    ))
    .max(MIN_TAIL);

    let (to_detector, from_capture) =
        mpsc::channel::<conduit_core::Result<AudioChunk>>(DETECTOR_DEPTH);
    let (verdict_sender, verdicts) = mpsc::channel(OUTPUT_DEPTH);
    let (to_recognizer, trimmed) =
        mpsc::channel::<conduit_core::Result<AudioChunk>>(OUTPUT_DEPTH);

    let scoring_node = node.clone();
    let scoring_emitter = emitter.clone();
    tokio::spawn(async move {
        let scored =
            detector.detect(Box::pin(ReceiverStream::new(from_capture)), options).await;
        let mut scored = match scored {
            Ok(scored) => scored,
            Err(error) => {
                tracing::warn!(node = scoring_node, %error, "detector failed to start");
                scoring_emitter.emit(Event::StageFailed {
                    node: scoring_node,
                    error: error.to_string(),
                    recovered: true,
                });
                // Dropping the sender is what tells the pairing task to stop
                // waiting for verdicts and forward the rest as it is.
                return;
            }
        };
        while let Some(item) = scored.next().await {
            match item {
                Ok(activity) => {
                    if verdict_sender.send(activity).await.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    tracing::warn!(node = scoring_node, %error, "detection failed mid-stream");
                    scoring_emitter.emit(Event::StageFailed {
                        node: scoring_node,
                        error: error.to_string(),
                        recovered: true,
                    });
                    return;
                }
            }
        }
    });

    tokio::spawn(async move {
        let mut verdicts = verdicts;
        let mut audio = audio;
        let mut lead_in = Held::new(format, LEAD_IN);
        let mut tail_remaining = 0_u64;
        let mut spoken = false;
        // Flipped the moment the detector stops answering, and never unset:
        // everything from then on is forwarded, because a stage that has stopped
        // trimming must not also start dropping.
        let mut untrimmed = false;

        while let Some(chunk) = audio.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    let _ = to_recognizer.send(Err(error)).await;
                    return;
                }
            };

            if untrimmed {
                if to_recognizer.send(Ok(chunk)).await.is_err() {
                    return;
                }
                continue;
            }

            // Sending fails once the detector task has gone, which is the
            // recovered-failure path: stop asking and forward the rest.
            if to_detector.send(Ok(chunk.clone())).await.is_err() {
                untrimmed = true;
                if to_recognizer.send(Ok(chunk)).await.is_err() {
                    return;
                }
                continue;
            }

            // The verdict for *this* chunk, by position. Waiting for it is what
            // makes the pairing exact, and what makes a detector that stopped
            // answering visible immediately rather than as drift.
            let Some(activity) = verdicts.recv().await else {
                untrimmed = true;
                // Whatever was held back is audio somebody spoke into a
                // microphone. Forwarding it is the recovery.
                for held in lead_in.take() {
                    if to_recognizer.send(Ok(held)).await.is_err() {
                        return;
                    }
                }
                if to_recognizer.send(Ok(chunk)).await.is_err() {
                    return;
                }
                continue;
            };

            // At least a millisecond, whatever the arithmetic says: a chunk whose
            // duration rounds to zero — a tiny buffer, or a compressed encoding
            // whose size says nothing about its length — would otherwise never
            // count against the tail, and a stage that never closes it forwards
            // the whole stream while appearing to trim.
            let duration = format.duration_ms(chunk.data.len()).unwrap_or(0).max(1);
            if activity.speech {
                if !spoken {
                    spoken = true;
                    // The detector confirmed speech a frame or two after it
                    // began, so the beginning of the word is in here.
                    for held in lead_in.take() {
                        if to_recognizer.send(Ok(held)).await.is_err() {
                            return;
                        }
                    }
                }
                tail_remaining = u64::try_from(tail.as_millis()).unwrap_or(u64::MAX);
                if to_recognizer.send(Ok(chunk)).await.is_err() {
                    return;
                }
                continue;
            }

            if tail_remaining > 0 {
                // Trailing silence a recognizer needs to know the word ended,
                // and the pause in the middle of a sentence.
                tail_remaining = tail_remaining.saturating_sub(duration);
                if to_recognizer.send(Ok(chunk)).await.is_err() {
                    return;
                }
                continue;
            }

            // Silence, with no speech recently enough to be part of an
            // utterance. Held rather than dropped: if the next chunk is speech,
            // this is its lead-in.
            lead_in.push(chunk);
            spoken = false;
        }
    });

    Box::pin(ReceiverStream::new(trimmed))
}

/// The most recent audio, kept in case what follows turns out to be speech.
///
/// Bounded by duration rather than by chunk count, for the reason the wake
/// gate's `PreRoll` is: a device sending 20 ms chunks and one sending 200 ms
/// chunks should keep the same amount of sound, and only one of those is a
/// number of chunks.
struct Held {
    format: AudioFormat,
    window: Duration,
    held: std::collections::VecDeque<AudioChunk>,
    bytes: usize,
}

impl Held {
    fn new(format: AudioFormat, window: Duration) -> Self {
        Self { format, window, held: std::collections::VecDeque::new(), bytes: 0 }
    }

    /// The most audio worth keeping, in bytes, or `None` for a compressed
    /// encoding whose duration cannot be derived from its size.
    fn budget(&self) -> Option<usize> {
        let per_second = self.format.duration_ms(1_000).map(|ms| 1_000_000 / ms.max(1))?;
        usize::try_from(per_second * self.window.as_millis() as u64 / 1_000).ok()
    }

    fn push(&mut self, chunk: AudioChunk) {
        self.bytes += chunk.data.len();
        self.held.push_back(chunk);
        // A compressed stream keeps a fixed number of chunks instead: still
        // bounded, which is the property that matters, and guessing a bitrate
        // would bound it wrongly rather than not at all.
        let Some(budget) = self.budget() else {
            while self.held.len() > 8 {
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
    use conduit_core::Result;
    use conduit_provider::descriptor::{Descriptor, Metadata};
    use conduit_provider::vad::Activity;
    use conduit_provider::{Health, Provider};

    /// A detector that calls chunks `speech` exactly when their index is in
    /// `speaking`, and answers about every chunk it is given.
    struct Scripted {
        speaking: Vec<usize>,
    }

    #[async_trait::async_trait]
    impl Provider for Scripted {
        conduit_provider::stub_descriptor!("test-vad", conduit_provider::Capability::Vad);
    }

    #[async_trait::async_trait]
    impl VoiceActivityDetector for Scripted {
        async fn detect(
            &self,
            audio: ChunkStream<AudioChunk>,
            _options: VadOptions,
        ) -> Result<ChunkStream<Activity>> {
            let speaking = self.speaking.clone();
            Ok(Box::pin(audio.enumerate().map(move |(index, _)| {
                if speaking.contains(&index) {
                    Ok(Activity::speech(0.9))
                } else {
                    Ok(Activity::silence(0.1))
                }
            })))
        }
    }

    /// A detector that will not start.
    struct WillNotLoad;

    #[async_trait::async_trait]
    impl Provider for WillNotLoad {
        conduit_provider::stub_descriptor!("test-vad", conduit_provider::Capability::Vad);
    }

    #[async_trait::async_trait]
    impl VoiceActivityDetector for WillNotLoad {
        async fn detect(
            &self,
            _audio: ChunkStream<AudioChunk>,
            _options: VadOptions,
        ) -> Result<ChunkStream<Activity>> {
            Err(conduit_core::Error::Config("no model on disk".to_owned()))
        }
    }

    /// A detector that answers about two chunks and then fails.
    struct FailsAfterTwo;

    #[async_trait::async_trait]
    impl Provider for FailsAfterTwo {
        conduit_provider::stub_descriptor!("test-vad", conduit_provider::Capability::Vad);
    }

    #[async_trait::async_trait]
    impl VoiceActivityDetector for FailsAfterTwo {
        async fn detect(
            &self,
            audio: ChunkStream<AudioChunk>,
            _options: VadOptions,
        ) -> Result<ChunkStream<Activity>> {
            Ok(Box::pin(audio.enumerate().map(|(index, _)| {
                if index < 2 {
                    Ok(Activity::speech(0.9))
                } else {
                    Err(conduit_core::Error::Config("the runtime died".to_owned()))
                }
            })))
        }
    }

    /// A detector that only scores 8 kHz, for the rate-mismatch case.
    struct NarrowBand;

    #[async_trait::async_trait]
    impl Provider for NarrowBand {
        fn descriptor(&self) -> &Descriptor {
            static DESCRIPTOR: std::sync::OnceLock<Descriptor> = std::sync::OnceLock::new();
            DESCRIPTOR.get_or_init(|| {
                Descriptor::new("narrow", conduit_provider::Capability::Vad)
                    .with_metadata(Metadata::default().with_sample_rates(vec![8_000]))
            })
        }

        async fn health(&self) -> Health {
            Health::Healthy
        }
    }

    #[async_trait::async_trait]
    impl VoiceActivityDetector for NarrowBand {
        async fn detect(
            &self,
            _audio: ChunkStream<AudioChunk>,
            _options: VadOptions,
        ) -> Result<ChunkStream<Activity>> {
            panic!("a detector refused for its rate is never asked to score");
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

    /// Chunks of 100 ms each at the interchange format, numbered from zero.
    fn chunks(count: usize) -> ChunkStream<AudioChunk> {
        Box::pin(futures_util::stream::iter((0..count).map(|index| {
            Ok(AudioChunk { sequence: index as u64, data: vec![index as u8; 3_200].into() })
        })))
    }

    async fn heard(stream: ChunkStream<AudioChunk>) -> Vec<u64> {
        stream.map(|chunk| chunk.expect("no failures").sequence).collect().await
    }

    #[tokio::test]
    async fn silence_around_what_was_said_does_not_reach_the_recognizer() {
        let bus = EventBus::default();
        let trimmed = trim(
            Arc::new(Scripted { speaking: vec![4, 5, 6] }),
            // The shortest pause the stage honours, so the trailing silence this
            // asserts about is reachable within twelve 100 ms chunks.
            VadOptions { threshold: None, silence_ms: Some(300) },
            "trim".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(12),
        );

        let sequences = heard(trimmed).await;
        assert!(sequences.contains(&5), "the speech itself is forwarded: {sequences:?}");
        assert!(!sequences.contains(&0), "the silence at the start is not: {sequences:?}");
        assert!(!sequences.contains(&11), "nor the silence at the end: {sequences:?}");
    }

    #[tokio::test]
    async fn the_moment_before_the_first_speech_is_not_clipped() {
        // The pre-roll problem, borrowed from the wake gate: a detector confirms
        // speech a frame or two after it began, so forwarding only from the
        // report cuts the first consonant off the first word.
        let bus = EventBus::default();
        let trimmed = trim(
            Arc::new(Scripted { speaking: vec![4, 5, 6] }),
            VadOptions::default(),
            "trim".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(12),
        );

        let sequences = heard(trimmed).await;
        let first = *sequences.first().expect("something was forwarded");
        assert!(first < 4, "audio from before the first speech frame is kept, got {first}");
    }

    #[tokio::test]
    async fn a_pause_in_the_middle_of_a_sentence_is_carried_through() {
        // "turn on the lights ... in the kitchen" is one utterance. Trimming the
        // gap would hand the recognizer two fragments.
        let bus = EventBus::default();
        let trimmed = trim(
            Arc::new(Scripted { speaking: vec![2, 3, 6, 7] }),
            VadOptions::default(),
            "trim".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(10),
        );

        let sequences = heard(trimmed).await;
        for gap in [4, 5] {
            assert!(sequences.contains(&gap), "the pause is forwarded whole: {sequences:?}");
        }
    }

    #[tokio::test]
    async fn the_pause_a_pipeline_configured_is_the_pause_it_waits_through() {
        // The setting an operator saves reaches the stage, which is the whole
        // reason it is on the definition: a `silence_ms` that validated and was
        // then ignored would be a box someone tunes with no effect. A second of
        // silence after the last speech, and a node that says to wait that long.
        let bus = EventBus::default();
        let trimmed = trim(
            Arc::new(Scripted { speaking: vec![0] }),
            VadOptions { threshold: None, silence_ms: Some(1_000) },
            "trim".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(12),
        );

        let sequences = heard(trimmed).await;
        // Ten 100 ms chunks of silence carried, and the eleventh trimmed. The
        // stage's own floor is 300 ms, so this cannot pass by accident.
        assert!(sequences.contains(&9), "the pause is waited through: {sequences:?}");
        assert!(!sequences.contains(&11), "and then trimming resumes: {sequences:?}");
    }

    #[tokio::test]
    async fn a_pause_shorter_than_a_recognizer_needs_is_raised_to_the_floor() {
        // An operator can tune how long a mid-sentence pause may run; they cannot
        // cut the trailing silence a recognizer needs to know a word ended.
        let bus = EventBus::default();
        let trimmed = trim(
            Arc::new(Scripted { speaking: vec![0] }),
            VadOptions { threshold: None, silence_ms: Some(0) },
            "trim".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(12),
        );

        let sequences = heard(trimmed).await;
        assert!(sequences.contains(&2), "{MIN_TAIL:?} is still forwarded: {sequences:?}");
        assert!(!sequences.contains(&5), "but no more than that: {sequences:?}");
    }

    #[tokio::test]
    async fn a_chunk_too_short_to_measure_still_counts_against_the_pause() {
        // Four bytes is not a millisecond of sound, and a tail that only closed
        // on measurable time would forward every one of them for ever. Enough of
        // them to pass `MIN_TAIL` if each counts for the millisecond it is
        // credited with, and not otherwise.
        let bus = EventBus::default();
        let tiny = Box::pin(futures_util::stream::iter((0..500).map(|index| {
            Ok(AudioChunk { sequence: index as u64, data: vec![index as u8; 4].into() })
        })));
        let trimmed = trim(
            Arc::new(Scripted { speaking: vec![0] }),
            VadOptions { threshold: None, silence_ms: Some(1) },
            "trim".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            tiny,
        );

        let sequences = heard(trimmed).await;
        assert!(sequences.len() < 500, "the pause closed: {sequences:?}");
    }

    #[tokio::test]
    async fn what_is_forwarded_is_the_audio_that_arrived() {
        // Trimming decides which chunks the recognizer hears, never what is in
        // them: re-cut samples would be a transcript of audio no microphone
        // produced.
        let bus = EventBus::default();
        let trimmed = trim(
            Arc::new(Scripted { speaking: (0..6).collect() }),
            VadOptions::default(),
            "trim".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(6),
        );

        let forwarded: Vec<AudioChunk> =
            trimmed.map(|chunk| chunk.expect("no failures")).collect().await;
        assert_eq!(forwarded.len(), 6, "all speech, so nothing is trimmed");
        for (index, chunk) in forwarded.iter().enumerate() {
            assert_eq!(chunk.sequence, index as u64, "in order");
            assert_eq!(chunk.data.len(), 3_200, "and unmodified");
            assert!(chunk.data.iter().all(|byte| *byte == index as u8), "byte for byte");
        }
    }

    #[tokio::test]
    async fn a_detector_that_will_not_start_leaves_the_turn_hearing_everything() {
        // The identification precedent, not the wake one: not knowing which
        // audio is speech is how every pipeline behaved before this stage, so
        // the turn still answers.
        let bus = EventBus::default();
        let mut events = bus.subscribe();
        let trimmed = trim(
            Arc::new(WillNotLoad),
            VadOptions::default(),
            "trim".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(6),
        );

        assert_eq!(heard(trimmed).await, vec![0, 1, 2, 3, 4, 5], "untrimmed, not lost");

        let mut failure = None;
        while let Some(envelope) = events.recv().await {
            if let Event::StageFailed { node, error, recovered } = &envelope.event {
                failure = Some((node.clone(), error.clone(), *recovered));
                break;
            }
        }
        let (node, error, recovered) = failure.expect("the failure is published");
        assert_eq!(node, "trim");
        assert!(error.contains("no model on disk"), "names the cause: {error}");
        assert!(recovered, "recovered: the turn was still heard");
    }

    #[tokio::test]
    async fn a_detector_that_fails_partway_leaves_the_rest_of_the_utterance_intact() {
        // The chunks it never scored are the ones a person is still speaking.
        // Dropping them would turn a detector fault into a truncated request.
        let bus = EventBus::default();
        let trimmed = trim(
            Arc::new(FailsAfterTwo),
            VadOptions::default(),
            "trim".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(6),
        );

        let sequences = heard(trimmed).await;
        for tail in [3, 4, 5] {
            assert!(
                sequences.contains(&tail),
                "everything after the failure is forwarded: {sequences:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_rate_the_detector_cannot_score_is_reported_rather_than_resampled() {
        // Refused because a fixed-window detector handed the wrong rate reports
        // confident nonsense — and not fatal, because the turn can still be
        // heard. `NarrowBand::detect` panics if this reaches it.
        let bus = EventBus::default();
        let mut events = bus.subscribe();
        let trimmed = trim(
            Arc::new(NarrowBand),
            VadOptions::default(),
            "trim".to_owned(),
            emitter(&bus),
            AudioFormat::DEFAULT,
            chunks(4),
        );

        assert_eq!(heard(trimmed).await, vec![0, 1, 2, 3], "forwarded untouched");

        let mut failure = None;
        while let Some(envelope) = events.recv().await {
            if let Event::StageFailed { error, recovered, .. } = &envelope.event {
                failure = Some((error.clone(), *recovered));
                break;
            }
        }
        let (error, recovered) = failure.expect("a rate mismatch is a published stage failure");
        assert!(error.contains("16000"), "names the rate asked for: {error}");
        assert!(error.contains("8000"), "and the one it scores: {error}");
        assert!(recovered, "the turn was still heard");
    }
}
