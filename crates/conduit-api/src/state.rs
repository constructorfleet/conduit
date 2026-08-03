//! Shared application state.

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use conduit_core::bus::EventBus;
use conduit_core::graph::PipelineGraph;
use conduit_core::Result;
use conduit_mcp::{McpClient, McpTool};
use conduit_metrics::Metrics;
use conduit_openai::{OpenAi, OpenAiConfig, OpenAiStt, OpenAiTts};
use conduit_provider::storage::{
    McpTransport, PipelineStore, ProviderDefinition, ProviderDefinitionStore,
    ProviderDefinitionVariant, ProviderSecret,
};
use conduit_provider::wake::DeviceWake;
use conduit_provider::Health;
use conduit_runtime::{Providers, DEFAULT_IDLE_TIMEOUT};
use conduit_speaker::diarization_server::DiarizationServerSpeakerId;
use conduit_speaker::HttpSpeakerId;
use conduit_store::MemoryStore;
use conduit_wyoming::stt::WyomingStt;
use conduit_wyoming::tts::WyomingTts;
use conduit_wyoming::wake::WyomingWake;
use tokio::time::timeout;

use crate::auth::Access;
use crate::status::RuntimeStatus;
use crate::turns::{TurnHistory, TurnHistoryRetention};

/// How long MCP tool discovery may take while rebuilding the runtime provider
/// registry snapshot. A provider write waits on this, so it is far shorter
/// than the client's own per-request budget.
const MCP_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// State shared by every request handler. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    /// The process-wide event bus.
    pub bus: EventBus,
    pipelines: Arc<dyn PipelineStore>,
    provider_definitions: Arc<dyn ProviderDefinitionStore>,
    /// Providers available to pipelines, if any have been configured. A
    /// server without them still serves everything except conversations.
    providers: Arc<RwLock<Option<Arc<Providers>>>>,
    /// Results from explicit provider reachability checks.
    provider_reachability: Arc<RwLock<BTreeMap<String, Health>>>,
    /// Metrics derived from the bus, rendered by the scrape endpoint.
    metrics: Arc<Metrics>,
    /// Runtime status projection used by the Operator Console.
    status: RuntimeStatus,
    /// Server-owned turn reconstruction read model.
    turns: Arc<TurnHistory>,
    /// Who is allowed to call the service API.
    access: Arc<Access>,
    /// How long a turn may publish nothing before it is abandoned.
    turn_idle_timeout: Option<Duration>,
}

impl AppState {
    /// Creates state backed by `bus` and an in-memory pipeline store.
    #[must_use]
    pub fn new(bus: EventBus) -> Self {
        Self::with_store(bus, Arc::new(MemoryStore::new()))
    }

    /// Creates state backed by `bus` and `pipelines`.
    #[must_use]
    pub fn with_store(bus: EventBus, pipelines: Arc<dyn PipelineStore>) -> Self {
        let provider_definitions = Arc::new(MemoryStore::new());
        Self::with_stores(bus, pipelines, provider_definitions)
    }

    /// Creates state backed by explicit pipeline and provider definition stores.
    #[must_use]
    pub fn with_stores(
        bus: EventBus,
        pipelines: Arc<dyn PipelineStore>,
        provider_definitions: Arc<dyn ProviderDefinitionStore>,
    ) -> Self {
        let turns = TurnHistory::spawn(&bus, TurnHistoryRetention::default());
        Self {
            bus,
            pipelines,
            provider_definitions,
            providers: Arc::new(RwLock::new(None)),
            provider_reachability: Arc::new(RwLock::new(BTreeMap::new())),
            metrics: Arc::new(Metrics::new()),
            status: RuntimeStatus::new(),
            turns,
            access: Arc::new(Access::anonymous()),
            turn_idle_timeout: Some(DEFAULT_IDLE_TIMEOUT),
        }
    }

    /// Bounds how long a conversation may publish nothing before it is given up
    /// on.
    ///
    /// `None` removes the bound, which leaves a provider that stops answering
    /// holding the socket until the device disconnects. See
    /// [`Runner::with_idle_timeout`](conduit_runtime::Runner::with_idle_timeout).
    #[must_use]
    pub const fn with_turn_idle_timeout(mut self, idle: Option<Duration>) -> Self {
        self.turn_idle_timeout = idle;
        self
    }

