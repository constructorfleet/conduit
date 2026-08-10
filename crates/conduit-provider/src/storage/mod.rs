//! Storage provider interface.
//!
//! Pipelines outlive the process that serves them, so where they live is a
//! deployment decision: a directory on a laptop, a shared database in a
//! cluster. The runtime only needs them to come back.

use conduit_core::graph::PipelineGraph;
use conduit_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub mod linked_service;
pub mod llm;
pub mod memory;
pub mod roster;
pub mod speaker;
pub mod stt;
pub mod tool;
pub mod transform;
pub mod tts;
pub mod vad;
pub mod wake;
pub mod wake_models;

pub use linked_service::{
    LinkedService, LinkedServiceKind, LinkedServicePanel, LinkedServiceStore,
};
pub use llm::LlmVariant;
pub use memory::MemoryVariant;
pub use roster::{EnrolledSpeaker, SpeakerRosterStore};
pub use speaker::SpeakerIdVariant;
pub use stt::SttVariant;
pub use tool::{McpTransport, ToolVariant};
pub use transform::{Rule, ScriptEngine, TransformVariant};
pub use tts::TtsVariant;
pub use vad::{default_silence_ms, VadVariant};
pub use wake::{MicroWakeWordRuntime, WakeRuntime, WakeVariant};

/// The longest a pipeline name may be.
const MAX_NAME: usize = 128;

/// Rejects names that are not safe to use as a storage key.
///
/// A name reaches this from a URL path, and a backend may turn it into a file
/// name — so `../../etc/passwd` must never get that far. Only characters that
/// are unambiguous in a path, a URL, and a SQL identifier are allowed.
///
/// # Errors
///
/// Returns [`Error::Config`] describing what is wrong with the name.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::Config("a pipeline name cannot be empty".to_owned()));
    }
    if name.len() > MAX_NAME {
        return Err(Error::Config(format!(
            "a pipeline name cannot be longer than {MAX_NAME} characters"
        )));
    }
    if let Some(bad) = name
        .chars()
        .find(|character| !matches!(character, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_'))
    {
        return Err(Error::Config(format!(
            "a pipeline name may only contain letters, digits, `-` and `_`; found `{bad}`"
        )));
    }
    Ok(())
}

/// Somewhere pipeline definitions are kept.
///
/// # Contract
///
/// Which backend a deployment uses is configuration, so it must not be
/// observable behaviour. Every implementation owes its callers all of this:
///
/// - **Names are validated on every method.** `list`, `get`, `put` and
///   `remove` all reject a name [`validate_name`] refuses. In particular
///   `get` and `remove` return [`Error::Config`] rather than `Ok(None)` /
///   `Ok(false)`: an unusable name is a malformed request, not a missing
///   pipeline, and reporting absence would tell the caller to go create one
///   under a name that can never be created. It must not depend on whether a
///   given backend happens to need the name to be safe — an in-memory map is
///   in no danger from `../../etc/passwd`, but it refuses it all the same, so
///   that a request accepted before a storage migration is accepted after it.
/// - **`list` only returns names `get` will answer for.** Storage outlives the
///   rule that governs it — a directory is editable by hand, a table is
///   writable by anything holding the credentials — so a name that predates
///   this contract may be sitting in it. Such entries are omitted rather than
///   returned as names that would then be rejected.
/// - **Absence is not failure.** `get` on a name that is merely not there is
///   `Ok(None)`, and `remove` on it is `Ok(false)`.
/// - **Unreadable is not absent.** A stored definition that will not decode is
///   an error, never `Ok(None)`: "it is not there" invites an editor to
///   overwrite something that is merely broken.
/// - **Round-tripping is lossless.** `get` after `put` returns an equal graph.
/// - **`put` reports replacement.** `true` when it overwrote an existing
///   pipeline, `false` when it created one.
///
/// The shared conformance suite in `conduit-store`
/// (`crates/conduit-store/tests/conformance/mod.rs`) is the executable form of
/// this list, and every backend is run through it.
///
/// [`Error::Config`]: conduit_core::Error::Config
#[async_trait::async_trait]
pub trait PipelineStore: Send + Sync + 'static {
    /// Names of every stored pipeline, sorted, excluding any that
    /// [`validate_name`] would refuse.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend is unavailable.
    async fn list(&self) -> Result<Vec<String>>;

    /// Fetches one pipeline, or `None` if there is no such name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if [`validate_name`] refuses `name`, or an
    /// error if the backend is unavailable or the stored definition cannot be
    /// read.
    async fn get(&self, name: &str) -> Result<Option<PipelineGraph>>;

    /// Stores a pipeline, returning `true` if it replaced an existing one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if [`validate_name`] refuses `name`, or an
    /// error if the write fails.
    async fn put(&self, name: &str, graph: PipelineGraph) -> Result<bool>;

    /// Removes a pipeline, returning `true` if it existed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if [`validate_name`] refuses `name`, or an
    /// error if the backend is unavailable.
    async fn remove(&self, name: &str) -> Result<bool>;
}

