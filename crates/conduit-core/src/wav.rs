//! Packaging captured audio into a file the transcription API will accept.
//!
//! The pipeline carries raw samples, but `/audio/transcriptions` takes an
//! uploaded *file* and sniffs its type. Raw PCM therefore needs a container,
//! and a WAV header is 44 bytes of arithmetic — far less than a dependency.

use crate::audio::{AudioFormat, Encoding};
use crate::{Error, Result};

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
        // `Encoding` is non-exhaustive to its dependents, but this now lives
        // beside the enum, so a new variant is a compile error here rather than
        // a runtime refusal — which is the better place to find out.
        Encoding::Opus => Err(Error::Config(
            "raw Opus frames cannot be uploaded; capture as PCM or FLAC".to_owned(),
        )),
    }
}

/// Raw samples read out of a container, and what they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    /// Encoding, rate, and channel count as the file declared them.
    pub format: AudioFormat,
    /// The sample bytes, with no header.
    pub samples: Vec<u8>,
}

/// Reads the samples out of a WAV file.
///
/// The inverse of [`package`], and here for the same reason: a caller that
/// *receives* a file — an enrollment upload, say — has to get back to the raw
/// samples every stage speaks, and the header is arithmetic rather than a
/// dependency. Chunks other than `fmt ` and `data` are skipped, because a file
/// written by a recorder carries `LIST` and `fact` chunks that mean nothing
/// here.
///
/// The format is returned as the file declares it rather than converted: what
/// a caller needs is decided by where the samples are going, and silently
/// resampling would hide a file that is not what it says it is.
///
/// # Errors
///
/// Returns [`Error::Config`] if `bytes` is not a RIFF/WAVE file, if it is
/// truncated, or if it declares an encoding that is not PCM.
pub fn parse(bytes: &[u8]) -> Result<Pcm> {
    // 12 bytes of RIFF header, then chunks; anything shorter cannot even say
    // it is a WAV.
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(Error::Config("not a WAV file: no RIFF/WAVE header".to_owned()));
    }

    let mut format = None;
    let mut samples = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let length =
            u32::from_le_bytes(bytes[at + 4..at + 8].try_into().map_err(|_| truncated())?)
                as usize;
        let body = at + 8;
        // A declared length past the end is a truncated file. The data chunk
        // is the one worth salvaging — a recorder killed mid-write leaves the
        // samples it did manage — so it is clamped rather than refused.
        let end = body.saturating_add(length).min(bytes.len());

        match id {
            b"fmt " => format = Some(read_format(bytes.get(body..end).ok_or_else(truncated)?)?),
            b"data" => samples = Some(bytes.get(body..end).ok_or_else(truncated)?.to_vec()),
            _ => {}
        }

        // Chunks are word-aligned: an odd length is followed by a pad byte
        // that belongs to nobody.
        at = body + length + (length & 1);
    }

    let format =
        format.ok_or_else(|| Error::Config("the WAV file has no `fmt ` chunk".to_owned()))?;
    let samples =
        samples.ok_or_else(|| Error::Config("the WAV file has no `data` chunk".to_owned()))?;
    Ok(Pcm { format, samples })
}

/// Reads a `fmt ` chunk body.
fn read_format(body: &[u8]) -> Result<AudioFormat> {
    // 16 bytes is the PCM form; extensible headers are longer and their first
    // 16 bytes say the same things.
    if body.len() < 16 {
        return Err(Error::Config("the WAV file's `fmt ` chunk is too short".to_owned()));
    }
    let short = |at: usize| u16::from_le_bytes([body[at], body[at + 1]]);
    let long =
        |at: usize| u32::from_le_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);

    let code = short(0);
    let bits = short(14);
    let encoding = match (code, bits) {
        (1, 16) => Encoding::PcmS16Le,
        (3, 32) => Encoding::PcmF32Le,
        // 0xFFFE is WAVE_FORMAT_EXTENSIBLE, whose real code lives in a
        // subformat GUID; the bit depth still says which of the two it is.
        (0xFFFE, 16) => Encoding::PcmS16Le,
        (0xFFFE, 32) => Encoding::PcmF32Le,
        _ => {
            return Err(Error::Config(format!(
                "the WAV file is not PCM: format {code}, {bits} bits per sample"
            )))
        }
    };

    Ok(AudioFormat { encoding, sample_rate: long(4), channels: short(2) })
}

