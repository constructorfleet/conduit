//! The OpenAI-compatible vendor: reasoning, recognition, and synthesis.

use conduit_core::Result;
use conduit_openai::{OpenAi as OpenAiLlm, OpenAiConfig, OpenAiStt, OpenAiTts};
use conduit_provider::storage::{
    LlmVariant, ProviderDefinition, ProviderDefinitionVariant, SttVariant, TtsVariant,
};
use conduit_runtime::Providers;

use super::{secret_value, unclaimed, ProviderFactory};

/// Providers reached over the OpenAI HTTP API, or anything that speaks it.
///
/// One factory for three capabilities because they are one configuration: a
/// base URL and a credential, pointed at a hosted API or at a local server
/// that implements the same routes.
pub struct OpenAi;

#[async_trait::async_trait]
impl ProviderFactory for OpenAi {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::Llm { variant: LlmVariant::OpenAi { .. } }
                | ProviderDefinitionVariant::Stt { variant: SttVariant::OpenAi { .. } }
                | ProviderDefinitionVariant::Tts { variant: TtsVariant::OpenAi { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        match &definition.variant {
            ProviderDefinitionVariant::Llm {
                variant: LlmVariant::OpenAi { base_url, api_key, models, system_prompt, .. },
            } => {
                let mut config = config(definition, base_url, api_key);
                config.models = models.clone();
                config.system_prompt = system_prompt.clone();
                Ok(providers.with_llm(OpenAiLlm::new(config)?))
            }
            ProviderDefinitionVariant::Stt {
                variant: SttVariant::OpenAi { base_url, model, api_key, .. },
            } => {
                let config = config(definition, base_url, api_key);
                Ok(providers.with_stt(OpenAiStt::new(&config, model)?))
            }
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::OpenAi { base_url, model, api_key, voices },
            } => {
                let config = config(definition, base_url, api_key);
                let provider = OpenAiTts::new(&config, model)?;
                Ok(providers.with_tts(with_voices(provider, voices)))
            }
            _ => Err(unclaimed(self.name(), definition)),
        }
    }
}

/// The client configuration every OpenAI-compatible provider shares.
fn config(
    definition: &ProviderDefinition,
    base_url: &str,
    api_key: &Option<conduit_provider::storage::ProviderSecret>,
) -> OpenAiConfig {
    OpenAiConfig {
        base_url: base_url.to_owned(),
        api_key: secret_value(api_key),
        name: definition.id.clone(),
        // The label an operator typed, kept distinct from the identity: the
        // provider is registered under the definition id and calls itself by
        // it, and this is only what a screen shows.
        label: Some(definition.label.clone()),
        ..OpenAiConfig::default()
    }
}

/// Declares the voices a definition listed, if it listed any.
///
/// An empty list is not an empty catalogue: a compatible server that
/// enumerates nothing still speaks with whatever voice a request names, so the
/// provider is left saying it restricts nothing.
fn with_voices(provider: OpenAiTts, voices: &[String]) -> OpenAiTts {
    if voices.is_empty() {
        return provider;
    }
    provider.with_voices(
        voices
            .iter()
            .map(|voice| conduit_provider::tts::Voice {
                id: voice.clone(),
                name: voice.clone(),
                language: "en-US".to_owned(),
            })
            .collect(),
    )
}
