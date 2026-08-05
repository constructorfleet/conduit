//! Scoring the Silero VAD model with `tract`.
//!
//! One model, three inputs, two outputs — which is what makes this shorter than
//! `conduit-wake`'s chain of three and, in one respect, harder:
//!
//! 1. `input` is `[1, window]` raw samples, normalized to `-1.0..=1.0`.
//! 2. `state` is `[2, 1, 128]`, the recurrent state carried between windows.
//! 3. `sr` is the sample rate, as a scalar `i64`.
//!
//! The outputs are a single probability and the updated `state`. That state is
//! the difference from the wake chain: openWakeWord's classifiers are stateless
//! and score a fixed window of embeddings, so a chunk can be scored in
//! isolation. Silero's cannot — the probability for a window depends on every
//! window before it, so the state has to be threaded through in order and a
//! session that lost it would report about a stream that never happened. It lives
//! on [`Scorer`], one per detection call, for exactly that reason.
//!
//! # Normalized here, unlike the wake models
//!
//! openWakeWord scores raw `i16` magnitudes and dividing by 32768 makes every
//! phrase score zero. Silero is the opposite: it was trained on floats in
//! `-1.0..=1.0`, and handing it raw magnitudes saturates every window into
//! confident speech. Two models, two conventions, and getting either backwards
//! produces a detector that is wrong in a way that looks like it is working.

use std::path::Path;

use conduit_core::{Error, Result};
use conduit_provider::vad::Activity;
use tract_onnx::prelude::*;

/// A model compiled for a fixed input shape.
type Plan = std::sync::Arc<TypedRunnableModel>;

/// Samples in one scoring window at 16 kHz — 32 ms, which is the only window
/// Silero v5 accepts at that rate.
pub(crate) const WINDOW_16K: usize = 512;

/// Samples in one scoring window at 8 kHz — the same 32 ms.
pub(crate) const WINDOW_8K: usize = 256;

/// Values in the recurrent state: two layers, one batch, 128 wide.
const STATE_SHAPE: [usize; 3] = [2, 1, 128];

/// The loaded model, and the window it was compiled for.
///
/// Shared across detection sessions — the weights are read-only — while the
/// state that makes scoring sequential lives on [`Scorer`].
pub(crate) struct Model {
    plan: Plan,
    /// Samples one scoring window consumes.
    window: usize,
    /// The rate this model was compiled for, passed to it on every window.
    sample_rate: i64,
}

/// Written by hand because a compiled `tract` plan is not `Debug`. Reports the
/// window and rate, which is what anyone printing this wants to know.
impl std::fmt::Debug for Model {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Model")
            .field("window", &self.window)
            .field("sample_rate", &self.sample_rate)
            .finish()
    }
}

