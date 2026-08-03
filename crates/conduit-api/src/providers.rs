//! Provider definition endpoints.

use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use conduit_provider::storage::{
    LlmVariant, McpTransport, ProviderCapability, ProviderDefinition,
    ProviderDefinitionVariant, SpeakerIdVariant, SttVariant, ToolVariant, TtsVariant,
    WakeVariant,
};
use conduit_provider::Health;
use serde::Serialize;

use crate::auth::ManagementCaller;
use crate::error::JsonBody;
use crate::pipelines::{component_catalog, ProviderComponentCatalog};
use crate::status::{ProviderKind, ProviderStatus, ProviderStatusState};
use crate::{ApiError, AppState};

/// A provider definition as rendered through the management API.
#[derive(Debug, Serialize)]
pub struct ProviderDefinitionView {
    /// Stable provider id referenced by pipeline graph nodes.
    pub id: String,
    /// Human-readable label for operator screens.
    pub label: String,
    /// Runtime capability supplied by this definition.
    pub kind: ProviderCapability,
    /// Typed provider-specific settings, with inline secrets redacted.
    pub variant: conduit_provider::storage::ProviderDefinitionVariant,
}

impl From<ProviderDefinition> for ProviderDefinitionView {
    fn from(definition: ProviderDefinition) -> Self {
        let definition = definition.redacted();
        Self {
            id: definition.id,
            label: definition.label,
            kind: definition.variant.capability(),
            variant: definition.variant,
        }
    }
}

/// `GET /v1/catalog/providers` — provider component catalog.
pub async fn catalog(_caller: ManagementCaller) -> Json<ProviderComponentCatalog> {
    Json(ProviderComponentCatalog { components: component_catalog() })
}

/// `GET /v1/providers` — ids of every provider definition.
pub async fn list(
    _caller: ManagementCaller,
    State(state): State<AppState>,
) -> Result<Json<Vec<String>>, ApiError> {
    state.provider_definition_ids().await.map(Json).map_err(store_failure)
}

