//! Google Cloud speech providers: synthesis and recognition.
//!
//! | Capability | Endpoint |
//! | --- | --- |
//! | [`GoogleTts`] | `POST https://texttospeech.googleapis.com/v1/text:synthesize` |
//! | [`GoogleStt`] | `POST https://speech.googleapis.com/v1/speech:recognize` |
//!
//! Both are reached over REST through [`conduit_http`], with a bearer token
//! from Application Default Credentials fetched per request because it expires.
//! See [`crates/conduit-google/README.md`][readme] for why the official SDK is
//! not used and what synthesis buffering costs.
//!
//! ```no_run
//! # async fn example() -> conduit_core::Result<()> {
//! use conduit_google::{GoogleConfig, GoogleStt, GoogleTts};
//!
//! // Credentials default to ADC, so a GCE or GKE host configures nothing.
//! let config = GoogleConfig::default();
//! let tts = GoogleTts::new(&config).await?;
//! let stt = GoogleStt::new(&config).await?;
//! # Ok(())
//! # }
//! ```
//!
//! [readme]: https://github.com/Teagan42/conduit/blob/main/crates/conduit-google/README.md

// Without credential discovery the ADC arm is gone, and with it the only use of
// some of what this crate imports. The providers still exist and still refuse
// with a message naming the feature, which is the point of compiling either way.
#![cfg_attr(not(feature = "google"), allow(unused_imports))]

pub mod auth;
pub mod stt;
pub mod tts;

use std::time::Duration;

pub use auth::Credentials;
// Re-exported so a caller classifying a Google failure does not have to know
// which crate does the classifying. Google's failures are the same shapes every
// HTTP-backed provider produces, so they are the same type.
pub use conduit_http::{Failure, FailureKind};
pub use stt::GoogleStt;
pub use tts::GoogleTts;

/// The Cloud Text-to-Speech v1 API.
pub const DEFAULT_TTS_BASE_URL: &str = "https://texttospeech.googleapis.com/v1";

/// The Cloud Speech-to-Text v1 API.
pub const DEFAULT_STT_BASE_URL: &str = "https://speech.googleapis.com/v1";

/// The language a voice speaks and a recognizer listens for when none is named.
///
/// Google requires `languageCode` on every synthesis request — unlike the model
/// name, it has no server-side default — so this is the value that makes an
/// unconfigured provider work rather than 400.
pub const DEFAULT_LANGUAGE: &str = "en-US";

/// How long to wait to reach a service.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a service may go silent mid-response.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// How a Google provider reaches its services.
///
/// One configuration describes one *credential and its settings*, and both
/// capabilities are built from it — a deployment authorizes itself to Google
/// once, not once per capability.
#[derive(Debug, Clone)]
pub struct GoogleConfig {
    /// Stable identity, so two differently configured credentials can coexist
    /// in one registry.
    ///
    /// This is what the provider calls itself, and what appears in metric labels
    /// and error messages. The key it is registered under is the deployment's to
    /// choose and need not match.
    pub name: String,
    /// Human-readable name for operator screens, e.g. `"Google (us-central1)"`.
    ///
    /// `None` shows the identity.
    pub label: Option<String>,
    /// How the provider proves who it is. Defaults to Application Default
    /// Credentials, which is what a real deployment uses.
    pub credentials: Credentials,
    /// BCP-47 language tag for synthesis and recognition, e.g. `"en-GB"`.
    ///
    /// A recognition request may override it per session; synthesis takes it
    /// from the chosen voice's own language when there is one.
    pub language: String,
    /// Voice name for synthesis, e.g. `"en-US-Neural2-F"`. `None` lets Google
    /// choose a voice for [`GoogleConfig::language`].
    pub voice: Option<String>,
    /// Recognition model, e.g. `"latest_long"` or `"telephony"`. `None` uses
    /// Google's default.
    pub model: Option<String>,
    /// Base URL of the Text-to-Speech service. Overridden only by tests and by a
    /// deployment behind a proxy.
    pub tts_base_url: String,
    /// Base URL of the Speech-to-Text service.
    pub stt_base_url: String,
    /// How long to wait for the TCP and TLS handshake.
    pub connect_timeout: Duration,
    /// How long the service may go silent before the request is abandoned.
    ///
    /// A read timeout rather than a total one: recognizing a long recording
    /// legitimately takes a long time, and what is never fine is silence.
    /// `None` disables the bound.
    pub read_timeout: Option<Duration>,
    /// Bytes of decoded audio in one emitted synthesis chunk.
    ///
    /// Synthesis is not streaming — see [`tts`] — so the decoded buffer is cut
    /// into pieces this size to hand downstream. Smaller pieces do not make the
    /// first one arrive sooner; they only bound how much a consumer holds.
    pub chunk_bytes: usize,
    /// Default request settings this configured provider applies.
    ///
    /// The reusable settings an operator saved on the Configured Provider,
    /// checked against this provider's declared schema before they were stored.
    /// They form the base of every request; a setting the request itself carries
    /// overrides the default of the same name.
    pub default_settings: serde_json::Map<String, serde_json::Value>,
}

/// How much decoded audio one synthesis chunk carries by default.
///
/// 32 kB of 16 kHz mono signed 16-bit PCM is one second, which is a comfortable
/// unit for a consumer to hold and small enough that a cancelled utterance stops
/// promptly.
pub const DEFAULT_CHUNK_BYTES: usize = 32_768;

