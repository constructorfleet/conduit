//! Rate conversion for streamed PCM.
//!
//! A synthesizer speaks at whatever rate its voice was trained at — a Piper
//! voice is usually 22.05 kHz — while a satellite plays at the one rate its
//! firmware was built for. Handing the device the other rate's samples does
//! not fail; it plays them at the wrong speed, which is heard as a voice
//! slowed down and pitched low rather than as an error anyone can act on. So
//! the conversion happens here, before the audio reaches the transport.
//!
//! Resampling is band-limited rather than interpolated: speech resampled by
//! linear interpolation aliases audibly, and the cost of doing it properly is
//! paid once per sentence on a server rather than on the device.

use rubato::{FftFixedIn, Resampler as _};

use crate::audio::{AudioFormat, Encoding};
use crate::{Error, Result};

/// How many input frames the resampler consumes per pass.
///
/// Rate conversion works on fixed blocks, so this is the granularity at which
/// buffered input turns into output. One 20 ms block at 22.05 kHz keeps the
/// added latency below what a listener notices while staying large enough that
/// the transform is not dominated by its own setup.
const BLOCK_FRAMES: usize = 441;

/// Converts streamed PCM from one sample rate to another.
///
/// Feed whole chunks as they arrive; each call returns whatever complete
/// output blocks are ready, which may be empty while input is still being
/// accumulated. [`Resampler::flush`] drains the tail at the end of a stream.
pub struct Resampler {
    inner: FftFixedIn<f32>,
    /// Input samples not yet consumed by a whole block.
    pending: Vec<f32>,
    /// Odd trailing byte from a chunk that split a sample in half.
    partial: Option<u8>,
    /// Real input frames seen, excluding any padding added at flush.
    speech: u64,
    /// Output frames already handed to the caller.
    emitted: u64,
    source: AudioFormat,
    target: AudioFormat,
}

impl Resampler {
    /// Builds a resampler from `source` to `target`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] unless both formats are mono
    /// [`Encoding::PcmS16Le`] — the interchange format every stage already
    /// speaks — or if the rate pair is one the transform cannot be built for.
    pub fn new(source: AudioFormat, target: AudioFormat) -> Result<Self> {
        for format in [source, target] {
            if format.encoding != Encoding::PcmS16Le {
                return Err(Error::Config(format!(
                    "cannot resample {:?} audio; only PcmS16Le is supported",
                    format.encoding
                )));
            }
            if format.channels != 1 {
                return Err(Error::Config(format!(
                    "cannot resample {} channels; only mono is supported",
                    format.channels
                )));
            }
            if format.sample_rate == 0 {
                return Err(Error::Config("a sample rate cannot be zero".to_owned()));
            }
        }

        let inner = FftFixedIn::<f32>::new(
            source.sample_rate as usize,
            target.sample_rate as usize,
            BLOCK_FRAMES,
            1,
            1,
        )
        .map_err(|error| {
            Error::Config(format!(
                "cannot resample {} Hz to {} Hz: {error}",
                source.sample_rate, target.sample_rate
            ))
        })?;

        Ok(Self {
            inner,
            pending: Vec::new(),
            partial: None,
            speech: 0,
            emitted: 0,
            source,
            target,
        })
    }

    /// The format this resampler reads.
    #[must_use]
    pub const fn source(&self) -> AudioFormat {
        self.source
    }

    /// The format this resampler writes.
    #[must_use]
    pub const fn target(&self) -> AudioFormat {
        self.target
    }

    /// Converts `pcm`, returning whatever whole blocks are ready.
    ///
    /// A chunk boundary that falls mid-sample is carried into the next call
    /// rather than dropped: a transport is free to split a stream anywhere,
    /// and losing a byte would desynchronize every sample after it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the underlying transform rejects a block.
    pub fn push(&mut self, pcm: &[u8]) -> Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(pcm.len() + 1);
        if let Some(held) = self.partial.take() {
            bytes.push(held);
        }
        bytes.extend_from_slice(pcm);
        if bytes.len() % 2 == 1 {
            self.partial = bytes.pop();
        }

        let frames = bytes.len() / 2;
        self.pending.extend(bytes.chunks_exact(2).map(|sample| {
            f32::from(i16::from_le_bytes([sample[0], sample[1]])) / f32::from(i16::MAX)
        }));
        self.speech += frames as u64;

