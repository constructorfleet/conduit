//! Detection against the real Silero model.
//!
//! The model is an upstream artifact rather than a checked-in binary, so these
//! tests skip when it is absent and say why. `scripts/fetch-vad-model.sh`
//! downloads it, and CI runs that before the suite — so a skip locally is a
//! convenience, not a gap in what is verified.
//!
//! What is worth verifying against the real model rather than a fake is the one
//! thing a unit test cannot catch: the PCM convention. openWakeWord scores raw
//! `i16` magnitudes and Silero wants floats in `-1.0..=1.0`, and a detector
//! built on the wrong one of those does not crash — it calls everything speech,
//! or nothing, and a trimming stage on top of it looks like it is working.

use std::path::PathBuf;

use bytes::Bytes;
use conduit_provider::stt::AudioChunk;
use conduit_provider::vad::{Activity, VadOptions, VoiceActivityDetector};
use conduit_provider::ChunkStream;
use conduit_vad::{SileroVad, DEFAULT_THRESHOLD};
use futures_util::StreamExt;

/// Where `scripts/fetch-vad-model.sh` puts the model.
fn model_path() -> Option<PathBuf> {
    let path = std::env::var_os("CONDUIT_VAD_TEST_MODEL").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/models")
                .join(conduit_vad::DEFAULT_MODEL_FILE)
        },
        PathBuf::from,
    );
    if path.exists() {
        Some(path)
    } else {
        eprintln!(
            "skipping: no Silero model at {}; run scripts/fetch-vad-model.sh",
            path.display()
        );
        None
    }
}

fn detector(path: &PathBuf) -> SileroVad {
    SileroVad::load("silero", path, DEFAULT_THRESHOLD, 700).expect("load the model")
}

/// 16-bit little-endian PCM at 16 kHz, as the chunks a microphone delivers.
///
/// Deliberately not a multiple of the 512-sample window: a device handing over
/// odd-sized reads has to score the same as one that does not.
fn chunks_of(samples: &[i16]) -> ChunkStream<AudioChunk> {
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        pcm.extend_from_slice(&sample.to_le_bytes());
    }
    let chunks: Vec<_> = pcm
        .chunks(1_000)
        .enumerate()
        .map(|(sequence, data)| {
            Ok(AudioChunk { sequence: sequence as u64, data: Bytes::copy_from_slice(data) })
        })
        .collect();
    Box::pin(futures_util::stream::iter(chunks))
}

/// Digital silence, which is the one signal every VAD must agree about.
fn silence(seconds: f32) -> Vec<i16> {
    vec![0; (16_000.0 * seconds) as usize]
}

/// Noise at full scale.
///
/// Not speech, but loud — which is what separates a learned detector from an
/// energy threshold, and what a detector fed raw `i16` magnitudes reports as
/// confident speech because every window saturates.
fn loud_noise(seconds: f32) -> Vec<i16> {
    let count = (16_000.0 * seconds) as usize;
    // A deterministic pseudo-random sequence: a fixed LCG rather than a random
    // number generator, so a failure here is reproducible.
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            #[allow(clippy::cast_possible_truncation)]
            ((state >> 33) as i16)
        })
        .collect()
}

async fn verdicts(detector: &SileroVad, audio: ChunkStream<AudioChunk>) -> Vec<Activity> {
    let mut stream = detector.detect(audio, VadOptions::default()).await.expect("session");
    let mut seen = Vec::new();
    while let Some(activity) = stream.next().await {
        seen.push(activity.expect("no scoring failures"));
    }
    seen
}

#[tokio::test]
async fn the_model_calls_digital_silence_silence() {
    let Some(path) = model_path() else { return };
    let audio = silence(2.0);
    let expected = (audio.len() * 2).div_ceil(1_000);

    let seen = verdicts(&detector(&path), chunks_of(&audio)).await;

    assert_eq!(
        seen.len(),
        expected,
        "one verdict per chunk, which the trimmer pairs by position"
    );
    assert!(
        seen.iter().all(|activity| !activity.speech),
        "two seconds of nothing is not speech: {} of {} said it was",
        seen.iter().filter(|activity| activity.speech).count(),
        seen.len()
    );
}

#[tokio::test]
async fn loudness_alone_is_not_speech() {
    // The assertion that catches the PCM convention being backwards. Handed raw
    // `i16` magnitudes, Silero saturates and calls this confident speech; handed
    // the floats it was trained on, it correctly does not. An energy threshold
    // would fail this too, which is the reason Conduit does not ship one.
    let Some(path) = model_path() else { return };

    let seen = verdicts(&detector(&path), chunks_of(&loud_noise(2.0))).await;

    let speech = seen.iter().filter(|activity| activity.speech).count();
    assert!(
        speech * 2 < seen.len(),
        "full-scale noise is mostly not speech: {speech} of {} windows said it was",
        seen.len()
    );
}

#[tokio::test]
async fn a_rate_the_model_was_not_trained_at_is_refused_when_it_is_loaded() {
    // Refused rather than resampled, and refused while an operator is still
    // looking at the definition: a wrong rate makes the window the wrong length
    // of sound, and the detector would report confidently about audio it never
    // heard.
    let Some(path) = model_path() else { return };

    let error = conduit_provider::vad::accepts_rate(
        "silero",
        &conduit_provider::descriptor::Metadata::default()
            .with_sample_rates(conduit_vad::SILERO_SAMPLE_RATES.to_vec()),
        44_100,
    )
    .expect_err("not a rate Silero scores")
    .to_string();

    assert!(error.contains("44100"), "names the rate asked for: {error}");
    // And the model itself loads at a rate it does know, so the refusal above is
    // about the rate rather than about a model that never worked.
    detector(&path);
}

/// The speech fixture `conduit-wake` already carries, at 16 kHz mono `i16`.
///
/// Borrowed rather than duplicated: it is a real voice saying a real phrase,
/// which is what the positive assertion needs, and a second copy of the same
/// recording in this crate would be a binary to keep in step for no gain.
fn spoken_phrase() -> Vec<i16> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../conduit-wake/tests/audio/hey_jarvis.wav");
    let mut reader = hound::WavReader::open(&path)
        .unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    reader.samples::<i16>().map(|sample| sample.expect("sample")).collect()
}

#[tokio::test]
async fn a_real_voice_is_found_in_the_silence_around_it() {
    // The assertion the negative ones cannot make between them: a detector that
    // called everything silence would satisfy both of those and trim away every
    // word. Speech in the middle, silence at the ends, and the verdicts have to
    // say so — which is also what the trimming stage downstream relies on.
    let Some(path) = model_path() else { return };
    let mut audio = silence(0.5);
    audio.extend_from_slice(&spoken_phrase());
    audio.extend_from_slice(&silence(0.5));

    let seen = verdicts(&detector(&path), chunks_of(&audio)).await;

    let speech = seen.iter().filter(|activity| activity.speech).count();
    assert!(speech > 0, "a real voice is speech: none of {} chunks said so", seen.len());
    assert!(
        speech < seen.len(),
        "and the half-second of silence on each end is not: all {} chunks said it was",
        seen.len()
    );
}
