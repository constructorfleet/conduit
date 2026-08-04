//! Learning who is speaking, without taking the audio away from recognition.
//!
//! Identification and recognition want the same utterance: one asks who said
//! it, the other what was said. Neither can go first, because the answer is
//! needed before the model runs and the turn should not be twice as slow for
//! having asked. So capture is forked and both listen at once.

use std::sync::Arc;

use conduit_core::event::Event;
use conduit_core::id::SpeakerId;
use conduit_provider::speaker::SpeakerIdentifier;
use conduit_provider::stt::AudioChunk;
use conduit_provider::ChunkStream;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::emit::Emitter;

/// How many chunks may be queued for either listener before capture waits.
const CHANNEL_DEPTH: usize = 32;

/// Splits one audio stream so two stages can hear the same utterance.
///
/// Chunks are cloned, which is cheap: the samples are reference-counted bytes,
/// so forking copies a handle rather than the audio.
///
/// Both halves are fed by one task, so a listener that stops reading applies
/// backpressure to capture rather than being silently skipped. A listener that
/// is *dropped* stops receiving and the other continues — which is what makes
/// identification optional at runtime as well as in the graph.
pub fn fork(
    mut audio: ChunkStream<AudioChunk>,
) -> (ChunkStream<AudioChunk>, ChunkStream<AudioChunk>) {
    let (left_sender, left) = mpsc::channel::<conduit_core::Result<AudioChunk>>(CHANNEL_DEPTH);
    let (right_sender, right) =
        mpsc::channel::<conduit_core::Result<AudioChunk>>(CHANNEL_DEPTH);

    tokio::spawn(async move {
        while let Some(chunk) = audio.next().await {
            let (to_left, to_right) = match chunk {
                Ok(chunk) => (Ok(chunk.clone()), Ok(chunk)),
                // A capture failure is reported to both: a stage that saw a
                // clean end of stream would report a short utterance rather
                // than a broken one.
                Err(error) => (Err(conduit_core::Error::Config(error.to_string())), Err(error)),
            };
            let left = left_sender.send(to_left).await;
            let right = right_sender.send(to_right).await;
            if left.is_err() && right.is_err() {
                return;
            }
        }
    });

    (Box::pin(ReceiverStream::new(left)), Box::pin(ReceiverStream::new(right)))
}

