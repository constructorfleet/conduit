//! Wake word detection that runs in the Conduit process.
//!
//! openWakeWord is a chain of three small ONNX models, which means Conduit can
//! score it directly rather than running a service beside itself to do it. A
//! definition whose runtime is `local` gets this detector; one whose runtime is
//! `wyoming` gets `conduit-wyoming`'s, listening for the same phrases.
//!
//! Which phrases it can hear is whatever `<phrase>.onnx` files sit in its model
//! directory, beside the `melspectrogram.onnx` and `embedding_model.onnx` every
//! openWakeWord installation ships. Nothing is downloaded: the models are the
//! operator's to place, and the compose file mounts a volume for them.
//!
//! # The other two engines
//!
//! **microWakeWord** is absent because it cannot be here: its models are
//! tflite-micro streaming graphs needing the TFLM micro-frontend operator,
//! which no Rust runtime implements. It detects on the satellite that was
//! built for it, or on a Wyoming server, and the provider definition says so
//! in its type — there is no `local` runtime to name.
//!
//! **nanoWakeWord** is absent because it is a different detector wearing a
//! similar name. It is ONNX too, and it borrows openWakeWord's mel and
//! embedding front end, but its phrase models are recurrent: each one takes a
//! `hidden_in` and a `cell_in` and returns the updated pair, so scoring a
//! chunk means threading LSTM state from the chunk before it. openWakeWord's
//! classifiers are stateless and score a fixed window of embeddings, which is
//! what [`onnx::Scorer`] is built around. Adding nanoWakeWord means a second
//! scorer that carries state, not a flag on this one — so until that exists
//! its definitions are refused with the reason, rather than quietly handed to
//! a chain that would score them as silence.

#![cfg_attr(not(feature = "onnx"), allow(unused_imports))]

use std::path::PathBuf;
use std::sync::Arc;

use conduit_core::Result;
use conduit_provider::stt::AudioChunk;
use conduit_provider::wake::{Detection, WakePhrase, WakeWordDetector};
use conduit_provider::{Capability, ChunkStream, Descriptor, Health, Metadata, Provider};

pub mod phrase;

#[cfg(feature = "onnx")]
mod onnx;

/// The directory an openWakeWord definition reads models from when it names
/// none, relative to the data directory.
pub const DEFAULT_MODELS_DIR: &str = "wake-models";

/// A wake word detector scoring openWakeWord models in this process.
pub struct OpenWakeWord {
    /// Identity, version, and the phrases the loaded models listen for.
    descriptor: Descriptor,
    /// Where the models were loaded from, for diagnostics.
    models_dir: PathBuf,
    /// Phrases the loaded models listen for.
    phrases: Vec<String>,
    /// Acceptance threshold from the definition, in `0.0..=1.0`.
    threshold: f32,
    #[cfg(feature = "onnx")]
    models: Arc<onnx::Models>,
}

impl std::fmt::Debug for OpenWakeWord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenWakeWord")
            .field("name", &self.name())
            .field("models_dir", &self.models_dir)
            .field("phrases", &self.phrases)
            .field("threshold", &self.threshold)
            .finish()
    }
}

impl OpenWakeWord {
    /// Loads the models in `models_dir` and listens for `phrases`.
    ///
    /// An empty `phrases` loads every model in the directory, which is what a
    /// definition naming none means.
    ///
    /// Loading happens here rather than at the first turn: a model directory
    /// that is missing, unreadable, or holds no phrase the definition asked for
    /// is a configuration error, and an operator should see it when they save
    /// the definition rather than when someone speaks.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the models cannot be loaded, or if this
    /// build has no inference runtime compiled in.
    #[cfg_attr(not(feature = "onnx"), allow(unused_variables))]
    pub fn load(
        name: impl Into<String>,
        models_dir: impl Into<PathBuf>,
        phrases: Vec<String>,
        threshold: f32,
    ) -> Result<Self> {
        let name = name.into();
        let models_dir = models_dir.into();

        #[cfg(not(feature = "onnx"))]
        {
            Err(conduit_core::Error::Config(format!(
                "provider `{name}` detects in process, which this build cannot do: it was \
                 compiled without the `onnx` feature"
            )))
        }

        #[cfg(feature = "onnx")]
        {
            let models = Arc::new(onnx::Models::load(&models_dir, &phrases)?);
            let loaded = models.phrases();
            tracing::info!(
                provider = name,
                directory = %models_dir.display(),
                phrases = ?loaded,
                "loaded openWakeWord models"
            );
            let descriptor = Descriptor::new(name, Capability::Wake).with_metadata(
                Metadata::default().with_phrases(
                    loaded
                        .iter()
                        .map(|phrase| WakePhrase::new(phrase).with_threshold(threshold))
                        .collect(),
                ),
            );
            Ok(Self { descriptor, models_dir, phrases: loaded, threshold, models })
        }
    }

