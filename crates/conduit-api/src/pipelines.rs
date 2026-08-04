//! Pipeline definition endpoints.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use bytes::Bytes;
use conduit_core::audio::AudioFormat;
use conduit_core::graph::{Node, NodeKind, PipelineGraph};
use conduit_core::id::ConversationId;
use conduit_provider::storage::{validate_name, ProviderCapability};
use conduit_provider::stt::AudioChunk;
use conduit_provider::ChunkStream;
use conduit_runtime::{Reply, Runner};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::auth::ManagementCaller;
use crate::error::JsonBody;
use crate::{ApiError, AppState};

/// A stored pipeline and its execution order.
#[derive(Debug, Serialize)]
pub struct PipelineView {
    /// The stored definition.
    pub graph: PipelineGraph,
    /// Node ids in execution order, as the runtime would walk them.
    pub order: Vec<String>,
}

/// A provider component that can be selected for a provider definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderComponentDescriptor {
    /// Stable component id used by the provider definition form.
    pub id: &'static str,
    /// Human-readable label for operator screens.
    pub label: &'static str,
    /// What this provider can do.
    ///
    /// A capability rather than a node kind: tools and memory are core
    /// bindings rather than graph stages, so there is no node kind left for
    /// their components to name.
    pub kind: ProviderCapability,
    /// Inner provider definition variant created from this catalog entry.
    ///
    /// The saved definition's outer variant is `kind` and its inner variant is
    /// this value, so the two together name the full two-level variant.
    pub definition_variant: &'static str,
    /// Configuration fields accepted by this provider component.
    pub schema: ComponentConfigSchema,
}

/// Minimal JSON Schema subset the operator console uses to render forms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentConfigSchema {
    /// Field definitions keyed by config object property name.
    pub properties: BTreeMap<&'static str, ComponentConfigProperty>,
    /// Property names required for a runnable component.
    pub required: Vec<&'static str>,
}

/// One component configuration field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentConfigProperty {
    /// Primitive JSON type the field accepts.
    #[serde(rename = "type")]
    pub value_type: ComponentConfigValueType,
    /// Optional string format hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<ComponentConfigFormat>,
    /// Optional regular expression hint for string inputs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<&'static str>,
    /// The only values this field accepts, when it is a closed set.
    ///
    /// A wake word definition names one of three engines, and a console that
    /// offered a free text box for it would let an operator save a definition
    /// the server then refuses. Empty means the field is open.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<&'static str>,
    /// What the field starts as when a form is opened fresh.
    ///
    /// A suggestion rather than a constraint: an operator can replace it, and a
    /// definition that omits the field is not filled in behind their back. It
    /// exists so a component that knows its own endpoint — a local Ollama on
    /// `11434` — does not make someone look the port up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<&'static str>,
}

/// Primitive config field value types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentConfigValueType {
    /// String input.
    String,
    /// Boolean input.
    Boolean,
    /// Whole-number input.
    Integer,
    /// A list of strings, e.g. the phrases a detector listens for.
    StringList,
}

/// Config field format hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentConfigFormat {
    /// URL text input.
    Url,
}

/// Provider component catalog response.
#[derive(Debug, Serialize)]
pub struct ProviderComponentCatalog {
    /// Known components, in stable display order.
    pub components: Vec<ProviderComponentDescriptor>,
}

/// Input for an operator-triggered pipeline test turn.
#[derive(Debug, Deserialize)]
pub struct PipelineTestRequest {
    /// Text fed into the configured STT stage by the built-in test harness.
    #[serde(default = "default_test_utterance")]
    pub utterance: String,
    /// Audio format advertised to providers for the synthetic input stream.
    #[serde(default)]
    pub format: AudioFormat,
}

