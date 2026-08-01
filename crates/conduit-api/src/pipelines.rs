//! Pipeline definition endpoints.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use bytes::Bytes;
use conduit_core::audio::AudioFormat;
use conduit_core::graph::{NodeKind, PipelineGraph};
use conduit_core::id::ConversationId;
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

/// A pipeline component that can be selected for a graph node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PipelineComponentDescriptor {
    /// Stable component id used by pipeline nodes as their provider name.
    pub id: &'static str,
    /// Human-readable label for operator screens.
    pub label: &'static str,
    /// What node kind this component can serve.
    pub kind: NodeKind,
    /// Configuration fields accepted by this component.
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

/// Component catalog response.
#[derive(Debug, Serialize)]
pub struct PipelineComponentCatalog {
    /// Known components, in stable display order.
    pub components: Vec<PipelineComponentDescriptor>,
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

/// `GET /v1/pipeline-components` — component configuration schema catalog.
pub async fn components(_caller: ManagementCaller) -> Json<PipelineComponentCatalog> {
    Json(PipelineComponentCatalog { components: component_catalog() })
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
    JsonBody(graph): JsonBody<PipelineGraph>,
) -> Result<Json<PipelineView>, ApiError> {
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
    validate_component_configs(&graph)?;
    let order = graph
        .topological_order()
        .map_err(|error| ApiError::unprocessable(error.to_string()))?
        .iter()
        .map(|node| node.id.clone())
        .collect();
    Ok(PipelineView { graph, order })
}

fn validate_component_configs(graph: &PipelineGraph) -> Result<(), ApiError> {
    let catalog = component_catalog();
    for node in &graph.nodes {
        let Some(component) = catalog
            .iter()
            .find(|component| component.id == node.provider && component.kind == node.kind)
        else {
            continue;
        };
        let config = match &node.config {
            serde_json::Value::Null => serde_json::Map::new(),
            serde_json::Value::Object(object) => object.clone(),
            _ => {
                return Err(ApiError::unprocessable(format!(
                    "node `{}` configuration must be an object",
                    node.id
                )));
            }
        };
        let missing = component
            .schema
            .required
            .iter()
            .copied()
            .filter(|field| {
                !config
                    .get(*field)
                    .is_some_and(|value| !value.as_str().is_some_and(str::is_empty))
            })
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(ApiError::unprocessable(format!(
                "node `{}` using component `{}` is missing required fields: {}",
                node.id,
                component.id,
                missing.join(", ")
            )));
        }
        for (field, property) in &component.schema.properties {
            let Some(value) = config.get(*field) else {
                continue;
            };
            let valid = match property.value_type {
                ComponentConfigValueType::String => value.is_string(),
                ComponentConfigValueType::Boolean => value.is_boolean(),
            };
            if !valid {
                return Err(ApiError::unprocessable(format!(
                    "node `{}` field `{field}` for component `{}` must be {}",
                    node.id,
                    component.id,
                    property.value_type.name()
                )));
            }
        }
    }
    Ok(())
}

impl ComponentConfigValueType {
    const fn name(self) -> &'static str {
        match self {
            Self::String => "a string",
            Self::Boolean => "a boolean",
        }
    }
}

/// Built-in component descriptors.
#[must_use]
pub fn component_catalog() -> Vec<PipelineComponentDescriptor> {
    vec![
        PipelineComponentDescriptor {
            id: "openai.responses",
            label: "OpenAI Responses",
            kind: NodeKind::Llm,
            schema: openai_llm_schema(),
        },
        PipelineComponentDescriptor {
            id: "openai.completions",
            label: "OpenAI Completions",
            kind: NodeKind::Llm,
            schema: openai_llm_schema(),
        },
        PipelineComponentDescriptor {
            id: "wyoming",
            label: "Wyoming",
            kind: NodeKind::Stt,
            schema: ComponentConfigSchema {
                properties: properties([
                    ("url", string_property(Some(ComponentConfigFormat::Url), None)),
                    ("model", string_property(None, None)),
                    ("streaming", boolean_property()),
                ]),
                required: vec!["url"],
            },
        },
        PipelineComponentDescriptor {
            id: "openai.transcription",
            label: "OpenAI Transcription",
            kind: NodeKind::Stt,
            schema: ComponentConfigSchema {
                properties: properties([
                    ("base_url", string_property(Some(ComponentConfigFormat::Url), None)),
                    ("model", string_property(None, None)),
                    ("stream", boolean_property()),
                ]),
                required: vec!["model"],
            },
        },
        PipelineComponentDescriptor {
            id: "openai.speech",
            label: "OpenAI Speech",
            kind: NodeKind::Tts,
            schema: ComponentConfigSchema {
                properties: properties([
                    ("base_url", string_property(Some(ComponentConfigFormat::Url), None)),
                    ("model", string_property(None, None)),
                ]),
                required: vec!["model"],
            },
        },
        PipelineComponentDescriptor {
            id: "wyoming.tts",
            label: "Wyoming TTS",
            kind: NodeKind::Tts,
            schema: ComponentConfigSchema {
                properties: properties([
                    ("url", string_property(Some(ComponentConfigFormat::Url), None)),
                    ("voice", string_property(None, None)),
                ]),
                required: vec!["url"],
            },
        },
        PipelineComponentDescriptor {
            id: "mcp.sse",
            label: "MCP SSE",
            kind: NodeKind::Tool,
            schema: ComponentConfigSchema {
                properties: properties([(
                    "url",
                    string_property(Some(ComponentConfigFormat::Url), None),
                )]),
                required: vec!["url"],
            },
        },
        PipelineComponentDescriptor {
            id: "mcp.streamable_http",
            label: "MCP Streamable HTTP",
            kind: NodeKind::Tool,
            schema: ComponentConfigSchema {
                properties: properties([(
                    "url",
                    string_property(Some(ComponentConfigFormat::Url), None),
                )]),
                required: vec!["url"],
            },
        },
        PipelineComponentDescriptor {
            id: "mcp.stdio",
            label: "MCP STDIO",
            kind: NodeKind::Tool,
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
            ("model", string_property(None, Some("[a-z0-9.]+"))),
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