/// Runtime capability a provider definition supplies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCapability {
    /// Speech recognition.
    Stt,
    /// Language model reasoning.
    Llm,
    /// Speech synthesis.
    Tts,
    /// Rewriting an utterance before it is rendered.
    Transform,
    /// Tool invocation.
    Tool,
    /// Wake word detection.
    Wake,
    /// Speaker identification.
    SpeakerId,
    /// Telling speech from silence.
    Vad,
    /// Recalling what was said before.
    Memory,
}

/// The detector behind a wake word definition.
///
/// Named rather than inferred from the endpoint because the three engines
/// differ in what a phrase *is* — a microWakeWord model file, an openWakeWord
/// model name, a nanoWakeWord embedding — so an operator choosing one is
/// choosing which phrases they can ask for.
/// Each engine is written as the one word its project is named by, rather than
/// as the `snake_case` its variant would produce: an operator reading a stored
/// definition should see `openwakeword`, which is what they installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WakeEngine {
    /// microWakeWord: small models built for microcontrollers, which is why it
    /// is also the engine an ESP32 satellite runs on-device.
    #[serde(rename = "microwakeword")]
    MicroWakeWord,
    /// openWakeWord: the general-purpose detector Home Assistant ships.
    #[serde(rename = "openwakeword")]
    OpenWakeWord,
    /// nanoWakeWord: openWakeWord's lighter successor, same model vocabulary.
    #[serde(rename = "nanowakeword")]
    NanoWakeWord,
}

impl WakeEngine {
    /// The word this engine is written as in a definition.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::MicroWakeWord => "microwakeword",
            Self::OpenWakeWord => "openwakeword",
            Self::NanoWakeWord => "nanowakeword",
        }
    }
}

/// The service behind a speaker identification definition.
///
/// All three speak the same HTTP contract — enroll, identify, forget — and
/// differ in the embedding model behind it. Naming the engine is what lets a
/// diagnostic say whose voice print an operator is looking at.
/// Written as each project names itself, for the same reason [`WakeEngine`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpeakerEngine {
    /// SpeechBrain ECAPA-TDNN embeddings.
    SpeechBrain,
    /// Resemblyzer d-vector embeddings.
    Resemblyzer,
    /// pyannote speaker embeddings.
    Pyannote,
}

impl SpeakerEngine {
    /// The word this engine is written as in a definition.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SpeechBrain => "speechbrain",
            Self::Resemblyzer => "resemblyzer",
            Self::Pyannote => "pyannote",
        }
    }
}

/// The confidence a detection or a match must reach, as a percentage.
///
/// A percentage rather than the `0.0..=1.0` float the provider traits use, so
/// that a definition stays comparable by value: an operator screen diffs two
/// definitions, and two floats that are equal to the eye are not always equal
/// to the machine.
pub const DEFAULT_THRESHOLD_PERCENT: u8 = 50;

/// The default acceptance threshold, as a serde default.
pub(crate) const fn default_threshold_percent() -> u8 {
    DEFAULT_THRESHOLD_PERCENT
}

/// A credential stored with a provider definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderSecret {
    /// An inline secret value supplied by the operator.
    Inline {
        /// The raw secret value. Never serialize this into read responses.
        value: String,
    },
    /// A reference to an external secret managed outside Conduit.
    External {
        /// The external secret reference.
        reference: String,
    },
    /// Placeholder used by update/read APIs.
    Redacted,
}

/// Server-owned provider configuration.
///
/// `Eq` is not derived because [`settings`](ProviderDefinition::settings) is a
/// `serde_json::Value`, which is only `PartialEq` — two definitions still
/// compare, but not by an equivalence relation `Eq` would promise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderDefinition {
    /// Stable provider id referenced by pipeline graph nodes.
    pub id: String,
    /// Human-readable label for operator screens.
    pub label: String,
    /// Typed provider-specific settings.
    pub variant: ProviderDefinitionVariant,
    /// Default request settings this configured provider carries.
    ///
    /// The reusable settings an operator sets once on the Configured Provider
    /// — sampling controls, model options — rather than on every pipeline that
    /// names it. Distinct from [`variant`](ProviderDefinition::variant), which
    /// is the connection: where the provider is and how to authenticate to it.
    /// These are the request-time options a provider declares through its
    /// [`Descriptor`](crate::Descriptor)'s settings schema, and they are
    /// checked against that schema before a definition is accepted.
    ///
    /// Absent in storage written before this field existed, and omitted from
    /// serialization when empty, so a definition that sets none is byte-for-byte
    /// what it was.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub settings: Map<String, Value>,
}