    /// Sets the human-readable name operator screens show.
    ///
    /// Separate from the identity this provider was built with: the identity
    /// is what a pipeline selects and what appears in metric labels, and this
    /// is only what a person reads.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.descriptor = self.descriptor.with_label(label);
        self
    }
}

#[async_trait::async_trait]
impl Provider for OpenWakeWord {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// Always healthy: the models are loaded, so there is nothing to reach.
    ///
    /// A directory that could not be read never became a detector at all — it
    /// failed the definition — so a registered one has everything it needs.
    async fn health(&self) -> Health {
        Health::Healthy
    }
}

#[async_trait::async_trait]
impl WakeWordDetector for OpenWakeWord {
    #[cfg_attr(not(feature = "onnx"), allow(unused_variables))]
    async fn detect(
        &self,
        audio: ChunkStream<AudioChunk>,
        phrases: Vec<WakePhrase>,
    ) -> Result<ChunkStream<Detection>> {
        #[cfg(not(feature = "onnx"))]
        {
            Err(conduit_core::Error::Config(format!(
                "provider `{}` was compiled without the `onnx` feature",
                self.name()
            )))
        }

        #[cfg(feature = "onnx")]
        {
            Ok(self.detect_with_models(audio, phrases))
        }
    }
}

#[cfg(feature = "onnx")]
impl OpenWakeWord {
    /// Scores `audio` on a worker thread, reporting what it hears.
    ///
    /// Inference is synchronous and CPU-bound, so it runs on a blocking thread
    /// rather than on the reactor. The channel carrying audio to it holds a
    /// single chunk: a source faster than real time — a file, a test, a device
    /// catching up after a stall — then cannot hand over a whole utterance
    /// before any of it has been scored, which is the same one-ahead rule the
    /// runtime's gate applies.
    fn detect_with_models(
        &self,
        audio: ChunkStream<AudioChunk>,
        phrases: Vec<WakePhrase>,
    ) -> ChunkStream<Detection> {
        use futures_util::StreamExt;

        let thresholds = onnx::thresholds_for(&self.models, &phrases, self.threshold);
        let mut scorer = onnx::Scorer::new(Arc::clone(&self.models), thresholds);
        let provider = self.name().to_owned();

        let (samples_out, mut samples_in) = tokio::sync::mpsc::channel::<Vec<f32>>(1);
        let (detections_out, detections_in) =
            tokio::sync::mpsc::channel::<Result<Detection>>(8);

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
                            "audio input failed; ending the wake session"
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
        let scorer_detections = detections_out.clone();
        tokio::task::spawn_blocking(move || {
            while let Some(samples) = samples_in.blocking_recv() {
                let scored = match scorer.push(&samples) {
                    Ok(scored) => scored,
                    Err(error) => {
                        // A detector that fails takes the turn down with it: a
                        // pipeline that cannot tell whether it was addressed
                        // should not guess.
                        let _ = scorer_detections.blocking_send(Err(error));
                        return;
                    }
                };
                for detection in scored {
                    tracing::debug!(
                        provider,
                        phrase = %detection.phrase,
                        confidence = detection.confidence,
                        accepted = detection.accepted,
                        "scored a wake phrase"
                    );
                    if scorer_detections.blocking_send(Ok(detection)).is_err() {
                        return;
                    }
                }
            }
        });
        drop(detections_out);

        Box::pin(tokio_stream::wrappers::ReceiverStream::new(detections_in))
    }
}
