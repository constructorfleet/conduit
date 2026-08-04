//! Detection against the real models and real speech.
//!
//! The models are upstream release artifacts rather than checked-in binaries,
//! so these tests skip when they are absent and say why. `scripts/fetch-wake-models.sh`
//! downloads them, and CI runs it before the suite — so a skip locally is a
//! convenience, not a gap in what is verified.

use std::path::PathBuf;

use bytes::Bytes;
use conduit_provider::stt::AudioChunk;
use conduit_provider::wake::{Detection, WakePhrase, WakeWordDetector};
use conduit_provider::{ChunkStream, Provider};
use conduit_wake::OpenWakeWord;
use futures_util::StreamExt;

/// Where `scripts/fetch-wake-models.sh` puts the models.
fn models_dir() -> Option<PathBuf> {
    let directory = std::env::var_os("CONDUIT_WAKE_TEST_MODELS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/models"),
        PathBuf::from,
    );
    if directory.join("melspectrogram.onnx").exists() {
        Some(directory)
    } else {
        eprintln!(
            "skipping: no openWakeWord models in {}; run scripts/fetch-wake-models.sh",
            directory.display()
        );
        None
    }
}

/// One WAV fixture as the chunks a microphone would deliver.
///
/// Deliberately not a multiple of the 1280-sample scoring step: a source that
/// hands over odd-sized reads has to score the same as one that does not.
fn wav_chunks(name: &str) -> ChunkStream<AudioChunk> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/audio").join(name);
    let mut reader = hound::WavReader::open(&path)
        .unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    let samples: Vec<i16> =
        reader.samples::<i16>().map(|sample| sample.expect("sample")).collect();

    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        pcm.extend_from_slice(&sample.to_le_bytes());
    }
    // A second of silence on each side. In front so the phrase is not sitting
    // inside the very first window the detector ever sees; behind because a
    // microphone does not stop when someone finishes speaking, and a near miss
    // is reported at its peak — which needs the scores after it to compare.
    let mut audio = vec![0u8; 16_000 * 2];
    audio.extend_from_slice(&pcm);
    audio.extend_from_slice(&vec![0u8; 16_000 * 2]);

    let chunks: Vec<_> = audio
        .chunks(1_000)
        .enumerate()
        .map(|(sequence, data)| {
            Ok(AudioChunk { sequence: sequence as u64, data: Bytes::copy_from_slice(data) })
        })
        .collect();
    Box::pin(futures_util::stream::iter(chunks))
}

async fn detections(detector: &OpenWakeWord, audio: ChunkStream<AudioChunk>) -> Vec<Detection> {
    let mut stream = detector.detect(audio, Vec::new()).await.expect("session");
    let mut seen = Vec::new();
    while let Some(detection) = stream.next().await {
        seen.push(detection.expect("scoring did not fail"));
    }
    seen
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spoken_phrase_wakes_the_detector() {
    let Some(directory) = models_dir() else { return };
    let detector = OpenWakeWord::load("openwakeword", &directory, Vec::new(), 0.5)
        .expect("the models load");

    let accepted: Vec<_> = detections(&detector, wav_chunks("hey_jarvis.wav"))
        .await
        .into_iter()
        .filter(|detection| detection.accepted)
        .collect();

    assert_eq!(accepted.len(), 1, "one utterance is one activation, not a burst: {accepted:?}");
    assert_eq!(accepted[0].phrase, "hey jarvis");
    assert!(accepted[0].confidence > 0.9, "confidence was {}", accepted[0].confidence);
}

#[tokio::test(flavor = "multi_thread")]
async fn speech_that_is_not_the_phrase_does_not_wake_it() {
    let Some(directory) = models_dir() else { return };
    let detector = OpenWakeWord::load("openwakeword", &directory, Vec::new(), 0.5)
        .expect("the models load");

    let accepted: Vec<_> = detections(&detector, wav_chunks("negative.wav"))
        .await
        .into_iter()
        .filter(|detection| detection.accepted)
        .collect();

    assert!(accepted.is_empty(), "a sentence that is not the phrase woke it: {accepted:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_threshold_no_one_can_reach_reports_the_near_miss_instead() {
    // Rejections are reported so an operator can tune a threshold that is
    // firing at the television — or, here, one nothing can reach.
    let Some(directory) = models_dir() else { return };
    let detector = OpenWakeWord::load("openwakeword", &directory, Vec::new(), 1.01)
        .expect("the models load");

    let seen = detections(&detector, wav_chunks("hey_jarvis.wav")).await;

    assert!(seen.iter().all(|detection| !detection.accepted), "nothing can reach 101%");
    let peak = seen
        .iter()
        .max_by(|left, right| left.confidence.total_cmp(&right.confidence))
        .expect("the near miss is reported rather than swallowed");
    assert!(peak.confidence > 0.9, "the near miss carries its score: {}", peak.confidence);
    assert_eq!(peak.phrase, "hey jarvis");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_detector_lists_the_phrases_it_has_models_for() {
    let Some(directory) = models_dir() else { return };
    let detector = OpenWakeWord::load("openwakeword", &directory, Vec::new(), 0.5)
        .expect("the models load");

    let configured = &detector.descriptor().metadata.phrases;
    assert_eq!(configured.len(), 1);
    assert_eq!(configured[0], WakePhrase::new("hey jarvis").with_threshold(0.5));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_directory_without_models_is_refused_when_the_definition_is_saved() {
    // Not at the first turn, when someone is standing there speaking to it.
    let empty = tempdir();
    let error = OpenWakeWord::load("openwakeword", &empty, Vec::new(), 0.5)
        .expect_err("an empty directory is not a model directory");

    assert!(
        error.to_string().contains("melspectrogram.onnx"),
        "the error names what is missing: {error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phrase_with_no_model_is_refused_by_name() {
    let Some(directory) = models_dir() else { return };
    let error =
        OpenWakeWord::load("openwakeword", &directory, vec!["hey mycroft".to_owned()], 0.5)
            .expect_err("there is no model for that phrase");

    assert!(error.to_string().contains("hey mycroft"), "the error names the phrase: {error}");
}

/// An empty scratch directory, named for this process so two runs cannot
/// collide.
fn tempdir() -> PathBuf {
    let path = std::env::temp_dir().join(format!("conduit-wake-{}", std::process::id()));
    std::fs::create_dir_all(&path).expect("create scratch directory");
    path
}
