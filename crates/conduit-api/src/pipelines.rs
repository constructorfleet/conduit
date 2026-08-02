//! Pipeline definition endpoints.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use bytes::Bytes;
use conduit_core::audio::AudioFormat;
use conduit_core::graph::{NodeKind, PipelineGraph};
use conduit_core::id::ConversationId;
use conduit_provider::storage::{validate_name, ProviderCapability};
use conduit_provider::stt::AudioChunk;
use conduit_provider::ChunkStream;
use conduit_runtime::Runner;
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
    /// What node kind this provider can serve.
    pub kind: NodeKind,
    /// Provider definition variant created from this catalog entry.
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
}

/// Primitive config field value types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentConfigValueType {
    /// String input.
    String,
    /// Boolean input.
    Boolean,
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
    /// Best-effort text rendering of the synthesized stream.
    pub reply_text: String,
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
    let conversation = runner.run(test_audio(request.utterance));
    let conversation_id = conversation.id;
    let mut audio = conversation.audio;
    let mut output = Vec::new();

    while let Some(chunk) = audio.next().await {
        let chunk = chunk.map_err(|error| ApiError::unavailable(error.to_string()))?;
        output.extend_from_slice(&chunk.data);
    }

    Ok(Json(PipelineTestResult {
        pipeline: name,
        conversation: conversation_id,
        status: "completed",
        audio_bytes: output.len(),
        reply_text: String::from_utf8_lossy(&output).into_owned(),
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
        .map(|node| node.id.clone())
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
        let Some(expected) = provider_capability_for_node(node.kind) else {
            continue;
        };
        // A qualified id such as `weather-tools.forecast` names one tool
        // discovered from an MCP definition, not a stored definition, so it is
        // resolved against the runtime snapshot instead of the store — which
        // would reject the string as an unusable key.
        let definition = if validate_name(&node.provider).is_ok() {
            state.provider_definition(&node.provider).await.map_err(store_failure)?
        } else {
            None
        };
        let actual = if let Some(definition) = definition {
            Some(definition.capability())
        } else {
            runtime_provider_capability(state.providers().as_deref(), &node.provider)
        };
        let Some(actual) = actual else {
            return Err(ApiError::unprocessable(format!(
                "provider definition `{}` is referenced by node `{}` but does not exist",
                node.provider, node.id
            )));
        };
        if actual != expected {
            return Err(ApiError::unprocessable(format!(
                "provider definition `{}` is {} but node `{}` requires {}",
                node.provider,
                provider_capability_label(actual),
                node.id,
                provider_capability_label(expected)
            )));
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
    } else {
        None
    }
}

fn provider_capability_for_node(kind: NodeKind) -> Option<ProviderCapability> {
    match kind {
        NodeKind::Stt => Some(ProviderCapability::Stt),
        NodeKind::Llm => Some(ProviderCapability::Llm),
        NodeKind::Tool => Some(ProviderCapability::Tool),
        NodeKind::Tts => Some(ProviderCapability::Tts),
        _ => None,
    }
}

fn provider_capability_label(capability: ProviderCapability) -> &'static str {
    match capability {
        ProviderCapability::Stt => "stt",
        ProviderCapability::Llm => "llm",
        ProviderCapability::Tool => "tool",
        ProviderCapability::Tts => "tts",
    }
}

/// Built-in component descriptors.
#[must_use]
pub fn component_catalog() -> Vec<ProviderComponentDescriptor> {
    vec![
        ProviderComponentDescriptor {
            id: "openai.responses",
            label: "OpenAI Responses",
            kind: NodeKind::Llm,
            definition_variant: "openai_llm",
            schema: openai_llm_schema(),
        },
        ProviderComponentDescriptor {
            id: "openai.completions",
            label: "OpenAI Completions",
            kind: NodeKind::Llm,
            definition_variant: "openai_llm",
            schema: openai_llm_schema(),
        },
        ProviderComponentDescriptor {
            id: "wyoming",
            label: "Wyoming",
            kind: NodeKind::Stt,
            definition_variant: "wyoming_stt",
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
            kind: NodeKind::Stt,
            definition_variant: "openai_stt",
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
            kind: NodeKind::Tts,
            definition_variant: "openai_tts",
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
            kind: NodeKind::Tts,
            definition_variant: "wyoming_tts",
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
            kind: NodeKind::Tool,
            definition_variant: "mcp_tool",
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
            kind: NodeKind::Tool,
            definition_variant: "mcp_tool",
            schema: ComponentConfigSchema {
                properties: properties([(
                    "url",
                    string_property(Some(ComponentConfigFormat::Url), None),
                )]),
                required: vec!["url"],
            },
        },
        ProviderComponentDescriptor {
            id: "mcp.stdio",
            label: "MCP STDIO",
            kind: NodeKind::Tool,
            definition_variant: "mcp_tool",
            schema: ComponentConfigSchema {
                properties: properties([("command", string_property(None, None))]),
                required: vec!["command"],
            },
        },
    ]
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
    ComponentConfigProperty { value_type: ComponentConfigValueType::String, format, pattern }
}

fn boolean_property() -> ComponentConfigProperty {
    ComponentConfigProperty {
        value_type: ComponentConfigValueType::Boolean,
        format: None,
        pattern: None,
    }
}

fn properties<const N: usize>(
    entries: [(&'static str, ComponentConfigProperty); N],
) -> BTreeMap<&'static str, ComponentConfigProperty> {
    entries.into_iter().collect()
}