impl Default for GoogleConfig {
    fn default() -> Self {
        Self {
            name: "google".to_owned(),
            label: None,
            credentials: Credentials::Adc,
            language: DEFAULT_LANGUAGE.to_owned(),
            voice: None,
            model: None,
            tts_base_url: DEFAULT_TTS_BASE_URL.to_owned(),
            stt_base_url: DEFAULT_STT_BASE_URL.to_owned(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: Some(DEFAULT_READ_TIMEOUT),
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            default_settings: serde_json::Map::new(),
        }
    }
}

impl GoogleConfig {
    /// The identity half of a descriptor for one capability.
    fn descriptor(
        &self,
        capability: conduit_provider::Capability,
    ) -> conduit_provider::Descriptor {
        conduit_provider::Descriptor::new(self.name.clone(), capability)
            .with_label(self.label.clone().unwrap_or_else(|| self.name.clone()))
            .with_version(env!("CARGO_PKG_VERSION"))
    }
}

/// A request's settings layered over the provider's configured defaults.
///
/// The Configured Provider's stored settings are the base; a setting the request
/// carries of the same name wins. Both were checked against the same schema, so
/// the result is too.
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

/// Checks that `tag` is a BCP-47 language tag and nothing else.
///
/// A language tag reaches a URL query on the voices endpoint and a JSON body on
/// both others, so it is checked rather than trusted: BCP-47 subtags are
/// alphanumeric separated by hyphens, and anything outside that alphabet is
/// either a typo or an attempt to smuggle a second query parameter.
///
/// Public so the management API can refuse a definition with this rule rather
/// than a second copy of it: a form that accepted a tag this crate rejects would
/// store a definition that fails to build on the next server start.
///
/// # Errors
///
/// Returns [`conduit_core::Error::Config`] naming the offending tag.
pub fn validate_language(tag: &str) -> conduit_core::Result<()> {
    let valid = !tag.is_empty()
        && tag.len() <= 35
        && tag.split('-').all(|subtag| {
            !subtag.is_empty()
                && subtag.chars().all(|character| character.is_ascii_alphanumeric())
        });
    if valid {
        return Ok(());
    }
    Err(conduit_core::Error::Config(format!(
        "`{tag}` is not a BCP-47 language tag: expected alphanumeric subtags separated by hyphens, \
         e.g. `en-US`"
    )))
}

/// Checks that `name` is a Google voice name and nothing else.
///
/// Voice names are `<language>-<family>-<variant>` — `en-US-Neural2-F`,
/// `en-GB-Chirp3-HD-Achernar`. The same alphabet as a language tag, so the same
/// check with a longer bound.
///
/// Public on the same terms as [`validate_language`].
///
/// # Errors
///
/// Returns [`conduit_core::Error::Config`] naming the offending voice.
pub fn validate_voice(name: &str) -> conduit_core::Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name.split('-').all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_alphanumeric())
        });
    if valid {
        return Ok(());
    }
    Err(conduit_core::Error::Config(format!(
        "`{name}` is not a Google voice name: expected alphanumeric parts separated by hyphens, \
         e.g. `en-US-Neural2-F`"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_language_tags_are_accepted() {
        for tag in ["en", "en-US", "en-GB", "zh-CN", "cmn-Hans-CN", "pt-BR"] {
            assert!(validate_language(tag).is_ok(), "{tag} should be accepted");
        }
    }

    #[test]
    fn a_language_tag_cannot_carry_a_second_query_parameter() {
        // The voices endpoint takes `languageCode` in the query string. A value
        // holding `&` or `?` would be a second parameter if it were trusted.
        for tag in [
            "en-US&key=leaked",
            "en-US?pageSize=1",
            "en US",
            "en/../../admin",
            "",
            "en-",
            "-US",
            "en_US",
            "en-US\n",
        ] {
            let error = validate_language(tag).expect_err("should be refused");
            assert!(error.to_string().contains("BCP-47"), "{error}");
        }
    }

    #[test]
    fn a_language_tag_has_a_length_bound() {
        assert!(validate_language(&"a".repeat(36)).is_err());
    }

    #[test]
    fn real_voice_names_are_accepted() {
        for voice in ["en-US-Neural2-F", "en-GB-Chirp3-HD-Achernar", "ja-JP-Standard-A"] {
            assert!(validate_voice(voice).is_ok(), "{voice} should be accepted");
        }
    }

    #[test]
    fn a_voice_name_outside_the_alphabet_is_refused() {
        for voice in ["", "en-US-Neural2-F ", "../secret", "en-US-\"F\"", &"a".repeat(65)] {
            let error = validate_voice(voice).expect_err("should be refused");
            assert!(error.to_string().contains("voice name"), "{error}");
        }
    }

    #[test]
    fn a_request_setting_overrides_a_stored_default_of_the_same_name() {
        let defaults = serde_json::json!({ "pitch": -2.0, "volumeGainDb": 3.0 })
            .as_object()
            .cloned()
            .expect("object");
        let request = serde_json::json!({ "pitch": 5.0 }).as_object().cloned().expect("object");

        let merged = layered_settings(&defaults, &request);
        assert_eq!(merged.get("pitch"), Some(&serde_json::json!(5.0)));
        assert_eq!(merged.get("volumeGainDb"), Some(&serde_json::json!(3.0)));
    }

    #[test]
    fn the_default_configuration_uses_application_default_credentials() {
        let config = GoogleConfig::default();
        assert!(matches!(config.credentials, Credentials::Adc));
        assert_eq!(config.language, "en-US");
    }
}
