//! Building runtime providers from stored provider definitions.
//!
//! Turning a definition into a provider used to be one function with one
//! `match` over every variant every vendor has: supporting a second vendor, or
//! a second capability from a vendor already supported, meant editing the
//! middle of the server's configuration path.
//!
//! A [`ProviderFactory`] is that arm, lifted out. A factory says what it is
//! called, which definitions it builds, and how — and [`Factories`] enumerates
//! whatever has been registered rather than naming vendors in a fixed order.
//! Supporting a new vendor is then a new type and one line in
//! [`Factories::builtin`], not a change to the code that loads every provider
//! a deployment has.
//!
//! Nothing is built that was not asked for: a definition is only turned into a
//! provider because an operator stored it, and a definition no factory claims
//! fails the load loudly rather than being skipped, because a server that
//! quietly drops a provider an operator configured is a pipeline that fails
//! later with an unrelated error.

use std::sync::Arc;

use conduit_core::{Error, Result};
use conduit_provider::storage::{ProviderDefinition, ProviderSecret};
use conduit_runtime::Providers;

mod anthropic;
mod bedrock;
mod elevenlabs;
mod google;
mod marytts;
mod mcp;
mod openai;
mod script;
mod speaker;
mod transform;
mod wake;
mod wyoming;

pub use anthropic::Anthropic;
// Named for the factory's role rather than for the vendor, because
// `conduit_bedrock::Bedrock` is the provider it builds and the two would collide
// wherever both are in scope.
pub use bedrock::Bedrock as BedrockRuntime;
pub use elevenlabs::ElevenLabs;
pub use google::Google;
// Named for what it reaches rather than for the vendor, on the same terms as
// `BedrockRuntime`: `conduit_marytts::MaryTts` is the provider it builds.
pub use marytts::MaryTtsServer;
pub use mcp::Mcp;
pub use openai::OpenAi;
// Named for the factory's role on the same terms as `BedrockRuntime`:
// `conduit_script::Script` is the provider it builds.
pub use script::ScriptedTransform;
pub use speaker::{DiarizationServer, HttpSpeaker};
pub use transform::BuiltinTransform;
pub use wake::{DeviceWake, OpenWakeWord};
pub use wyoming::Wyoming;

/// One vendor's contribution to the provider registry.
///
/// Implementations are stateless descriptions of how a family of stored
/// definitions becomes running providers. A vendor that supplies three
/// capabilities — as OpenAI does — is one factory that claims all three, not
/// three factories: what belongs together is the configuration, and a base URL
/// and a credential are configured once.
#[async_trait::async_trait]
pub trait ProviderFactory: Send + Sync {
    /// What this factory is called in diagnostics, e.g. `openai`.
    fn name(&self) -> &'static str;

    /// Whether this factory builds the providers `definition` describes.
    ///
    /// Two factories must never claim the same definition. [`Factories`]
    /// consults them in registration order and stops at the first claim, so
    /// overlapping claims would make which provider a deployment gets depend
    /// on the order factories happen to be listed in.
    fn handles(&self, definition: &ProviderDefinition) -> bool;

    /// Builds the providers `definition` describes into `providers`.
    ///
    /// # Errors
    ///
    /// Returns an error if the definition cannot be turned into a provider —
    /// an unusable endpoint, models that will not load, a runtime this factory
    /// cannot supply.
    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers>;
}

/// The provider factories a server builds definitions with.
///
/// Ordered, and consulted in order, so that a deployment which registers a
/// factory of its own gets a predictable answer. Claims are disjoint, so the
/// order only matters as a tiebreak that should never be needed.
pub struct Factories {
    factories: Vec<Arc<dyn ProviderFactory>>,
}

impl Factories {
    /// No factories at all: a server that builds nothing from anything.
    #[must_use]
    pub const fn new() -> Self {
        Self { factories: Vec::new() }
    }

