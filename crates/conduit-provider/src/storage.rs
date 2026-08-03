//! Storage provider interface.
//!
//! Pipelines outlive the process that serves them, so where they live is a
//! deployment decision: a directory on a laptop, a shared database in a
//! cluster. The runtime only needs them to come back.

use conduit_core::graph::PipelineGraph;
use conduit_core::{Error, Result};
use serde::{Deserialize, Serialize};

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
    /// Tool invocation.
    Tool,
    /// Wake word detection.
    Wake,
    /// Speaker identification.
    SpeakerId,
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

    /// Whether this engine can run on satellite hardware.
    ///
    /// Only microWakeWord is small enough for an ESP32, so a `device_wake`
    /// definition naming either of the others describes a detector the
    /// satellite cannot load.
    #[must_use]
    pub const fn runs_on_device(self) -> bool {
        matches!(self, Self::MicroWakeWord)
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
const fn default_threshold_percent() -> u8 {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDefinition {
    /// Stable provider id referenced by pipeline graph nodes.
    pub id: String,
    /// Human-readable label for operator screens.
    pub label: String,
    /// Typed provider-specific settings.
    pub variant: ProviderDefinitionVariant,
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

/// LLM provider variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LlmVariant {
    /// OpenAI-compatible language model.
    #[serde(rename = "openai")]
    OpenAi {
        /// Base URL including any version prefix.
        base_url: String,
        /// Optional API key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<ProviderSecret>,
        /// Advertised model ids. Empty allows any graph-selected model.
        #[serde(default)]
        models: Vec<String>,
        /// Whether the provider should stream completions when supported.
        #[serde(default)]
        streaming: bool,
        /// Optional system prompt attached at provider configuration time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
    },
}

/// Speech recognition provider variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SttVariant {
    /// OpenAI-compatible speech recognizer.
    #[serde(rename = "openai")]
    OpenAi {
        /// Base URL including any version prefix.
        base_url: String,
        /// Model used for transcription.
        model: String,
        /// Optional API key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<ProviderSecret>,
        /// Whether the recognizer streams partials.
        #[serde(default)]
        stream: bool,
    },
    /// Wyoming speech recognizer.
    Wyoming {
        /// Wyoming endpoint URL.
        url: String,
        /// Optional model hint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Whether streaming is enabled.
        #[serde(default)]
        streaming: bool,
    },
}

/// Speech synthesis provider variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TtsVariant {
    /// OpenAI-compatible speech synthesizer.
    #[serde(rename = "openai")]
    OpenAi {
        /// Base URL including any version prefix.
        base_url: String,
        /// Speech model.
        model: String,
        /// Optional API key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<ProviderSecret>,
        /// Optional voice catalogue.
        #[serde(default)]
        voices: Vec<String>,
    },
    /// Wyoming speech synthesizer.
    Wyoming {
        /// Wyoming endpoint URL.
        url: String,
        /// Canonical voice id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
        /// Whether streaming is enabled.
        #[serde(default)]
        streaming: bool,
    },
}

/// Tool provider variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolVariant {
    /// MCP tool provider.
    Mcp {
        /// Tool transport configuration.
        transport: McpTransport,
    },
}

/// Wake word detection provider variants.
///
/// Where detection runs is a deployment choice, not a different kind of stage:
/// a pipeline naming either definition has a wake word stage. The Wyoming
/// variant points at a server; the device variant names a satellite that wakes
/// itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WakeVariant {
    /// Wake word detection on a Wyoming server.
    ///
    /// All three engines are packaged as Wyoming services, so one variant
    /// serves them and [`WakeEngine`] says which is listening.
    Wyoming {
        /// Wyoming endpoint URL.
        url: String,
        /// Which detector is behind the endpoint.
        engine: WakeEngine,
        /// Phrases to listen for. Empty asks the server for whatever it loaded.
        #[serde(default)]
        phrases: Vec<String>,
        /// Minimum confidence to accept, as a percentage.
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
    },
    /// Wake word detection performed on the satellite itself.
    ///
    /// There is no endpoint because there is no server: the device runs the
    /// detector and only streams audio once it has activated. The definition
    /// exists so that a pipeline can *say* it wakes on-device — which is what
    /// makes the stage visible in the editor, in validation, and in the event
    /// stream — rather than the stage silently being absent.
    Device {
        /// Which detector the satellite runs.
        engine: WakeEngine,
        /// Phrases the satellite is flashed with, for operator screens. The
        /// server never scores them.
        #[serde(default)]
        phrases: Vec<String>,
    },
}

