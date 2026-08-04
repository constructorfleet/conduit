//! Scoring openWakeWord models with `tract`.
//!
//! Three models in a chain, all ONNX, all shipped by openWakeWord itself:
//!
//! 1. `melspectrogram.onnx` turns 16 kHz mono samples into mel frames of
//!    `bins` channels, one frame per 10 ms.
//! 2. `embedding_model.onnx` turns `frames` mel frames into one embedding.
//! 3. `<phrase>.onnx` turns the last `embeddings` embeddings into one
//!    probability, one model per phrase.
//!
//! Rather than reproduce openWakeWord's own streaming arithmetic — how many
//! mel frames a chunk adds, and where the seam between two chunks falls — this
//! keeps a rolling window of raw audio and recomputes the mel over all of it on
//! every chunk, then hands the newest `frames` frames to the embedding model.
//! There is no seam to get wrong, the mel plan is built once because its input
//! length never changes, and the cost is a fraction of the budget: scoring one
//! 80 ms chunk takes about 3 ms.

use std::path::{Path, PathBuf};

use conduit_core::{Error, Result};
use conduit_provider::wake::{Detection, WakePhrase};
use tract_onnx::prelude::*;
use tract_onnx::tract_hir::infer::Factoid;

use crate::phrase::{phrase_from_model_name, phrase_matches};

/// A model compiled for a fixed input shape.
type Plan = std::sync::Arc<TypedRunnableModel>;

/// The file every openWakeWord installation has, turning audio into mel frames.
const MEL_MODEL: &str = "melspectrogram.onnx";
/// The file every openWakeWord installation has, turning mel frames into an
/// embedding. Shared by every phrase, which is why a phrase costs one small
/// model rather than three.
const EMBEDDING_MODEL: &str = "embedding_model.onnx";

/// Samples in one scoring step: 80 ms at the 16 kHz the models were trained
/// at, which is openWakeWord's own frame rate.
pub(crate) const CHUNK_SAMPLES: usize = 1_280;
/// Samples between two mel frames — 10 ms — and the samples one frame spans.
/// Used only to size the audio window, which is then verified against the
/// model rather than trusted.
const MEL_HOP: usize = 160;
const MEL_SPAN: usize = 400;

/// openWakeWord scales the mel model's output before the embedding model sees
/// it. Without this the embeddings are meaningless and every phrase scores
/// zero — which looks exactly like a detector that simply never fires.
const MEL_SCALE: f32 = 10.0;
const MEL_OFFSET: f32 = 2.0;

/// One phrase and the model that scores it.
struct PhraseModel {
    /// The phrase as an operator writes it, e.g. `hey jarvis`.
    phrase: String,
    /// The classifier, compiled for `[1, embeddings, dim]`.
    plan: Plan,
}

/// Every model one detector needs, and the shapes they declared.
pub(crate) struct Models {
    mel: Plan,
    embedding: Plan,
    phrases: Vec<PhraseModel>,
    /// Mel frames one embedding consumes, from the embedding model's shape.
    frames: usize,
    /// Mel channels per frame, from the embedding model's shape.
    bins: usize,
    /// Embeddings one score consumes, from a classifier's shape.
    embeddings: usize,
    /// Values in one embedding, from a classifier's shape.
    dim: usize,
    /// Samples held so the mel pass can produce `frames` frames.
    window: usize,
}

