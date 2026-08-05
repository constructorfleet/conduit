//! MaryTTS speech synthesis for Conduit.
//!
//! MaryTTS is an open-source synthesizer that runs as a self-hosted HTTP
//! server. There is no API key, no account, and no vendor: it is the provider
//! for a deployment that has decided its users' speech does not leave the
//! building, and it is the reason that decision does not have to cost the
//! assistant its voice.
//!
//! ```no_run
//! # use conduit_marytts::{MaryTts, MaryTtsConfig};
//! // A server on the LAN, speaking with whatever voice it has installed.
//! let tts = MaryTts::new(MaryTtsConfig {
//!     base_url: "http://marytts:59125".to_owned(),
//!     ..MaryTtsConfig::default()
//! })?;
//! # Ok::<(), conduit_core::Error>(())
//! ```
//!
//! A MaryTTS install has only the voices someone dropped into it, so the
//! catalogue is per-deployment and cannot be a constant here. Ask the server:
//!
//! ```no_run
//! # async fn example() -> conduit_core::Result<()> {
//! # use conduit_marytts::{MaryTts, MaryTtsConfig};
//! let mut tts = MaryTts::new(MaryTtsConfig::default())?;
//! for voice in tts.refresh_catalogue().await? {
//!     println!("{} speaks {}", voice.id, voice.language);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # What this provider does not do
//!
//! It does not stream. `/process` computes a whole utterance and answers with a
//! WAV file, so [`TextToSpeech::synthesize`](conduit_provider::tts::TextToSpeech::synthesize)
//! yields one chunk after synthesis finishes rather than pretending otherwise.
//! See the crate README for what that costs and what to do about it.

pub mod audio;
pub mod catalogue;
pub mod tts;
pub mod validate;

use std::time::Duration;

use conduit_http::{Credential, HttpConfig};

pub use tts::MaryTts;

/// Where a MaryTTS server listens by default.
///
/// 59125 is the port MaryTTS binds when started with no `socket.port` override.
const DEFAULT_BASE_URL: &str = "http://localhost:59125";

/// The locale assumed when a deployment names none.
const DEFAULT_LOCALE: &str = "en_US";

/// How long to wait to reach the server.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the server may go silent before a request is abandoned.
///
/// Longer than a hosted synthesizer would need, because this bounds the whole
/// of an unstreamed synthesis rather than the gap between two chunks: MaryTTS
/// sends nothing at all until the utterance is finished, so a bound that suited
/// a streaming provider would cut off a long reply that was going fine.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(120);

/// How to reach a MaryTTS server.
#[derive(Debug, Clone)]
pub struct MaryTtsConfig {
    /// Base URL of the server, e.g. `http://marytts:59125`. No version prefix:
    /// MaryTTS serves `/process` at the root.
    pub base_url: String,
    /// Stable identity, so two servers can coexist in one registry. Appears in
    /// metric labels and error messages.
    pub name: String,
    /// Human-readable name for operator screens. `None` shows the identity.
    pub label: Option<String>,
    /// Voice to speak with, e.g. `cmu-slt-hsmm`.
    ///
    /// `None` sends no `VOICE` at all, which asks the server for its own
    /// default for the locale. That is deliberately not a name this crate
    /// guesses: MaryTTS ships no voices, so any default here would be wrong on
    /// some installs and right by luck on others.
    pub voice: Option<String>,
    /// Locale to synthesize in, as either `en_US` or `en-US`.
    ///
    /// Required by `/process` whenever no voice is named. A voice from the
    /// catalogue overrides it, since a voice determines its own locale and
    /// disagreeing with it is how a request gets rejected.
    pub locale: String,
    /// How long to wait for the TCP handshake.
    pub connect_timeout: Duration,
    /// How long the server may go silent before a request is abandoned.
    ///
    /// `None` disables the bound, which is what a caller wants only when
    /// something above it already imposes a deadline.
    pub read_timeout: Option<Duration>,
}

impl MaryTtsConfig {
    /// The shared client configuration this describes.
    ///
    /// There is no credential: MaryTTS has no authentication to configure, and
    /// [`Credential::None`] says so rather than leaving a reader to wonder
    /// whether a key was forgotten. A deployment that wants one puts the server
    /// behind a reverse proxy, which is the only place it can live.
    fn into_http(self) -> HttpConfig {
        HttpConfig {
            base_url: self.base_url,
            name: self.name,
            credential: Credential::None,
            headers: Vec::new(),
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
        }
    }
}

impl Default for MaryTtsConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            name: "marytts".to_owned(),
            label: None,
            voice: None,
            locale: DEFAULT_LOCALE.to_owned(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: Some(DEFAULT_READ_TIMEOUT),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_config_points_at_a_local_server_with_no_credential() {
        let config = MaryTtsConfig::default();
        assert_eq!(config.base_url, "http://localhost:59125");
        assert_eq!(config.name, "marytts");
        assert!(config.voice.is_none(), "no voice ships with MaryTTS to guess at");

        let http = config.into_http();
        assert_eq!(http.credential, Credential::None, "there is no key to configure");
        assert!(http.headers.is_empty());
    }

    #[test]
    fn a_config_never_prints_a_credential_because_there_is_none() {
        // Stated as a test so that adding one later has to confront the
        // redaction question rather than discovering it in a log.
        let printed = format!("{:?}", MaryTtsConfig::default());
        assert!(!printed.contains("Bearer"), "{printed}");
    }
}