fn truncated() -> Error {
    Error::Config("the WAV file is truncated".to_owned())
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

    #[test]
    fn what_was_packaged_is_what_comes_back() {
        // The pair is the point: a file this code wrote and a file a recorder
        // wrote are read by the same parser, so round-tripping is the cheapest
        // statement that the header arithmetic agrees with itself.
        let samples: Vec<u8> = (0..3200).map(|index| (index % 251) as u8).collect();
        let upload = package(AudioFormat::DEFAULT, samples.clone()).expect("packages");

        let pcm = parse(&upload.bytes).expect("parses");
        assert_eq!(pcm.format, AudioFormat::DEFAULT);
        assert_eq!(pcm.samples, samples);
    }

    #[test]
    fn a_float_stereo_file_is_read_as_what_it_declares() {
        let format =
            AudioFormat { encoding: Encoding::PcmF32Le, channels: 2, sample_rate: 48_000 };
        let upload = package(format, vec![7_u8; 64]).expect("packages");

        let pcm = parse(&upload.bytes).expect("parses");
        assert_eq!(pcm.format, format, "read back, not converted");
        assert_eq!(pcm.samples.len(), 64);
    }

    #[test]
    fn chunks_nobody_here_cares_about_are_skipped() {
        // A recorder writes `LIST` and `fact` chunks before the samples. A
        // parser that assumed `data` came first would read metadata as audio.
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&0_u32.to_le_bytes());
        file.extend_from_slice(b"WAVE");
        // An odd-length chunk, which is followed by a pad byte.
        file.extend_from_slice(b"LIST");
        file.extend_from_slice(&3_u32.to_le_bytes());
        file.extend_from_slice(b"abc\0");
        let packaged = package(AudioFormat::DEFAULT, vec![9_u8; 16]).expect("packages");
        file.extend_from_slice(&packaged.bytes[12..]);

        let pcm = parse(&file).expect("parses");
        assert_eq!(pcm.format, AudioFormat::DEFAULT);
        assert_eq!(pcm.samples, vec![9_u8; 16]);
    }

    #[test]
    fn a_file_that_is_not_a_wav_is_refused() {
        assert!(parse(b"fLaC-data").is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn a_compressed_wav_is_refused_rather_than_read_as_samples() {
        // Format 0x0055 is MP3 inside a WAV wrapper. Reading its payload as
        // PCM would enroll a voice print built from noise.
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&0_u32.to_le_bytes());
        file.extend_from_slice(b"WAVE");
        file.extend_from_slice(b"fmt ");
        file.extend_from_slice(&16_u32.to_le_bytes());
        file.extend_from_slice(&0x0055_u16.to_le_bytes());
        file.extend_from_slice(&1_u16.to_le_bytes());
        file.extend_from_slice(&16_000_u32.to_le_bytes());
        file.extend_from_slice(&32_000_u32.to_le_bytes());
        file.extend_from_slice(&2_u16.to_le_bytes());
        file.extend_from_slice(&0_u16.to_le_bytes());

        let error = parse(&file).expect_err("refused");
        assert!(error.to_string().contains("not PCM"), "{error}");
    }

    #[test]
    fn a_truncated_data_chunk_yields_the_samples_that_survived() {
        // A recorder killed mid-write leaves a declared length longer than the
        // file. What it did manage to write is still a usable utterance.
        let mut file = package(AudioFormat::DEFAULT, vec![5_u8; 32]).expect("packages").bytes;
        file.truncate(file.len() - 8);

        let pcm = parse(&file).expect("parses");
        assert_eq!(pcm.samples, vec![5_u8; 24]);
    }
}