/// `GET /v1/providers/{id}` — one provider definition with redacted secrets.
pub async fn get(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProviderDefinitionView>, ApiError> {
    let definition = state
        .provider_definition(&id)
        .await
        .map_err(store_failure)?
        .ok_or_else(|| ApiError::not_found(format!("no provider definition `{id}`")))?;
    Ok(Json(definition.into()))
}

/// `PUT /v1/providers/{id}` — creates or replaces one provider definition.
pub async fn put(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
    JsonBody(definition): JsonBody<ProviderDefinition>,
) -> Result<(StatusCode, Json<ProviderDefinitionView>), ApiError> {
    if definition.id != id {
        return Err(ApiError::unprocessable(format!(
            "provider definition id `{}` does not match route id `{id}`",
            definition.id
        )));
    }
    let existing = state.provider_definition(&id).await.map_err(store_failure)?;
    let definition = definition.with_secret_updates_from(existing.as_ref());
    validate_provider_definition(&definition)?;
    let replaced =
        state.put_provider_definition(&id, definition.clone()).await.map_err(store_failure)?;
    let status = if replaced { StatusCode::OK } else { StatusCode::CREATED };
    Ok((status, Json(definition.into())))
}

/// `DELETE /v1/providers/{id}` — removes one provider definition when unused.
pub async fn delete(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let affected = affected_pipelines(&state, &id).await?;
    if !affected.is_empty() {
        return Ok((
            StatusCode::CONFLICT,
            Json(DeleteConflict {
                error: "conflict",
                detail: "provider definition is still referenced by pipelines",
                affected_pipelines: affected,
            }),
        )
            .into_response());
    }

    if state.remove_provider_definition(&id).await.map_err(store_failure)? {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Err(ApiError::not_found(format!("no provider definition `{id}`")))
    }
}

/// The voices a synthesizer offers.
#[derive(Debug, Serialize)]
pub struct ProviderVoices {
    /// Provider definition the voices belong to.
    pub provider: String,
    /// Voices the provider reported, in the order it reported them.
    ///
    /// Empty is a real answer: a provider that passes any voice name through
    /// to its backend has no catalogue to offer, and the console should let an
    /// operator type one rather than pretend there is nothing to choose.
    pub voices: Vec<conduit_provider::tts::Voice>,
}

/// `GET /v1/providers/{id}/voices` — the voices one synthesizer offers.
///
/// The pipeline editor asks so an operator picks a voice their provider
/// actually has, rather than typing one and finding out at the first reply.
///
/// # Errors
///
/// Returns 404 if there is no such definition, 422 if the definition is not a
/// synthesizer, and 503 if the provider is registered but its catalogue cannot
/// be retrieved.
pub async fn voices(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProviderVoices>, ApiError> {
    let definition = state
        .provider_definition(&id)
        .await
        .map_err(store_failure)?
        .ok_or_else(|| ApiError::not_found(format!("no provider definition `{id}`")))?;
    if definition.capability() != ProviderCapability::Tts {
        return Err(ApiError::unprocessable(format!(
            "provider definition `{id}` is not a text-to-speech provider, so it has no voices"
        )));
    }

    // A definition that is saved but not registered — its service was down
    // when the snapshot was built — has no catalogue to read. That is not a
    // failure of the request: the console falls back to a typed voice, which
    // is what an operator had before this endpoint existed.
    let Some(provider) = state.providers().and_then(|providers| providers.tts().get(&id))
    else {
        return Ok(Json(ProviderVoices { provider: id, voices: Vec::new() }));
    };

    let voices = provider.voices().await.map_err(|error| {
        ApiError::unavailable(format!("provider `{id}` could not list its voices: {error}"))
    })?;
    Ok(Json(ProviderVoices { provider: id, voices }))
}

/// `POST /v1/providers/{id}/test` — active reachability check for one provider.
pub async fn test(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProviderStatus>, ApiError> {
    let definition = state
        .provider_definition(&id)
        .await
        .map_err(store_failure)?
        .ok_or_else(|| ApiError::not_found(format!("no provider definition `{id}`")))?;
    let kind = provider_kind(definition.capability());
    let affected_pipelines = affected_pipelines(&state, &id).await?;

    // An MCP server is probed through its definition rather than the registry.
    // A server that was down when the definition was saved registers no tools,
    // and reporting that as "not registered" would hide the connection error
    // the operator needs to see. A probe that succeeds also rediscovers the
    // tools, so a provider becomes usable without another write.
    if let ProviderDefinitionVariant::Tool { variant: ToolVariant::Mcp { transport } } =
        &definition.variant
    {
        let health = crate::state::probe_mcp(transport).await;
        if health.is_usable() {
            state.reload_provider_definitions().await.map_err(store_failure)?;
        }
        state.record_provider_reachability(&id, health.clone());
        return Ok(Json(status_from_health(kind, id, health, affected_pipelines)));
    }

    let Some(providers) = state.providers() else {
        return Ok(Json(unregistered_status(kind, id, affected_pipelines)));
    };

    let health = match definition.capability() {
        ProviderCapability::Stt => match providers.stt().get(&id) {
            Some(provider) => Some(provider.health().await),
            None => None,
        },
        ProviderCapability::Llm => match providers.llm().get(&id) {
            Some(provider) => Some(provider.health().await),
            None => None,
        },
        ProviderCapability::Tts => match providers.tts().get(&id) {
            Some(provider) => Some(provider.health().await),
            None => None,
        },
        ProviderCapability::Tool => match providers.tools().get(&id) {
            Some(provider) => Some(provider.health().await),
            None => None,
        },
        ProviderCapability::Wake => match providers.wake().get(&id) {
            Some(provider) => Some(provider.health().await),
            None => None,
        },
        ProviderCapability::SpeakerId => match providers.speaker().get(&id) {
            Some(provider) => Some(provider.health().await),
            None => None,
        },
    };

    let Some(health) = health else {
        return Ok(Json(unregistered_status(kind, id, affected_pipelines)));
    };

    state.record_provider_reachability(&id, health.clone());
    Ok(Json(status_from_health(kind, id, health, affected_pipelines)))
}

#[derive(Serialize)]
struct DeleteConflict {
    error: &'static str,
    detail: &'static str,
    affected_pipelines: Vec<String>,
}

fn provider_kind(capability: ProviderCapability) -> ProviderKind {
    match capability {
        ProviderCapability::Stt => ProviderKind::Stt,
        ProviderCapability::Llm => ProviderKind::Llm,
        ProviderCapability::Tts => ProviderKind::Tts,
        ProviderCapability::Tool => ProviderKind::Tool,
        ProviderCapability::Wake => ProviderKind::Wake,
        ProviderCapability::SpeakerId => ProviderKind::SpeakerId,
    }
}

fn validate_provider_definition(definition: &ProviderDefinition) -> Result<(), ApiError> {
    match &definition.variant {
        ProviderDefinitionVariant::Llm { variant: LlmVariant::OpenAi { base_url, .. } }
        | ProviderDefinitionVariant::Stt { variant: SttVariant::OpenAi { base_url, .. } }
        | ProviderDefinitionVariant::Tts { variant: TtsVariant::OpenAi { base_url, .. } } => {
            validate_http_url("base_url", base_url)?;
        }
        ProviderDefinitionVariant::Stt { variant: SttVariant::Wyoming { url, .. } }
        | ProviderDefinitionVariant::Tts { variant: TtsVariant::Wyoming { url, .. } }
        | ProviderDefinitionVariant::Wake { variant: WakeVariant::Wyoming { url, .. } } => {
            validate_tcp_url("url", url)?;
        }
        ProviderDefinitionVariant::SpeakerId {
            variant: SpeakerIdVariant::Http { base_url, .. },
        }
        | ProviderDefinitionVariant::SpeakerId {
            variant: SpeakerIdVariant::DiarizationServer { base_url, .. },
        } => {
            validate_http_url("base_url", base_url)?;
        }
        // A satellite that wakes itself has no endpoint to check: the
        // detector is flashed onto the device, and the only thing that could
        // be wrong here is an engine too large for it to run.
        ProviderDefinitionVariant::Wake { variant: WakeVariant::Device { engine, .. } } => {
            if !engine.runs_on_device() {
                return Err(ApiError::unprocessable(format!(
                    "`{}` cannot run on a satellite; use a wyoming_wake definition for it, \
                     or microwakeword on the device",
                    engine.name()
                )));
            }
        }
        ProviderDefinitionVariant::Tool { variant: ToolVariant::Mcp { transport } } => {
            validate_mcp_transport(transport)?;
        }
    }
    Ok(())
}

fn validate_mcp_transport(transport: &McpTransport) -> Result<(), ApiError> {
    match transport {
        McpTransport::Sse { url } | McpTransport::StreamableHttp { url } => {
            validate_http_url("url", url)
        }
        McpTransport::Stdio { .. } => Ok(()),
    }
}

/// Wyoming speaks its own protocol over a plain TCP socket, so the scheme is
/// checked here rather than at registration: a definition that stores cleanly
/// must also be one the runtime can build a provider from.
fn validate_tcp_url(field: &str, value: &str) -> Result<(), ApiError> {
    let uri = validate_absolute_url(field, value)?;
    let scheme = uri.scheme_str().expect("absolute URL has a scheme");
    if scheme != "tcp" {
        return Err(ApiError::unprocessable(format!("{field} must use tcp, got `{scheme}`")));
    }
    if uri.port().is_none() {
        return Err(ApiError::unprocessable(format!("{field} must include a port")));
    }
    Ok(())
}

fn validate_http_url(field: &str, value: &str) -> Result<(), ApiError> {
    let uri = validate_absolute_url(field, value)?;
    let scheme = uri.scheme_str().expect("absolute URL has a scheme");
    if !matches!(scheme, "http" | "https") {
        return Err(ApiError::unprocessable(format!(
            "{field} must use http or https, got `{}`",
            scheme
        )));
    }
    Ok(())
}

fn validate_absolute_url(field: &str, value: &str) -> Result<Uri, ApiError> {
    let uri = value.parse::<Uri>().map_err(|error| {
        ApiError::unprocessable(format!("{field} is not a valid URL: {error}"))
    })?;
    if uri.host().is_none() {
        return Err(ApiError::unprocessable(format!("{field} must include a host")));
    }
    if uri.scheme_str().is_none() {
        return Err(ApiError::unprocessable(format!("{field} must include a URL scheme")));
    }
    Ok(uri)
}

fn status_from_health(
    kind: ProviderKind,
    id: String,
    health: Health,
    affects_pipelines: Vec<String>,
) -> ProviderStatus {
    let reachable = health.is_usable();
    let (state, message) = match health {
        Health::Healthy => (ProviderStatusState::Reachable, None),
        Health::Degraded { reason } => (ProviderStatusState::Reachable, Some(reason)),
        Health::Unhealthy { reason } => (ProviderStatusState::Configured, Some(reason)),
    };
    ProviderStatus {
        id,
        kind,
        state,
        configured: true,
        reachable,
        proven_by_turn: None,
        message,
        affects_pipelines,
        offers_tools: Vec::new(),
    }
}

fn unregistered_status(
    kind: ProviderKind,
    id: String,
    affects_pipelines: Vec<String>,
) -> ProviderStatus {
    ProviderStatus {
        id: id.clone(),
        kind,
        state: ProviderStatusState::Unavailable,
        configured: true,
        reachable: false,
        proven_by_turn: None,
        message: Some(format!(
            "provider definition `{id}` is not registered in the runtime provider snapshot"
        )),
        affects_pipelines,
        offers_tools: Vec::new(),
    }
}

async fn affected_pipelines(
    state: &AppState,
    provider_id: &str,
) -> Result<Vec<String>, ApiError> {
    // An MCP definition also registers each discovered tool as
    // `<id>.<tool name>`, so a node naming one of those tools is a reference
    // to this definition too — deleting it would break that pipeline.
    let qualified = format!("{provider_id}.");
    let references =
        |provider: &str| provider == provider_id || provider.starts_with(&qualified);
    let mut affected = Vec::new();
    for name in state.pipeline_names().await.map_err(store_failure)? {
        // A pipeline that will not parse is stepped over rather than failing
        // the scan. It cannot be read, so it cannot be shown to reference
        // anything — and refusing to delete a provider because some *other*
        // pipeline is corrupt leaves an operator unable to fix either one.
        let graph = match state.pipeline(&name).await {
            Ok(Some(graph)) => graph,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(
                    pipeline = %name,
                    %error,
                    "skipping unreadable pipeline while checking provider references"
                );
                continue;
            }
        };
        if graph.nodes.iter().any(|node| node.provider_references().into_iter().any(references))
        {
            affected.push(name);
        }
    }
    Ok(affected)
}

fn store_failure(error: conduit_core::Error) -> ApiError {
    match error {
        conduit_core::Error::Config(detail) => ApiError::unprocessable(detail),
        other => ApiError::unavailable(other.to_string()),
    }
}
