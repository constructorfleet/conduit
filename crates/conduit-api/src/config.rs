//! Building a provider registry from the environment.
//!
//! Configuration is explicit: nothing is registered unless it was asked for,
//! and a partial configuration is an error rather than a silently missing
//! stage. A server that cannot hear should say so at startup, not halfway
//! through someone's first sentence.

use std::collections::HashMap;

use std::sync::Arc;
use std::time::Duration;

use conduit_core::{Error, Result};
use conduit_openai::{OpenAi, OpenAiConfig, OpenAiStt, OpenAiTts};
use conduit_provider::storage::PipelineStore;
use conduit_runtime::{Providers, DEFAULT_IDLE_TIMEOUT};
use conduit_store::{FileStore, MemoryStore};

use crate::auth::{Access, Tokens, ALLOW_ANONYMOUS, TOKENS_FILE};

/// Base URL of an OpenAI-compatible server.
const BASE_URL: &str = "CONDUIT_OPENAI_BASE_URL";
/// Bearer token for that server. Local servers usually need none.
const API_KEY: &str = "CONDUIT_OPENAI_API_KEY";
/// Registry name, so two servers can be configured side by side.
const NAME: &str = "CONDUIT_OPENAI_NAME";
/// Transcription model, e.g. `whisper-1`. Enables the recognizer.
const STT_MODEL: &str = "CONDUIT_OPENAI_STT_MODEL";
/// Speech model, e.g. `tts-1`. Enables the synthesizer.
const TTS_MODEL: &str = "CONDUIT_OPENAI_TTS_MODEL";
/// How long the server may go silent mid-response, in seconds. `0` disables it.
const READ_TIMEOUT: &str = "CONDUIT_OPENAI_READ_TIMEOUT_SECS";

/// How long a turn may publish nothing before it is abandoned, in seconds.
/// `0` removes the bound.
const TURN_IDLE_TIMEOUT: &str = "CONDUIT_TURN_IDLE_TIMEOUT_SECS";

/// Directory to keep pipeline definitions in. Unset means memory only.
const PIPELINE_DIR: &str = "CONDUIT_PIPELINE_DIR";
/// PostgreSQL connection URL. Takes precedence over a directory.
const DATABASE_URL: &str = "CONDUIT_DATABASE_URL";

/// Decides who may call the service API.
///
/// There is no open default. A server that authenticated nobody unless
/// configured otherwise would mean every deployment that forgot the token file
/// is exposed and looks fine, which is the failure this exists to prevent. An
/// operator who genuinely wants an open server says so with
/// [`ALLOW_ANONYMOUS`], and gets a warning every time it starts.
///
/// # Errors
///
/// Returns [`Error::Config`] if neither variable is set, if both are, or if the
/// token file cannot be read, is readable by other users, or is invalid.
pub async fn access_from_env() -> Result<Access> {
    let file = std::env::var(TOKENS_FILE).ok().filter(|path| !path.trim().is_empty());
    let anonymous = std::env::var(ALLOW_ANONYMOUS)
        .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "yes"));

    match (file, anonymous) {
        // Both is a contradiction, and guessing which was meant could silently
        // discard the token file and serve the API open.
        (Some(_), true) => Err(Error::Config(format!(
            "{TOKENS_FILE} and {ALLOW_ANONYMOUS} are both set; pick one"
        ))),
        (Some(path), false) => {
            let tokens = Tokens::load(&path).await?;
            tracing::info!(
                path = %path,
                tokens = tokens.len(),
                "authenticating callers against the token file"
            );
            Ok(Access::Tokens(tokens))
        }
        (None, true) => {
            tracing::warn!(
                "{ALLOW_ANONYMOUS} is set: anyone who can reach this port can talk to \
                 the assistant, read transcripts off /v1/events, and delete pipelines"
            );
            Ok(Access::anonymous())
        }
        // Refusing to start is the whole point: an operator is never left
        // unsure whether their deployment is protected.
        (None, false) => Err(Error::Config(format!(
            "no authentication is configured; set {TOKENS_FILE} to a token file, or \
             {ALLOW_ANONYMOUS}=1 to serve the API to anyone who can reach the port"
        ))),
    }
}

