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

/// Closed set of provider definition variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderDefinitionVariant {
    /// OpenAI-compatible language model.
    #[serde(rename = "openai_llm")]
    OpenAiLlm {
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
    /// OpenAI-compatible speech recognizer.
    #[serde(rename = "openai_stt")]
    OpenAiStt {
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
    /// OpenAI-compatible speech synthesizer.
    #[serde(rename = "openai_tts")]
    OpenAiTts {
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
    /// Wyoming speech recognizer.
    WyomingStt {
        /// Wyoming endpoint URL.
        url: String,
        /// Optional model hint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Whether streaming is enabled.
        #[serde(default)]
        streaming: bool,
    },
    /// Wyoming speech synthesizer.
    WyomingTts {
        /// Wyoming endpoint URL.
        url: String,
        /// Canonical voice id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
        /// Whether streaming is enabled.
        #[serde(default)]
        streaming: bool,
    },
    /// MCP tool provider.
    McpTool {
        /// Tool transport configuration.
        transport: McpTransport,
    },
}

impl ProviderDefinitionVariant {
    /// Runtime capability this variant supplies.
    #[must_use]
    pub const fn capability(&self) -> ProviderCapability {
        match self {
            Self::OpenAiLlm { .. } => ProviderCapability::Llm,
            Self::OpenAiStt { .. } | Self::WyomingStt { .. } => ProviderCapability::Stt,
            Self::OpenAiTts { .. } | Self::WyomingTts { .. } => ProviderCapability::Tts,
            Self::McpTool { .. } => ProviderCapability::Tool,
        }
    }

    fn redacted(&self) -> Self {
        match self {
            Self::OpenAiLlm { base_url, api_key, models, streaming, system_prompt } => {
                Self::OpenAiLlm {
                    base_url: base_url.clone(),
                    api_key: redact_secret(api_key),
                    models: models.clone(),
                    streaming: *streaming,
                    system_prompt: system_prompt.clone(),
                }
            }
            Self::OpenAiStt { base_url, model, api_key, stream } => Self::OpenAiStt {
                base_url: base_url.clone(),
                model: model.clone(),
                api_key: redact_secret(api_key),
                stream: *stream,
            },
            Self::OpenAiTts { base_url, model, api_key, voices } => Self::OpenAiTts {
                base_url: base_url.clone(),
                model: model.clone(),
                api_key: redact_secret(api_key),
                voices: voices.clone(),
            },
            Self::WyomingStt { url, model, streaming } => Self::WyomingStt {
                url: url.clone(),
                model: model.clone(),
                streaming: *streaming,
            },
            Self::WyomingTts { url, voice, streaming } => Self::WyomingTts {
                url: url.clone(),
                voice: voice.clone(),
                streaming: *streaming,
            },
            Self::McpTool { transport } => Self::McpTool { transport: transport.clone() },
        }
    }

    fn with_secret_updates_from(self, existing: Option<&Self>) -> Self {
        match self {
            Self::OpenAiLlm { base_url, api_key, models, streaming, system_prompt } => {
                Self::OpenAiLlm {
                    base_url,
                    api_key: merge_secret(api_key, existing.and_then(Self::api_key)),
                    models,
                    streaming,
                    system_prompt,
                }
            }
            Self::OpenAiStt { base_url, model, api_key, stream } => Self::OpenAiStt {
                base_url,
                model,
                api_key: merge_secret(api_key, existing.and_then(Self::api_key)),
                stream,
            },
            Self::OpenAiTts { base_url, model, api_key, voices } => Self::OpenAiTts {
                base_url,
                model,
                api_key: merge_secret(api_key, existing.and_then(Self::api_key)),
                voices,
            },
            other => other,
        }
    }

    fn api_key(&self) -> Option<&ProviderSecret> {
        match self {
            Self::OpenAiLlm { api_key, .. }
            | Self::OpenAiStt { api_key, .. }
            | Self::OpenAiTts { api_key, .. } => api_key.as_ref(),
            _ => None,
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
    fn the_error_names_the_offending_character() {
        let error = validate_name("kitchen light").expect_err("spaces are not allowed");
        assert!(error.to_string().contains('`'), "{error}");
    }
}