    /// Every factory compiled into Conduit.
    ///
    /// The one list that grows when a vendor is added. OpenAI comes first
    /// because it is the vendor most deployments configure, not because
    /// anything depends on the order.
    #[must_use]
    pub fn builtin() -> Self {
        Self::new()
            .with(OpenAi)
            .with(Anthropic)
            .with(BedrockRuntime)
            .with(ElevenLabs)
            .with(Google)
            .with(MaryTtsServer)
            .with(Wyoming)
            .with(OpenWakeWord)
            .with(DeviceWake)
            .with(Mcp)
            .with(DiarizationServer)
            .with(HttpSpeaker)
            .with(BuiltinTransform)
            .with(ScriptedTransform)
    }

    /// Adds `factory` after the ones already registered.
    #[must_use]
    pub fn with(mut self, factory: impl ProviderFactory + 'static) -> Self {
        self.factories.push(Arc::new(factory));
        self
    }

    /// The registered factories' names, in order.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.factories.iter().map(|factory| factory.name()).collect()
    }

    /// Builds `definition` with whichever factory claims it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if no factory builds this definition, or
    /// whatever error the factory that claimed it raised.
    pub async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let factory = self
            .factories
            .iter()
            .find(|factory| factory.handles(definition))
            .ok_or_else(|| unbuildable(definition, &self.names()))?;
        factory.register(providers, definition).await
    }
}

impl Default for Factories {
    fn default() -> Self {
        Self::builtin()
    }
}

impl std::fmt::Debug for Factories {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Factories").field("factories", &self.names()).finish()
    }
}