impl ProviderDefinition {
    /// Runtime capability this definition supplies.
    #[must_use]
    pub const fn capability(&self) -> ProviderCapability {
        self.variant.capability()
    }

    /// Returns a copy suitable for API reads, with inline secrets redacted.
    #[must_use]
    pub fn redacted(&self) -> Self {
        Self {
            id: self.id.clone(),
            label: self.label.clone(),
            variant: self.variant.redacted(),
            // Request settings are not secret: they are sampling controls and
            // model options, and an operator screen needs to read them back.
            settings: self.settings.clone(),
        }
    }

    /// Applies update secret semantics against an existing definition.
    #[must_use]
    pub fn with_secret_updates_from(mut self, existing: Option<&Self>) -> Self {
        self.variant = self
            .variant
            .with_secret_updates_from(existing.map(|definition| &definition.variant));
        self
    }
}

/// Closed set of provider definition variants, grouped by the capability the
/// variant supplies.
///
/// The type is two levels deep on purpose. Capability and provider are
/// independent axes — OpenAI ships an LLM, an STT, and a TTS — and flattening
/// them, as `openai_llm`, `openai_stt`, `openai_tts`, put every provider in one
/// namespace and made the capability a hand-written classification. Grouping by
/// capability makes [`ProviderDefinitionVariant::capability`] structural: one
/// arm per group, so a new provider under an existing capability can never
/// mislabel what it does.
///
/// On the wire the outer `type` is the capability and the inner `type` is the
/// provider, so `{"type":"llm","variant":{"type":"openai",...}}`. Stored
/// definitions written before this grouping used a single flat tag
/// (`{"type":"openai_llm",...}`); they still read, via the legacy
/// [`Deserialize`] fallback, and saving a definition rewrites them in the
/// two-level shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderDefinitionVariant {
    /// Language model reasoning.
    Llm {
        /// Provider-specific settings.
        variant: LlmVariant,
    },
    /// Speech recognition.
    Stt {
        /// Provider-specific settings.
        variant: SttVariant,
    },
    /// Speech synthesis.
    Tts {
        /// Provider-specific settings.
        variant: TtsVariant,
    },
    /// Rewriting an utterance before it is rendered.
    Transform {
        /// Provider-specific settings.
        variant: TransformVariant,
    },
    /// Tool invocation.
    Tool {
        /// Provider-specific settings.
        variant: ToolVariant,
    },
    /// Wake word detection.
    Wake {
        /// Provider-specific settings.
        variant: WakeVariant,
    },
    /// Speaker identification.
    SpeakerId {
        /// Provider-specific settings.
        variant: SpeakerIdVariant,
    },
    /// Telling speech from silence.
    Vad {
        /// Provider-specific settings.
        variant: VadVariant,
    },
    /// Recalling what was said before.
    Memory {
        /// Provider-specific settings.
        variant: MemoryVariant,
    },
}

impl ProviderDefinitionVariant {
    /// Runtime capability this variant supplies.
    #[must_use]
    pub const fn capability(&self) -> ProviderCapability {
        match self {
            Self::Llm { .. } => ProviderCapability::Llm,
            Self::Stt { .. } => ProviderCapability::Stt,
            Self::Tts { .. } => ProviderCapability::Tts,
            Self::Transform { .. } => ProviderCapability::Transform,
            Self::Tool { .. } => ProviderCapability::Tool,
            Self::Wake { .. } => ProviderCapability::Wake,
            Self::SpeakerId { .. } => ProviderCapability::SpeakerId,
            Self::Vad { .. } => ProviderCapability::Vad,
            Self::Memory { .. } => ProviderCapability::Memory,
        }
    }

    /// Returns a copy with inline secrets redacted.
    #[must_use]
    fn redacted(&self) -> Self {
        match self {
            Self::Llm { variant } => Self::Llm { variant: variant.redacted() },
            Self::Stt { variant } => Self::Stt { variant: variant.redacted() },
            Self::Tts { variant } => Self::Tts { variant: variant.redacted() },
            Self::Transform { variant } => Self::Transform { variant: variant.redacted() },
            Self::Tool { variant } => Self::Tool { variant: variant.redacted() },
            Self::Wake { variant } => Self::Wake { variant: variant.redacted() },
            Self::SpeakerId { variant } => Self::SpeakerId { variant: variant.redacted() },
            Self::Vad { variant } => Self::Vad { variant: variant.redacted() },
            Self::Memory { variant } => Self::Memory { variant: variant.redacted() },
        }
    }

