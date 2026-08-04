//! What a server builds from the provider definitions it has stored.

use conduit_api::factory::Factories;
use conduit_api::AppState;
use conduit_core::bus::EventBus;
use conduit_provider::storage::{
    LlmVariant, MicroWakeWordRuntime, ProviderDefinition, ProviderDefinitionVariant,
    ProviderSecret, SttVariant, TransformVariant, WakeVariant,
};

fn definition(id: &str, variant: ProviderDefinitionVariant) -> ProviderDefinition {
    ProviderDefinition {
        id: id.to_owned(),
        label: format!("{id} label"),
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

#[tokio::test]
async fn vendors_coexist_in_one_server_under_distinct_names() {
    // Four factories, four capabilities, one deployment: what a definition is
    // built by follows from the definition, so configuring a second vendor
    // takes nothing but storing it.
    let state = AppState::new(EventBus::new(16));
    for (id, variant) in [
        ("cloud", openai_llm()),
        (
            "whisper",
            ProviderDefinitionVariant::Stt {
                variant: SttVariant::Wyoming {
                    url: "tcp://whisper:10300".to_owned(),
                    model: None,
                    streaming: false,
                },
            },
        ),
        (
            "satellite",
            ProviderDefinitionVariant::Wake {
                variant: WakeVariant::MicroWakeWord {
                    runtime: MicroWakeWordRuntime::Device,
                    phrases: vec!["okay nabu".to_owned()],
                },
            },
        ),
        (
            "tidy",
            ProviderDefinitionVariant::Transform {
                variant: TransformVariant::Builtin { rules: Vec::new() },
            },
        ),
    ] {
        state
            .put_provider_definition(id, definition(id, variant))
            .await
            .expect("a definition every build has a factory for");
    }

    let providers = state.providers().expect("a snapshot was built");
    assert_eq!(providers.llm().names().collect::<Vec<_>>(), ["cloud"]);
    assert_eq!(providers.stt().names().collect::<Vec<_>>(), ["whisper"]);
    assert_eq!(providers.wake().names().collect::<Vec<_>>(), ["satellite"]);
    assert_eq!(providers.transform().names().collect::<Vec<_>>(), ["tidy"]);
}

#[tokio::test]
async fn default_settings_the_provider_accepts_are_stored() {
    // The reusable half of a Configured Provider: settings an operator sets once
    // on the definition, checked against what the provider says it accepts, and
    // read back intact.
    let state = AppState::new(EventBus::new(16));
    let mut definition = definition("cloud", openai_llm());
    definition.settings.insert("top_p".to_owned(), serde_json::json!(0.2));

    state
        .put_provider_definition("cloud", definition)
        .await
        .expect("settings the schema accepts are stored");

    let stored = state
        .provider_definition("cloud")
        .await
        .expect("read")
        .expect("the definition is there");
    assert_eq!(stored.settings.get("top_p"), Some(&serde_json::json!(0.2)));
}

#[tokio::test]
async fn default_settings_the_provider_schema_rejects_fail_the_write() {
    // The point of validating against the descriptor: a setting the provider
    // never had used to travel to a request and be ignored. `top_p` is bounded
    // at 1.0, so 5.0 is refused, and the definition is named.
    let state = AppState::new(EventBus::new(16));
    let mut definition = definition("cloud", openai_llm());
    definition.settings.insert("top_p".to_owned(), serde_json::json!(5.0));

    let error = state
        .put_provider_definition("cloud", definition)
        .await
        .expect_err("out of the schema's bounds");

    assert!(error.to_string().contains("top_p"), "{error}");
}

#[tokio::test]
async fn a_default_setting_the_provider_never_declared_fails_the_write() {
    // A typo — `top-p` for `top_p` — is a mistake to report, not a value to
    // carry silently, so an unknown setting is refused too.
    let state = AppState::new(EventBus::new(16));
    let mut definition = definition("cloud", openai_llm());
    definition.settings.insert("top-p".to_owned(), serde_json::json!(0.2));

    let error = state
        .put_provider_definition("cloud", definition)
        .await
        .expect_err("an unknown setting");

    assert!(error.to_string().contains("top-p"), "{error}");
}

#[tokio::test]
async fn a_definition_no_factory_builds_fails_the_write() {
    // A server whose vendors are narrower than what is stored says so at the
    // write that discovered it, rather than serving a registry quietly missing
    // a provider an operator configured.
    let state = AppState::new(EventBus::new(16)).with_factories(Factories::new());

    let error = state
        .put_provider_definition("cloud", definition("cloud", openai_llm()))
        .await
        .expect_err("no factory builds it");

    assert!(error.to_string().contains("cloud"), "{error}");
}