impl Model {
    /// Loads the model at `path`, compiled for the 16 kHz window.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the file is missing or is not a model
    /// `tract` can drive.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        Self::load_for_rate(path, 16_000)
    }

    /// Loads the model at `path`, compiled for `sample_rate`'s window.
    ///
    /// The window is a property of the compiled plan rather than of a call, so a
    /// detector scores one rate. That is why the runtime refuses a rate mismatch
    /// rather than adapting: there is nothing here to adapt.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if `sample_rate` is not one Silero was trained
    /// at, if the file is missing, or if it is not a model `tract` can drive.
    pub(crate) fn load_for_rate(path: &Path, sample_rate: u32) -> Result<Self> {
        let window = match sample_rate {
            16_000 => WINDOW_16K,
            8_000 => WINDOW_8K,
            other => {
                return Err(Error::Config(format!(
                    "Silero scores 8000 Hz and 16000 Hz, not {other} Hz"
                )))
            }
        };
        if !path.exists() {
            return Err(Error::Config(format!(
                "`{}` is not a Silero VAD model: no such file",
                path.display()
            )));
        }

        let plan = tract_onnx::onnx()
            .model_for_path(path)
            // Samples, state, rate — the input order of the 16 kHz export.
            .and_then(|model| model.with_input_fact(0, f32::fact([1, window]).into()))
            .and_then(|model| model.with_input_fact(1, f32::fact(STATE_SHAPE).into()))
            // The rate as a rank-0 *constant* rather than as an input shape: the
            // model compares it against a literal, and a comparison a runtime
            // cannot fold leaves a branch it cannot analyse. This is also why the
            // window is a property of the compiled plan — one plan, one rate.
            .and_then(|model| {
                model.with_input_fact(2, Tensor::from(i64::from(sample_rate)).into())
            })
            .and_then(tract_onnx::prelude::InferenceModelExt::into_optimized)
            .and_then(tract_onnx::prelude::IntoRunnable::into_runnable)
            .map_err(|error| {
                Error::Config(format!(
                    "cannot load the Silero VAD model `{}`: {error}",
                    path.display()
                ))
            })?;

        Ok(Self { plan, window, sample_rate: i64::from(sample_rate) })
    }

    /// Scores one window, returning the speech probability and the state that
    /// follows it.
    fn run(&self, samples: &[f32], state: &[f32]) -> Result<(f32, Vec<f32>)> {
        let failed = |error: TractError| Error::Provider {
            provider: "silero".to_owned(),
            source: Box::new(std::io::Error::other(error.to_string())),
        };
        let input = Tensor::from_shape(&[1, self.window], samples).map_err(failed)?;
        let carried = Tensor::from_shape(&STATE_SHAPE, state).map_err(failed)?;
        let rate = Tensor::from(self.sample_rate);

        let output =
            self.plan.run(tvec!(input.into(), carried.into(), rate.into())).map_err(failed)?;

        let probability: &Tensor = &output[0];
        let probability = *probability
            .view()
            .as_slice::<f32>()
            .map_err(failed)?
            .first()
            .ok_or_else(|| Error::Provider {
                provider: "silero".to_owned(),
                source: Box::new(std::io::Error::other("the model produced no probability")),
            })?;

        // A model that returned no state is one this session cannot continue
        // against. Carrying the old state forward would score every later window
        // as though the stream restarted, which reads as a detector that goes
        // deaf partway through an utterance.
        let next: &Tensor = output.get(1).ok_or_else(|| Error::Provider {
            provider: "silero".to_owned(),
            source: Box::new(std::io::Error::other(
                "the model returned no recurrent state, so a stream cannot be scored",
            )),
        })?;

        Ok((probability, next.view().as_slice::<f32>().map_err(failed)?.to_vec()))
    }
}

/// A live scoring session over one audio stream.
///
/// One per [`crate::SileroVad::detect`] call: the model is shared, the recurrent
/// state and the leftover samples are not.
pub(crate) struct Scorer {
    model: std::sync::Arc<Model>,
    /// Minimum probability to call a window speech.
    threshold: f32,
    /// The recurrent state, carried from the last window scored.
    state: Vec<f32>,
    /// Samples that have arrived but do not yet make a whole window.
    pending: Vec<f32>,
    /// The last verdict, reported again for a chunk too short to complete a
    /// window.
    last: Activity,
}

impl Scorer {
    pub(crate) fn new(model: std::sync::Arc<Model>, threshold: f32) -> Self {
        Self {
            model,
            threshold,
            state: vec![0.0; STATE_SHAPE.iter().product()],
            pending: Vec::new(),
            // Silence, so a stream that opens with a chunk shorter than a window
            // is not announced as speech before anything has been scored.
            last: Activity::silence(0.0),
        }
    }