        let out = self.drain_blocks()?;
        self.emitted += (out.len() / 2) as u64;
        Ok(out)
    }

    /// Converts any buffered input, padding the final partial block.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the underlying transform rejects a block.
    pub fn flush(&mut self) -> Result<Vec<u8>> {
        if self.pending.is_empty() {
            return Ok(Vec::new());
        }
        // The transform only accepts whole blocks, so the tail is padded with
        // silence rather than cut. The padding covers two things: the block the
        // tail did not fill, and the transform's own delay line, which still
        // holds the last of the speech. Without both, the final syllable of
        // every sentence never comes out.
        let delay_frames = (self.inner.output_delay() as u64
            * u64::from(self.source.sample_rate))
        .div_ceil(u64::from(self.target.sample_rate)) as usize;
        let padded = (self.pending.len() + delay_frames).div_ceil(BLOCK_FRAMES) * BLOCK_FRAMES;
        self.pending.resize(padded, 0.0);

        let mut out = self.drain_blocks()?;

        // Emit exactly the speech that went in, converted — never the silence
        // that was only there to push it through.
        let total = (self.speech * u64::from(self.target.sample_rate))
            .div_ceil(u64::from(self.source.sample_rate));
        let allowed = total.saturating_sub(self.emitted) as usize;
        out.truncate(allowed * 2);
        self.emitted += (out.len() / 2) as u64;
        Ok(out)
    }

    /// Runs every whole block currently buffered.
    fn drain_blocks(&mut self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        while self.pending.len() >= BLOCK_FRAMES {
            let block: Vec<f32> = self.pending.drain(..BLOCK_FRAMES).collect();
            let resampled = self
                .inner
                .process(&[block], None)
                .map_err(|error| Error::Config(format!("resampling failed: {error}")))?;
            for sample in &resampled[0] {
                let scaled = (sample * f32::from(i16::MAX))
                    .clamp(f32::from(i16::MIN), f32::from(i16::MAX));
                out.extend_from_slice(&(scaled as i16).to_le_bytes());
            }
        }
        Ok(out)
    }
}

impl std::fmt::Debug for Resampler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Resampler")
            .field("source", &self.source)
            .field("target", &self.target)
            .field("pending", &self.pending.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(sample_rate: u32) -> AudioFormat {
        AudioFormat { encoding: Encoding::PcmS16Le, sample_rate, channels: 1 }
    }

    /// One second of a sine wave, as little-endian 16-bit samples.
    fn tone(sample_rate: u32, hz: f32) -> Vec<u8> {
        (0..sample_rate)
            .flat_map(|index| {
                let time = index as f32 / sample_rate as f32;
                let sample = (std::f32::consts::TAU * hz * time).sin() * 0.5;
                ((sample * f32::from(i16::MAX)) as i16).to_le_bytes()
            })
            .collect()
    }

    #[test]
    fn output_length_follows_the_rate_ratio() {
        // The whole point: 22.05 kHz of samples has to become 16 kHz of
        // samples, or the device plays it at the wrong speed.
        let mut resampler = Resampler::new(format(22_050), format(16_000)).expect("built");
        let mut out = resampler.push(&tone(22_050, 440.0)).expect("resampled");
        out.extend(resampler.flush().expect("flushed"));

        let frames = out.len() / 2;
        let expected: i64 = 16_000;
        let drift = (frames as i64 - expected).abs();
        assert!(
            drift < expected / 50,
            "one second in must be about one second out, got {frames} frames"
        );
    }

    #[test]
    fn a_chunk_split_mid_sample_keeps_both_halves() {
        // A transport may split anywhere. Dropping the odd byte would shift
        // every following sample by one and turn speech into noise.
        let source = tone(22_050, 300.0);
        let (head, tail) = source.split_at(1001);

        let mut split = Resampler::new(format(22_050), format(16_000)).expect("built");
        let mut from_split = split.push(head).expect("head");
        from_split.extend(split.push(tail).expect("tail"));
        from_split.extend(split.flush().expect("flush"));

        let mut whole = Resampler::new(format(22_050), format(16_000)).expect("built");
        let mut from_whole = whole.push(&source).expect("whole");
        from_whole.extend(whole.flush().expect("flush"));

        assert_eq!(from_split, from_whole, "chunk boundaries must not change the output");
    }

    #[test]
    fn matching_rates_round_trip_unchanged_in_length() {
        let mut resampler = Resampler::new(format(16_000), format(16_000)).expect("built");
        let source = tone(16_000, 220.0);
        let mut out = resampler.push(&source).expect("resampled");
        out.extend(resampler.flush().expect("flushed"));
        assert_eq!(out.len(), source.len());
    }

    #[test]
    fn upsampling_lengthens_the_stream() {
        let mut resampler = Resampler::new(format(16_000), format(24_000)).expect("built");
        let mut out = resampler.push(&tone(16_000, 440.0)).expect("resampled");
        out.extend(resampler.flush().expect("flushed"));

        let frames = out.len() / 2;
        assert!((frames as i64 - 24_000).abs() < 480, "got {frames} frames");
    }

    #[test]
    fn a_partial_block_still_reaches_the_listener() {
        // Shorter than one block: without a flush this would be silence, and
        // every short reply would be lost entirely.
        let mut resampler = Resampler::new(format(22_050), format(16_000)).expect("built");
        let ready = resampler.push(&tone(22_050, 440.0)[..200]).expect("pushed");
        assert!(ready.is_empty(), "a partial block is not ready yet");
        assert!(!resampler.flush().expect("flushed").is_empty(), "flush must emit the tail");
    }

    #[test]
    fn non_pcm_and_multichannel_formats_are_refused() {
        let opus = AudioFormat { encoding: Encoding::Opus, sample_rate: 16_000, channels: 1 };
        let error = Resampler::new(opus, format(16_000)).expect_err("unsupported");
        assert!(error.to_string().contains("PcmS16Le"), "{error}");

        let stereo =
            AudioFormat { encoding: Encoding::PcmS16Le, sample_rate: 16_000, channels: 2 };
        let error = Resampler::new(stereo, format(16_000)).expect_err("unsupported");
        assert!(error.to_string().contains("mono"), "{error}");
    }
}
