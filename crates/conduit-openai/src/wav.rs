//! Packaging captured audio into a file the transcription API will accept.
//!
//! The pipeline carries raw samples, but `/audio/transcriptions` takes an
//! uploaded *file* and sniffs its type. Raw PCM therefore needs a container,
//! and a WAV header is 44 bytes of arithmetic — far less than a dependency.

use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::{Error, Result};

/// Audio packaged for upload.
pub struct Upload {
    /// File contents.
    pub bytes: Vec<u8>,
    /// File name, which is how the API infers the format.
    pub filename: &'static str,
    /// MIME type.
    pub mime: &'static str,
}

/// Packages captured samples as an uploadable file.
///
/// PCM is wrapped in a WAV header. FLAC is already a container and passes
/// through untouched.
///
/// # Errors
///
/// Returns [`Error::Config`] for raw Opus frames, which are not a file: Opus
/// needs an Ogg container this code does not build. Capture as PCM or FLAC.
pub fn package(format: AudioFormat, samples: Vec<u8>) -> Result<Upload> {
    match format.encoding {
        Encoding::PcmS16Le | Encoding::PcmF32Le => Ok(Upload {
            bytes: wav(format, &samples),
            filename: "audio.wav",
            mime: "audio/wav",
        }),
        Encoding::Flac => {
            Ok(Upload { bytes: samples, filename: "audio.flac", mime: "audio/flac" })
        }
        Encoding::Opus => Err(Error::Config(
            "raw Opus frames cannot be uploaded; capture as PCM or FLAC".to_owned(),
        )),
        // `Encoding` is non-exhaustive: a newer core may name a format this
        // packager predates. Refusing beats mislabelling the bytes.
        other => Err(Error::Config(format!("cannot package {other:?} audio for upload"))),
    }
}

/// Builds a WAV file from raw samples.
fn wav(format: AudioFormat, samples: &[u8]) -> Vec<u8> {
    // 1 is integer PCM, 3 is IEEE float; the header is otherwise identical.
    let (code, bits) = match format.encoding {
        Encoding::PcmF32Le => (3_u16, 32_u16),
        _ => (1_u16, 16_u16),
    };
    let channels = format.channels;
    let rate = format.sample_rate;
    let block_align = channels * bits / 8;
    let byte_rate = rate * u32::from(block_align);
    let data_len = u32::try_from(samples.len()).unwrap_or(u32::MAX);

    let mut out = Vec::with_capacity(44 + samples.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");

    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&code.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());

    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(samples);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
    }

    fn short(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(bytes[at..at + 2].try_into().expect("two bytes"))
    }

    #[test]
    fn pcm_is_wrapped_in_a_readable_header() {
        let samples = vec![0_u8; 3200];
        let upload = package(AudioFormat::DEFAULT, samples.clone()).expect("packages");

        assert_eq!(upload.filename, "audio.wav");
        assert_eq!(&upload.bytes[0..4], b"RIFF");
        assert_eq!(&upload.bytes[8..12], b"WAVE");
        assert_eq!(&upload.bytes[36..40], b"data");

        // The declared sizes must match the payload or decoders truncate.
        assert_eq!(field(&upload.bytes, 4) as usize, 36 + samples.len());
        assert_eq!(field(&upload.bytes, 40) as usize, samples.len());
        assert_eq!(upload.bytes.len(), 44 + samples.len());

        assert_eq!(short(&upload.bytes, 20), 1, "integer PCM");
        assert_eq!(short(&upload.bytes, 22), 1, "mono");
        assert_eq!(field(&upload.bytes, 24), 16_000);
        assert_eq!(field(&upload.bytes, 28), 32_000, "byte rate");
        assert_eq!(short(&upload.bytes, 32), 2, "block align");
        assert_eq!(short(&upload.bytes, 34), 16, "bits per sample");
    }

    #[test]
    fn float_pcm_is_tagged_as_float() {
        let format = AudioFormat { encoding: Encoding::PcmF32Le, ..AudioFormat::DEFAULT };
        let upload = package(format, vec![0_u8; 8]).expect("packages");
        assert_eq!(short(&upload.bytes, 20), 3, "IEEE float");
        assert_eq!(short(&upload.bytes, 34), 32, "bits per sample");
    }

    #[test]
    fn stereo_rates_account_for_both_channels() {
        let format = AudioFormat { channels: 2, ..AudioFormat::DEFAULT };
        let upload = package(format, vec![0_u8; 8]).expect("packages");
        assert_eq!(short(&upload.bytes, 32), 4, "block align");
        assert_eq!(field(&upload.bytes, 28), 64_000, "byte rate");
    }

    #[test]
    fn flac_is_already_a_file_and_passes_through() {
        let format = AudioFormat { encoding: Encoding::Flac, ..AudioFormat::DEFAULT };
        let upload = package(format, b"fLaC-data".to_vec()).expect("packages");
        assert_eq!(upload.bytes, b"fLaC-data");
        assert_eq!(upload.filename, "audio.flac");
    }

    #[test]
    fn raw_opus_is_refused_rather_than_uploaded_as_nonsense() {
        let format = AudioFormat { encoding: Encoding::Opus, ..AudioFormat::DEFAULT };
        assert!(package(format, vec![0_u8; 8]).is_err());
    }
}