    /// Applies update secret semantics against an existing definition.
    ///
    /// Written against the credential slot rather than per variant: every
    /// keyed variant merges its key the same way, and spelling that out arm by
    /// arm meant a new keyed variant silently kept whatever the update sent —
    /// including the `Redacted` placeholder a read response hands back.
    #[must_use]
    fn with_secret_updates_from(mut self, existing: Option<&Self>) -> Self {
        let existing_key = existing.and_then(Self::api_key).cloned();
        if let Some(slot) = self.api_key_mut() {
            *slot = merge_secret(slot.take(), existing_key.as_ref());
        }
        self
    }

    /// The inline-or-external secret a keyed provider carries, if any.
    fn api_key(&self) -> Option<&ProviderSecret> {
        match self {
            Self::Llm {
                variant:
                    LlmVariant::OpenAi { api_key, .. }
                    | LlmVariant::Anthropic { api_key, .. }
                    | LlmVariant::Bedrock { api_key, .. },
            }
            | Self::Stt {
                variant:
                    SttVariant::OpenAi { api_key, .. } | SttVariant::ElevenLabs { api_key, .. },
            }
            | Self::Tts {
                variant:
                    TtsVariant::OpenAi { api_key, .. }
                    | TtsVariant::ElevenLabs { api_key, .. }
                    | TtsVariant::Deepgram { api_key, .. },
            }
            | Self::SpeakerId { variant: SpeakerIdVariant::Http { api_key, .. } }
            | Self::Memory { variant: MemoryVariant::PgVector { api_key, .. } } => {
                api_key.as_ref()
            }
            _ => None,
        }
    }

    /// The credential slot a keyed provider carries, for rewriting in place.
    fn api_key_mut(&mut self) -> Option<&mut Option<ProviderSecret>> {
        match self {
            Self::Llm {
                variant:
                    LlmVariant::OpenAi { api_key, .. }
                    | LlmVariant::Anthropic { api_key, .. }
                    | LlmVariant::Bedrock { api_key, .. },
            }
            | Self::Stt {
                variant:
                    SttVariant::OpenAi { api_key, .. } | SttVariant::ElevenLabs { api_key, .. },
            }
            | Self::Tts {
                variant:
                    TtsVariant::OpenAi { api_key, .. }
                    | TtsVariant::ElevenLabs { api_key, .. }
                    | TtsVariant::Deepgram { api_key, .. },
            }
            | Self::SpeakerId { variant: SpeakerIdVariant::Http { api_key, .. } }
            | Self::Memory { variant: MemoryVariant::PgVector { api_key, .. } } => {
                Some(api_key)
            }
            _ => None,
        }
    }
}

/// The two-level shape as a wire-only mirror.
///
/// [`ProviderDefinitionVariant`] derives [`Serialize`] but implements
/// [`Deserialize`] by hand so that legacy flat definitions keep reading; a
/// mirror is how that hand-written [`Deserialize`] reaches the derived shape
/// without the two implementations living in one type.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireProviderDefinitionVariant {
    Llm { variant: LlmVariant },
    Stt { variant: SttVariant },
    Tts { variant: TtsVariant },
    Transform { variant: TransformVariant },
    Tool { variant: ToolVariant },
    Wake { variant: WakeVariant },
    SpeakerId { variant: SpeakerIdVariant },
    Memory { variant: MemoryVariant },
}

impl From<WireProviderDefinitionVariant> for ProviderDefinitionVariant {
    fn from(wire: WireProviderDefinitionVariant) -> Self {
        match wire {
            WireProviderDefinitionVariant::Llm { variant } => Self::Llm { variant },
            WireProviderDefinitionVariant::Stt { variant } => Self::Stt { variant },
            WireProviderDefinitionVariant::Tts { variant } => Self::Tts { variant },
            WireProviderDefinitionVariant::Transform { variant } => Self::Transform { variant },
            WireProviderDefinitionVariant::Tool { variant } => Self::Tool { variant },
            WireProviderDefinitionVariant::Wake { variant } => Self::Wake { variant },
            WireProviderDefinitionVariant::SpeakerId { variant } => Self::SpeakerId { variant },
            WireProviderDefinitionVariant::Memory { variant } => Self::Memory { variant },
        }
    }
}

