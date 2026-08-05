//! Provider definition endpoints.

use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use conduit_provider::storage::{
    LlmVariant, McpTransport, MemoryVariant, ProviderCapability, ProviderDefinition,
    ProviderDefinitionVariant, ScriptEngine, SpeakerIdVariant, SttVariant, ToolVariant,
    TransformVariant, TtsVariant, WakeEngine,
};
use conduit_provider::Health;
use conduit_script::Script as ScriptTransform;
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
    /// Default request settings this configured provider carries.
    ///
    /// Read back rather than hidden: these are sampling controls and model
    /// options, not credentials, and an operator editing a configured provider
    /// has to see what it is already set to. Omitted when empty, so a
    /// definition that carries none reads exactly as it did before they
    /// existed.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

impl From<ProviderDefinition> for ProviderDefinitionView {
    fn from(definition: ProviderDefinition) -> Self {
        let definition = definition.redacted();
        Self {
            id: definition.id,
            label: definition.label,
            kind: definition.variant.capability(),
            variant: definition.variant,
            settings: definition.settings,
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

/// The new id a provider definition should be stored under.
#[derive(Debug, serde::Deserialize)]
pub struct ProviderRenameRequest {
    /// Id to move the definition to.
    pub id: String,
}

/// What a rename moved.
#[derive(Debug, Serialize)]
pub struct ProviderRenameResult {
    /// The definition as it now reads, under its new id.
    pub provider: ProviderDefinitionView,
    /// Pipelines whose references were rewritten, in listing order.
    ///
    /// Reported rather than counted so the console can tell an operator which
    /// of their pipelines their edit touched — a rename of a shared provider
    /// changes graphs they were not looking at.
    pub renamed_pipelines: Vec<String>,
}

/// `POST /v1/providers/{id}/rename` — moves one definition to a new id.
///
/// Renaming is its own operation because a provider id is not private to the
/// definition: pipelines name it. A `PUT` under the new id would leave the old
/// definition in place and every pipeline still pointing at it, which is what an
/// operator editing the id field experienced as their edit creating a second
/// provider.
///
/// # Errors
///
/// Returns 404 if there is no such definition, 409 if the new id is already
/// taken, and 422 if the new id is not one the store can use.
pub async fn rename(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
    JsonBody(request): JsonBody<ProviderRenameRequest>,
) -> Result<Json<ProviderRenameResult>, ApiError> {
    let definition = state
        .provider_definition(&id)
        .await
        .map_err(store_failure)?
        .ok_or_else(|| ApiError::not_found(format!("no provider definition `{id}`")))?;

    if request.id == id {
        return Ok(Json(ProviderRenameResult {
            provider: definition.into(),
            renamed_pipelines: Vec::new(),
        }));
    }
    // Checked before anything moves: a rename onto an occupied id would replace
    // a provider the operator did not mean to touch, and what it overwrote is
    // not recoverable.
    if state.provider_definition(&request.id).await.map_err(store_failure)?.is_some() {
        return Err(ApiError::conflict(format!(
            "a provider definition `{}` already exists",
            request.id
        )));
    }

    let renamed_pipelines =
        state.rename_provider_definition(&id, &request.id).await.map_err(store_failure)?;
    let renamed =
        state.provider_definition(&request.id).await.map_err(store_failure)?.ok_or_else(
            || ApiError::unavailable("renamed provider definition cannot be read"),
        )?;
    Ok(Json(ProviderRenameResult { provider: renamed.into(), renamed_pipelines }))
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
/// The catalogue is read off the provider's descriptor rather than fetched, so
/// this cannot fail on a provider that is registered: what a synthesizer can
/// say is settled when it is built.
///
/// # Errors
///
/// Returns 404 if there is no such definition, and 422 if the definition is not
/// a synthesizer.
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

    let voices = provider.descriptor().metadata.voices.clone();
    Ok(Json(ProviderVoices { provider: id, voices }))
}

/// The phrases a wake word detector offers.
#[derive(Debug, Serialize)]
pub struct ProviderPhrases {
    /// Provider definition the phrases belong to.
    pub provider: String,
    /// Phrases the detector reported, in the order it reported them.
    ///
    /// Empty is a real answer, for the same reason it is for voices: a Wyoming
    /// server scores whatever it loaded and enumerates nothing, and a satellite
    /// knows only what it was flashed with. The console falls back to typing,
    /// which is what an operator had before this endpoint existed.
    pub phrases: Vec<String>,
}

/// `GET /v1/providers/{id}/phrases` — the phrases one detector offers.
///
/// A detector that scores models in process knows exactly which phrases it has,
/// because they are the files it loaded. Asking is what lets the console offer
/// them rather than making an operator type a phrase and find out whether the
/// model exists when someone speaks to it.
///
/// # Errors
///
/// Returns 404 if there is no such definition, and 422 if the definition is not
/// a wake word detector.
pub async fn phrases(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProviderPhrases>, ApiError> {
    let definition = state
        .provider_definition(&id)
        .await
        .map_err(store_failure)?
        .ok_or_else(|| ApiError::not_found(format!("no provider definition `{id}`")))?;
    if definition.capability() != ProviderCapability::Wake {
        return Err(ApiError::unprocessable(format!(
            "provider definition `{id}` is not a wake word provider, so it has no phrases"
        )));
    }

    // A definition saved but not registered — its models would not load, or its
    // service was down when the snapshot was built — has nothing to enumerate.
    let Some(provider) = state.providers().and_then(|providers| providers.wake().get(&id))
    else {
        return Ok(Json(ProviderPhrases { provider: id, phrases: Vec::new() }));
    };

    let phrases = provider
        .descriptor()
        .metadata
        .phrases
        .iter()
        .map(|phrase| phrase.phrase.clone())
        .collect();
    Ok(Json(ProviderPhrases { provider: id, phrases }))
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
        return Ok(Json(status_from_health(kind, id, None, health, affected_pipelines)));
    }

    let Some(providers) = state.providers() else {
        return Ok(Json(unregistered_status(kind, id, affected_pipelines)));
    };

    // Asked through the capability the definition declares rather than through
    // a lookup per capability: which registry answers is the capability, and
    // spelling that out seven times is seven places to forget the eighth.
    let capability = runtime_capability(definition.capability());
    let Some(health) = providers.health(capability, &id).await else {
        return Ok(Json(unregistered_status(kind, id, affected_pipelines)));
    };
    let descriptor = providers
        .descriptors()
        .into_iter()
        .find(|(registered, key, _)| *registered == capability && *key == id)
        .map(|(_, _, descriptor)| descriptor);

    state.record_provider_reachability(&id, health.clone());
    Ok(Json(status_from_health(kind, id, descriptor.as_ref(), health, affected_pipelines)))
}

/// The runtime capability a stored definition's capability names.
const fn runtime_capability(capability: ProviderCapability) -> conduit_provider::Capability {
    match capability {
        ProviderCapability::Stt => conduit_provider::Capability::Stt,
        ProviderCapability::Llm => conduit_provider::Capability::Llm,
        ProviderCapability::Tts => conduit_provider::Capability::Tts,
        ProviderCapability::Transform => conduit_provider::Capability::Transform,
        ProviderCapability::Tool => conduit_provider::Capability::Tool,
        ProviderCapability::Wake => conduit_provider::Capability::Wake,
        ProviderCapability::SpeakerId => conduit_provider::Capability::SpeakerId,
        ProviderCapability::Memory => conduit_provider::Capability::Memory,
    }
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
        ProviderCapability::Transform => ProviderKind::Transform,
        ProviderCapability::Tool => ProviderKind::Tool,
        ProviderCapability::Wake => ProviderKind::Wake,
        ProviderCapability::SpeakerId => ProviderKind::SpeakerId,
        ProviderCapability::Memory => ProviderKind::Memory,
    }
}

/// Why a definition that detects in process cannot be used.
///
/// Shared by validation and by registration so that an operator saving one and
/// a server loading one already stored are told the same thing.
///
/// Only nanoWakeWord reaches this: microWakeWord has no `local` runtime to
/// name, and openWakeWord is implemented. nanoWakeWord runs in process
/// perfectly well — the reason it does not run in *this* process is that its
/// phrase models are recurrent, threading an LSTM hidden and cell state from
/// one chunk to the next, where openWakeWord's score a fixed window of
/// embeddings and keep nothing. That is a second detector, not a setting on
/// this one, and a definition should hear the difference now rather than
/// discover it as a detector that never fires.
pub(crate) fn local_wake_unavailable(engine: &str) -> String {
    format!(
        "`{engine}` cannot yet detect in process: its models are recurrent and Conduit only \
         scores openWakeWord's in-process. Point the definition at a Wyoming server instead."
    )
}

fn validate_provider_definition(definition: &ProviderDefinition) -> Result<(), ApiError> {
    match &definition.variant {
        ProviderDefinitionVariant::Llm {
            variant:
                LlmVariant::OpenAi { base_url, .. } | LlmVariant::Anthropic { base_url, .. },
        }
        | ProviderDefinitionVariant::Stt { variant: SttVariant::OpenAi { base_url, .. } }
        | ProviderDefinitionVariant::Tts { variant: TtsVariant::OpenAi { base_url, .. } } => {
            validate_http_url("base_url", base_url)?;
        }
        // A region rather than an endpoint: the SDK builds the URL, so what an
        // operator can get wrong here is the region name itself, and a wrong
        // one resolves to a host that does not exist.
        ProviderDefinitionVariant::Llm { variant: LlmVariant::Bedrock { region, .. } } => {
            validate_aws_region(region)?;
        }
        ProviderDefinitionVariant::Stt { variant: SttVariant::Wyoming { url, .. } }
        | ProviderDefinitionVariant::Tts { variant: TtsVariant::Wyoming { url, .. } } => {
            validate_tcp_url("url", url)?;
        }
        // Nothing to check on the recognizer: the model is a name the vendor
        // either knows or 4xxs about, and there is no endpoint to get wrong.
        ProviderDefinitionVariant::Stt { variant: SttVariant::ElevenLabs { .. } } => {}
        // The voice becomes a URL path segment with the account's credential
        // attached, so it is checked with the provider's own validator rather
        // than a second rule that could drift from it.
        ProviderDefinitionVariant::Tts { variant: TtsVariant::ElevenLabs { voice, .. } } => {
            if let Some(voice) = voice {
                refuse_config(conduit_elevenlabs::voice_id::validate(voice))?;
            }
        }
        // No endpoint to get wrong: there is one Deepgram. The model reaches a
        // query parameter, so it is checked with the provider's own validator
        // rather than a second rule here that could drift from it.
        ProviderDefinitionVariant::Tts { variant: TtsVariant::Deepgram { model, .. } } => {
            if let Some(model) = model {
                refuse_config(conduit_deepgram::model_id::validate(model))?;
            }
        }
        // No endpoint and no credential — Google's are discovered. What an
        // operator can get wrong is a language tag or a voice name, and both
        // reach a request, so both are checked with the provider's own
        // validators.
        ProviderDefinitionVariant::Stt { variant: SttVariant::Google { language, .. } } => {
            if let Some(language) = language {
                refuse_config(conduit_google::validate_language(language))?;
            }
        }
        ProviderDefinitionVariant::Tts { variant: TtsVariant::Google { language, voice } } => {
            if let Some(language) = language {
                refuse_config(conduit_google::validate_language(language))?;
            }
            if let Some(voice) = voice {
                refuse_config(conduit_google::validate_voice(voice))?;
            }
        }
        ProviderDefinitionVariant::Tts {
            variant: TtsVariant::MaryTts { url, voice, locale },
        } => {
            validate_http_url("url", url)?;
            if let Some(voice) = voice {
                refuse_config(conduit_marytts::validate::voice(&definition.id, voice))?;
            }
            if let Some(locale) = locale {
                refuse_config(conduit_marytts::validate::locale(&definition.id, locale))?;
            }
        }
        ProviderDefinitionVariant::SpeakerId {
            variant: SpeakerIdVariant::Http { base_url, .. },
        }
        | ProviderDefinitionVariant::SpeakerId {
            variant: SpeakerIdVariant::DiarizationServer { base_url, .. },
        } => {
            validate_http_url("base_url", base_url)?;
        }
        // Where a wake definition detects is the shape of the definition
        // rather than two fields that can disagree, so an engine on hardware
        // too small for it is no longer something to reject — it is no longer
        // something to write. What is left to check is the endpoint, when
        // there is one; a satellite has none, because the detector is flashed
        // onto the device.
        ProviderDefinitionVariant::Wake { variant } => {
            if let Some(url) = variant.wyoming_url() {
                validate_tcp_url("url", url)?;
            }
            // Detecting in process is openWakeWord's alone for now. The models
            // are checked when the detector is built, not here: whether a
            // directory holds them is not something a definition can say.
            if variant.local_models_dir().is_some()
                && variant.engine() != WakeEngine::OpenWakeWord
            {
                return Err(ApiError::unprocessable(local_wake_unavailable(
                    variant.engine().name(),
                )));
            }
        }
        ProviderDefinitionVariant::Tool { variant: ToolVariant::Mcp { transport } } => {
            validate_mcp_transport(transport)?;
        }
        // Built-in rules name nothing outside the process: no endpoint to
        // reach, no credential to check. An empty rule list is a definition an
        // operator is still filling in, and refusing to save one would be the
        // form arguing with them mid-edit.
        ProviderDefinitionVariant::Transform { variant: TransformVariant::Builtin { .. } } => {}
        // A script reaches nothing outside the process either, but unlike a
        // rule list it can be wrong in a way the operator wrote: a typo, or a
        // deadline the engine will not accept. Both are asked of
        // `conduit-script` rather than restated here, because a second copy of
        // the rule is how a form comes to accept a definition that fails to
        // build on the next server start.
        ProviderDefinitionVariant::Transform {
            variant: TransformVariant::Script { engine, source, timeout_ms },
        } => match engine {
            ScriptEngine::Rhai => {
                validate_script_timeout(*timeout_ms)?;
                ScriptTransform::check(source)
                    .map_err(|error| ApiError::unprocessable(error.to_string()))?;
            }
        },
        // A capacity of zero is a store that accepts every write and remembers
        // nothing. The store's own builder refuses it, so an operator who saves
        // one would get a definition that stores cleanly and fails to build on
        // the next start; refusing it here is the same rule, applied while the
        // form is still on screen.
        ProviderDefinitionVariant::Memory {
            variant: MemoryVariant::Builtin { capacity, .. },
        } => {
            if *capacity == 0 {
                return Err(ApiError::unprocessable(
                    "capacity must be at least 1: a store that keeps nothing remembers nothing"
                        .to_owned(),
                ));
            }
        }
        ProviderDefinitionVariant::Memory {
            variant: MemoryVariant::PgVector { url, embedding_base_url, dimensions, .. },
        } => {
            validate_postgres_url("url", url)?;
            validate_http_url("embedding_base_url", embedding_base_url)?;
            if *dimensions == 0 {
                return Err(ApiError::unprocessable(
                    "dimensions must be at least 1: it is the width of the vector column"
                        .to_owned(),
                ));
            }
        }
    }
    Ok(())
}

/// Refuses a scripted transform's deadline before it reaches the engine.
///
/// The bound is `conduit-script`'s, read from its own constants rather than
/// restated, so widening it there widens it here. What this adds is *when*: an
/// operator saving the definition is told, rather than the server refusing to
/// build it on the next start.
fn validate_script_timeout(timeout_ms: u64) -> Result<(), ApiError> {
    let requested = std::time::Duration::from_millis(timeout_ms);
    if requested < conduit_script::MIN_TIMEOUT || requested > conduit_script::MAX_TIMEOUT {
        return Err(ApiError::unprocessable(format!(
            "timeout_ms must be between {} and {}, got `{timeout_ms}`",
            conduit_script::MIN_TIMEOUT.as_millis(),
            conduit_script::MAX_TIMEOUT.as_millis(),
        )));
    }
    Ok(())
}

/// Checks that `value` is a PostgreSQL connection URL with no password in it.
///
/// The password is refused rather than redacted. Every other credential in a
/// definition lives in its own [`ProviderSecret`] field, which is what makes a
/// read response able to hide it and an update able to keep it; a password
/// buried in a URL's userinfo has neither, so it would be stored in the clear
/// and handed back in the clear to every operator who can read the provider
/// list. `PGPASSWORD`, a `.pgpass` file, or a password-less local role are all
/// ways to say it somewhere the definition does not.
///
/// [`ProviderSecret`]: conduit_provider::storage::ProviderSecret
fn validate_postgres_url(field: &str, value: &str) -> Result<(), ApiError> {
    let uri = validate_absolute_url(field, value)?;
    let scheme = uri.scheme_str().expect("absolute URL has a scheme");
    if !matches!(scheme, "postgres" | "postgresql") {
        return Err(ApiError::unprocessable(format!(
            "{field} must use postgres or postgresql, got `{scheme}`"
        )));
    }
    // Split on `@` rather than searching for a colon: a port is a colon too, and
    // only the userinfo half of the authority can carry a password.
    let carries_password = uri
        .authority()
        .and_then(|authority| authority.as_str().split_once('@'))
        .is_some_and(|(userinfo, _)| userinfo.contains(':'));
    if carries_password {
        return Err(ApiError::unprocessable(format!(
            "{field} must not carry a password: a definition cannot hide one written into the \
             URL. Use PGPASSWORD, a .pgpass file, or a role that needs none."
        )));
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

/// Checks that `region` is shaped like an AWS region name.
///
/// A shape check rather than a list: the regions are AWS's to add, and a build
/// that rejected one opened last month would be wrong in the direction an
/// operator cannot work around. What is caught here is the mistake that is
/// actually common — pasting an endpoint URL, or an ARN, into the field.
fn validate_aws_region(region: &str) -> Result<(), ApiError> {
    let shaped = !region.is_empty()
        && region.len() <= 32
        && region.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        && region.contains('-');
    if !shaped {
        return Err(ApiError::unprocessable(format!(
            "region must be an AWS region name such as `us-west-2`, got `{region}`"
        )));
    }
    Ok(())
}

/// Reports a provider crate's own validation failure as a rejected definition.
///
/// The alternative was a second copy of each rule here, which is how the API and
/// the provider come to disagree about what is valid: a definition the form
/// accepted would then fail to build on the next server start, long after the
/// operator stopped looking.
fn refuse_config<T>(checked: conduit_core::Result<T>) -> Result<(), ApiError> {
    checked.map(|_| ()).map_err(|error| ApiError::unprocessable(error.to_string()))
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
    descriptor: Option<&conduit_provider::Descriptor>,
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
        provider: descriptor.map(|descriptor| descriptor.id.clone()),
        label: descriptor.map(|descriptor| descriptor.label.clone()),
        version: descriptor.map(|descriptor| descriptor.version.clone()),
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
        // Nothing was built, so there is no implementation to state an
        // identity, a label or a version.
        provider: None,
        label: None,
        version: None,
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