/// Result of an operator-triggered pipeline test turn.
#[derive(Debug, Serialize)]
pub struct PipelineTestResult {
    /// Pipeline that ran.
    pub pipeline: String,
    /// Conversation id emitted on the event stream.
    pub conversation: ConversationId,
    /// Completion state for the test turn.
    pub status: &'static str,
    /// Number of synthesized audio bytes returned by the TTS stage.
    pub audio_bytes: usize,
    /// The reply as written text, for a pipeline that writes rather than
    /// speaks. `None` when the turn synthesized its reply instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_text: Option<String>,
    /// The reply as a playable WAV file, base64-encoded, or `None` when the
    /// turn produced no audio.
    ///
    /// The point of a test turn is hearing what the pipeline says. The samples
    /// were previously rendered as lossy UTF-8, which put the raw PCM on
    /// screen as mojibake — audio is not text, and no operator could tell a
    /// working synthesizer from a broken one by reading it. A container and an
    /// encoding make it something a browser can simply play.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_audio: Option<String>,
}

/// `GET /v1/pipelines` — names of every stored pipeline.
///
/// # Errors
///
/// Returns 503 if the store cannot be read.
pub async fn list(
    _caller: ManagementCaller,
    State(state): State<AppState>,
) -> Result<Json<Vec<String>>, ApiError> {
    state.pipeline_names().await.map(Json).map_err(store_failure)
}

/// Turns a store failure into a response.
///
/// A name the store refuses is the client's mistake; anything else is the
/// server's, and the status has to say which.
fn store_failure(error: conduit_core::Error) -> ApiError {
    match error {
        conduit_core::Error::Config(detail) => ApiError::unprocessable(detail),
        other => ApiError::unavailable(other.to_string()),
    }
}

/// `GET /v1/pipelines/{name}` — one pipeline with its execution order.
///
/// # Errors
///
/// Returns 404 if no pipeline is stored under `name`, or 503 if the store
/// cannot be read.
pub async fn get(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<PipelineView>, ApiError> {
    let graph = state
        .pipeline(&name)
        .await
        .map_err(store_failure)?
        .ok_or_else(|| ApiError::not_found(format!("no pipeline named `{name}`")))?;
    Ok(Json(view(graph)?))
}

/// `PUT /v1/pipelines/{name}` — stores a pipeline after validating it.
///
/// Returns 201 for a new pipeline and 200 when replacing one. Invalid graphs
/// are rejected rather than stored, so anything readable back is executable.
///
/// # Errors
///
/// Returns 422 if the graph fails validation.
pub async fn put(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(name): Path<String>,
    JsonBody(graph): JsonBody<PipelineGraph>,
) -> Result<(StatusCode, Json<PipelineView>), ApiError> {
    validate_provider_references(&state, &graph).await?;
    let view = view(graph.clone())?;
    let replaced = state.put_pipeline(&name, graph).await.map_err(store_failure)?;
    let status = if replaced { StatusCode::OK } else { StatusCode::CREATED };
    Ok((status, Json(view)))
}

/// `DELETE /v1/pipelines/{name}` — removes a pipeline.
///
/// # Errors
///
/// Returns 404 if no pipeline is stored under `name`.
pub async fn delete(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    if state.remove_pipeline(&name).await.map_err(store_failure)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("no pipeline named `{name}`")))
    }
}

/// `POST /v1/pipelines/validate` — checks a graph without storing it.
///
/// This is what the graph editor calls on every edit.
///
/// # Errors
///
/// Returns 422 if the graph fails validation.
pub async fn validate(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    JsonBody(graph): JsonBody<PipelineGraph>,
) -> Result<Json<PipelineView>, ApiError> {
    validate_provider_references(&state, &graph).await?;
    view(graph).map(Json)
}

