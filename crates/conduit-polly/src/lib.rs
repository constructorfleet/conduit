//! Amazon Polly speech synthesis for Conduit.
//!
//! A crate rather than a base URL, for the same reason [`conduit-bedrock`] is:
//! there is no URL to configure. The SDK resolves the endpoint from the region,
//! the credential is SigV4 over a chain rather than a key in a header, and both
//! of those need the SDK to reach at all.
//!
//! ```no_run
//! # async fn example() -> conduit_core::Result<()> {
//! # #[cfg(feature = "polly")] {
//! use conduit_polly::{PollyTts, PollyTtsConfig};
//!
//! let voice = PollyTts::new(PollyTtsConfig {
//!     region: "us-west-2".to_owned(),
//!     ..PollyTtsConfig::default()
//! })
//! .await?;
//! # }
//! # Ok(())
//! # }
//! ```
//!
//! # What an operator configures, and what is deliberately absent
//!
//! A region, optionally a named profile, a voice, and an engine. **No API key**:
//! Polly has none. It authenticates through the AWS credential chain, so a box to
//! paste a key into would be a box that does nothing — the same reasoning that
//! keeps a credential field off the Google descriptors.
//!
//! # Only PCM leaves this crate
//!
//! Polly offers `pcm`, `mp3`, `ogg_vorbis`, `ogg_opus`, `alaw`, `mulaw`, and
//! `json`. This crate requests `pcm` and nothing else, which is a decision rather
//! than an omission:
//!
//! - [`Encoding`](conduit_core::audio::Encoding) can name none of the compressed
//!   container formats, so their bytes could only be labelled as something they
//!   are not — and a mislabelled chunk plays back as noise several stages later
//!   with nothing pointing here.
//! - `json` is not audio at all. It is the speech-marks channel — sentence, word,
//!   viseme, and SSML timings — so a provider that accepted it would hand timing
//!   metadata to a stage expecting samples. There is nowhere in a
//!   [`SpeechChunk`](conduit_provider::tts::SpeechChunk) to put a viseme, so that
//!   format and the speech-mark types are absent from the schema entirely rather
//!   than accepted and ignored.
//!
//! Polly's `pcm` is signed 16-bit little-endian, which is already the pipeline's
//! interchange format, so the common case needs no transcode.
//!
//! # Without the `polly` feature
//!
//! The AWS SDK is some forty transitive crates. Compiled without the feature the
//! provider still exists and still claims its definitions, refusing to build with
//! a message naming the feature — so an operator learns this binary cannot reach
//! Polly rather than watching a configured voice fail its first turn. This is
//! `conduit-bedrock`'s arrangement, and it is why a lean build is still a build
//! that explains itself.
//!
//! [`conduit-bedrock`]: https://docs.rs/conduit-bedrock

#![cfg_attr(not(feature = "polly"), allow(unused_imports))]

use std::time::Duration;

pub mod tts;
pub mod validate;

#[cfg(feature = "polly")]
mod failure;

// Re-exported rather than re-implemented: a caller classifying a failure from
// this provider should not have to know which crate the classification lives in,
// and it is deliberately the same classification either way.
pub use conduit_http::{Failure, FailureKind};

pub use tts::PollyTts;

/// The region used when a definition names none.
///
/// Polly is available in nearly every region and the SDK will not guess one, so a
/// definition that omits it has to mean *something* rather than fail to build.
/// `us-east-1` is the region every AWS account has.
pub const DEFAULT_REGION: &str = "us-east-1";

/// The voice used when a definition names none.
///
/// Joanna is a US English neural voice present in every region that has Polly at
/// all. Stated here rather than left to the API — which has no default and
/// rejects a request carrying no voice — so the descriptor can report the voice a
/// turn will actually use.
pub const DEFAULT_VOICE: &str = "Joanna";

/// The engine used when a definition names none.
///
/// `neural` rather than `generative`: generative sounds better and is available
/// for far fewer voices in far fewer regions, so defaulting to it would mean a
/// definition naming a region and nothing else often fails. `standard` is the
/// older, worse-sounding engine, and picking it for someone would be picking the
/// wrong one.
pub const DEFAULT_ENGINE: &str = "neural";