/// Opens the pipeline store the environment asks for.
///
/// Defaults to memory, and says so: a server that silently forgot every
/// pipeline on restart would be a nasty surprise in production.
///
/// # Errors
///
/// Returns [`Error::Config`] if the configured directory cannot be used.
pub async fn store_from_env() -> Result<Arc<dyn PipelineStore>> {
    #[cfg(feature = "postgres")]
    if let Ok(url) = std::env::var(DATABASE_URL) {
        if !url.is_empty() {
            // A shared database is what more than one replica needs, so it
            // wins over a directory only this process can see.
            tracing::info!("storing pipelines in PostgreSQL");
            return Ok(Arc::new(conduit_store::PostgresStore::connect(&url).await?));
        }
    }

    #[cfg(not(feature = "postgres"))]
    if std::env::var(DATABASE_URL).is_ok_and(|url| !url.is_empty()) {
        // Silently falling back to memory would lose pipelines a deployment
        // clearly meant to keep.
        return Err(Error::Config(format!(
            "{DATABASE_URL} is set but this build has no PostgreSQL support; \
             rebuild with --features postgres"
        )));
    }

    match std::env::var(PIPELINE_DIR) {
        Ok(directory) if !directory.is_empty() => {
            tracing::info!(%directory, "storing pipelines on disk");
            Ok(Arc::new(FileStore::open(directory).await?))
        }
        _ => {
            tracing::warn!(
                "pipelines are kept in memory and will be lost on restart; set \
                 {PIPELINE_DIR} to keep them"
            );
            Ok(Arc::new(MemoryStore::new()))
        }
    }
}

/// What a configuration registered, for logging and for deciding whether any
/// providers exist at all.
#[derive(Debug)]
pub struct Registered {
    /// Human-readable descriptions of what was registered.
    pub descriptions: Vec<String>,
    /// How long a turn may publish nothing before it is abandoned. `None` means
    /// an operator deliberately removed the bound.
    pub turn_idle_timeout: Option<Duration>,
}

/// Written by hand rather than derived, because a derived `Option<Duration>`
/// default is `None` — which here means "no timeout" and would hand an
/// unconfigured deployment the exact hang this bound exists to prevent.
impl Default for Registered {
    fn default() -> Self {
        Self { descriptions: Vec::new(), turn_idle_timeout: Some(DEFAULT_IDLE_TIMEOUT) }
    }
}

impl Registered {
    /// Whether nothing was registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.descriptions.is_empty()
    }
}

/// How long a provider may go silent mid-response, as configured.
///
/// Unset means the provider's own default. `0` means no bound, which is a
/// deliberate choice a deployment can make rather than an accident — a stalled
/// provider then hangs the turn until the client disconnects.
///
/// # Errors
///
/// Returns [`Error::Config`] if the value is not a whole number of seconds. A
/// misspelled duration silently falling back to the default is how a
/// deployment ends up with a timeout it did not ask for.
fn read_timeout(vars: &HashMap<String, String>) -> Result<Option<Duration>> {
    seconds_or(vars, READ_TIMEOUT, OpenAiConfig::default().read_timeout, || {
        tracing::warn!(
            "{READ_TIMEOUT} is 0, so a provider that stops responding will not be given up on"
        );
    })
}

/// How long a turn may publish nothing before it is abandoned, as configured.
///
/// Bounds the turn rather than one provider, so it catches a stage that stalls
/// somewhere the HTTP client's own read timeout cannot see it — including a
/// provider that is not an HTTP client at all.
///
/// # Errors
///
/// Returns [`Error::Config`] if the value is not a whole number of seconds.
fn turn_idle_timeout(vars: &HashMap<String, String>) -> Result<Option<Duration>> {
    seconds_or(vars, TURN_IDLE_TIMEOUT, Some(DEFAULT_IDLE_TIMEOUT), || {
        tracing::warn!(
            "{TURN_IDLE_TIMEOUT} is 0, so a turn whose provider stops answering will hold \
             the conversation until the client disconnects"
        );
    })
}