/// The flat shape provider definitions were written in before variants were
/// grouped by capability.
///
/// Storage outlives the rule that wrote it, so a definition saved by an older
/// release is not something to refuse: it describes a provider an operator
/// configured, and it must keep working. Reads therefore accept both shapes;
/// writes always emit the two-level one, so saving a definition upgrades its
/// stored record in place.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LegacyProviderDefinitionVariant {
    #[serde(rename = "openai_llm")]
    OpenAiLlm {
        base_url: String,
        #[serde(default)]
        api_key: Option<ProviderSecret>,
        #[serde(default)]
        models: Vec<String>,
        #[serde(default)]
        streaming: bool,
        #[serde(default)]
        system_prompt: Option<String>,
    },
    #[serde(rename = "openai_stt")]
    OpenAiStt {
        base_url: String,
        model: String,
        #[serde(default)]
        api_key: Option<ProviderSecret>,
        #[serde(default)]
        stream: bool,
    },
    #[serde(rename = "openai_tts")]
    OpenAiTts {
        base_url: String,
        model: String,
        #[serde(default)]
        api_key: Option<ProviderSecret>,
        #[serde(default)]
        voices: Vec<String>,
    },
    #[serde(rename = "wyoming_stt")]
    WyomingStt {
        url: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        streaming: bool,
    },
    #[serde(rename = "wyoming_tts")]
    WyomingTts {
        url: String,
        #[serde(default)]
        voice: Option<String>,
        #[serde(default)]
        streaming: bool,
    },
    #[serde(rename = "mcp_tool")]
    McpTool { transport: McpTransport },
    #[serde(rename = "wyoming_wake")]
    WyomingWake {
        url: String,
        engine: WakeEngine,
        #[serde(default)]
        phrases: Vec<String>,
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
    },
    #[serde(rename = "device_wake")]
    DeviceWake {
        engine: WakeEngine,
        #[serde(default)]
        phrases: Vec<String>,
    },
    #[serde(rename = "diarization_server_speaker_id")]
    DiarizationServerSpeakerId {
        base_url: String,
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
    },
    #[serde(rename = "http_speaker_id")]
    HttpSpeakerId {
        base_url: String,
        #[serde(default)]
        api_key: Option<ProviderSecret>,
        engine: SpeakerEngine,
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
    },
}

impl From<LegacyProviderDefinitionVariant> for ProviderDefinitionVariant {
    fn from(legacy: LegacyProviderDefinitionVariant) -> Self {
        match legacy {
            LegacyProviderDefinitionVariant::OpenAiLlm {
                base_url,
                api_key,
                models,
                streaming,
                system_prompt,
            } => Self::Llm {
                variant: LlmVariant::OpenAi {
                    base_url,
                    api_key,
                    models,
                    streaming,
                    system_prompt,
                },
            },
            LegacyProviderDefinitionVariant::OpenAiStt { base_url, model, api_key, stream } => {
                Self::Stt { variant: SttVariant::OpenAi { base_url, model, api_key, stream } }
            }
            LegacyProviderDefinitionVariant::OpenAiTts { base_url, model, api_key, voices } => {
                Self::Tts { variant: TtsVariant::OpenAi { base_url, model, api_key, voices } }
            }
            LegacyProviderDefinitionVariant::WyomingStt { url, model, streaming } => {
                Self::Stt { variant: SttVariant::Wyoming { url, model, streaming } }
            }
            LegacyProviderDefinitionVariant::WyomingTts { url, voice, streaming } => {
                Self::Tts { variant: TtsVariant::Wyoming { url, voice, streaming } }
            }
            LegacyProviderDefinitionVariant::McpTool { transport } => {
                Self::Tool { variant: ToolVariant::Mcp { transport } }
            }
            LegacyProviderDefinitionVariant::WyomingWake {
                url,
                engine,
                phrases,
                threshold_percent,
            } => Self::Wake {
                variant: WakeVariant::from_engine_on_wyoming(
                    engine,
                    url,
                    phrases,
                    threshold_percent,
                ),
            },
            LegacyProviderDefinitionVariant::DeviceWake { engine, phrases } => {
                Self::Wake { variant: WakeVariant::from_engine_on_device(engine, phrases) }
            }
            LegacyProviderDefinitionVariant::DiarizationServerSpeakerId {
                base_url,
                threshold_percent,
            } => Self::SpeakerId {
                variant: SpeakerIdVariant::DiarizationServer { base_url, threshold_percent },
            },
            LegacyProviderDefinitionVariant::HttpSpeakerId {
                base_url,
                api_key,
                engine,
                threshold_percent,
            } => Self::SpeakerId {
                variant: SpeakerIdVariant::Http {
                    base_url,
                    api_key,
                    engine,
                    threshold_percent,
                },
            },
        }
    }
}