impl Models {
    /// Loads the shared models and one classifier per phrase found in `dir`.
    ///
    /// `wanted` narrows which phrases are loaded; empty loads every model in
    /// the directory, which is what a definition naming no phrases means.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the directory is unreadable, if either
    /// shared model is missing, if a model's declared shape is not one this
    /// chain can drive, or if no phrase model was found at all.
    pub(crate) fn load(dir: &Path, wanted: &[String]) -> Result<Self> {
        let mel_path = dir.join(MEL_MODEL);
        let embedding_path = dir.join(EMBEDDING_MODEL);
        for required in [&mel_path, &embedding_path] {
            if !required.exists() {
                return Err(Error::Config(format!(
                    "`{}` is not an openWakeWord model directory: it has no `{}`",
                    dir.display(),
                    required.file_name().unwrap_or_default().to_string_lossy()
                )));
            }
        }

        // The embedding model declares how much context it wants and how wide a
        // mel frame is, so neither is a constant here: a re-exported model with
        // a different window drives this chain correctly or fails loudly.
        let (frames, bins) = embedding_shape(&embedding_path)?;

        let discovered = discover_phrase_models(dir, wanted)?;
        let Some((_, first)) = discovered.first() else {
            return Err(Error::Config(phrase_models_missing(dir, wanted)));
        };
        let (embeddings, dim) = classifier_shape(first)?;

        // Enough audio for `frames` frames, rounded up to whole chunks so the
        // window is a multiple of what arrives.
        let needed = frames * MEL_HOP + MEL_SPAN;
        let window = needed.div_ceil(CHUNK_SAMPLES) * CHUNK_SAMPLES;

        let mel = compile(&mel_path, &[1, window])?;
        let produced = mel_frames(&mel, window, bins)?;
        if produced < frames {
            return Err(Error::Config(format!(
                "`{}` produced {produced} mel frames from {window} samples, but the embedding \
                 model wants {frames}",
                mel_path.display()
            )));
        }

        let embedding = compile(&embedding_path, &[1, frames, bins, 1])?;
        // How wide an embedding is comes from the classifier, which is a
        // different file — so the two have to be checked against each other.
        // Left unchecked, a mismatched pair drifts the rolling buffer out of
        // shape and fails on every chunk instead of once, here.
        let produced = run(&embedding, &[1, frames, bins, 1], &vec![0.0; frames * bins])?.len();
        if produced != dim {
            return Err(Error::Config(format!(
                "`{}` produces {produced} values per embedding, but the phrase models want \
                 {dim}",
                embedding_path.display()
            )));
        }
        let mut loaded = Vec::with_capacity(discovered.len());
        for (phrase, path) in discovered {
            loaded.push(PhraseModel { phrase, plan: compile(&path, &[1, embeddings, dim])? });
        }

        Ok(Self { mel, embedding, phrases: loaded, frames, bins, embeddings, dim, window })
    }

    /// The phrases this detector has models for, in the order they were found.
    pub(crate) fn phrases(&self) -> Vec<String> {
        self.phrases.iter().map(|model| model.phrase.clone()).collect()
    }
}

/// Why no phrase model was found, said in terms of what the operator asked for.
fn phrase_models_missing(dir: &Path, wanted: &[String]) -> String {
    if wanted.is_empty() {
        format!(
            "`{}` has no phrase models: an openWakeWord directory needs at least one \
             `<phrase>.onnx` beside `{MEL_MODEL}` and `{EMBEDDING_MODEL}`",
            dir.display()
        )
    } else {
        format!("`{}` has no model for any of: {}", dir.display(), wanted.join(", "))
    }
}

/// Every `<phrase>.onnx` in `dir`, excluding the two shared models.
fn discover_phrase_models(dir: &Path, wanted: &[String]) -> Result<Vec<(String, PathBuf)>> {
    let entries = std::fs::read_dir(dir).map_err(|error| {
        Error::Config(format!("cannot read wake models from `{}`: {error}", dir.display()))
    })?;

    let mut found = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| {
                Error::Config(format!(
                    "cannot read wake models from `{}`: {error}",
                    dir.display()
                ))
            })?
            .path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("onnx") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name == MEL_MODEL || name == EMBEDDING_MODEL {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let phrase = phrase_from_model_name(stem);
        if wanted.is_empty()
            || wanted.iter().any(|candidate| phrase_matches(candidate, &phrase))
        {
            found.push((phrase, path));
        }
    }
    // Directory order is whatever the filesystem says; a stable order keeps the
    // phrase list an operator sees from reshuffling between restarts.
    found.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(found)
}

/// Compiles `path` for one fixed input shape.
fn compile(path: &Path, shape: &[usize]) -> Result<Plan> {
    let model = tract_onnx::onnx()
        .model_for_path(path)
        .and_then(|model| model.with_input_fact(0, f32::fact(shape).into()))
        .and_then(tract_onnx::prelude::InferenceModelExt::into_optimized)
        .and_then(tract_onnx::prelude::IntoRunnable::into_runnable);
    model.map_err(|error| {
        Error::Config(format!("cannot load wake model `{}`: {error}", path.display()))
    })
}