    /// How long a conversation may publish nothing before it is given up on.
    #[must_use]
    pub const fn turn_idle_timeout(&self) -> Option<Duration> {
        self.turn_idle_timeout
    }

    /// Configures retention for completed turn reconstruction history.
    #[must_use]
    pub fn with_turn_history_retention(self, retention: TurnHistoryRetention) -> Self {
        self.turns.set_retention(retention);
        self
    }

    /// Requires callers to present a token from `access`.
    ///
    /// State starts out [`Access::anonymous`] because a library type has no way
    /// to know what a caller intends. What makes a *deployment* safe is the
    /// binary, which refuses to start without a token file unless the operator
    /// asked for an open server in as many words — see
    /// [`crate::config::access_from_env`].
    #[must_use]
    pub fn with_access(mut self, access: Access) -> Self {
        self.access = Arc::new(access);
        self
    }

    /// Who is allowed to call the service API.
    #[must_use]
    pub fn access(&self) -> &Access {
        &self.access
    }

    /// The metrics this server exposes.
    #[must_use]
    pub fn metrics(&self) -> Arc<Metrics> {
        Arc::clone(&self.metrics)
    }

    /// Runtime status projection used by the Operator Console.
    #[must_use]
    pub fn status(&self) -> RuntimeStatus {
        self.status.clone()
    }

    /// Server-owned turn reconstruction read model.
    #[must_use]
    pub fn turns(&self) -> Arc<TurnHistory> {
        Arc::clone(&self.turns)
    }

    /// Makes `providers` available to conversations.
    #[must_use]
    pub fn with_providers(self, providers: Providers) -> Self {
        *self.provider_lock() = Some(Arc::new(providers));
        self
    }

