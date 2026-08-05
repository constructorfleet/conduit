//! Voice activity detection that runs in the Conduit process.
//!
//! Silero VAD is one small ONNX model — under two megabytes — so Conduit scores
//! it directly rather than running a service beside itself to answer a yes-or-no
//! question. There is no base URL and no API key, because there is nothing to
//! reach: the model is a file an operator places, and the compose file mounts a
//! volume for it, exactly as it does for the wake models.
//!
//! # A fixed window, and why the mismatch is refused
//!
//! Silero scores a fixed number of samples at a time — 512 at 16 kHz, 256 at
//! 8 kHz — and those are the only two rates it was trained at. A wrong rate does
//! not degrade the detector, it makes the window the wrong *length of sound*: the
//! same 512 samples become 11 ms instead of 32 ms, and the model reports
//! confidently about something it has never heard. So [`SILERO_SAMPLE_RATES`] is
//! declared on the descriptor and the runtime refuses a rate outside it rather
//! than resampling, which would be deciding on an operator's behalf that a rate
//! they configured was wrong.
//!
//! Chunks, meanwhile, are whatever a device's buffer holds. So this detector
//! buffers internally and reports each chunk as speech if any window it
//! completed within that chunk was speech. Erring toward speech is deliberate: a
//! frame wrongly called speech costs the recognizer a few milliseconds of
//! silence, and a frame wrongly called silence costs a word.
//!
//! # One verdict per chunk
//!
//! [`VoiceActivityDetector::detect`] promises exactly one verdict per chunk, in
//! order, and the trimming stage pairs them positionally. That is a contract
//! rather than a convenience: a detector that answered about only the chunks it
//! had opinions on would leave the stage unable to tell which chunk a verdict
//! skipped. A chunk too short to complete a single window still gets a verdict —
//! the previous window's, because the sound has not been re-evaluated rather than
//! having fallen silent.
//!
//! # Without the `onnx` feature
//!
//! Compiled without it the detector still exists and refuses to load with a
//! message naming the feature, so an operator learns this binary cannot detect
//! rather than watching a configured stage silently trim nothing. This is
//! `conduit-wake`'s arrangement, and it is why a lean build is still a build that
//! explains itself.

#![cfg_attr(not(feature = "onnx"), allow(unused_imports))]

use std::path::PathBuf;
use std::sync::Arc;

use conduit_core::Result;
use conduit_provider::stt::AudioChunk;
use conduit_provider::vad::{Activity, VadOptions, VoiceActivityDetector};
use conduit_provider::{Capability, ChunkStream, Descriptor, Health, Metadata, Provider};

#[cfg(feature = "onnx")]
mod onnx;

/// The directory a definition reads the model from when it names none, relative
/// to the data directory.
pub const DEFAULT_MODELS_DIR: &str = "vad-models";

/// The file name the model is read from.
///
/// Upstream's `ifless` export rather than the headline `silero_vad.onnx`: the
/// default export dispatches on the sample rate with an ONNX `If`, and a graph
/// whose branch condition a runtime cannot fold is a graph it cannot analyse, so
/// `tract` refuses to load it at all. Silero publishes this variant for exactly
/// that case, and `scripts/fetch-vad-model.sh` fetches it.
pub const DEFAULT_MODEL_FILE: &str = "silero_vad_16k_op15.onnx";

/// The sample rates Silero was trained at, in hertz.
///
/// The whole list, and short on purpose. Declared on the descriptor so the
/// runtime refuses a mismatch at the stage rather than resampling it away.
pub const SILERO_SAMPLE_RATES: [u32; 2] = [8_000, 16_000];

/// The acceptance threshold used when a definition names none, in `0.0..=1.0`.
///
/// Silero's own recommended value. A trimmer erring low keeps silence, which
/// costs a recognizer milliseconds; erring high drops speech, which costs a
/// word — so the published default is left alone rather than tuned toward
/// either.
pub const DEFAULT_THRESHOLD: f32 = 0.5;

/// A voice activity detector scoring the Silero model in this process.
pub struct SileroVad {
    /// Identity, version, and the rates the model scores.
    descriptor: Descriptor,
    /// Where the model was loaded from, for diagnostics.
    model_path: PathBuf,
    /// Acceptance threshold from the definition, in `0.0..=1.0`.
    threshold: f32,
    /// How much silence ends an utterance, from the definition, in milliseconds.
    silence_ms: u32,
    #[cfg(feature = "onnx")]
    model: Arc<onnx::Model>,
}

impl std::fmt::Debug for SileroVad {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SileroVad")
            .field("name", &self.name())
            .field("model_path", &self.model_path)
            .field("threshold", &self.threshold)
            .field("silence_ms", &self.silence_ms)
            .finish()
    }
}

