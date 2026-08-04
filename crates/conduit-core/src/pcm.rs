//! Getting arbitrary PCM into the format every stage speaks.
//!
//! Capture inside a pipeline is already 16 kHz mono signed 16-bit — the
//! transports negotiate it. Audio that arrives from *outside* one is not: a
//! WAV an operator uploads to enroll a voice was recorded by whatever they had
//! to hand, at 44.1 kHz in stereo as often as not.
//!
//! Converting it is three small steps that are only obvious in hindsight, and
//! getting any of them wrong produces audio that plays back fine and embeds as
//! noise. So they live here, once, with tests.

use crate::audio::{AudioFormat, Encoding};
use crate::resample::Resampler;
use crate::{Error, Result};

/// Converts `samples` from `format` into the pipeline's interchange format.
///
/// Float samples become signed 16-bit, channels are mixed down to mono, and
/// the result is resampled to 16 kHz. Audio that is already in
/// [`AudioFormat::DEFAULT`] is returned untouched.
///
/// # Errors
///
/// Returns [`Error::Config`] for compressed encodings, which are not samples
/// at all, for a format that declares no channels, and if the rate conversion
/// cannot be built.
pub fn to_interchange(format: AudioFormat, samples: Vec<u8>) -> Result<Vec<u8>> {
    if format == AudioFormat::DEFAULT {
        return Ok(samples);
    }
    if format.channels == 0 {
        return Err(Error::Config("the audio declares no channels".to_owned()));
    }

    let frames = match format.encoding {
        Encoding::PcmS16Le => from_s16(&samples),
        Encoding::PcmF32Le => from_f32(&samples),
        // Reached only by a caller that decoded a container itself and got
        // this wrong: compressed audio has to be decoded before it is samples.
        Encoding::Opus | Encoding::Flac => {
            return Err(Error::Config(format!(
                "{:?} audio must be decoded before it can be converted",
                format.encoding
            )))
        }
    };

    let mono = downmix(&frames, format.channels as usize);
    let mono: Vec<u8> = mono.iter().flat_map(|sample| sample.to_le_bytes()).collect();

    if format.sample_rate == AudioFormat::DEFAULT.sample_rate {
        return Ok(mono);
    }

    let source = AudioFormat {
        encoding: Encoding::PcmS16Le,
        sample_rate: format.sample_rate,
        channels: 1,
    };
    let mut resampler = Resampler::new(source, AudioFormat::DEFAULT)?;
    let mut out = resampler.push(&mono)?;
    out.extend(resampler.flush()?);
    Ok(out)
}

/// Reads little-endian 16-bit samples, dropping a trailing half sample.
fn from_s16(samples: &[u8]) -> Vec<i16> {
    samples.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect()
}

/// Reads little-endian floats and scales them into 16-bit.
///
/// Clamped rather than wrapped: a float file that peaks slightly above 1.0 is
/// ordinary, and wrapping turns its loudest moment into the opposite sign,
/// which is audible as a click and disastrous for an embedding.
fn from_f32(samples: &[u8]) -> Vec<i16> {
    samples
        .chunks_exact(4)
        .map(|quad| {
            let sample = f32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]);
            (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
        })
        .collect()
}

/// Averages interleaved channels into one.
///
/// Averaged rather than taking the first channel: a stereo recording with the
/// speaker panned to one side would otherwise become near-silence.
fn downmix(frames: &[i16], channels: usize) -> Vec<i16> {
    if channels <= 1 {
        return frames.to_vec();
    }
    frames
        .chunks_exact(channels)
        .map(|frame| {
            // Summed as i32: two loud channels overflow i16 before the divide.
            let total: i32 = frame.iter().map(|sample| i32::from(*sample)).sum();
            (total / channels as i32) as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s16(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|sample| sample.to_le_bytes()).collect()
    }

    #[test]
    fn audio_already_in_the_interchange_format_is_untouched() {
        let samples = s16(&[1, -1, 2, -2]);
        let out = to_interchange(AudioFormat::DEFAULT, samples.clone()).expect("converts");
        assert_eq!(out, samples);
    }

    #[test]
    fn stereo_is_averaged_rather_than_half_discarded() {
        // A recording with the speaker panned hard left: keeping only the
        // right channel would enroll silence.
        let format = AudioFormat { channels: 2, ..AudioFormat::DEFAULT };
        let out = to_interchange(format, s16(&[1000, 0, -1000, 0])).expect("converts");
        assert_eq!(out, s16(&[500, -500]));
    }

    #[test]
    fn floats_are_scaled_into_sixteen_bits_and_clamped() {
        let format = AudioFormat { encoding: Encoding::PcmF32Le, ..AudioFormat::DEFAULT };
        let samples: Vec<u8> =
            [0.0_f32, 0.5, -0.5, 2.0, -2.0].iter().flat_map(|s| s.to_le_bytes()).collect();

        let out = to_interchange(format, samples).expect("converts");
        let decoded = from_s16(&out);
        assert_eq!(decoded[0], 0);
        assert_eq!(decoded[1], 16383);
        assert_eq!(decoded[2], -16383);
        assert_eq!(decoded[3], i16::MAX, "clamped, not wrapped");
        assert_eq!(decoded[4], -i16::MAX, "clamped, not wrapped");
    }

    #[test]
    fn a_higher_rate_is_resampled_to_the_interchange_rate() {
        // One second of 44.1 kHz becomes roughly one second of 16 kHz. The
        // tail is padded rather than cut, so the count is close rather than
        // exact — a fixed number here would be asserting on the block size.
        let format = AudioFormat { sample_rate: 44_100, ..AudioFormat::DEFAULT };
        let out = to_interchange(format, vec![0_u8; 44_100 * 2]).expect("converts");

        let frames = out.len() / 2;
        assert!(
            (15_500..=17_500).contains(&frames),
            "a second in should be about a second out, got {frames} frames"
        );
    }

    #[test]
    fn stereo_at_another_rate_is_mixed_before_it_is_resampled() {
        // Both at once is the case a real upload hits, and mixing after
        // resampling would interleave two channels the resampler had already
        // stretched independently.
        let format = AudioFormat { channels: 2, sample_rate: 48_000, ..AudioFormat::DEFAULT };
        let out = to_interchange(format, vec![0_u8; 48_000 * 2 * 2]).expect("converts");

        let frames = out.len() / 2;
        assert!((15_500..=17_500).contains(&frames), "got {frames} frames");
    }

    #[test]
    fn compressed_audio_is_refused_rather_than_read_as_samples() {
        let format = AudioFormat { encoding: Encoding::Opus, ..AudioFormat::DEFAULT };
        assert!(to_interchange(format, vec![0_u8; 8]).is_err());
    }

    #[test]
    fn audio_with_no_channels_is_refused_rather_than_divided_by_zero() {
        let format = AudioFormat { channels: 0, ..AudioFormat::DEFAULT };
        assert!(to_interchange(format, vec![0_u8; 8]).is_err());
    }
}