/// `POST /v1/pipelines/{name}/test-turn` — runs a stored pipeline once.
///
/// # Errors
///
/// Returns 404 when the pipeline is missing, 422 when it cannot be prepared
/// with the configured runtime providers, and 503 when the turn fails while
/// running.
pub async fn test_turn(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(name): Path<String>,
    JsonBody(request): JsonBody<PipelineTestRequest>,
) -> Result<Json<PipelineTestResult>, ApiError> {
    let graph = state
        .pipeline(&name)
        .await
        .map_err(store_failure)?
        .ok_or_else(|| ApiError::not_found(format!("no pipeline named `{name}`")))?;
    let providers = state
        .providers()
        .ok_or_else(|| ApiError::unprocessable("no providers are configured".to_owned()))?;
    let runner = Runner::prepare(&graph, &providers, state.bus.clone())
        .map_err(|error| ApiError::unprocessable(error.to_string()))?
        .with_format(request.format)
        .map_err(|error| ApiError::unprocessable(error.to_string()))?
        .with_idle_timeout(state.turn_idle_timeout());
    // Which input a test turn produces is the pipeline's choice, not the
    // operator's: the same typed utterance is spoken at a voice pipeline and
    // handed straight to a text one.
    let conversation = if runner.expects_audio() {
        runner.run(test_audio(request.utterance))
    } else {
        runner.run_text(request.utterance)
    };
    let conversation_id = conversation.id;
    let mut replies = conversation.output;
    let mut output = Vec::new();
    let mut written = String::new();

    while let Some(reply) = replies.next().await {
        match reply.map_err(|error| ApiError::unavailable(error.to_string()))? {
            Reply::Speech(chunk) => output.extend_from_slice(&chunk.data),
            Reply::Text(segment) => {
                if !written.is_empty() {
                    written.push(' ');
                }
                written.push_str(&segment);
            }
        }
    }

    let reply_audio = if output.is_empty() {
        None
    } else {
        let upload = conduit_core::wav::package(request.format, output.clone())
            .map_err(|error| ApiError::unprocessable(error.to_string()))?;
        Some(BASE64.encode(&upload.bytes))
    };

    Ok(Json(PipelineTestResult {
        pipeline: name,
        conversation: conversation_id,
        status: "completed",
        audio_bytes: output.len(),
        reply_text: (!written.is_empty()).then_some(written),
        reply_audio,
    }))
}

fn test_audio(utterance: String) -> ChunkStream<AudioChunk> {
    Box::pin(futures_util::stream::iter([Ok(AudioChunk {
        sequence: 0,
        data: Bytes::from(utterance.into_bytes()),
    })]))
}

fn default_test_utterance() -> String {
    "conduit test".to_owned()
}

/// Validates `graph` and pairs it with its execution order.
fn view(graph: PipelineGraph) -> Result<PipelineView, ApiError> {
    let order = graph
        .topological_order()
        .map_err(|error| ApiError::unprocessable(error.to_string()))?
        .iter()
        .map(|node| node.id().clone())
        .collect();
    Ok(PipelineView { graph, order })
}

async fn validate_provider_references(
    state: &AppState,
    graph: &PipelineGraph,
) -> Result<(), ApiError> {
    for node in
        graph.topological_order().map_err(|error| ApiError::unprocessable(error.to_string()))?
    {
        // A core is checked binding by binding rather than as one node: its
        // model must be a language model and each tool must be a tool, and no
        // single capability could say that.
        let expectations: Vec<(&str, ProviderCapability)> = match node {
            Node::Core { core, .. } => {
                std::iter::once((core.model.provider.as_str(), ProviderCapability::Llm))
                    .chain(
                        core.tools
                            .iter()
                            .map(|tool| (tool.provider.as_str(), ProviderCapability::Tool)),
                    )
                    .collect()
            }
            other => provider_capability_for_node(other.kind())
                .map(|capability| vec![(other.provider(), capability)])
                .unwrap_or_default(),
        };

        for (provider, expected) in expectations {
            // A qualified id such as `weather-tools.forecast` names one tool
            // discovered from an MCP definition, not a stored definition, so it is
            // resolved against the runtime snapshot instead of the store — which
            // would reject the string as an unusable key.
            let definition = if validate_name(provider).is_ok() {
                state.provider_definition(provider).await.map_err(store_failure)?
            } else {
                None
            };
            let actual = if let Some(definition) = definition {
                Some(definition.capability())
            } else {
                runtime_provider_capability(state.providers().as_deref(), provider)
            };
            let Some(actual) = actual else {
                return Err(ApiError::unprocessable(format!(
                    "provider definition `{provider}` is referenced by node `{}` but \
                     does not exist",
                    node.id()
                )));
            };
            if actual != expected {
                return Err(ApiError::unprocessable(format!(
                    "provider definition `{provider}` is {} but node `{}` requires {}",
                    provider_capability_label(actual),
                    node.id(),
                    provider_capability_label(expected)
                )));
            }
        }
    }
    Ok(())
}

