//! Deepgram Aura speech synthesis for Conduit.
//!
//! ```no_run
//! # use conduit_deepgram::{DeepgramTts, DeepgramTtsConfig};
//! let tts = DeepgramTts::new(DeepgramTtsConfig {
//!     api_key: Some("dg-...".to_owned()),
//!     ..DeepgramTtsConfig::default()
//! })?;
//! # Ok::<(), conduit_core::Error>(())
//! ```
//!
//! # Why this is not the `openai` variant with a different URL
//!
//! Three reasons, any one of which is enough:
//!
//! - **The credential is `Authorization: Token <key>`, not `Bearer`.** This is
//!   the easy mistake in this vendor: a `Bearer` key yields a 401, which reads
//!   as a wrong key rather than as a wrong scheme, and an operator checking
//!   their key against the dashboard finds nothing wrong with it.
//! - **The voice is the model, and the model is a query parameter.** OpenAI puts
//!   `model` and `voice` in the body as separate fields. Deepgram encodes both
//!   into one id — `aura-2-thalia-en` is `[family]-[voice]-[language]` — and
//!   reads it off the query string.
//! - **The body is `{"text": ...}`**, not `{"input": ...}`.
//!
//! # What this provider does not do
//!
//! It does not use the streaming WebSocket interface. Deepgram offers one, and
//! the REST endpoint this crate uses already streams audio from the first byte,
//! which is the latency property the socket is usually wanted for. What the
//! socket adds is incremental *input* — sending text while a model is still
//! generating it — and nothing upstream can supply that:
//! [`SynthesisRequest`](conduit_provider::tts::SynthesisRequest) arrives with
//! its text already complete. Building the framing before a caller exists to
//! feed it would be a protocol maintained for no one. This follows the
//! precedent `conduit-elevenlabs` set for realtime transcription: a websocket is
//! a different protocol rather than a setting, so it would be a second provider
//! and not a flag on this one.

pub mod model_id;
pub mod tts;

use std::time::Duration;

use conduit_http::{Credential, HttpConfig};

pub use tts::DeepgramTts;

/// Where Deepgram's API lives.
const DEFAULT_BASE_URL: &str = "https://api.deepgram.com/v1";

/// The voice used when a deployment names none, which is Deepgram's own default
/// for `/v1/speak`. Stated here rather than left to the server so that the
/// descriptor can report the voice a turn will actually use.
const DEFAULT_MODEL: &str = "aura-asteria-en";

/// How long to wait to reach the API.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the API may go silent mid-response before the request is abandoned.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How to reach Deepgram, and what to ask it for.
#[derive(Debug, Clone)]
pub struct DeepgramTtsConfig {
    /// Base URL including the version prefix.
    ///
    /// Defaulted and not exposed in the provider definition schema, the way
    /// `conduit-elevenlabs` handles the same situation: there is one Deepgram and
    /// nothing else speaks its API, so a field in the console would be a box with
    /// exactly one correct value in it. It stays on the config because a test
    /// needs to point the provider at a stand-in server.
    pub base_url: String,
    /// Provider identity used in errors, metrics, and registry lookups.
    pub name: String,
    /// Human-readable name for operator screens.
    pub label: Option<String>,
    /// API key. `None` builds a provider that will be refused by the API, which
    /// is deliberate: a missing key is a configuration error to surface at the
    /// first turn rather than a reason to refuse to construct.
    pub api_key: Option<String>,
    /// The Aura model, which is also the voice — `aura-2-thalia-en`.
    pub model: Option<String>,
    /// How long to wait for the TCP and TLS handshake.
    pub connect_timeout: Duration,
    /// How long the API may go silent before a request is abandoned.
    pub read_timeout: Option<Duration>,
}

impl Default for DeepgramTtsConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            name: "deepgram".to_owned(),
            label: None,
            api_key: None,
            model: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: Some(DEFAULT_READ_TIMEOUT),
        }
    }
}

impl DeepgramTtsConfig {
    /// The HTTP client configuration this implies.
    fn into_http(self) -> HttpConfig {
        HttpConfig {
            base_url: self.base_url,
            name: self.name,
            // `Token`, not `Bearer`. `Credential::Header` carries the scheme in
            // the value because that is what Deepgram's own header does, and it
            // redacts itself in `Debug` the way a bearer token does.
            credential: Credential::header(
                "Authorization",
                self.api_key.map(|key| format!("Token {key}")),
            ),
            headers: Vec::new(),
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
        }
    }
}
