//! ElevenLabs as a Conduit speech provider: synthesis and batch transcription.
//!
//! A separate crate from [`conduit-openai`] rather than a base URL under it,
//! because neither endpoint is a chat-completions endpoint wearing a different
//! host:
//!
//! | | OpenAI speech | ElevenLabs |
//! | --- | --- | --- |
//! | Credential | `Authorization: Bearer` | `xi-api-key` |
//! | Voice | a body field | **a URL path segment** |
//! | Output format | `response_format` in the body | `output_format` in the query |
//! | Voice controls | one `speed` | `stability`, `similarity_boost`, `style`, `use_speaker_boost`, `speed` |
//! | Voice catalogue | six fixed names | `GET /v1/voices`, per account |
//! | Transcription response | `{text, language}` | `{text, language_code, language_probability, words}` |
//!
//! The voice being a *path* segment is the difference that matters most, and it
//! is why [`voice_id`] exists: a voice id arrives from a stored provider
//! definition or a pipeline setting, so a value containing `../` would move the
//! request to a different API path with the account's credential attached. Every
//! voice id is checked against an allowlist before it reaches a URL.
//!
//! What this shares with every other HTTP provider — sending an authenticated
//! request, classifying a failure — lives in [`conduit-http`].
//!
//! ```no_run
//! # use conduit_elevenlabs::{ElevenLabsConfig, ElevenLabsStt, ElevenLabsTts};
//! let config = ElevenLabsConfig {
//!     api_key: std::env::var("ELEVENLABS_API_KEY").ok(),
//!     ..ElevenLabsConfig::default()
//! };
//! let tts = ElevenLabsTts::new(&config)?;
//! let stt = ElevenLabsStt::new(&config)?;
//! # Ok::<(), conduit_core::Error>(())
//! ```
//!
//! A configuration describes one *account*, not one capability, so a deployment
//! using both capabilities describes it once.
//!
//! # What is not here
//!
//! Realtime websocket transcription (`scribe_v2_realtime`) is deliberately
//! absent — see the crate README. It is a different protocol with
//! partial-transcript semantics, not a setting on the batch endpoint.
//!
//! [`conduit-openai`]: https://docs.rs/conduit-openai
//! [`conduit-http`]: https://docs.rs/conduit-http

pub mod stt;
pub mod tts;
pub mod voice_id;
pub mod wire;

use std::time::Duration;

// Re-exported rather than re-implemented: a caller classifying a failure from
// this provider should not have to know which crate the classification lives
// in, and it is the same classification either way.
pub use conduit_http::{Failure, FailureKind};
pub use stt::ElevenLabsStt;
pub use tts::ElevenLabsTts;

/// The public API, including the version prefix.
const DEFAULT_BASE_URL: &str = "https://api.elevenlabs.io/v1";

/// The header the API key travels in.
///
/// Not a bearer token, which is the single most common way to misconfigure this
/// vendor: `Authorization: Bearer <key>` is accepted by the TLS layer and
/// rejected by the API with a 401 that says nothing about the header.
pub const API_KEY_HEADER: &str = "xi-api-key";

/// The synthesis model used when a definition names none.
///
/// The flash family is the vendor's low-latency line (~75 ms), which is what a
/// spoken turn needs: `eleven_multilingual_v2` sounds better and arrives later,
/// and a voice assistant that pauses to sound better is a worse voice
/// assistant. An operator who wants the expressive model names it.
pub const DEFAULT_TTS_MODEL: &str = "eleven_flash_v2_5";

/// Synthesis models advertised when a definition names none.
///
/// A definition that lists its own overrides this; the list exists so a freshly
/// configured provider offers an operator something to choose rather than an
/// empty menu. `eleven_turbo_v2_5` and `eleven_turbo_v2` are omitted: the vendor
/// documents them as replaced by the flash models.
pub const DEFAULT_TTS_MODELS: &[&str] =
    &["eleven_flash_v2_5", "eleven_flash_v2", "eleven_multilingual_v2", "eleven_v3"];

/// The transcription model used when a definition names none.
///
/// `scribe_v2` rather than `scribe_v1`, which the vendor documents as
/// deprecated, and rather than `scribe_v2_realtime`, which is reached over a
/// websocket this crate does not implement — naming it here would produce a
/// provider that 4xxs on every utterance.
pub const DEFAULT_STT_MODEL: &str = "scribe_v2";