/// The sample rates Polly produces for `pcm`.
///
/// Only two, and fewer than `mp3` offers — that is the cost of refusing the
/// compressed formats. Conduit's own default is 16 kHz, so the common case is
/// exact rather than approximated.
pub const PCM_SAMPLE_RATES: [u32; 2] = [8_000, 16_000];

/// The longest text this crate will send in one request, in characters.
///
/// Polly bills up to 3 000 characters per request and allows 6 000 including SSML
/// tags. This crate sends no tags of its own, so the billed limit is the real one.
/// Enforced here so the message names the limit and the actual length rather than
/// relaying a vendor error.
pub const MAX_CHARACTERS: usize = 3_000;

/// How long to wait to reach the endpoint.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the endpoint may go silent mid-response.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How a provider reaches Polly, and what it asks for.
///
/// Note what is absent: a base URL, because the SDK resolves the endpoint from
/// the region, and an API key, because Polly does not have them.
#[derive(Debug, Clone)]
pub struct PollyTtsConfig {
    /// AWS region to synthesize in, e.g. `us-west-2`.
    pub region: String,
    /// Named profile from the shared AWS config file to load credentials from.
    ///
    /// `None` uses the default chain — environment, task role, instance profile,
    /// default profile — which is what a deployment given its own role wants.
    pub profile: Option<String>,
    /// Provider identity used in errors, metrics, and registry lookups.
    pub name: String,
    /// Human-readable name for operator screens.
    pub label: Option<String>,
    /// Voice to speak with. `None` uses [`DEFAULT_VOICE`].
    pub voice: Option<String>,
    /// Engine to synthesize with. `None` uses [`DEFAULT_ENGINE`].
    pub engine: Option<String>,
    /// How long to wait for the TCP and TLS handshake.
    pub connect_timeout: Duration,
    /// How long the endpoint may go silent before a request is abandoned.
    pub read_timeout: Option<Duration>,
}

impl Default for PollyTtsConfig {
    fn default() -> Self {
        Self {
            region: DEFAULT_REGION.to_owned(),
            profile: None,
            name: "polly".to_owned(),
            label: None,
            voice: None,
            engine: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: Some(DEFAULT_READ_TIMEOUT),
        }
    }
}

impl PollyTtsConfig {
    /// The voice this configuration speaks with.
    #[must_use]
    pub fn voice(&self) -> &str {
        self.voice.as_deref().unwrap_or(DEFAULT_VOICE)
    }

    /// The engine this configuration synthesizes with.
    #[must_use]
    pub fn engine(&self) -> &str {
        self.engine.as_deref().unwrap_or(DEFAULT_ENGINE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configuration_naming_nothing_still_names_a_voice_and_an_engine() {
        // A definition carrying only a region is the common one — a task role
        // supplies the credential — so it has to resolve to something that works
        // rather than to a request the API rejects for naming no voice.
        let config = PollyTtsConfig::default();

        assert_eq!(config.voice(), "Joanna");
        assert_eq!(config.engine(), "neural");
        assert_eq!(config.region, "us-east-1");
        assert!(config.profile.is_none(), "the default chain, not a named profile");
    }

    #[test]
    fn what_an_operator_names_wins_over_the_default() {
        let config = PollyTtsConfig {
            voice: Some("Matthew".to_owned()),
            engine: Some("generative".to_owned()),
            ..PollyTtsConfig::default()
        };

        assert_eq!(config.voice(), "Matthew");
        assert_eq!(config.engine(), "generative");
    }

    #[test]
    fn sixteen_kilohertz_is_a_rate_polly_produces_for_pcm() {
        // Conduit's interchange rate. If this ever stops being offered, every
        // turn silently resamples, so it is asserted rather than assumed.
        assert!(PCM_SAMPLE_RATES.contains(&16_000));
    }
}