impl SileroVad {
    /// Loads the model at `model_path`.
    ///
    /// Loading happens here rather than at the first turn: a model file that is
    /// missing or unreadable is a configuration error, and an operator should see
    /// it when they save the definition rather than when someone speaks. The same
    /// reasoning `conduit-wake` loads by.
    ///
    /// # Errors
    ///
    /// Returns [`conduit_core::Error::Config`] if the model cannot be loaded, or
    /// if this build has no inference runtime compiled in.
    #[cfg_attr(not(feature = "onnx"), allow(unused_variables))]
    pub fn load(
        name: impl Into<String>,
        model_path: impl Into<PathBuf>,
        threshold: f32,
        silence_ms: u32,
    ) -> Result<Self> {
        let name = name.into();
        let model_path = model_path.into();

        #[cfg(not(feature = "onnx"))]
        {
            Err(conduit_core::Error::Config(format!(
                "provider `{name}` detects in process, which this build cannot do: it was \
                 compiled without the `onnx` feature"
            )))
        }

        #[cfg(feature = "onnx")]
        {
            let model = Arc::new(onnx::Model::load(&model_path)?);
            tracing::info!(
                provider = name,
                path = %model_path.display(),
                "loaded the Silero VAD model"
            );
            let descriptor = Descriptor::new(name, Capability::Vad).with_metadata(
                Metadata::default().with_sample_rates(SILERO_SAMPLE_RATES.to_vec()),
            );
            Ok(Self { descriptor, model_path, threshold, silence_ms, model })
        }
    }

    /// Sets the human-readable name operator screens show.
    ///
    /// Separate from the identity this provider was built with: the identity is
    /// what a pipeline selects and what appears in metric labels, and this is
    /// only what a person reads.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.descriptor = self.descriptor.with_label(label);
        self
    }
}

#[async_trait::async_trait]
impl Provider for SileroVad {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// Always healthy: the model is loaded, so there is nothing to reach.
    ///
    /// A file that could not be read never became a detector at all — it failed
    /// the definition — so a registered one has everything it needs.
    async fn health(&self) -> Health {
        Health::Healthy
    }
}

#[async_trait::async_trait]
impl VoiceActivityDetector for SileroVad {
    #[cfg_attr(not(feature = "onnx"), allow(unused_variables))]
    async fn detect(
        &self,
        audio: ChunkStream<AudioChunk>,
        options: VadOptions,
    ) -> Result<ChunkStream<Activity>> {
        #[cfg(not(feature = "onnx"))]
        {
            Err(conduit_core::Error::Config(format!(
                "provider `{}` was compiled without the `onnx` feature",
                self.name()
            )))
        }

        #[cfg(feature = "onnx")]
        {
            Ok(self.detect_with_model(audio, options))
        }
    }

    /// The pause this definition was saved with.
    fn silence_ms(&self) -> u32 {
        self.silence_ms
    }
}

#[cfg(feature = "onnx")]
impl SileroVad {
    /// Scores `audio` on a worker thread, one verdict per chunk.
    ///
    /// Inference is synchronous and CPU-bound, so it runs on a blocking thread
    /// rather than on the reactor. The channel carrying audio to it holds a
    /// single chunk: a source faster than real time — a file, a test, a device
    /// catching up after a stall — then cannot hand over a whole utterance before
    /// any of it has been scored. The same one-ahead rule the trimming stage and
    /// the wake gate both apply.
    fn detect_with_model(
        &self,
        audio: ChunkStream<AudioChunk>,
        options: VadOptions,
    ) -> ChunkStream<Activity> {
        use futures_util::StreamExt;

        let threshold = options.threshold.unwrap_or(self.threshold);
        let mut scorer = onnx::Scorer::new(Arc::clone(&self.model), threshold);
        let provider = self.name().to_owned();

        let (samples_out, mut samples_in) = tokio::sync::mpsc::channel::<Vec<f32>>(1);
        let (verdicts_out, verdicts_in) = tokio::sync::mpsc::channel::<Result<Activity>>(8);

        // The reader: audio off the stream, samples into the scorer.
        let reader_provider = provider.clone();
        tokio::spawn(async move {
            let mut audio = audio;
            while let Some(chunk) = audio.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        tracing::warn!(
                            provider = reader_provider,
                            error = %error,
                            "audio input failed; ending the detection session"
                        );
                        return;
                    }
                };
                if samples_out.send(onnx::samples_from_pcm(&chunk.data)).await.is_err() {
                    return;
                }
            }
        });

        // The scorer: one blocking thread, owning the rolling state.
        tokio::task::spawn_blocking(move || {
            while let Some(samples) = samples_in.blocking_recv() {
                // One verdict per chunk, always: the trimming stage pairs
                // verdicts with chunks by position, so skipping one would shift
                // every later pairing onto the wrong audio.
                let verdict = scorer.push(&samples);
                if verdicts_out.blocking_send(verdict).is_err() {
                    return;
                }
            }
        });

        Box::pin(tokio_stream::wrappers::ReceiverStream::new(verdicts_in))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_only_rates_offered_are_the_ones_silero_was_trained_at() {
        // Conduit's interchange rate is one of them, so the common case needs no
        // resampling — and 44.1 kHz is not, which is what makes the refusal in
        // the trimming stage reachable rather than theoretical.
        assert!(SILERO_SAMPLE_RATES.contains(&16_000));
        assert!(!SILERO_SAMPLE_RATES.contains(&44_100));
    }

    #[cfg(not(feature = "onnx"))]
    #[test]
    fn a_build_without_the_feature_refuses_by_naming_it() {
        // Not by reporting a missing file: an operator whose model is exactly
        // where they put it would go looking for a path problem that is not one.
        let error = SileroVad::load(
            "silero",
            "/models/silero_vad.onnx",
            DEFAULT_THRESHOLD,
            conduit_provider::vad::DEFAULT_SILENCE_MS,
        )
        .expect_err("no inference runtime")
        .to_string();

        assert!(error.contains("onnx"), "names the feature: {error}");
    }
}
