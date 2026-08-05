//! Getting a WAV file the server produced into the format the pipeline plays.
//!
//! `AUDIO=WAVE_FILE` returns a *file*: a RIFF header and then samples. Two
//! things follow, and both have bitten this codebase before.
//!
//! The header is not audio. Handing it downstream as if it were prepends 44
//! bytes of `RIFFWAVEfmt ` to the utterance, which is an audible click before
//! every reply.
//!
//! And the rate in that header is the voice's, not the pipeline's. MaryTTS
//! voices are built at whatever rate their recordings were made at — 16 kHz for
//! some HMM voices, 22.05 kHz and 44.1 kHz for others — and the server resamples
//! to none of them on the way out. Playing 22.05 kHz samples as though they were
//! 16 kHz stretches the utterance and drops the pitch by roughly a fifth, which
//! is exactly the bug the speaker-enrollment path already had to fix. So the
//! rate and channel count are *read from the header* and the samples are
//! converted, rather than assumed to be what was wanted.
//!
//! Neither step is written here. [`conduit_core::wav::parse`] already reads the
//! container and [`conduit_core::pcm::to_interchange`] already does the
//! conversion, both with their own tests; a second copy of either would be a
//! second thing to get wrong.

use conduit_core::audio::AudioFormat;
use conduit_core::{pcm, wav, Error, Result};
use conduit_http::Failure;

/// Reads the samples out of a WAV file and converts them for the pipeline.
///
/// Returns the samples in [`AudioFormat::DEFAULT`] — 16 kHz mono signed 16-bit —
/// whatever the file declared, so a caller can report one constant format for
/// the stream it emits.
///
/// # Errors
///
/// Returns [`Error::Provider`] classified as
/// [`Malformed`](conduit_http::FailureKind::Malformed) if the body is not a
/// readable PCM WAV file, or if its declared format cannot be converted. A
/// server answering `200` with an HTML error page or an MP3 lands here, and
/// none of those become audio by being retried.
pub fn to_interchange(provider: &str, body: &[u8]) -> Result<Vec<u8>> {
    let audio = wav::parse(body).map_err(|error| malformed(provider, &error))?;

    if audio.samples.is_empty() {
        return Err(Error::provider(
            provider,
            Failure::malformed("the server returned a WAV file with no samples"),
        ));
    }

    // The one place the real rate is honoured. `to_interchange` returns the
    // samples untouched when the format already matches, so a 16 kHz mono voice
    // costs nothing here.
    let declared = audio.format;
    if declared != AudioFormat::DEFAULT {
        tracing::debug!(
            provider = %provider,
            rate = declared.sample_rate,
            channels = declared.channels,
            encoding = ?declared.encoding,
            "converting synthesized audio to the interchange format"
        );
    }

    pcm::to_interchange(declared, audio.samples).map_err(|error| malformed(provider, &error))
}