    /// Scores `samples`, returning the one verdict this chunk gets.
    ///
    /// A chunk carries whatever a device's buffer held, so it may complete
    /// several windows or none. The verdict is speech if **any** window completed
    /// within it was speech, at the highest confidence any of them reported:
    /// erring toward speech costs the recognizer a few milliseconds of silence,
    /// and erring toward silence costs a word.
    ///
    /// A chunk too short to complete a window repeats the previous verdict, which
    /// is what has not changed rather than a guess — the sound has not been
    /// re-evaluated, not fallen silent. Reporting silence there would punch a hole
    /// in the middle of a word for any device sending chunks under 32 ms.
    ///
    /// # Errors
    ///
    /// Returns an error if the model fails to run, which ends the session. The
    /// trimming stage recovers by forwarding the rest of the audio untrimmed.
    pub(crate) fn push(&mut self, samples: &[f32]) -> Result<Activity> {
        self.pending.extend_from_slice(samples);

        let mut scored: Option<Activity> = None;
        while self.pending.len() >= self.model.window {
            let window: Vec<f32> = self.pending.drain(..self.model.window).collect();
            let (probability, next) = self.model.run(&window, &self.state)?;
            self.state = next;

            let verdict = if probability >= self.threshold {
                Activity::speech(probability)
            } else {
                Activity::silence(probability)
            };
            scored = Some(match scored {
                // Any speech in the chunk makes the chunk speech, and the
                // confidence reported is the strongest evidence for it.
                Some(previous) if previous.speech && !verdict.speech => previous,
                Some(previous)
                    if previous.confidence > verdict.confidence && !verdict.speech =>
                {
                    previous
                }
                _ => verdict,
            });
        }

        if let Some(verdict) = scored {
            self.last = verdict;
        }
        Ok(self.last)
    }
}

/// Reads little-endian 16-bit PCM as the floats Silero was trained on.
///
/// Normalized to `-1.0..=1.0`, which is the opposite of `conduit-wake`'s reader:
/// openWakeWord scores raw `i16` magnitudes. Handing Silero those saturates every
/// window into confident speech, so a trimmer built on the wrong convention
/// forwards everything and looks like it is working.
pub(crate) fn samples_from_pcm(data: &[u8]) -> Vec<f32> {
    data.chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32_768.0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_is_normalized_to_the_range_silero_was_trained_on() {
        // Not raw magnitudes, unlike the wake models: handing Silero 32767.0
        // makes every window confident speech, which is a trimmer that trims
        // nothing while appearing to work.
        let data = [0x00, 0x00, 0xff, 0x7f, 0x00, 0x80];
        let samples = samples_from_pcm(&data);

        assert!((samples[0] - 0.0).abs() < f32::EPSILON);
        assert!(samples[1] > 0.99 && samples[1] < 1.0, "full scale, got {}", samples[1]);
        assert!((samples[2] + 1.0).abs() < f32::EPSILON, "full negative scale");
    }

    #[test]
    fn an_odd_trailing_byte_is_not_half_a_sample() {
        assert_eq!(samples_from_pcm(&[0x10, 0x00, 0x7f]).len(), 1);
    }

    #[test]
    fn a_rate_silero_was_not_trained_at_is_refused_by_name() {
        // Reached before the file is opened, so the message is about the rate
        // rather than about a path that may be perfectly fine.
        let error = Model::load_for_rate(Path::new("/nonexistent.onnx"), 44_100)
            .expect_err("not a Silero rate")
            .to_string();

        assert!(error.contains("44100"), "names the rate asked for: {error}");
        assert!(error.contains("16000"), "and one it has: {error}");
    }

    #[test]
    fn a_missing_model_is_reported_as_a_missing_file() {
        let error = Model::load(Path::new("/nonexistent/silero_vad.onnx"))
            .expect_err("no such file")
            .to_string();

        assert!(error.contains("no such file"), "says what is wrong: {error}");
    }

    #[test]
    fn the_two_windows_are_the_same_length_of_sound() {
        // 32 ms at either rate. If these ever diverge, the trimmer's tail and
        // lead-in arithmetic silently means something different per rate.
        assert_eq!(WINDOW_16K * 1_000 / 16_000, WINDOW_8K * 1_000 / 8_000);
    }
}
