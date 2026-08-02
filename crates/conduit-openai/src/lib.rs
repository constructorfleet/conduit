//! OpenAI-compatible providers: language models, speech recognition, and
//! synthesis.
//!
//! These three APIs are the closest thing to a lingua franca among model and
//! speech servers, so one implementation of each covers a great many of them.
//! Only the base URL changes:
//!
//! | Capability | Endpoint | Also served by |
//! | --- | --- | --- |
//! | [`OpenAi`] | `/chat/completions` | Ollama, vLLM, LM Studio, OpenRouter |
//! | [`OpenAiStt`] | `/audio/transcriptions` | Speaches, `whisper.cpp`, `faster-whisper` |
//! | [`OpenAiTts`] | `/audio/speech` | `openedai-speech`, which fronts Piper |
//!
//! ```no_run
//! # use conduit_openai::{OpenAi, OpenAiConfig};
//! // A local Ollama server.
//! let local = OpenAi::new(OpenAiConfig {
//!     base_url: "http://localhost:11434/v1".to_owned(),
//!     ..OpenAiConfig::default()
//! })?;
//!
//! // Or the hosted API.
//! let hosted = OpenAi::new(OpenAiConfig {
//!     api_key: std::env::var("OPENAI_API_KEY").ok(),
//!     ..OpenAiConfig::default()
//! })?;
//! # Ok::<(), conduit_core::Error>(())
//! ```
//!
//! A configuration describes one *server*, not one capability, so a single
//! host serving all three is described once:
//!
//! ```no_run
//! # use conduit_openai::{OpenAi, OpenAiConfig, OpenAiStt, OpenAiTts};
//! let config = OpenAiConfig {
//!     api_key: std::env::var("OPENAI_API_KEY").ok(),
//!     ..OpenAiConfig::default()
//! };
//! let stt = OpenAiStt::new(&config, "whisper-1")?;
//! let tts = OpenAiTts::new(&config, "tts-1")?;
//! let llm = OpenAi::new(config)?;
//! # Ok::<(), conduit_core::Error>(())
//! ```

pub mod failure;
pub mod llm;
pub mod sse;
pub mod stt;
pub mod tts;
pub mod wire;

mod http;
mod stream;

use std::time::Duration;

pub use failure::{Failure, FailureKind};
pub use llm::OpenAi;
pub use stt::OpenAiStt;
pub use tts::OpenAiTts;

/// The public OpenAI API.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// How long to wait to reach a server.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a server may go silent mid-response.
///
/// Generous enough that a large local model loading weights on the first token
/// is not mistaken for a stall, and short enough that a hung provider does not
/// hold a turn open for minutes.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// How a provider reaches its server.
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    /// Base URL including any version prefix, e.g. `http://localhost:11434/v1`.
    pub base_url: String,
    /// Bearer token. Local servers usually need none.
    pub api_key: Option<String>,
    /// Registration name, so two differently configured servers can coexist
    /// in one registry — `"openai"` and `"ollama"`, say.
    pub name: String,
    /// How long to wait for the TCP and TLS handshake. This bounds *reaching*
    /// the server and nothing after it.
    pub connect_timeout: Duration,
    /// How long the server may go silent before the request is abandoned.
    ///
    /// This is a read timeout rather than a total request timeout, and
    /// deliberately so: a long answer and a slow synthesis both stream for as
    /// long as they need, so capping the whole response would truncate work
    /// that is going fine. What is never fine is silence — a server that
    /// completes the handshake and then says nothing would otherwise hang the
    /// turn for as long as the client stays connected.
    ///
    /// `None` disables the bound, which is the shape a caller wants only when
    /// something above it already imposes a deadline.
    pub read_timeout: Option<Duration>,
    /// Models this provider advertises. Empty passes any name through.
    pub models: Vec<String>,
    /// A system prompt attached to every turn this provider serves.
    ///
    /// Belongs to the endpoint rather than to any one pipeline: it is how a
    /// deployment says what this server should be, and every pipeline pointing
    /// at it inherits it.
    pub system_prompt: Option<String>,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            api_key: None,
            name: "openai".to_owned(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: Some(DEFAULT_READ_TIMEOUT),
            models: Vec::new(),
            system_prompt: None,
        }
    }
}