/// The declared input dimensions of `path`, where they are concrete.
fn declared_shape(path: &Path) -> Result<Vec<Option<usize>>> {
    let model = tract_onnx::onnx().model_for_path(path).map_err(|error| {
        Error::Config(format!("cannot read wake model `{}`: {error}", path.display()))
    })?;
    let describe = |error: TractError| {
        Error::Config(format!("cannot read the shape of `{}`: {error}", path.display()))
    };
    let outlet = model.input_outlets().map_err(describe)?[0];
    let fact = model.outlet_fact(outlet).map_err(describe)?;
    let rank = fact.shape.rank().concretize().unwrap_or_default();
    Ok((0..rank)
        .map(|axis| {
            fact.shape
                .dim(axis as usize)
                .and_then(|dim| dim.concretize())
                .and_then(|dim| dim.to_i64().ok())
                .and_then(|dim| usize::try_from(dim).ok())
        })
        .collect())
}

/// How many mel frames an embedding wants, and how wide each is.
fn embedding_shape(path: &Path) -> Result<(usize, usize)> {
    match declared_shape(path)?.as_slice() {
        [_, Some(frames), Some(bins), _] => Ok((*frames, *bins)),
        other => Err(Error::Config(format!(
            "`{}` declares {other:?}, which is not an openWakeWord embedding model",
            path.display()
        ))),
    }
}

/// How many embeddings a score wants, and how wide each is.
fn classifier_shape(path: &Path) -> Result<(usize, usize)> {
    match declared_shape(path)?.as_slice() {
        [_, Some(embeddings), Some(dim)] => Ok((*embeddings, *dim)),
        other => Err(Error::Config(format!(
            "`{}` declares {other:?}, which is not an openWakeWord phrase model",
            path.display()
        ))),
    }
}

/// How many mel frames the mel model makes from `samples` samples.
fn mel_frames(mel: &Plan, samples: usize, bins: usize) -> Result<usize> {
    let silence = vec![0.0f32; samples];
    let produced = run(mel, &[1, samples], &silence)?;
    Ok(produced.len() / bins)
}

/// Runs one plan over one input, returning its output as a flat vector.
fn run(plan: &Plan, shape: &[usize], values: &[f32]) -> Result<Vec<f32>> {
    let failed = |error: TractError| Error::Provider {
        provider: "openwakeword".to_owned(),
        source: Box::new(std::io::Error::other(error.to_string())),
    };
    let input = Tensor::from_shape(shape, values).map_err(failed)?;
    let output = plan.run(tvec!(input.into())).map_err(failed)?;
    let tensor: &Tensor = &output[0];
    Ok(tensor.view().as_slice::<f32>().map_err(failed)?.to_vec())
}

/// A live scoring session over one audio stream.
///
/// One per [`crate::OpenWakeWord::detect`] call: the models are shared, the
/// rolling state is not.
pub(crate) struct Scorer {
    models: std::sync::Arc<Models>,
    /// The acceptance threshold per requested phrase.
    thresholds: Vec<f32>,
    /// Rolling raw audio, always `window` samples.
    audio: Vec<f32>,
    /// Rolling embeddings, always `embeddings * dim` values.
    embeddings: Vec<f32>,
    /// Samples that have arrived but do not yet make a whole chunk.
    pending: Vec<f32>,
    /// Chunks left to ignore per phrase after it fired.
    refractory: Vec<usize>,
    /// The previous score per phrase, for spotting a peak.
    previous: Vec<f32>,
}

/// Chunks to ignore a phrase for after it fires — two seconds.
///
/// One utterance crosses the threshold on several consecutive chunks, and a
/// gate that opened once does not need to be told again. Without this a single
/// "hey jarvis" is a dozen detections on the event stream.
const REFRACTORY_CHUNKS: usize = 25;

/// The score below which a near miss is not worth reporting.
///
/// Rejections exist so an operator can tune a threshold, which means the
/// interesting ones are the near misses. Reporting every chunk that scored
/// 0.0001 would put twelve events a second per phrase on the bus and bury them.
const NEAR_MISS: f32 = 0.1;

impl Scorer {
    pub(crate) fn new(models: std::sync::Arc<Models>, thresholds: Vec<f32>) -> Self {
        let audio = vec![0.0; models.window];
        let embeddings = vec![0.0; models.embeddings * models.dim];
        let count = models.phrases.len();
        Self {
            models,
            thresholds,
            audio,
            embeddings,
            pending: Vec::with_capacity(CHUNK_SAMPLES),
            refractory: vec![0; count],
            previous: vec![0.0; count],
        }
    }