/// Transcription models advertised when a definition names none.
pub const DEFAULT_STT_MODELS: &[&str] = &["scribe_v2", "scribe_v1"];

/// The language a voice is advertised as speaking when nothing says otherwise.
///
/// [`Voice::language`] is not optional, and an empty tag reads as a bug rather
/// than as "unknown".
///
/// [`Voice::language`]: conduit_provider::tts::Voice::language
pub const DEFAULT_LANGUAGE: &str = "en";

/// How long to wait to reach the server.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the server may go silent mid-response.
///
/// A read timeout rather than a total one: synthesis streams for as long as the
/// text takes to speak, and a ten-hour recording takes a while to transcribe.
/// What must be bounded is *silence* — a server that completes the handshake and
/// then says nothing would otherwise hold a turn open indefinitely.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// How a provider reaches an ElevenLabs account.
#[derive(Debug, Clone)]
pub struct ElevenLabsConfig {
    /// Base URL including the version prefix.
    pub base_url: String,
    /// The API key, sent as [`API_KEY_HEADER`].
    pub api_key: Option<String>,
    /// Stable identity, so two differently configured accounts can coexist in
    /// one registry.
    ///
    /// This is what the provider calls itself, and what appears in metric
    /// labels and error messages.
    pub name: String,
    /// Human-readable name for operator screens, e.g. `"ElevenLabs (house)"`.
    ///
    /// `None` shows the identity.
    pub label: Option<String>,
    /// How long to wait for the TCP and TLS handshake.
    pub connect_timeout: Duration,
    /// How long the server may go silent before the request is abandoned.
    ///
    /// `None` disables the bound, which is the shape a caller wants only when
    /// something above it already imposes a deadline.
    pub read_timeout: Option<Duration>,
    /// Models this provider advertises. Empty advertises the capability's
    /// defaults — [`DEFAULT_TTS_MODELS`] or [`DEFAULT_STT_MODELS`].
    pub models: Vec<String>,
    /// The voice synthesis speaks with when a request names none.
    ///
    /// Checked against the same allowlist as a requested voice, because it
    /// reaches the same URL path. A configured value that cannot be a path
    /// segment fails at construction rather than on the first turn.
    pub voice_id: Option<String>,
    /// Voices this provider advertises.
    ///
    /// Empty means the catalogue has not been read. `GET /v1/voices` needs the
    /// credential and a round trip, so it is not done during construction —
    /// [`ElevenLabsTts::load_voices`] fetches them when a caller has an async
    /// context and wants the menu.
    pub voices: Vec<conduit_provider::tts::Voice>,
    /// Default request settings this configured provider applies.
    ///
    /// Checked against the provider's declared schema before they were stored.
    /// They form the base of every request; a setting the request itself
    /// carries overrides the default of the same name.
    pub default_settings: serde_json::Map<String, serde_json::Value>,
}

impl ElevenLabsConfig {
    /// How to reach this account, as the shared client wants it.
    fn http(&self) -> conduit_http::HttpConfig {
        conduit_http::HttpConfig {
            base_url: self.base_url.clone(),
            name: self.name.clone(),
            // The key travels in a header of its own rather than as a bearer
            // token, and `Credential` is what keeps it out of a log line.
            credential: conduit_http::Credential::header(API_KEY_HEADER, self.api_key.clone()),
            headers: Vec::new(),
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
        }
    }

    /// The identity half of a descriptor for one capability this account
    /// serves.
    fn descriptor(
        &self,
        capability: conduit_provider::Capability,
    ) -> conduit_provider::Descriptor {
        conduit_provider::Descriptor::new(self.name.clone(), capability)
            .with_label(self.label.clone().unwrap_or_else(|| self.name.clone()))
            .with_version(env!("CARGO_PKG_VERSION"))
    }

    /// The models to advertise, falling back to `defaults` when none are named.
    fn models_or(&self, defaults: &[&str]) -> Vec<String> {
        if self.models.is_empty() {
            return defaults.iter().map(|model| (*model).to_owned()).collect();
        }
        self.models.clone()
    }
}

impl Default for ElevenLabsConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            api_key: None,
            name: "elevenlabs".to_owned(),
            label: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: Some(DEFAULT_READ_TIMEOUT),
            models: Vec::new(),
            voice_id: None,
            voices: Vec::new(),
            default_settings: serde_json::Map::new(),
        }
    }
}