fn runtime_provider_capability(
    providers: Option<&conduit_runtime::Providers>,
    id: &str,
) -> Option<ProviderCapability> {
    let providers = providers?;
    if providers.stt().get(id).is_some() {
        Some(ProviderCapability::Stt)
    } else if providers.llm().get(id).is_some() {
        Some(ProviderCapability::Llm)
    } else if providers.tools().get(id).is_some() {
        Some(ProviderCapability::Tool)
    } else if providers.tts().get(id).is_some() {
        Some(ProviderCapability::Tts)
    } else if providers.transform().get(id).is_some() {
        Some(ProviderCapability::Transform)
    } else {
        None
    }
}

/// What a transport node's provider has to be able to do.
///
/// A core is absent on purpose: it names a model, any number of tools, and any
/// number of stores, so one capability could not describe it. Its bindings are
/// checked against their own capabilities instead.
fn provider_capability_for_node(kind: NodeKind) -> Option<ProviderCapability> {
    match kind {
        NodeKind::Stt => Some(ProviderCapability::Stt),
        NodeKind::Tts => Some(ProviderCapability::Tts),
        NodeKind::Transform => Some(ProviderCapability::Transform),
        NodeKind::WakeWord => Some(ProviderCapability::Wake),
        NodeKind::SpeakerId => Some(ProviderCapability::SpeakerId),
        _ => None,
    }
}

fn provider_capability_label(capability: ProviderCapability) -> &'static str {
    match capability {
        ProviderCapability::Stt => "stt",
        ProviderCapability::Llm => "llm",
        ProviderCapability::Tool => "tool",
        ProviderCapability::Tts => "tts",
        ProviderCapability::Transform => "transform",
        ProviderCapability::Wake => "wake",
        ProviderCapability::SpeakerId => "speaker_id",
    }
}

/// The rewriting rules a built-in transform can apply, in the order the form
/// offers them.
const TRANSFORM_RULES: [&str; 3] = ["markdown_to_speech", "strip_emoji", "collapse_whitespace"];

/// The embedding models a speaker identification service may be running.
const SPEAKER_ENGINES: [&str; 3] = ["speechbrain", "resemblyzer", "pyannote"];