/// Why a stored definition could not be built at all.
///
/// Names the factories that were asked, because the usual cause is a
/// definition written by a build that had a vendor this one does not.
fn unbuildable(definition: &ProviderDefinition, factories: &[&'static str]) -> Error {
    Error::Config(format!(
        "no provider factory builds `{id}`, a {capability:?} definition; asked: {factories}",
        id = definition.id,
        capability = definition.capability(),
        factories = factories.join(", "),
    ))
}

/// Why a factory refused a definition [`ProviderFactory::handles`] claimed.
///
/// A programmer error rather than a configuration one — the two must agree —
/// but reported rather than panicked, because a provider that will not build
/// is something a running server should say and keep serving the rest.
fn unclaimed(factory: &str, definition: &ProviderDefinition) -> Error {
    Error::Config(format!(
        "the `{factory}` provider factory was handed `{}`, which it does not build",
        definition.id
    ))
}

/// The credential a definition carries, if it is one this process can use.
///
/// An external reference is resolved by whatever manages it, and a redacted
/// one is what a read response carries rather than a value — neither is a
/// secret to send to a vendor.
fn secret_value(secret: &Option<ProviderSecret>) -> Option<String> {
    match secret {
        Some(ProviderSecret::Inline { value }) => Some(value.clone()),
        Some(ProviderSecret::External { .. } | ProviderSecret::Redacted) | None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_provider::storage::{
        LlmVariant, McpTransport, MicroWakeWordRuntime, ProviderDefinitionVariant,
        ScriptEngine, SpeakerEngine, SpeakerIdVariant, SttVariant, ToolVariant,
        TransformVariant, TtsVariant, WakeRuntime, WakeVariant,
    };
    use conduit_provider::testing::EchoStt;

    fn definition(variant: ProviderDefinitionVariant) -> ProviderDefinition {
        ProviderDefinition {
            id: "kitchen".to_owned(),
            label: "Kitchen".to_owned(),
            variant,
            settings: Default::default(),
        }
    }

    fn openai_llm() -> ProviderDefinitionVariant {
        ProviderDefinitionVariant::Llm {
            variant: LlmVariant::OpenAi {
                base_url: "https://api.openai.com/v1".to_owned(),
                api_key: Some(ProviderSecret::Inline { value: "sk-test".to_owned() }),
                models: vec!["gpt-4o".to_owned()],
                streaming: true,
                system_prompt: None,
            },
        }
    }

    fn anthropic_llm() -> ProviderDefinitionVariant {
        ProviderDefinitionVariant::Llm {
            variant: LlmVariant::Anthropic {
                base_url: "https://api.anthropic.com/v1".to_owned(),
                api_key: Some(ProviderSecret::Inline { value: "sk-ant-test".to_owned() }),
                models: vec!["claude-opus-5".to_owned()],
                streaming: true,
                system_prompt: None,
            },
        }
    }

    fn bedrock_llm() -> ProviderDefinitionVariant {
        ProviderDefinitionVariant::Llm {
            variant: LlmVariant::Bedrock {
                region: "us-west-2".to_owned(),
                profile: None,
                api_key: None,
                models: vec!["us.anthropic.claude-opus-4-5-20251101-v1:0".to_owned()],
                streaming: true,
                system_prompt: None,
            },
        }
    }

    /// One definition of every shape a factory dispatches on.
    ///
    /// Written out rather than derived because dispatch is what is under test:
    /// a shape this list forgets is a definition no factory would be shown to
    /// claim.
    fn every_variant() -> Vec<ProviderDefinitionVariant> {
        vec![
            openai_llm(),
            anthropic_llm(),
            bedrock_llm(),
            ProviderDefinitionVariant::Stt {
                variant: SttVariant::OpenAi {
                    base_url: "https://api.openai.com/v1".to_owned(),
                    model: "whisper-1".to_owned(),
                    api_key: None,
                    stream: false,
                },
            },
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::OpenAi {
                    base_url: "https://api.openai.com/v1".to_owned(),
                    model: "tts-1".to_owned(),
                    api_key: None,
                    voices: vec!["alloy".to_owned()],
                },
            },
            ProviderDefinitionVariant::Stt {
                variant: SttVariant::Wyoming {
                    url: "tcp://whisper:10300".to_owned(),
                    model: None,
                    streaming: false,
                },
            },
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::Wyoming {
                    url: "tcp://piper:10200".to_owned(),
                    voice: None,
                    streaming: false,
                },
            },
            ProviderDefinitionVariant::Wake {
                variant: WakeVariant::OpenWakeWord {
                    runtime: WakeRuntime::Wyoming {
                        url: "tcp://openwakeword:10400".to_owned(),
                        threshold_percent: 50,
                    },
                    phrases: vec!["okay nabu".to_owned()],
                },
            },
            ProviderDefinitionVariant::Wake {
                variant: WakeVariant::OpenWakeWord {
                    runtime: WakeRuntime::Local { models_dir: None, threshold_percent: 50 },
                    phrases: vec!["okay nabu".to_owned()],
                },
            },
            ProviderDefinitionVariant::Wake {
                variant: WakeVariant::NanoWakeWord {
                    runtime: WakeRuntime::Local { models_dir: None, threshold_percent: 50 },
                    phrases: vec!["okay nabu".to_owned()],
                },
            },
            ProviderDefinitionVariant::Wake {
                variant: WakeVariant::MicroWakeWord {
                    runtime: MicroWakeWordRuntime::Device,
                    phrases: vec!["okay nabu".to_owned()],
                },
            },
            ProviderDefinitionVariant::Tool {
                variant: ToolVariant::Mcp {
                    transport: McpTransport::Sse {
                        url: "https://tools.example/sse".to_owned(),
                    },
                },
            },
            ProviderDefinitionVariant::SpeakerId {
                variant: SpeakerIdVariant::DiarizationServer {
                    base_url: "https://voices.example".to_owned(),
                    threshold_percent: 70,
                },
            },
            ProviderDefinitionVariant::SpeakerId {
                variant: SpeakerIdVariant::Http {
                    base_url: "https://voices.example".to_owned(),
                    api_key: None,
                    engine: SpeakerEngine::SpeechBrain,
                    threshold_percent: 70,
                },
            },
            ProviderDefinitionVariant::Stt {
                variant: SttVariant::ElevenLabs { api_key: None, model: None },
            },
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::ElevenLabs { api_key: None, model: None, voice: None },
            },
            ProviderDefinitionVariant::Stt {
                variant: SttVariant::Google { language: None, model: None },
            },
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::Google { language: None, voice: None },
            },
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::MaryTts {
                    url: "http://marytts:59125".to_owned(),
                    voice: None,
                    locale: None,
                },
            },
            ProviderDefinitionVariant::Transform {
                variant: TransformVariant::Builtin { rules: Vec::new() },
            },
            ProviderDefinitionVariant::Transform {
                variant: TransformVariant::Script {
                    engine: ScriptEngine::Rhai,
                    source: "segment".to_owned(),
                    timeout_ms: 50,
                },
            },
        ]
    }

    /// A factory that claims everything and registers a stand-in, so that
    /// dispatch can be observed without reaching a vendor.
    struct Anything;

    #[async_trait::async_trait]
    impl ProviderFactory for Anything {
        fn name(&self) -> &'static str {
            "anything"
        }

        fn handles(&self, _definition: &ProviderDefinition) -> bool {
            true
        }

        async fn register(
            &self,
            providers: Providers,
            _definition: &ProviderDefinition,
        ) -> Result<Providers> {
            Ok(providers.with_stt(EchoStt))
        }
    }

    #[tokio::test]
    async fn a_registered_factory_builds_the_definitions_it_claims() {
        // The point of the trait: a vendor is added by registering a type,
        // rather than by editing whatever loads provider definitions.
        let providers = Factories::new()
            .with(Anything)
            .register(Providers::new(), &definition(openai_llm()))
            .await
            .expect("the factory claims it");

        assert_eq!(providers.stt().names().collect::<Vec<_>>(), ["echo-stt"]);
    }

    #[tokio::test]
    async fn a_definition_no_factory_builds_fails_loudly() {
        // Skipping it would leave a pipeline naming the provider to fail later
        // with an error about the graph rather than about the definition.
        let error = Factories::new()
            .register(Providers::new(), &definition(openai_llm()))
            .await
            .expect_err("nothing builds it");

        assert!(error.to_string().contains("kitchen"), "{error}");
    }

    #[test]
    fn every_stored_definition_is_built_by_exactly_one_factory() {
        // Two lists that have to agree: the variants an operator can store and
        // the factories compiled in. A variant nothing claims cannot be loaded
        // at all, and one two factories claim resolves by list order.
        let factories = Factories::builtin();
        for variant in every_variant() {
            let definition = definition(variant);
            let claimants: Vec<_> = factories
                .factories
                .iter()
                .filter(|factory| factory.handles(&definition))
                .map(|factory| factory.name())
                .collect();
            assert_eq!(claimants.len(), 1, "{claimants:?} claim {:?}", definition.variant);
        }
    }

    #[test]
    fn the_built_in_factories_are_listed_in_order() {
        assert_eq!(Factories::builtin().names().first(), Some(&"openai"));
    }

    #[tokio::test]
    async fn a_local_engine_conduit_cannot_score_says_so() {
        // Claimed rather than unrecognized: an operator who stored a
        // definition from an older build is told what is wrong with it.
        let error = Factories::builtin()
            .register(
                Providers::new(),
                &definition(ProviderDefinitionVariant::Wake {
                    variant: WakeVariant::NanoWakeWord {
                        runtime: WakeRuntime::Local {
                            models_dir: Some("/models".to_owned()),
                            threshold_percent: 50,
                        },
                        phrases: vec!["okay nabu".to_owned()],
                    },
                }),
            )
            .await
            .expect_err("nanoWakeWord does not detect in process");

        assert!(error.to_string().contains("nanowakeword"), "{error}");
    }

    #[tokio::test]
    async fn a_secret_only_travels_when_this_process_holds_it() {
        assert_eq!(
            secret_value(&Some(ProviderSecret::Inline { value: "sk-live".to_owned() })),
            Some("sk-live".to_owned())
        );
        assert_eq!(secret_value(&Some(ProviderSecret::Redacted)), None);
        assert_eq!(
            secret_value(&Some(ProviderSecret::External {
                reference: "vault://key".to_owned()
            })),
            None
        );
        assert_eq!(secret_value(&None), None);
    }
}