/// Reads a duration in whole seconds from `vars`, or `fallback` if unset.
///
/// `0` means no bound, and calls `on_removed` so a deployment that removed one
/// says so in its logs rather than discovering it during an incident.
///
/// # Errors
///
/// Returns [`Error::Config`] if the value is not a whole number of seconds. A
/// misspelled duration silently falling back to the default is how a deployment
/// ends up with a timeout it did not ask for.
fn seconds_or(
    vars: &HashMap<String, String>,
    name: &str,
    fallback: Option<Duration>,
    on_removed: impl FnOnce(),
) -> Result<Option<Duration>> {
    let Some(value) =
        vars.get(name).map(|value| value.trim()).filter(|value| !value.is_empty())
    else {
        return Ok(fallback);
    };

    let seconds: u64 = value.parse().map_err(|_| {
        Error::Config(format!("{name} must be a whole number of seconds, got `{value}`"))
    })?;
    if seconds == 0 {
        on_removed();
        return Ok(None);
    }
    Ok(Some(Duration::from_secs(seconds)))
}

/// Reads provider configuration from the process environment.
///
/// # Errors
///
/// Returns [`Error::Config`] if a provider is configured but cannot be built.
pub fn from_env() -> Result<(Providers, Registered)> {
    from_vars(&std::env::vars().collect())
}