/// Built-in component descriptors.
#[must_use]
pub fn component_catalog() -> Vec<ProviderComponentDescriptor> {
    vec![
        ProviderComponentDescriptor {
            id: "openai.responses",
            label: "OpenAI Responses",
            kind: ProviderCapability::Llm,
            definition_variant: "openai",
            schema: openai_llm_schema(),
        },
        ProviderComponentDescriptor {
            id: "openai.completions",
            label: "OpenAI Completions",
            kind: ProviderCapability::Llm,
            definition_variant: "openai",
            schema: openai_llm_schema(),
        },
        preset("ollama", "Ollama", "http://localhost:11434/v1"),
        preset("vllm", "vLLM", "http://localhost:8000/v1"),
        preset("lmstudio", "LM Studio", "http://localhost:1234/v1"),
        preset("openrouter", "OpenRouter", "https://openrouter.ai/api/v1"),
        ProviderComponentDescriptor {
            id: "anthropic.messages",
            label: "Anthropic Messages",
            kind: ProviderCapability::Llm,
            definition_variant: "anthropic",
            schema: ComponentConfigSchema {
                properties: properties([
                    ("base_url", string_property(Some(ComponentConfigFormat::Url), None)),
                    ("api_key", string_property(None, None)),
                    ("model", string_property(None, Some("[A-Za-z0-9._:/-]+"))),
                    ("streaming", boolean_property()),
                ]),
                // No `base_url`: the public API is the one an operator who
                // typed nothing meant, unlike an OpenAI-compatible server,
                // which could be anywhere.
                required: vec!["model"],
            },
        },
        ProviderComponentDescriptor {
            id: "bedrock.converse",
            label: "Amazon Bedrock",
            kind: ProviderCapability::Llm,
            definition_variant: "bedrock",
            schema: ComponentConfigSchema {
                properties: properties([
                    ("region", string_property(None, Some("[a-z0-9-]+"))),
                    ("profile", string_property(None, None)),
                    ("api_key", string_property(None, None)),
                    // Bedrock names a model by inference profile as often as by
                    // model id, and a profile carries the `us.` region prefix
                    // the plain id does not.
                    ("model", string_property(None, Some("[A-Za-z0-9._:-]+"))),
                    ("streaming", boolean_property()),
                ]),
                // Only the region: the usual deployment names no credential
                // because a task role or an instance profile already supplies
                // one, and no URL because the region is the endpoint.
                required: vec!["region", "model"],
            },
        },
        ProviderComponentDescriptor {
            id: "wyoming",
            label: "Wyoming",
            kind: ProviderCapability::Stt,
            definition_variant: "wyoming",
            schema: ComponentConfigSchema {
                properties: properties([
                    ("url", string_property(Some(ComponentConfigFormat::Url), None)),
                    ("model", string_property(None, None)),
                    ("streaming", boolean_property()),
                ]),
                required: vec!["url"],
            },
        },
        ProviderComponentDescriptor {
            id: "openai.transcription",
            label: "OpenAI Transcription",
            kind: ProviderCapability::Stt,
            definition_variant: "openai",
            schema: ComponentConfigSchema {
                properties: properties([
                    ("base_url", string_property(Some(ComponentConfigFormat::Url), None)),
                    ("model", string_property(None, None)),
                    ("stream", boolean_property()),
                ]),
                required: vec!["model"],
            },
        },
        ProviderComponentDescriptor {
            id: "openai.speech",
            label: "OpenAI Speech",
            kind: ProviderCapability::Tts,
            definition_variant: "openai",
            schema: ComponentConfigSchema {
                properties: properties([
                    ("base_url", string_property(Some(ComponentConfigFormat::Url), None)),
                    ("model", string_property(None, None)),
                ]),
                required: vec!["model"],
            },
        },
        ProviderComponentDescriptor {
            id: "wyoming.tts",
            label: "Wyoming TTS",
            kind: ProviderCapability::Tts,
            definition_variant: "wyoming",
            schema: ComponentConfigSchema {
                properties: properties([
                    ("url", string_property(Some(ComponentConfigFormat::Url), None)),
                    ("voice", string_property(None, None)),
                    ("model", string_property(None, None)),
                    ("mode", string_property(None, None)),
                    ("streaming", boolean_property()),
                ]),
                required: vec!["url"],
            },
        },
        ProviderComponentDescriptor {
            id: "mcp.sse",
            label: "MCP SSE",
            kind: ProviderCapability::Tool,
            definition_variant: "mcp",
            schema: ComponentConfigSchema {
                properties: properties([(
                    "url",
                    string_property(Some(ComponentConfigFormat::Url), None),
                )]),
                required: vec!["url"],
            },
        },
        ProviderComponentDescriptor {
            id: "mcp.streamable_http",
            label: "MCP Streamable HTTP",
            kind: ProviderCapability::Tool,
            definition_variant: "mcp",
            schema: ComponentConfigSchema {
                properties: properties([(
                    "url",
                    string_property(Some(ComponentConfigFormat::Url), None),
                )]),
                required: vec!["url"],
            },
        },
        ProviderComponentDescriptor {
            id: "openwakeword",
            label: "openWakeWord",
            kind: ProviderCapability::Wake,
            definition_variant: "openwakeword",
            schema: scored_wake_schema(),
        },
        ProviderComponentDescriptor {
            id: "nanowakeword",
            label: "nanoWakeWord",
            kind: ProviderCapability::Wake,
            definition_variant: "nanowakeword",
            schema: scored_wake_schema(),
        },
        ProviderComponentDescriptor {
            id: "microwakeword",
            label: "microWakeWord",
            kind: ProviderCapability::Wake,
            definition_variant: "microwakeword",
            schema: ComponentConfigSchema {
                // `device` rather than `local`: microWakeWord's models are
                // tflite-micro graphs Conduit cannot score itself, and it is
                // the only engine small enough for satellite hardware. A
                // satellite needs no URL — it streams only once it has
                // activated — and no threshold, having already decided.
                properties: properties([
                    ("where", choice_property(vec!["device", "wyoming"])),
                    ("url", string_property(Some(ComponentConfigFormat::Url), None)),
                    ("phrases", string_list_property()),
                    ("threshold_percent", integer_property()),
                ]),
                required: vec!["where"],
            },
        },
        ProviderComponentDescriptor {
            id: "speaker.http",
            label: "Speaker identification",
            kind: ProviderCapability::SpeakerId,
            definition_variant: "http",
            schema: ComponentConfigSchema {
                properties: properties([
                    ("base_url", string_property(Some(ComponentConfigFormat::Url), None)),
                    ("api_key", string_property(None, None)),
                    ("engine", choice_property(SPEAKER_ENGINES.to_vec())),
                    ("threshold_percent", integer_property()),
                ]),
                required: vec!["base_url", "engine"],
            },
        },
        ProviderComponentDescriptor {
            id: "speaker.diarization_server",
            label: "Diarization Server",
            kind: ProviderCapability::SpeakerId,
            definition_variant: "diarization_server",
            schema: ComponentConfigSchema {
                // No engine: the server decides which model it runs, and no
                // API key: it has no authentication to offer one to.
                properties: properties([
                    ("base_url", string_property(Some(ComponentConfigFormat::Url), None)),
                    ("threshold_percent", integer_property()),
                ]),
                required: vec!["base_url"],
            },
        },
        ProviderComponentDescriptor {
            id: "transform.builtin",
            label: "Speech cleanup",
            kind: ProviderCapability::Transform,
            definition_variant: "builtin",
            schema: ComponentConfigSchema {
                // Rules rather than free text: each one is a statement about
                // how speech differs from writing, and an operator typing a
                // name this build does not implement would save a definition
                // that quietly rewrites nothing.
                properties: properties([(
                    "rules",
                    choice_list_property(TRANSFORM_RULES.to_vec()),
                )]),
                required: vec!["rules"],
            },
        },
        ProviderComponentDescriptor {
            id: "mcp.stdio",
            label: "MCP STDIO",
            kind: ProviderCapability::Tool,
            definition_variant: "mcp",
            schema: ComponentConfigSchema {
                properties: properties([("command", string_property(None, None))]),
                required: vec!["command"],
            },
        },
    ]
}