/// A request's settings layered over the provider's configured defaults.
///
/// The Configured Provider's stored settings are the base; a setting the request
/// carries of the same name wins, so a pipeline can still override what the
/// operator set as a default. Both were checked against the same schema, so the
/// result is too.
pub(crate) fn layered_settings(
    defaults: &serde_json::Map<String, serde_json::Value>,
    request: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut merged = defaults.clone();
    for (name, value) in request {
        merged.insert(name.clone(), value.clone());
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_travels_as_xi_api_key_rather_than_a_bearer_token() {
        // The single most common way to misconfigure this vendor. A bearer token
        // is accepted by the transport and rejected by the API.
        let config =
            ElevenLabsConfig { api_key: Some("sk_test".to_owned()), ..Default::default() };

        assert_eq!(
            config.http().credential,
            conduit_http::Credential::Header {
                name: "xi-api-key".to_owned(),
                value: "sk_test".to_owned(),
            }
        );
    }

    #[test]
    fn no_key_is_no_credential_rather_than_an_empty_header() {
        assert!(!ElevenLabsConfig::default().http().credential.is_some());
    }

    #[test]
    fn the_api_key_is_never_printed_by_anything_that_holds_it() {
        // Providers derive `Debug`, so this is what stands between the key and
        // a log line.
        let config =
            ElevenLabsConfig { api_key: Some("sk_secret".to_owned()), ..Default::default() };

        let printed = format!("{config:?}");
        assert!(printed.contains("sk_secret"), "the config itself holds the plain key");

        let credential = format!("{:?}", config.http().credential);
        assert!(!credential.contains("sk_secret"), "{credential}");
        assert!(credential.contains("xi-api-key"), "the header name is not the secret");
    }

    #[test]
    fn the_key_is_not_pinned_as_an_ordinary_header() {
        // `HttpConfig::headers` prints itself, so a secret placed there would
        // reach a log. The credential is the only place the key belongs.
        let config =
            ElevenLabsConfig { api_key: Some("sk_secret".to_owned()), ..Default::default() };
        let printed = format!("{:?}", config.http().headers);

        assert!(!printed.contains("sk_secret"), "{printed}");
        assert_eq!(config.http().headers, Vec::new());
    }

    #[test]
    fn naming_no_models_advertises_the_capabilitys_defaults() {
        let config = ElevenLabsConfig::default();
        assert_eq!(config.models_or(DEFAULT_TTS_MODELS), DEFAULT_TTS_MODELS);
        assert_eq!(config.models_or(DEFAULT_STT_MODELS), DEFAULT_STT_MODELS);

        let named = ElevenLabsConfig { models: vec!["eleven_v3".to_owned()], ..config };
        assert_eq!(named.models_or(DEFAULT_TTS_MODELS), ["eleven_v3"]);
    }

    #[test]
    fn the_default_models_are_reachable_over_http_rather_than_a_websocket() {
        // `scribe_v2_realtime` is a websocket protocol this crate does not
        // implement. Advertising it would produce a provider that fails on
        // every utterance with a 4xx an operator cannot act on.
        assert!(!DEFAULT_STT_MODELS.contains(&"scribe_v2_realtime"));
        assert_eq!(DEFAULT_STT_MODEL, "scribe_v2");
    }

    #[test]
    fn the_default_synthesis_model_is_a_low_latency_one() {
        // A spoken turn is latency-bound; the expressive model is opt-in.
        assert!(DEFAULT_TTS_MODEL.contains("flash"), "{DEFAULT_TTS_MODEL}");
        assert_eq!(DEFAULT_TTS_MODELS.first(), Some(&DEFAULT_TTS_MODEL));
    }

    #[test]
    fn deprecated_turbo_models_are_not_advertised() {
        for model in DEFAULT_TTS_MODELS {
            assert!(!model.contains("turbo"), "`{model}` is superseded by a flash model");
        }
    }

    #[test]
    fn a_request_setting_overrides_a_stored_default_of_the_same_name() {
        let defaults = serde_json::json!({ "stability": 0.5, "style": 0.1 });
        let request = serde_json::json!({ "stability": 0.9 });
        let merged = layered_settings(
            defaults.as_object().expect("object"),
            request.as_object().expect("object"),
        );

        assert_eq!(merged["stability"], 0.9, "the request wins");
        assert_eq!(merged["style"], 0.1, "the untouched default survives");
    }
}