impl<'de> Deserialize<'de> for ProviderDefinitionVariant {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match serde_json::from_value::<WireProviderDefinitionVariant>(value.clone()) {
            Ok(wire) => Ok(wire.into()),
            Err(wire_error) => serde_json::from_value::<LegacyProviderDefinitionVariant>(value)
                .map(ProviderDefinitionVariant::from)
                .map_err(|legacy_error| {
                    serde::de::Error::custom(format!(
                        "provider definition variant matches neither the two-level shape \
                             ({wire_error}) nor the legacy flat shape ({legacy_error})"
                    ))
                }),
        }
    }
}

fn redact_secret(secret: &Option<ProviderSecret>) -> Option<ProviderSecret> {
    secret.as_ref().map(|secret| match secret {
        ProviderSecret::Inline { .. } => ProviderSecret::Redacted,
        ProviderSecret::External { reference } => {
            ProviderSecret::External { reference: reference.clone() }
        }
        ProviderSecret::Redacted => ProviderSecret::Redacted,
    })
}

fn merge_secret(
    update: Option<ProviderSecret>,
    existing: Option<&ProviderSecret>,
) -> Option<ProviderSecret> {
    match update {
        Some(ProviderSecret::Redacted) => existing.cloned(),
        Some(ProviderSecret::Inline { value }) if value.is_empty() => None,
        Some(ProviderSecret::External { reference }) if reference.is_empty() => None,
        Some(other) => Some(other),
        None => None,
    }
}