/// The shape shared by the two engines Conduit can score itself.
///
/// openWakeWord and nanoWakeWord are ONNX end-to-end, so each can either be
/// loaded from disk and scored in process or handed to a Wyoming server. Which
/// of the two fields matters follows from `where`: `models_dir` for `local`,
/// `url` for `wyoming`.
fn scored_wake_schema() -> ComponentConfigSchema {
    ComponentConfigSchema {
        properties: properties([
            ("where", choice_property(vec!["local", "wyoming"])),
            ("url", string_property(Some(ComponentConfigFormat::Url), None)),
            ("models_dir", string_property(None, None)),
            ("phrases", string_list_property()),
            ("threshold_percent", integer_property()),
        ]),
        required: vec!["where"],
    }
}

/// A named server that speaks the OpenAI chat completions API.
///
/// The same variant and the same fields as `openai.responses`, with the
/// endpoint already filled in. Nothing new is registered: `conduit-openai`
/// reaches all of these, and what an operator was missing was not a provider
/// but the knowledge that a local Ollama listens on `11434` and wants a `/v1`
/// suffix. A preset is the catalogue telling them.
fn preset(
    id: &'static str,
    label: &'static str,
    base_url: &'static str,
) -> ProviderComponentDescriptor {
    let mut schema = openai_llm_schema();
    schema.properties.insert("base_url", defaulted_url(base_url));
    ProviderComponentDescriptor {
        id,
        label,
        kind: ProviderCapability::Llm,
        definition_variant: "openai",
        schema,
    }
}

