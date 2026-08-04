//! Shared application state.

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use conduit_core::bus::EventBus;
use conduit_core::graph::PipelineGraph;
use conduit_core::Result;
use conduit_mcp::McpClient;
use conduit_metrics::Metrics;
use conduit_provider::storage::{
    EnrolledSpeaker, McpTransport, PipelineStore, ProviderCapability, ProviderDefinition,
    ProviderDefinitionStore, ProviderDefinitionVariant, SpeakerRosterStore, ToolVariant,
};
use conduit_provider::Health;
use conduit_runtime::{Providers, DEFAULT_IDLE_TIMEOUT};
use conduit_store::MemoryStore;

use crate::auth::Access;
use crate::factory::Factories;
use crate::status::RuntimeStatus;
use crate::turns::{TurnHistory, TurnHistoryRetention};

/// State shared by every request handler. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    /// The process-wide event bus.
    pub bus: EventBus,
    pipelines: Arc<dyn PipelineStore>,
    provider_definitions: Arc<dyn ProviderDefinitionStore>,
    /// Who the deployment has named and enrolled.
    speakers: Arc<dyn SpeakerRosterStore>,
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
    /// What turns stored provider definitions into running providers.
    factories: Arc<Factories>,
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
            speakers: Arc::new(MemoryStore::new()),
            providers: Arc::new(RwLock::new(None)),
            provider_reachability: Arc::new(RwLock::new(BTreeMap::new())),
            metrics: Arc::new(Metrics::new()),
            status: RuntimeStatus::new(),
            turns,
            access: Arc::new(Access::anonymous()),
            turn_idle_timeout: Some(DEFAULT_IDLE_TIMEOUT),
            factories: Arc::new(Factories::builtin()),
        }
    }

    /// Keeps the speaker roster in `speakers` rather than in memory.
    ///
    /// Separate from the other stores because it is the one that holds
    /// people's names: a deployment may reasonably want it somewhere other
    /// than wherever its pipelines live.
    #[must_use]
    pub fn with_speaker_roster(mut self, speakers: Arc<dyn SpeakerRosterStore>) -> Self {
        self.speakers = speakers;
        self
    }

    /// Speaker ids in the roster, in order.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is unavailable.
    pub async fn speaker_ids(&self) -> Result<Vec<String>> {
        self.speakers.list().await
    }

    /// Fetches one roster entry.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is unavailable or the entry cannot be
    /// read.
    pub async fn speaker(&self, id: &str) -> Result<Option<EnrolledSpeaker>> {
        self.speakers.get(id).await
    }

    /// Stores a roster entry, returning `true` if it replaced one.
    ///
    /// # Errors
    ///
    /// Returns an error if the id is unusable or the write fails.
    pub async fn put_speaker(&self, speaker: EnrolledSpeaker) -> Result<bool> {
        self.speakers.put(&speaker.id.to_string(), speaker).await
    }

    /// Removes a roster entry, returning `true` if it existed.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is unavailable.
    pub async fn remove_speaker(&self, id: &str) -> Result<bool> {
        self.speakers.remove(id).await
    }

    /// Builds provider definitions with `factories` rather than with the
    /// vendors compiled into Conduit.
    ///
    /// What an embedder registers a vendor of its own through: the built-in
    /// list is the default, not the only one. A definition no factory here
    /// claims fails the load, so replacing the list narrows what a deployment
    /// can store.
    #[must_use]
    pub fn with_factories(mut self, factories: Factories) -> Self {
        self.factories = Arc::new(factories);
        self
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
            snapshot = self.factories.register(snapshot, &definition).await?;
            // Checked here rather than at the store because the schema lives on
            // the provider that was just built: a definition's default settings
            // must be ones the provider it configures said it accepts, or the
            // write that stored them — and the startup that loaded them — fails
            // loudly instead of a mistyped setting reaching a request.
            validate_definition_settings(&snapshot, &definition)?;
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
                // Every non-MCP factory registers its provider under the
                // definition id, so whichever registry lists the id is the
                // capability the provider supplies. Asking through the
                // registry rather than naming capabilities one at a time is
                // the whole point: a capability added after this loop was
                // written is probed without editing it — the named chain it
                // replaced skipped transforms until a regression test caught
                // it.
                let health = match providers
                    .capabilities()
                    .into_iter()
                    .find(|(_, names)| names.iter().any(|name| name == &id))
                {
                    Some((capability, _)) => providers.health(capability, &id).await,
                    // The registry holds no provider under the definition id.
                    // An MCP definition registers its tools as
                    // `<definition id>.<tool name>` rather than under the id
                    // itself — and none at all while its server is down — so it
                    // can never be found by the listing above. Probe the server
                    // through its transport, exactly as the explicit test
                    // endpoint does.
                    None => {
                        let Some(definition) =
                            state.provider_definition(&id).await.ok().flatten()
                        else {
                            continue;
                        };
                        let ProviderDefinitionVariant::Tool {
                            variant: ToolVariant::Mcp { transport },
                        } = &definition.variant
                        else {
                            continue;
                        };
                        Some(probe_mcp(transport).await)
                    }
                };
                // A definition whose provider registered nothing under its id
                // reads as unprobed rather than unhealthy: "no successful
                // reachability check yet" is the honest answer for a provider
                // that is not in the runtime.
                let Some(health) = health else {
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

/// Checks a definition's default settings against the schema the provider it
/// built declares.
///
/// The settings live on the definition but the schema lives on the provider, so
/// this runs after the provider is built and looks it up by the id it was
/// registered under. A definition with no default settings has nothing to
/// check. A capability whose provider is not registered under the definition id
/// — an MCP tool server registers each tool as `<id>.<tool>` — is skipped: its
/// per-tool schemas are a request-time concern, not a default on the definition.
///
/// # Errors
///
/// Returns [`conduit_core::Error::Config`] naming the offending setting.
fn validate_definition_settings(
    providers: &Providers,
    definition: &ProviderDefinition,
) -> Result<()> {
    if definition.settings.is_empty() {
        return Ok(());
    }
    let values = serde_json::Value::Object(definition.settings.clone());
    let id = &definition.id;
    match definition.capability() {
        ProviderCapability::Stt => {
            if let Some(provider) = providers.stt().get(id) {
                provider.descriptor().validate_settings(&values)?;
            }
        }
        ProviderCapability::Llm => {
            if let Some(provider) = providers.llm().get(id) {
                provider.descriptor().validate_settings(&values)?;
            }
        }
        ProviderCapability::Tts => {
            if let Some(provider) = providers.tts().get(id) {
                provider.descriptor().validate_settings(&values)?;
            }
        }
        ProviderCapability::Transform => {
            if let Some(provider) = providers.transform().get(id) {
                provider.descriptor().validate_settings(&values)?;
            }
        }
        ProviderCapability::Wake => {
            if let Some(provider) = providers.wake().get(id) {
                provider.descriptor().validate_settings(&values)?;
            }
        }
        ProviderCapability::SpeakerId => {
            if let Some(provider) = providers.speaker().get(id) {
                provider.descriptor().validate_settings(&values)?;
            }
        }
        // An MCP tool server registers no provider under the definition id, so
        // there is no single descriptor to check default settings against.
        ProviderCapability::Tool => {}
    }
    Ok(())
}

/// Lists an MCP server's tools: the narrowest check that proves the server is
/// reachable and speaks the protocol, without invoking anything.
pub(crate) async fn probe_mcp(transport: &McpTransport) -> Health {
    match McpClient::new(transport.clone()).list_tools().await {
        Ok(_) => Health::Healthy,
        Err(error) => Health::Unhealthy { reason: error.to_string() },
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").field("providers", &self.providers).finish_non_exhaustive()
    }
}