/// Speaker identification provider variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpeakerIdVariant {
    /// Speaker identification on a Diarization_Server instance.
    ///
    /// A separate variant rather than a flag on [`Self::Http`] because the two
    /// speak different dialects — raw samples and query parameters against a
    /// container and paths — and a definition should say which service it is
    /// describing rather than which options that service happens to want.
    DiarizationServer {
        /// Base URL of the Diarization_Server instance.
        base_url: String,
        /// Minimum similarity to call a voice a match, as a percentage.
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
    },
    /// Speaker identification over the Conduit speaker HTTP contract.
    Http {
        /// Base URL of the identification service.
        base_url: String,
        /// Optional API key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<ProviderSecret>,
        /// Which embedding model is behind the endpoint.
        engine: SpeakerEngine,
        /// Minimum similarity to call a voice a match, as a percentage.
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
    },
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
}

impl ProviderDefinitionVariant {
    /// Runtime capability this variant supplies.
    #[must_use]
    pub const fn capability(&self) -> ProviderCapability {
        match self {
            Self::Llm { .. } => ProviderCapability::Llm,
            Self::Stt { .. } => ProviderCapability::Stt,
            Self::Tts { .. } => ProviderCapability::Tts,
            Self::Tool { .. } => ProviderCapability::Tool,
            Self::Wake { .. } => ProviderCapability::Wake,
            Self::SpeakerId { .. } => ProviderCapability::SpeakerId,
        }
    }

    /// Returns a copy with inline secrets redacted.
    #[must_use]
    fn redacted(&self) -> Self {
        match self {
            Self::Llm { variant } => Self::Llm { variant: variant.redacted() },
            Self::Stt { variant } => Self::Stt { variant: variant.redacted() },
            Self::Tts { variant } => Self::Tts { variant: variant.redacted() },
            Self::Tool { variant } => Self::Tool { variant: variant.redacted() },
            Self::Wake { variant } => Self::Wake { variant: variant.redacted() },
            Self::SpeakerId { variant } => Self::SpeakerId { variant: variant.redacted() },
        }
    }

    /// Applies update secret semantics against an existing definition.
    #[must_use]
    fn with_secret_updates_from(self, existing: Option<&Self>) -> Self {
        match self {
            Self::Llm {
                variant:
                    LlmVariant::OpenAi { base_url, api_key, models, streaming, system_prompt },
            } => Self::Llm {
                variant: LlmVariant::OpenAi {
                    base_url,
                    api_key: merge_secret(api_key, existing.and_then(Self::api_key)),
                    models,
                    streaming,
                    system_prompt,
                },
            },
            Self::Stt { variant: SttVariant::OpenAi { base_url, model, api_key, stream } } => {
                Self::Stt {
                    variant: SttVariant::OpenAi {
                        base_url,
                        model,
                        api_key: merge_secret(api_key, existing.and_then(Self::api_key)),
                        stream,
                    },
                }
            }
            Self::Tts { variant: TtsVariant::OpenAi { base_url, model, api_key, voices } } => {
                Self::Tts {
                    variant: TtsVariant::OpenAi {
                        base_url,
                        model,
                        api_key: merge_secret(api_key, existing.and_then(Self::api_key)),
                        voices,
                    },
                }
            }
            Self::SpeakerId {
                variant: SpeakerIdVariant::Http { base_url, api_key, engine, threshold_percent },
            } => Self::SpeakerId {
                variant: SpeakerIdVariant::Http {
                    base_url,
                    api_key: merge_secret(api_key, existing.and_then(Self::api_key)),
                    engine,
                    threshold_percent,
                },
            },
            other => other,
        }
    }