/// Wraps a parse or conversion failure as a classified provider error.
///
/// Both helpers report [`Error::Config`], which is the right shape for the
/// local caller they were written for and the wrong one here: the file came
/// from a server, so this is that server answering unreadably rather than this
/// deployment being misconfigured. Classifying it as `Malformed` is also what
/// tells a caller not to retry it.
fn malformed(provider: &str, error: &Error) -> Error {
    Error::provider(provider, Failure::malformed(format!("unreadable synthesis: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::audio::Encoding;

    /// A WAV file in `format` whose samples are `frames` alternating values,
    /// which is enough of a signal to tell a resampled stream from a copied one.
    fn wav_file(format: AudioFormat, frames: usize) -> Vec<u8> {
        let samples: Vec<u8> = (0..frames)
            .flat_map(|index| {
                let sample = if index % 2 == 0 { 8_000_i16 } else { -8_000_i16 };
                sample.to_le_bytes()
            })
            .collect();
        wav::package(format, samples).expect("packages").bytes
    }

    #[test]
    fn the_riff_header_never_reaches_the_pipeline_as_audio() {
        // 44 bytes of header played as samples is an audible click before every
        // reply, and the string is the cheapest way to prove it is gone.
        let file = wav_file(AudioFormat::DEFAULT, 160);
        let samples = to_interchange("marytts", &file).expect("converted");

        assert_eq!(samples.len(), 320, "the samples, and not the 44-byte header");
        assert!(!samples.starts_with(b"RIFF"), "the header is not audio");
        assert!(
            !samples.windows(4).any(|window| window == b"fmt "),
            "no part of the container survived"
        );
    }

    #[test]
    fn a_voice_at_another_rate_is_resampled_rather_than_played_at_the_wrong_pitch() {
        // A 22.05 kHz MaryTTS voice. Handing these samples on as 16 kHz plays
        // one second of speech over 1.38 seconds, a fifth of an octave low.
        let format = AudioFormat { sample_rate: 22_050, ..AudioFormat::DEFAULT };
        let file = wav_file(format, 22_050);

        let samples = to_interchange("marytts", &file).expect("converted");

        // One second in is about one second out. The tail is padded rather than
        // cut, so this is a range: a fixed number would assert on the
        // resampler's block size.
        let frames = samples.len() / 2;
        assert!(
            (15_500..=17_500).contains(&frames),
            "a second of 22.05 kHz should be about 16 000 frames, got {frames}"
        );
    }

    #[test]
    fn a_forty_four_kilohertz_voice_is_also_converted() {
        let format = AudioFormat { sample_rate: 44_100, ..AudioFormat::DEFAULT };
        let samples = to_interchange("marytts", &wav_file(format, 44_100)).expect("converted");

        let frames = samples.len() / 2;
        assert!((15_500..=17_500).contains(&frames), "got {frames} frames");
    }

    #[test]
    fn audio_already_at_the_interchange_rate_is_passed_through_untouched() {
        // The common case for a 16 kHz voice: no resampling, so no chance of
        // the conversion degrading audio that was already right.
        let file = wav_file(AudioFormat::DEFAULT, 8);
        let expected = wav::parse(&file).expect("parses").samples;

        assert_eq!(to_interchange("marytts", &file).expect("converted"), expected);
    }

    #[test]
    fn a_stereo_voice_is_mixed_down_rather_than_played_twice_as_fast() {
        // Interleaved frames read as mono are two half-speed channels spliced
        // together. Frame count, not byte count, is what must survive.
        let format = AudioFormat { channels: 2, ..AudioFormat::DEFAULT };
        let samples = to_interchange("marytts", &wav_file(format, 320)).expect("converted");

        assert_eq!(samples.len(), 320, "160 mono frames from 160 stereo frames");
    }

    #[test]
    fn the_rate_is_read_from_the_header_rather_than_from_what_was_asked_for() {
        // The guarantee, stated once: two files differing only in their
        // declared rate must not convert to the same length. If the header were
        // ignored, they would.
        let low = wav_file(AudioFormat { sample_rate: 8_000, ..AudioFormat::DEFAULT }, 8_000);
        let high = wav_file(AudioFormat { sample_rate: 32_000, ..AudioFormat::DEFAULT }, 8_000);

        let low = to_interchange("marytts", &low).expect("converted");
        let high = to_interchange("marytts", &high).expect("converted");

        assert!(low.len() > high.len(), "8 kHz upsamples and 32 kHz downsamples");
    }

    #[test]
    fn a_response_that_is_not_a_wav_file_is_refused_as_malformed_and_not_retried() {
        // A misconfigured reverse proxy answers 200 with an HTML error page.
        // Playing it would be noise; retrying it would get the same page.
        let error =
            to_interchange("marytts", b"<html>Bad Gateway</html>").expect_err("refused");

        let failure = Failure::of(&error).expect("classified");
        assert!(!failure.is_retryable(), "the same bytes come back next time");
        assert!(error.to_string().contains("marytts"), "names the provider: {error}");
    }

    #[test]
    fn a_compressed_payload_is_refused_rather_than_played_as_noise() {
        // `AUDIO=MP3_FILE` answered where WAVE was asked for, or a voice the
        // server encoded differently. There is no decoder here.
        assert!(to_interchange("marytts", b"ID3\x03\x00\x00\x00frame").is_err());
    }

    #[test]
    fn an_empty_body_is_refused_rather_than_reported_as_a_silent_reply() {
        // A server that answers 200 with nothing has failed, and a caller that
        // received an empty stream would think the assistant chose to say
        // nothing.
        let error =
            to_interchange("marytts", &wav_file(AudioFormat::DEFAULT, 0)).expect_err("refused");
        assert!(error.to_string().contains("no samples"), "{error}");

        assert!(to_interchange("marytts", b"").is_err(), "and not even a header");
    }

    #[test]
    fn a_float_voice_is_converted_to_the_integer_pcm_the_pipeline_carries() {
        let format = AudioFormat { encoding: Encoding::PcmF32Le, ..AudioFormat::DEFAULT };
        let samples: Vec<u8> =
            [0.0_f32, 0.5, -0.5, 0.25].iter().flat_map(|s| s.to_le_bytes()).collect();
        let file = wav::package(format, samples).expect("packages").bytes;

        let out = to_interchange("marytts", &file).expect("converted");
        assert_eq!(out.len(), 8, "four float samples become four 16-bit ones");
    }
}
