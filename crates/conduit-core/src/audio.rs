//! Audio description types.
//!
//! Conduit never copies audio payloads through the event bus — events carry
//! *descriptions* of audio while the samples themselves travel over the
//! dedicated transport. These types are that description.

use serde::{Deserialize, Serialize};

/// Wire encoding of an audio stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Encoding {
    /// Signed 16-bit little-endian PCM.
    PcmS16Le,
    /// 32-bit little-endian float PCM.
    PcmF32Le,
    /// Opus frames.
    Opus,
    /// FLAC frames.
    Flac,
}

/// A fully specified audio format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioFormat {
    /// Sample encoding.
    pub encoding: Encoding,
    /// Samples per second, e.g. `16_000`.
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u16,
}

impl AudioFormat {
    /// 16 kHz mono signed 16-bit PCM — the pipeline's interchange format.
    pub const DEFAULT: Self =
        Self { encoding: Encoding::PcmS16Le, sample_rate: 16_000, channels: 1 };

    /// Duration in milliseconds of `bytes` worth of audio in this format.
    ///
    /// Returns `None` for compressed encodings, whose bitrate is variable.
    #[must_use]
    pub const fn duration_ms(&self, bytes: usize) -> Option<u64> {
        let bytes_per_sample = match self.encoding {
            Encoding::PcmS16Le => 2,
            Encoding::PcmF32Le => 4,
            Encoding::Opus | Encoding::Flac => return None,
        };
        let frame = bytes_per_sample * self.channels as usize;
        if frame == 0 || self.sample_rate == 0 {
            return None;
        }
        Some((bytes as u64 * 1_000) / (frame as u64 * self.sample_rate as u64))
    }
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_duration_is_derived_from_format() {
        // One second of 16 kHz mono s16 is 32 000 bytes.
        assert_eq!(AudioFormat::DEFAULT.duration_ms(32_000), Some(1_000));
    }

    #[test]
    fn compressed_duration_is_unknown() {
        let opus = AudioFormat { encoding: Encoding::Opus, ..AudioFormat::DEFAULT };
        assert_eq!(opus.duration_ms(32_000), None);
    }
}