    /// The inline-or-external secret a keyed provider carries, if any.
    fn api_key(&self) -> Option<&ProviderSecret> {
        match self {
            Self::Llm { variant: LlmVariant::OpenAi { api_key, .. } }
            | Self::Stt { variant: SttVariant::OpenAi { api_key, .. } }
            | Self::Tts { variant: TtsVariant::OpenAi { api_key, .. } }
            | Self::SpeakerId { variant: SpeakerIdVariant::Http { api_key, .. } } => {
                api_key.as_ref()
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
    Tool { variant: ToolVariant },
    Wake { variant: WakeVariant },
    SpeakerId { variant: SpeakerIdVariant },
}

impl From<WireProviderDefinitionVariant> for ProviderDefinitionVariant {
    fn from(wire: WireProviderDefinitionVariant) -> Self {
        match wire {
            WireProviderDefinitionVariant::Llm { variant } => Self::Llm { variant },
            WireProviderDefinitionVariant::Stt { variant } => Self::Stt { variant },
            WireProviderDefinitionVariant::Tts { variant } => Self::Tts { variant },
            WireProviderDefinitionVariant::Tool { variant } => Self::Tool { variant },
            WireProviderDefinitionVariant::Wake { variant } => Self::Wake { variant },
            WireProviderDefinitionVariant::SpeakerId { variant } => Self::SpeakerId { variant },
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
                variant: WakeVariant::Wyoming { url, engine, phrases, threshold_percent },
            },
            LegacyProviderDefinitionVariant::DeviceWake { engine, phrases } => {
                Self::Wake { variant: WakeVariant::Device { engine, phrases } }
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

impl LlmVariant {
    fn redacted(&self) -> Self {
        match self {
            Self::OpenAi { base_url, api_key, models, streaming, system_prompt } => {
                Self::OpenAi {
                    base_url: base_url.clone(),
                    api_key: redact_secret(api_key),
                    models: models.clone(),
                    streaming: *streaming,
                    system_prompt: system_prompt.clone(),
                }
            }
        }
    }
}

impl SttVariant {
    fn redacted(&self) -> Self {
        match self {
            Self::OpenAi { base_url, model, api_key, stream } => Self::OpenAi {
                base_url: base_url.clone(),
                model: model.clone(),
                api_key: redact_secret(api_key),
                stream: *stream,
            },
            Self::Wyoming { url, model, streaming } => {
                Self::Wyoming { url: url.clone(), model: model.clone(), streaming: *streaming }
            }
        }
    }
}

impl TtsVariant {
    fn redacted(&self) -> Self {
        match self {
            Self::OpenAi { base_url, model, api_key, voices } => Self::OpenAi {
                base_url: base_url.clone(),
                model: model.clone(),
                api_key: redact_secret(api_key),
                voices: voices.clone(),
            },
            Self::Wyoming { url, voice, streaming } => {
                Self::Wyoming { url: url.clone(), voice: voice.clone(), streaming: *streaming }
            }
        }
    }
}

impl ToolVariant {
    fn redacted(&self) -> Self {
        self.clone()
    }
}

impl WakeVariant {
    fn redacted(&self) -> Self {
        self.clone()
    }
}

impl SpeakerIdVariant {
    fn redacted(&self) -> Self {
        match self {
            Self::DiarizationServer { base_url, threshold_percent } => {
                Self::DiarizationServer {
                    base_url: base_url.clone(),
                    threshold_percent: *threshold_percent,
                }
            }
            Self::Http { base_url, api_key, engine, threshold_percent } => Self::Http {
                base_url: base_url.clone(),
                api_key: redact_secret(api_key),
                engine: *engine,
                threshold_percent: *threshold_percent,
            },
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

/// MCP transport variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    /// Server-sent events transport.
    Sse {
        /// MCP endpoint URL.
        url: String,
    },
    /// Streamable HTTP transport.
    StreamableHttp {
        /// MCP endpoint URL.
        url: String,
    },
    /// Local stdio command transport.
    Stdio {
        /// Command to run.
        command: String,
        /// Command arguments.
        #[serde(default)]
        args: Vec<String>,
    },
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
    fn wake_definitions_supply_the_wake_capability_wherever_they_detect() {
        // Where detection runs is a deployment choice, not a different kind of
        // stage: a pipeline naming either definition has a wake word stage.
        let remote = ProviderDefinitionVariant::Wake {
            variant: WakeVariant::Wyoming {
                url: "tcp://openwakeword:10400".to_owned(),
                engine: WakeEngine::OpenWakeWord,
                phrases: vec!["hey jarvis".to_owned()],
                threshold_percent: DEFAULT_THRESHOLD_PERCENT,
            },
        };
        let on_device = ProviderDefinitionVariant::Wake {
            variant: WakeVariant::Device {
                engine: WakeEngine::MicroWakeWord,
                phrases: vec!["okay nabu".to_owned()],
            },
        };

        assert_eq!(remote.capability(), ProviderCapability::Wake);
        assert_eq!(on_device.capability(), ProviderCapability::Wake);
    }

    #[test]
    fn a_wake_definition_that_omits_its_threshold_reads_as_the_documented_default() {
        let variant: ProviderDefinitionVariant = serde_json::from_value(serde_json::json!({
            "type": "wake",
            "variant": {
                "type": "wyoming",
                "url": "tcp://openwakeword:10400",
                "engine": "openwakeword",
            },
        }))
        .expect("deserialize");

        let ProviderDefinitionVariant::Wake {
            variant: WakeVariant::Wyoming { threshold_percent, phrases, .. },
        } = variant
        else {
            panic!("a `wake`/`wyoming` tag deserializes to a Wyoming wake definition");
        };
        assert_eq!(threshold_percent, DEFAULT_THRESHOLD_PERCENT);
        assert!(phrases.is_empty(), "no phrases named means whatever the server loaded");
    }

    #[test]
    fn only_microwakeword_is_small_enough_for_a_satellite() {
        assert!(WakeEngine::MicroWakeWord.runs_on_device());
        assert!(!WakeEngine::OpenWakeWord.runs_on_device());
        assert!(!WakeEngine::NanoWakeWord.runs_on_device());
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
    fn a_speaker_definitions_key_is_redacted_and_survives_an_update_that_omits_it() {
        // The same secret semantics every keyed definition has: a read never
        // shows the key, and saving what a read returned must not erase it.
        let stored = ProviderDefinitionVariant::SpeakerId {
            variant: SpeakerIdVariant::Http {
                base_url: "https://voices.example".to_owned(),
                api_key: Some(ProviderSecret::Inline { value: "sk-live".to_owned() }),
                engine: SpeakerEngine::SpeechBrain,
                threshold_percent: 70,
            },
        };

        let read = stored.redacted();
        assert_eq!(
            read,
            ProviderDefinitionVariant::SpeakerId {
                variant: SpeakerIdVariant::Http {
                    base_url: "https://voices.example".to_owned(),
                    api_key: Some(ProviderSecret::Redacted),
                    engine: SpeakerEngine::SpeechBrain,
                    threshold_percent: 70,
                }
            }
        );

        let saved = read.with_secret_updates_from(Some(&stored));
        assert_eq!(saved, stored, "saving a redacted key keeps the stored one");
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

        let ProviderDefinitionVariant::Wake {
            variant: WakeVariant::Wyoming { threshold_percent, phrases, .. },
        } = variant
        else {
            panic!("a legacy `wyoming_wake` reads as a Wyoming wake definition");
        };
        assert_eq!(threshold_percent, DEFAULT_THRESHOLD_PERCENT);
        assert!(phrases.is_empty(), "no phrases named means whatever the server loaded");
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
}