    /// Scores `samples`, returning whatever the audio decided.
    ///
    /// Samples are buffered until they make a whole chunk, so a source that
    /// hands over odd-sized reads scores identically to one that does not.
    pub(crate) fn push(&mut self, samples: &[f32]) -> Result<Vec<Detection>> {
        let mut detections = Vec::new();
        self.pending.extend_from_slice(samples);
        while self.pending.len() >= CHUNK_SAMPLES {
            let chunk: Vec<f32> = self.pending.drain(..CHUNK_SAMPLES).collect();
            detections.extend(self.score_chunk(&chunk)?);
        }
        Ok(detections)
    }

    fn score_chunk(&mut self, chunk: &[f32]) -> Result<Vec<Detection>> {
        self.audio.drain(..chunk.len());
        self.audio.extend_from_slice(chunk);

        let mels = run(&self.models.mel, &[1, self.models.window], &self.audio)?;
        let produced = mels.len() / self.models.bins;
        let newest = (produced - self.models.frames) * self.models.bins;
        let window: Vec<f32> =
            mels[newest..].iter().map(|value| value / MEL_SCALE + MEL_OFFSET).collect();

        let embedding = run(
            &self.models.embedding,
            &[1, self.models.frames, self.models.bins, 1],
            &window,
        )?;
        self.embeddings.drain(..self.models.dim);
        self.embeddings.extend_from_slice(&embedding);

        let mut detections = Vec::new();
        for (index, model) in self.models.phrases.iter().enumerate() {
            let scored = run(
                &model.plan,
                &[1, self.models.embeddings, self.models.dim],
                &self.embeddings,
            )?;
            let confidence = scored.first().copied().unwrap_or_default();
            let threshold = self.thresholds.get(index).copied().unwrap_or(0.5);

            if self.refractory[index] > 0 {
                self.refractory[index] -= 1;
                self.previous[index] = confidence;
                continue;
            }

            if confidence >= threshold {
                detections.push(Detection {
                    phrase: model.phrase.clone(),
                    confidence,
                    accepted: true,
                });
                self.refractory[index] = REFRACTORY_CHUNKS;
            } else {
                // A near miss is reported once, at its peak, rather than on
                // every chunk of the slope on either side of it.
                let peaked = self.previous[index] >= NEAR_MISS
                    && self.previous[index] < threshold
                    && confidence < self.previous[index];
                if peaked {
                    detections.push(Detection {
                        phrase: model.phrase.clone(),
                        confidence: self.previous[index],
                        accepted: false,
                    });
                }
            }
            self.previous[index] = confidence;
        }
        Ok(detections)
    }
}

/// The thresholds to score each loaded phrase at.
///
/// A pipeline that named phrases with their own thresholds gets those; one that
/// named none gets the definition's.
pub(crate) fn thresholds_for(
    models: &Models,
    requested: &[WakePhrase],
    default: f32,
) -> Vec<f32> {
    models
        .phrases
        .iter()
        .map(|model| {
            requested
                .iter()
                .find(|candidate| phrase_matches(&candidate.phrase, &model.phrase))
                .map_or(default, |candidate| candidate.threshold)
        })
        .collect()
}

/// Reads 16-bit little-endian mono samples as the floats the models want.
///
/// openWakeWord was trained on raw `int16` magnitudes rather than on samples
/// normalized to `-1.0..=1.0`, so this scales nothing.
pub(crate) fn samples_from_pcm(data: &[u8]) -> Vec<f32> {
    data.chunks_exact(2).map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]]))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_is_read_as_the_magnitudes_the_models_were_trained_on() {
        // Not normalized to -1.0..=1.0: openWakeWord scores raw int16 values,
        // and dividing by 32768 here makes every phrase score zero.
        let data = [0x00, 0x00, 0xff, 0x7f, 0x00, 0x80];
        assert_eq!(samples_from_pcm(&data), vec![0.0, 32767.0, -32768.0]);
    }

    #[test]
    fn an_odd_trailing_byte_is_not_half_a_sample() {
        assert_eq!(samples_from_pcm(&[0x10, 0x00, 0x7f]), vec![16.0]);
    }
}