/// Somewhere provider definitions are kept.
#[async_trait::async_trait]
pub trait ProviderDefinitionStore: Send + Sync + 'static {
    /// Provider ids in stable order.
    async fn list(&self) -> Result<Vec<String>>;

    /// Fetches one provider definition.
    async fn get(&self, id: &str) -> Result<Option<ProviderDefinition>>;

    /// Stores a provider definition, returning whether it replaced one.
    async fn put(&self, id: &str, definition: ProviderDefinition) -> Result<bool>;

    /// Removes a provider definition, returning whether it existed.
    async fn remove(&self, id: &str) -> Result<bool>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_are_accepted() {
        for name in ["kitchen", "living-room", "desk_2", "A1"] {
            validate_name(name).unwrap_or_else(|error| panic!("{name} rejected: {error}"));
        }
    }

    #[test]
    fn path_traversal_is_rejected() {
        // This is the whole point: a name becomes a file name in some backends.
        for name in ["../etc/passwd", "..", "a/b", "a\\b", "a\0b"] {
            assert!(validate_name(name).is_err(), "{name} should be rejected");
        }
    }

    #[test]
    fn an_empty_name_is_rejected() {
        assert!(validate_name("").is_err());
    }

    #[test]
    fn an_overlong_name_is_rejected() {
        assert!(validate_name(&"a".repeat(MAX_NAME + 1)).is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME)).is_ok());
    }

    #[test]
    fn an_engines_name_is_the_word_it_is_written_as_on_the_wire() {
        for engine in
            [WakeEngine::MicroWakeWord, WakeEngine::OpenWakeWord, WakeEngine::NanoWakeWord]
        {
            let written = serde_json::to_value(engine).expect("serialize");
            assert_eq!(written, serde_json::Value::String(engine.name().to_owned()));
        }
        for engine in
            [SpeakerEngine::SpeechBrain, SpeakerEngine::Resemblyzer, SpeakerEngine::Pyannote]
        {
            let written = serde_json::to_value(engine).expect("serialize");
            assert_eq!(written, serde_json::Value::String(engine.name().to_owned()));
        }
    }

    #[test]
    fn a_legacy_flat_definition_reads_as_the_new_shape() {
        let variant: ProviderDefinitionVariant = serde_json::from_value(serde_json::json!({
            "type": "openai_llm",
            "base_url": "https://api.openai.example/v1",
            "api_key": { "type": "inline", "value": "sk-test" },
            "models": ["gpt-5"],
            "streaming": true,
            "system_prompt": "Be terse.",
        }))
        .expect("legacy flat shape still reads");

        assert_eq!(
            variant,
            ProviderDefinitionVariant::Llm {
                variant: LlmVariant::OpenAi {
                    base_url: "https://api.openai.example/v1".to_owned(),
                    api_key: Some(ProviderSecret::Inline { value: "sk-test".to_owned() }),
                    models: vec!["gpt-5".to_owned()],
                    streaming: true,
                    system_prompt: Some("Be terse.".to_owned()),
                }
            }
        );
    }

    #[test]
    fn a_legacy_wake_definition_still_reads_its_threshold_default() {
        let variant: ProviderDefinitionVariant = serde_json::from_value(serde_json::json!({
            "type": "wyoming_wake",
            "url": "tcp://openwakeword:10400",
            "engine": "openwakeword",
        }))
        .expect("legacy flat wake shape still reads");

        let ProviderDefinitionVariant::Wake { variant } = variant else {
            panic!("a legacy `wyoming_wake` reads as a wake definition");
        };
        assert_eq!(variant.engine(), WakeEngine::OpenWakeWord, "the engine it named");
        assert_eq!(variant.wyoming_url(), Some("tcp://openwakeword:10400"));
        assert_eq!(variant.threshold_percent(), Some(DEFAULT_THRESHOLD_PERCENT));
        assert!(
            variant.phrases().is_empty(),
            "no phrases named means whatever the server loaded"
        );
    }

    #[test]
    fn a_definition_serializes_as_the_two_level_shape_and_round_trips() {
        let variant = ProviderDefinitionVariant::Stt {
            variant: SttVariant::OpenAi {
                base_url: "https://api.openai.example/v1".to_owned(),
                model: "whisper-1".to_owned(),
                api_key: None,
                stream: true,
            },
        };

        let value = serde_json::to_value(&variant).expect("serialize");
        assert_eq!(
            value,
            serde_json::json!({
                "type": "stt",
                "variant": {
                    "type": "openai",
                    "base_url": "https://api.openai.example/v1",
                    "model": "whisper-1",
                    "stream": true,
                },
            })
        );

        let back: ProviderDefinitionVariant =
            serde_json::from_value(value).expect("deserialize");
        assert_eq!(back, variant, "the two-level shape round-trips");
    }

    #[test]
    fn the_error_names_the_offending_character() {
        let error = validate_name("kitchen light").expect_err("spaces are not allowed");
        assert!(error.to_string().contains('`'), "{error}");
    }

    fn openai_llm_definition() -> ProviderDefinition {
        ProviderDefinition {
            id: "cloud".to_owned(),
            label: "Cloud".to_owned(),
            variant: ProviderDefinitionVariant::Llm {
                variant: LlmVariant::OpenAi {
                    base_url: "https://api.openai.example/v1".to_owned(),
                    api_key: None,
                    models: Vec::new(),
                    streaming: false,
                    system_prompt: None,
                },
            },
            settings: Map::new(),
        }
    }

    #[test]
    fn a_definition_without_settings_reads_and_writes_without_the_field() {
        // Storage written before this field existed has no `settings` key, and a
        // definition that sets none must serialize the same way, so nothing
        // already stored changes shape underneath an operator.
        let value = serde_json::json!({
            "id": "cloud",
            "label": "Cloud",
            "variant": { "type": "llm", "variant": { "type": "openai",
                "base_url": "https://api.openai.example/v1" } },
        });
        let definition: ProviderDefinition =
            serde_json::from_value(value).expect("no settings key still reads");
        assert!(definition.settings.is_empty());

        let written = serde_json::to_value(&definition).expect("serialize");
        assert!(
            written.get("settings").is_none(),
            "an empty settings map is omitted rather than written as `{{}}`: {written}"
        );
    }

    #[test]
    fn stored_settings_round_trip() {
        let mut definition = openai_llm_definition();
        definition.settings.insert("top_p".to_owned(), serde_json::json!(0.2));

        let back: ProviderDefinition =
            serde_json::from_value(serde_json::to_value(&definition).expect("serialize"))
                .expect("deserialize");
        assert_eq!(back, definition);
        assert_eq!(back.settings.get("top_p"), Some(&serde_json::json!(0.2)));
    }

    #[test]
    fn every_keyed_variant_keeps_its_existing_key_on_a_redacted_update() {
        // `api_key` falls through to `None` for variants it does not name, so a
        // keyed variant that is not listed there loses its credential the first
        // time an operator saves the form back without retyping it. That is
        // silent, so it is asserted for each keyed variant rather than trusted.
        let keyed: Vec<ProviderDefinitionVariant> = vec![
            ProviderDefinitionVariant::Llm {
                variant: LlmVariant::OpenAi {
                    base_url: "https://api.openai.example/v1".to_owned(),
                    api_key: None,
                    models: Vec::new(),
                    streaming: false,
                    system_prompt: None,
                },
            },
            ProviderDefinitionVariant::Llm {
                variant: LlmVariant::Anthropic {
                    base_url: "https://api.anthropic.com/v1".to_owned(),
                    api_key: None,
                    models: Vec::new(),
                    streaming: false,
                    system_prompt: None,
                },
            },
            ProviderDefinitionVariant::Llm {
                variant: LlmVariant::Bedrock {
                    region: "us-west-2".to_owned(),
                    profile: None,
                    api_key: None,
                    models: Vec::new(),
                    streaming: false,
                    system_prompt: None,
                },
            },
            ProviderDefinitionVariant::Stt {
                variant: SttVariant::OpenAi {
                    base_url: "https://api.openai.example/v1".to_owned(),
                    model: "whisper-1".to_owned(),
                    api_key: None,
                    stream: false,
                },
            },
            ProviderDefinitionVariant::Stt {
                variant: SttVariant::ElevenLabs { api_key: None, model: None },
            },
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::OpenAi {
                    base_url: "https://api.openai.example/v1".to_owned(),
                    model: "tts-1".to_owned(),
                    api_key: None,
                    voices: Vec::new(),
                },
            },
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::ElevenLabs { api_key: None, model: None, voice: None },
            },
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::Deepgram { api_key: None, model: None },
            },
            ProviderDefinitionVariant::SpeakerId {
                variant: SpeakerIdVariant::Http {
                    base_url: "https://speakers.example".to_owned(),
                    api_key: None,
                    engine: SpeakerEngine::SpeechBrain,
                    threshold_percent: default_threshold_percent(),
                },
            },
            ProviderDefinitionVariant::Memory {
                variant: MemoryVariant::PgVector {
                    url: "postgres://localhost/conduit".to_owned(),
                    embedding_base_url: "https://api.openai.com/v1".to_owned(),
                    api_key: None,
                    embedding_model: "text-embedding-3-small".to_owned(),
                    dimensions: 1536,
                },
            },
        ];

        for variant in keyed {
            let stored =
                with_key(&variant, Some(ProviderSecret::Inline { value: "k".to_owned() }));
            let update = with_key(&variant, Some(ProviderSecret::Redacted));

            let merged = update.with_secret_updates_from(Some(&stored));
            assert_eq!(
                merged.api_key(),
                Some(&ProviderSecret::Inline { value: "k".to_owned() }),
                // Named by the whole variant, not by its capability: two keyed
                // variants can share a capability, and `Tts` alone would not say
                // which of them lost its key.
                "a redacted update keeps the stored key for {variant:?}"
            );
        }
    }

    #[test]
    fn a_keyless_vendor_carries_no_credential_slot_to_lose() {
        // The other side of the test above. Polly and Google authenticate from
        // the host rather than from a stored key, so they are deliberately absent
        // from the `api_key` arms — and this asserts that absence is the intended
        // one. Were a slot ever added to either variant without being added to
        // those arms, the test above would not notice, because it enumerates by
        // hand; this one fails instead.
        let keyless = [
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::Polly {
                    region: "us-east-1".to_owned(),
                    profile: None,
                    voice: None,
                    engine: None,
                },
            },
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::Google { language: None, voice: None },
            },
        ];

        for variant in keyless {
            assert!(
                variant.api_key().is_none(),
                "{variant:?} authenticates from the host, so it has no key to store"
            );
            // And a redacted update cannot damage what is not there: the merge is
            // a no-op rather than a write of the placeholder.
            let merged = variant.clone().with_secret_updates_from(Some(&variant));
            assert_eq!(merged, variant);
        }
    }

    /// Rewrites the credential on a keyed variant, whatever variant it is.
    fn with_key(
        variant: &ProviderDefinitionVariant,
        key: Option<ProviderSecret>,
    ) -> ProviderDefinitionVariant {
        let mut value = serde_json::to_value(variant).expect("serialize");
        let inner = value
            .get_mut("variant")
            .and_then(Value::as_object_mut)
            .expect("a two-level variant has an inner object");
        match key {
            Some(key) => {
                inner.insert(
                    "api_key".to_owned(),
                    serde_json::to_value(key).expect("serialize"),
                );
            }
            None => {
                inner.remove("api_key");
            }
        }
        serde_json::from_value(value).expect("deserialize")
    }

    #[test]
    fn redaction_preserves_request_settings() {
        // Settings are not secret, so a read response still carries them; only
        // the credential in the variant is hidden.
        let mut definition = openai_llm_definition();
        definition.settings.insert("top_p".to_owned(), serde_json::json!(0.2));

        assert_eq!(definition.redacted().settings, definition.settings);
    }
}