fn openai_llm_schema() -> ComponentConfigSchema {
    ComponentConfigSchema {
        properties: properties([
            ("base_url", string_property(Some(ComponentConfigFormat::Url), None)),
            ("api_key", string_property(None, None)),
            // Model names are the server's to define, not ours: an `ollama`
            // tag carries `:` (`qwen3:8b`) and a Hugging Face repo carries `/`.
            // A pattern narrower than that rejects names that work.
            ("model", string_property(None, Some("[A-Za-z0-9._:/-]+"))),
            ("streaming", boolean_property()),
        ]),
        required: vec!["base_url", "model"],
    }
}

fn string_property(
    format: Option<ComponentConfigFormat>,
    pattern: Option<&'static str>,
) -> ComponentConfigProperty {
    ComponentConfigProperty {
        value_type: ComponentConfigValueType::String,
        format,
        pattern,
        options: Vec::new(),
        default: None,
    }
}

/// The same field, arriving with `default` already in the box.
fn defaulted_url(default: &'static str) -> ComponentConfigProperty {
    ComponentConfigProperty {
        default: Some(default),
        ..string_property(Some(ComponentConfigFormat::Url), None)
    }
}

fn boolean_property() -> ComponentConfigProperty {
    ComponentConfigProperty {
        value_type: ComponentConfigValueType::Boolean,
        format: None,
        pattern: None,
        options: Vec::new(),
        default: None,
    }
}

/// A field whose value must be one of `options`.
fn choice_property(options: Vec<&'static str>) -> ComponentConfigProperty {
    ComponentConfigProperty {
        value_type: ComponentConfigValueType::String,
        format: None,
        pattern: None,
        options,
        default: None,
    }
}

/// A field holding any number of values, each one of `options`.
fn choice_list_property(options: Vec<&'static str>) -> ComponentConfigProperty {
    ComponentConfigProperty {
        value_type: ComponentConfigValueType::StringList,
        format: None,
        pattern: None,
        options,
        default: None,
    }
}

fn integer_property() -> ComponentConfigProperty {
    ComponentConfigProperty {
        value_type: ComponentConfigValueType::Integer,
        format: None,
        pattern: None,
        options: Vec::new(),
        default: None,
    }
}

/// A list of free-text values, e.g. the phrases a detector listens for.
fn string_list_property() -> ComponentConfigProperty {
    ComponentConfigProperty {
        value_type: ComponentConfigValueType::StringList,
        format: None,
        pattern: None,
        options: Vec::new(),
        default: None,
    }
}

fn properties<const N: usize>(
    entries: [(&'static str, ComponentConfigProperty); N],
) -> BTreeMap<&'static str, ComponentConfigProperty> {
    entries.into_iter().collect()
}
