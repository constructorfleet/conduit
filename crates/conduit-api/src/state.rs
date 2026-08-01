//! Shared application state.

use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use conduit_core::bus::EventBus;
use conduit_core::graph::PipelineGraph;
use conduit_core::Result;
use conduit_metrics::Metrics;
use conduit_openai::{OpenAi, OpenAiConfig, OpenAiStt, OpenAiTts};
use conduit_provider::storage::{
    PipelineStore, ProviderDefinition, ProviderDefinitionStore, ProviderDefinitionVariant,
    ProviderSecret,
};
use conduit_runtime::{Providers, DEFAULT_IDLE_TIMEOUT};
use conduit_store::MemoryStore;

use crate::auth::Access;
use crate::status::RuntimeStatus;
use crate::turns::{TurnHistory, TurnHistoryRetention};

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
            snapshot = register_definition(snapshot, &definition)?;
        }
        *self.provider_lock() = Some(Arc::new(snapshot));
        Ok(())
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

fn register_definition(
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
        ProviderDefinitionVariant::OpenAiLlm { base_url, api_key, models, .. } => {
            let mut config = config(base_url, api_key);
            config.models = models.clone();
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
        ProviderDefinitionVariant::WyomingStt { .. }
        | ProviderDefinitionVariant::WyomingTts { .. }
        | ProviderDefinitionVariant::McpTool { .. } => Ok(providers),
    }
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