/// Builds providers from `vars`.
///
/// Taking a map rather than reading the environment directly keeps this
/// testable: process environment is global, and tests that mutate it race.
///
/// # Errors
///
/// Returns [`Error::Config`] if a provider is configured but cannot be built.
pub fn from_vars(vars: &HashMap<String, String>) -> Result<(Providers, Registered)> {
    let mut providers = Providers::new();
    let mut registered =
        Registered { turn_idle_timeout: turn_idle_timeout(vars)?, ..Registered::default() };

    let base_url = vars.get(BASE_URL);
    let api_key = vars.get(API_KEY);
    let stt_model = vars.get(STT_MODEL);
    let tts_model = vars.get(TTS_MODEL);

    // A model without a server to run it on is a typo, not a configuration.
    if base_url.is_none() && api_key.is_none() {
        if stt_model.is_some() || tts_model.is_some() {
            return Err(Error::Config(format!(
                "a model is configured but no server is; set {BASE_URL} or {API_KEY}"
            )));
        }
        return Ok((providers, registered));
    }

    let config = OpenAiConfig {
        base_url: base_url.cloned().unwrap_or_else(|| OpenAiConfig::default().base_url),
        api_key: api_key.cloned(),
        name: vars.get(NAME).cloned().unwrap_or_else(|| OpenAiConfig::default().name),
        read_timeout: read_timeout(vars)?,
        ..OpenAiConfig::default()
    };

    if let Some(model) = stt_model {
        providers = providers.with_stt(OpenAiStt::new(&config, model)?);
        registered.descriptions.push(format!("stt `{}` using {model}", config.name));
    }
    if let Some(model) = tts_model {
        providers = providers.with_tts(OpenAiTts::new(&config, model)?);
        registered.descriptions.push(format!("tts `{}` using {model}", config.name));
    }

    // The language model needs no model name here: a pipeline names its own,
    // which is what lets one server serve several graphs.
    let name = config.name.clone();
    providers = providers.with_llm(OpenAi::new(config)?);
    registered.descriptions.push(format!("llm `{name}`"));

    Ok((providers, registered))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
    }

    #[test]
    fn an_empty_environment_registers_nothing() {
        let (providers, registered) = from_vars(&vars(&[])).expect("builds");
        assert!(registered.is_empty());
        assert!(providers.llm().is_empty());
    }

    #[test]
    fn a_base_url_alone_registers_a_language_model() {
        let (providers, registered) =
            from_vars(&vars(&[(BASE_URL, "http://localhost:11434/v1")])).expect("builds");
        assert_eq!(providers.llm().names().collect::<Vec<_>>(), ["openai"]);
        assert!(providers.stt().is_empty(), "no recognizer was asked for");
        assert_eq!(registered.descriptions.len(), 1);
    }

    #[test]
    fn naming_models_registers_speech_providers() {
        let map = vars(&[
            (BASE_URL, "http://localhost:8000/v1"),
            (STT_MODEL, "whisper-1"),
            (TTS_MODEL, "tts-1"),
        ]);
        let (providers, registered) = from_vars(&map).expect("builds");

        assert_eq!(providers.stt().names().collect::<Vec<_>>(), ["openai"]);
        assert_eq!(providers.tts().names().collect::<Vec<_>>(), ["openai"]);
        assert_eq!(providers.llm().names().collect::<Vec<_>>(), ["openai"]);
        assert_eq!(registered.descriptions.len(), 3);
    }

    #[test]
    fn the_registry_name_can_be_chosen() {
        let map = vars(&[(BASE_URL, "http://localhost:11434/v1"), (NAME, "ollama")]);
        let (providers, _) = from_vars(&map).expect("builds");
        assert_eq!(providers.llm().names().collect::<Vec<_>>(), ["ollama"]);
    }

    #[test]
    fn a_key_alone_is_enough_for_the_hosted_api() {
        let (providers, _) = from_vars(&vars(&[(API_KEY, "sk-test")])).expect("builds");
        assert_eq!(providers.llm().names().collect::<Vec<_>>(), ["openai"]);
    }

    #[test]
    fn the_read_timeout_defaults_to_the_providers_own() {
        assert_eq!(
            read_timeout(&vars(&[])).expect("builds"),
            OpenAiConfig::default().read_timeout
        );
        assert!(
            read_timeout(&vars(&[])).expect("builds").is_some(),
            "a deployment that configures nothing must still bound a stalled provider"
        );
    }

    #[test]
    fn the_read_timeout_can_be_set_in_seconds() {
        let timeout = read_timeout(&vars(&[(READ_TIMEOUT, "5")])).expect("builds");
        assert_eq!(timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn a_zero_read_timeout_removes_the_bound() {
        assert_eq!(read_timeout(&vars(&[(READ_TIMEOUT, "0")])).expect("builds"), None);
    }

    #[test]
    fn an_unreadable_read_timeout_is_refused_rather_than_ignored() {
        // Falling back to the default would give a deployment a timeout it did
        // not ask for and no indication of why.
        let error = read_timeout(&vars(&[(READ_TIMEOUT, "30s")])).expect_err("not a number");
        assert!(error.to_string().contains(READ_TIMEOUT), "{error}");
    }

    #[test]
    fn a_turn_is_bounded_by_default() {
        // The deployment that configures nothing must not be the one that hangs.
        let (_, registered) = from_vars(&vars(&[])).expect("builds");
        assert_eq!(registered.turn_idle_timeout, Some(DEFAULT_IDLE_TIMEOUT));
    }

    #[test]
    fn a_default_registered_is_still_bounded() {
        // `Default` is what a caller that skipped `from_vars` gets, and an
        // `Option<Duration>` defaulting to `None` would mean no timeout at all.
        assert_eq!(Registered::default().turn_idle_timeout, Some(DEFAULT_IDLE_TIMEOUT));
    }

    #[test]
    fn the_turn_idle_timeout_can_be_set_in_seconds() {
        let (_, registered) = from_vars(&vars(&[(TURN_IDLE_TIMEOUT, "5")])).expect("builds");
        assert_eq!(registered.turn_idle_timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn a_zero_turn_idle_timeout_removes_the_bound() {
        // Expressible on purpose, for a deployment whose own layer above the
        // runtime already bounds a turn.
        let (_, registered) = from_vars(&vars(&[(TURN_IDLE_TIMEOUT, "0")])).expect("builds");
        assert_eq!(registered.turn_idle_timeout, None);
    }

    #[test]
    fn an_unreadable_turn_idle_timeout_is_refused_rather_than_ignored() {
        let error =
            from_vars(&vars(&[(TURN_IDLE_TIMEOUT, "a minute")])).expect_err("not a number");
        assert!(error.to_string().contains(TURN_IDLE_TIMEOUT), "{error}");
    }

    #[test]
    fn a_model_without_a_server_is_a_configuration_error() {
        // Otherwise the recognizer silently goes missing and the first
        // conversation fails instead of the server refusing to start.
        let error =
            from_vars(&vars(&[(STT_MODEL, "whisper-1")])).expect_err("a model needs a server");
        assert!(error.to_string().contains(BASE_URL), "{error}");
    }
}
