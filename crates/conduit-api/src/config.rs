//! Building a provider registry from the environment.
//!
//! Configuration is explicit: nothing is registered unless it was asked for,
//! and a partial configuration is an error rather than a silently missing
//! stage. A server that cannot hear should say so at startup, not halfway
//! through someone's first sentence.

use std::collections::HashMap;

use conduit_core::{Error, Result};
use conduit_openai::{OpenAi, OpenAiConfig, OpenAiStt, OpenAiTts};
use conduit_runtime::Providers;

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

/// What a configuration registered, for logging and for deciding whether any
/// providers exist at all.
#[derive(Debug, Default)]
pub struct Registered {
    /// Human-readable descriptions of what was registered.
    pub descriptions: Vec<String>,
}

impl Registered {
    /// Whether nothing was registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.descriptions.is_empty()
    }
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
    let mut registered = Registered::default();

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
    fn a_model_without_a_server_is_a_configuration_error() {
        // Otherwise the recognizer silently goes missing and the first
        // conversation fails instead of the server refusing to start.
        let error =
            from_vars(&vars(&[(STT_MODEL, "whisper-1")])).expect_err("a model needs a server");
        assert!(error.to_string().contains(BASE_URL), "{error}");
    }
}