/// Identifies the voice in `audio`, publishing what it found.
///
/// Runs to completion on its own task so it overlaps recognition rather than
/// following it.
///
/// A failure is published as a recovered stage failure and answered with
/// `None`: not knowing who is speaking is how every pipeline behaved before
/// identification existed, and a service that is down should cost a turn its
/// per-speaker policies rather than its answer.
pub fn identify(
    provider: Arc<dyn SpeakerIdentifier>,
    node: String,
    emitter: Emitter,
    audio: ChunkStream<AudioChunk>,
) -> tokio::task::JoinHandle<Option<SpeakerId>> {
    tokio::spawn(async move {
        match provider.identify(audio).await {
            Ok(identified) => {
                emitter.emit(Event::SpeakerIdentified {
                    speaker: identified.speaker,
                    confidence: identified.confidence,
                });
                identified.speaker
            }
            Err(error) => {
                tracing::warn!(
                    node,
                    %error,
                    "speaker identification failed; the turn continues without a speaker"
                );
                emitter.emit(Event::StageFailed {
                    node,
                    error: error.to_string(),
                    recovered: true,
                });
                None
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::audio::AudioFormat;
    use conduit_core::bus::EventBus;
    use conduit_core::Result;
    use conduit_provider::speaker::Identification;
    use conduit_provider::Provider;

    struct Knows(SpeakerId);

    #[async_trait::async_trait]
    impl Provider for Knows {
        conduit_provider::stub_descriptor!(
            "test-speaker",
            conduit_provider::Capability::SpeakerId
        );
    }

    #[async_trait::async_trait]
    impl SpeakerIdentifier for Knows {
        async fn identify(&self, mut audio: ChunkStream<AudioChunk>) -> Result<Identification> {
            let mut heard = 0;
            while let Some(chunk) = audio.next().await {
                heard += chunk?.data.len();
            }
            assert!(heard > 0, "identification is given the audio, not an empty stream");
            Ok(Identification { speaker: Some(self.0), confidence: 0.97 })
        }

        async fn enroll(&self, _: SpeakerId, _: ChunkStream<AudioChunk>) -> Result<()> {
            Ok(())
        }

        async fn forget(&self, _: SpeakerId) -> Result<()> {
            Ok(())
        }
    }

    struct Unreachable;

    #[async_trait::async_trait]
    impl Provider for Unreachable {
        conduit_provider::stub_descriptor!(
            "test-speaker",
            conduit_provider::Capability::SpeakerId
        );
    }

    #[async_trait::async_trait]
    impl SpeakerIdentifier for Unreachable {
        async fn identify(&self, _: ChunkStream<AudioChunk>) -> Result<Identification> {
            Err(conduit_core::Error::Config("service is down".to_owned()))
        }

        async fn enroll(&self, _: SpeakerId, _: ChunkStream<AudioChunk>) -> Result<()> {
            Ok(())
        }

        async fn forget(&self, _: SpeakerId) -> Result<()> {
            Ok(())
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

    fn chunks(count: usize) -> ChunkStream<AudioChunk> {
        Box::pin(futures_util::stream::iter(
            (0..count).map(|index| {
                Ok(AudioChunk { sequence: index as u64, data: vec![7; 320].into() })
            }),
        ))
    }

    #[tokio::test]
    async fn both_halves_of_a_fork_hear_the_whole_utterance() {
        let (left, right) = fork(chunks(5));
        let left: Vec<_> = left.map(|chunk| chunk.expect("no failures")).collect().await;
        let right: Vec<_> = right.map(|chunk| chunk.expect("no failures")).collect().await;

        assert_eq!(left.len(), 5);
        assert_eq!(right.len(), 5);
        assert_eq!(
            left.iter().map(|chunk| chunk.sequence).collect::<Vec<_>>(),
            right.iter().map(|chunk| chunk.sequence).collect::<Vec<_>>(),
            "both stages hear the same audio in the same order"
        );
    }

    #[tokio::test]
    async fn dropping_one_half_does_not_starve_the_other() {
        // Identification is optional at runtime as well as in the graph: a
        // turn that stops caring who is speaking must still be transcribed.
        let (left, right) = fork(chunks(40));
        drop(right);
        let left: Vec<_> = left.map(|chunk| chunk.expect("no failures")).collect().await;
        assert_eq!(left.len(), 40);
    }

    #[tokio::test]
    async fn a_recognized_voice_is_published_and_returned() {
        let bus = EventBus::default();
        let mut events = bus.subscribe();
        let speaker = SpeakerId::new();

        let found = identify(
            Arc::new(Knows(speaker)),
            "speaker_id".to_owned(),
            emitter(&bus),
            chunks(3),
        )
        .await
        .expect("the identification task");

        assert_eq!(found, Some(speaker));
        let mut published = None;
        while let Some(envelope) = events.recv().await {
            if let Event::SpeakerIdentified { speaker, confidence } = &envelope.event {
                published = Some((*speaker, *confidence));
                break;
            }
        }
        let (published, confidence) = published.expect("the match is published");
        assert_eq!(published, Some(speaker));
        assert!((confidence - 0.97).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn a_service_that_is_down_costs_the_speaker_rather_than_the_turn() {
        let bus = EventBus::default();
        let mut events = bus.subscribe();

        let found =
            identify(Arc::new(Unreachable), "speaker_id".to_owned(), emitter(&bus), chunks(3))
                .await
                .expect("the identification task");

        assert_eq!(found, None, "the turn continues without knowing who asked");
        while let Some(envelope) = events.recv().await {
            if let Event::StageFailed { node, recovered, .. } = &envelope.event {
                assert_eq!(node, "speaker_id");
                assert!(*recovered, "the turn goes on, so the failure is recovered");
                return;
            }
        }
        panic!("the failure is published");
    }
}