    /// The configured providers, if any.
    #[must_use]
    pub fn providers(&self) -> Option<Arc<Providers>> {
        self.providers.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn provider_lock(&self) -> std::sync::RwLockWriteGuard<'_, Option<Arc<Providers>>> {
        self.providers.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Latest explicit reachability results, keyed by provider definition id.
    #[must_use]
    pub fn provider_reachability(&self) -> BTreeMap<String, Health> {
        self.provider_reachability.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Records the result of an explicit provider reachability check.
    pub fn record_provider_reachability(&self, id: &str, health: Health) {
        self.provider_reachability
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned(), health);
    }

    fn clear_provider_reachability(&self, id: &str) {
        self.provider_reachability.write().unwrap_or_else(PoisonError::into_inner).remove(id);
    }

    /// Names of every stored pipeline, in order.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is unavailable.
    pub async fn pipeline_names(&self) -> Result<Vec<String>> {
        self.pipelines.list().await
    }

    /// Fetches a pipeline by name.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is unavailable or the definition cannot
    /// be read.
    pub async fn pipeline(&self, name: &str) -> Result<Option<PipelineGraph>> {
        self.pipelines.get(name).await
    }

    /// Stores a pipeline, returning `true` if it replaced an existing one.
    ///
    /// # Errors
    ///
    /// Returns an error if the name is unusable or the write fails.
    pub async fn put_pipeline(&self, name: &str, graph: PipelineGraph) -> Result<bool> {
        self.pipelines.put(name, graph).await
    }

    /// Removes a pipeline, returning `true` if it existed.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is unavailable.
    pub async fn remove_pipeline(&self, name: &str) -> Result<bool> {
        self.pipelines.remove(name).await
    }

    /// Provider definition ids, in order.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is unavailable.
    pub async fn provider_definition_ids(&self) -> Result<Vec<String>> {
        self.provider_definitions.list().await
    }

    /// Fetches one provider definition.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is unavailable or the definition cannot be read.
    pub async fn provider_definition(&self, id: &str) -> Result<Option<ProviderDefinition>> {
        self.provider_definitions.get(id).await
    }

    /// Stores a provider definition.
    ///
    /// # Errors
    ///
    /// Returns an error if the id is unusable or the write fails.
    pub async fn put_provider_definition(
        &self,
        id: &str,
        definition: ProviderDefinition,
    ) -> Result<bool> {
        let replaced = self.provider_definitions.put(id, definition).await?;
        // Cleared before the rebuild, because the rebuild starts the probe that
        // replaces this result: clearing afterwards would race it and could
        // discard the answer it just produced.
        self.clear_provider_reachability(id);
        self.rebuild_provider_snapshot().await?;
        Ok(replaced)
    }

    /// Removes a provider definition.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is unavailable.
    pub async fn remove_provider_definition(&self, id: &str) -> Result<bool> {
        let removed = self.provider_definitions.remove(id).await?;
        if removed {
            self.clear_provider_reachability(id);
            self.rebuild_provider_snapshot().await?;
        }
        Ok(removed)
    }

    async fn rebuild_provider_snapshot(&self) -> Result<()> {
        let mut snapshot = Providers::new();
        for id in self.provider_definition_ids().await? {
            let Some(definition) = self.provider_definition(&id).await? else {
                continue;
            };
            snapshot = register_definition(snapshot, &definition).await?;
        }
        *self.provider_lock() = Some(Arc::new(snapshot));
        self.spawn_reachability_probe();
        Ok(())
    }

    /// Asks every registered provider how it is, in the background.
    ///
    /// Reachability was only ever written by the explicit test endpoint, so a
    /// provider an operator created in the console read "no successful
    /// reachability check yet" however healthy it was — and said so again after
    /// every restart, since the results do not outlive the process. Probing
    /// here rather than while building a status snapshot keeps the cost tied to
    /// how often definitions change rather than to how often the console polls:
    /// a probe can mean a request to a paid API, and the console polls.
    ///
    /// Detached so that saving a definition does not wait on a provider that is
    /// slow or down, and failures are recorded rather than raised: an
    /// unreachable provider is a status to display, not an error that should
    /// fail the write that discovered it.
    fn spawn_reachability_probe(&self) {
        let state = self.clone();
        tokio::spawn(async move {
            let Some(providers) = state.providers() else {
                return;
            };
            let ids = match state.provider_definition_ids().await {
                Ok(ids) => ids,
                Err(error) => {
                    tracing::debug!(%error, "cannot list definitions to probe");
                    return;
                }
            };
            for id in ids {
                let health = if let Some(provider) = providers.stt().get(&id) {
                    provider.health().await
                } else if let Some(provider) = providers.llm().get(&id) {
                    provider.health().await
                } else if let Some(provider) = providers.tts().get(&id) {
                    provider.health().await
                } else if let Some(provider) = providers.tools().get(&id) {
                    provider.health().await
                } else {
                    continue;
                };
                tracing::debug!(provider = %id, ?health, "probed provider reachability");
                state.record_provider_reachability(&id, health);
            }
        });
    }

    /// Rebuilds runtime providers from stored provider definitions.
    ///
    /// # Errors
    ///
    /// Returns an error if definitions cannot be read or converted.
    pub async fn reload_provider_definitions(&self) -> Result<()> {
        self.rebuild_provider_snapshot().await
    }
}

async fn register_definition(
    providers: Providers,
    definition: &ProviderDefinition,
) -> Result<Providers> {
    let config = |base_url: &str, api_key: &Option<ProviderSecret>| OpenAiConfig {
        base_url: base_url.to_owned(),
        api_key: secret_value(api_key),
        name: definition.id.clone(),
        ..OpenAiConfig::default()
    };

    match &definition.variant {
        ProviderDefinitionVariant::OpenAiLlm {
            base_url,
            api_key,
            models,
            system_prompt,
            ..
        } => {
            let mut config = config(base_url, api_key);
            config.models = models.clone();
            config.system_prompt = system_prompt.clone();
            Ok(providers.with_llm(OpenAi::new(config)?))
        }
        ProviderDefinitionVariant::OpenAiStt { base_url, model, api_key, .. } => {
            let config = config(base_url, api_key);
            Ok(providers.with_stt(OpenAiStt::new(&config, model)?))
        }
        ProviderDefinitionVariant::OpenAiTts { base_url, model, api_key, voices } => {
            let config = config(base_url, api_key);
            let provider = OpenAiTts::new(&config, model)?;
            let provider = if voices.is_empty() {
                provider
            } else {
                provider.with_voices(
                    voices
                        .iter()
                        .map(|voice| conduit_provider::tts::Voice {
                            id: voice.clone(),
                            name: voice.clone(),
                            language: "en-US".to_owned(),
                        })
                        .collect(),
                )
            };
            Ok(providers.with_tts(provider))
        }
        ProviderDefinitionVariant::WyomingStt { url, model, streaming } => Ok(providers
            .with_stt(WyomingStt::new(&definition.id, url, model.clone(), *streaming)?)),
        ProviderDefinitionVariant::WyomingTts { url, voice, streaming } => Ok(providers
            .with_tts(WyomingTts::new(&definition.id, url, voice.clone(), *streaming)?)),
        ProviderDefinitionVariant::McpTool { transport } => {
            Ok(register_mcp_tools(providers, &definition.id, transport).await)
        }
        ProviderDefinitionVariant::WyomingWake { url, phrases, threshold_percent, .. } => {
            Ok(providers.with_wake(WyomingWake::new(
                &definition.id,
                url,
                phrases.clone(),
                f32::from(*threshold_percent) / 100.0,
            )?))
        }
        // A satellite that wakes itself still registers a detector, so that a
        // pipeline naming the stage resolves and the activation reaches the
        // event stream. It scores nothing: the device already decided.
        ProviderDefinitionVariant::DeviceWake { phrases, .. } => {
            Ok(providers.with_wake(DeviceWake::new(&definition.id, phrases.clone())))
        }
        ProviderDefinitionVariant::DiarizationServerSpeakerId {
            base_url,
            threshold_percent,
        } => Ok(providers.with_speaker(DiarizationServerSpeakerId::new(
            &definition.id,
            base_url,
            f32::from(*threshold_percent) / 100.0,
        )?)),
        ProviderDefinitionVariant::HttpSpeakerId {
            base_url,
            api_key,
            threshold_percent,
            ..
        } => Ok(providers.with_speaker(HttpSpeakerId::new(
            &definition.id,
            base_url,
            secret_value(api_key),
            f32::from(*threshold_percent) / 100.0,
        )?)),
    }
}

/// Registers whatever tools an MCP server currently advertises.
///
/// Discovery needs the server, but saving a provider definition must not: an
/// operator can configure an endpoint before the service behind it is running.
/// So a server that cannot be reached registers no tools and is logged, rather
/// than failing the write. A later reachability test or provider write
/// rediscovers them.
///
/// Every tool is registered as `<definition id>.<tool name>`. A server that
/// advertises exactly one tool is also registered under the definition id
/// itself, because the provider component catalog offers one MCP component per
/// definition and a graph node written from it names the definition.
async fn register_mcp_tools(
    providers: Providers,
    id: &str,
    transport: &McpTransport,
) -> Providers {
    let client = Arc::new(McpClient::new(transport.clone()));
    let discovery = timeout(MCP_DISCOVERY_TIMEOUT, client.list_tools()).await;
    let tools = match discovery {
        Ok(Ok(tools)) => tools,
        Ok(Err(error)) => {
            tracing::warn!(
                provider = id,
                error = %error,
                "MCP tool discovery failed; the provider definition is saved but registers \
                 no tools until the server can be reached"
            );
            return providers;
        }
        Err(_) => {
            tracing::warn!(
                provider = id,
                timeout_secs = MCP_DISCOVERY_TIMEOUT.as_secs(),
                "MCP tool discovery timed out; the provider definition is saved but \
                 registers no tools until the server answers"
            );
            return providers;
        }
    };

    let only_tool = tools.len() == 1;
    let mut providers = providers;
    for tool in tools {
        let qualified = format!("{id}.{}", tool.name);
        if only_tool {
            providers =
                providers.with_tool(McpTool::new(id, tool.clone(), Arc::clone(&client)));
        }
        providers = providers.with_tool(McpTool::new(qualified, tool, Arc::clone(&client)));
    }
    providers
}

fn secret_value(secret: &Option<ProviderSecret>) -> Option<String> {
    match secret {
        Some(ProviderSecret::Inline { value }) => Some(value.clone()),
        Some(ProviderSecret::External { .. } | ProviderSecret::Redacted) | None => None,
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").field("providers", &self.providers).finish_non_exhaustive()
    }
}
