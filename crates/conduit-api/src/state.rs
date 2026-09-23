//! Shared application state.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};
use std::time::Duration;

use conduit_core::bus::EventBus;
use conduit_core::graph::PipelineGraph;
use conduit_core::Result;
use conduit_mcp::{McpClient, McpTool, McpToolInfo};
use conduit_metrics::Metrics;
use conduit_provider::storage::{
    EnrolledSpeaker, LinkedService, LinkedServiceStore, McpTransport, PipelineStore,
    ProviderCapability, ProviderDefinition, ProviderDefinitionStore, ProviderDefinitionVariant,
    SpeakerRosterStore, ToolVariant, TransformVariant,
};
use conduit_provider::Health;
use conduit_runtime::{Providers, DEFAULT_IDLE_TIMEOUT};
use conduit_store::MemoryStore;
use conduit_transform::dicta::DictaTransform;

use crate::auth::Access;
use crate::esphome::EsphomeDashboard;
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
    /// Conduit Vox peers this deployment is linked to.
    linked_services: Arc<dyn LinkedServiceStore>,
    link_token_cipher: Option<Arc<crate::link_token::LinkTokenCipher>>,
    wake_event_keys: Arc<Mutex<BTreeMap<String, VecDeque<String>>>>,
    /// Providers available to pipelines, if any have been configured. A
    /// server without them still serves everything except conversations.
    providers: Arc<RwLock<Option<Arc<Providers>>>>,
    /// Serializes snapshot rebuilds with asynchronous MCP tool-list refreshes.
    provider_snapshot_update: Arc<tokio::sync::Mutex<()>>,
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
    /// Long-lived MCP sessions that report tool-list changes.
    mcp_watchers: Arc<Mutex<BTreeMap<String, McpWatcher>>>,
    /// Where rendered firmware fragments are handed off, if anywhere.
    esphome: Option<Arc<EsphomeDashboard>>,
}

async fn watch_mcp_tools(
    providers: Weak<RwLock<Option<Arc<Providers>>>>,
    snapshot_update: Arc<tokio::sync::Mutex<()>>,
    definitions: Arc<dyn ProviderDefinitionStore>,
    id: String,
    transport: McpTransport,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
) {
    let client = Arc::new(McpClient::new(transport.clone()));
    let mut retry_delay = Duration::from_secs(1);
    loop {
        let connection = tokio::select! {
            _ = &mut cancel => return,
            connection = tokio::time::timeout(Duration::from_secs(30), client.connect_session()) => connection,
        };
        let mut session = match connection {
            Ok(Ok(session)) => session,
            Ok(Err(error)) => {
                tracing::warn!(provider = %id, %error, "MCP notification session failed to connect");
                if wait_mcp_retry(&mut cancel, retry_delay).await {
                    return;
                }
                retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
                continue;
            }
            Err(_) => {
                tracing::warn!(provider = %id, "MCP notification session connection timed out");
                if wait_mcp_retry(&mut cancel, retry_delay).await {
                    return;
                }
                retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
                continue;
            }
        };

        if !session.supports_tool_list_changed() {
            session.close().await;
            return;
        }

        match session.list_tools().await {
            Ok(tools) => {
                refresh_mcp_tools(
                    &providers,
                    &snapshot_update,
                    &definitions,
                    &id,
                    &transport,
                    &tools,
                    &client,
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(provider = %id, %error, "MCP notification session discovery failed");
                session.close().await;
                if wait_mcp_retry(&mut cancel, retry_delay).await {
                    return;
                }
                retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
                continue;
            }
        }

        loop {
            let notification = tokio::select! {
                _ = &mut cancel => {
                    session.close().await;
                    return;
                }
                notification = session.next_notification() => notification,
            };
            match notification {
                Ok(notification)
                    if notification.method == "notifications/tools/list_changed" =>
                {
                    retry_delay = Duration::from_secs(1);
                    match session.list_tools().await {
                        Ok(tools) => {
                            refresh_mcp_tools(
                                &providers,
                                &snapshot_update,
                                &definitions,
                                &id,
                                &transport,
                                &tools,
                                &client,
                            )
                            .await
                        }
                        Err(error) => tracing::warn!(
                            provider = %id,
                            %error,
                            "MCP tool-list refresh failed; retaining the current tools"
                        ),
                    }
                }
                Ok(notification) => tracing::debug!(
                    provider = %id,
                    method = %notification.method,
                    "ignoring unsupported MCP server notification"
                ),
                Err(error) => {
                    tracing::warn!(provider = %id, %error, "MCP notification stream ended");
                    break;
                }
            }
        }
        session.close().await;
        if wait_mcp_retry(&mut cancel, retry_delay).await {
            return;
        }
        retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
    }
}

async fn wait_mcp_retry(
    cancel: &mut tokio::sync::oneshot::Receiver<()>,
    delay: Duration,
) -> bool {
    tokio::select! {
        _ = cancel => true,
        () = tokio::time::sleep(delay) => false,
    }
}

async fn refresh_mcp_tools(
    providers: &Weak<RwLock<Option<Arc<Providers>>>>,
    snapshot_update: &Arc<tokio::sync::Mutex<()>>,
    definitions: &Arc<dyn ProviderDefinitionStore>,
    id: &str,
    transport: &McpTransport,
    tools: &[McpToolInfo],
    client: &Arc<McpClient>,
) {
    let _update = snapshot_update.lock().await;
    let definition = match definitions.get(id).await {
        Ok(Some(definition)) => definition,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(provider = %id, %error, "could not verify MCP definition before refreshing tools");
            return;
        }
    };
    if !matches!(
        definition.variant,
        ProviderDefinitionVariant::Tool { variant: ToolVariant::Mcp { transport: ref current } }
            if current == transport
    ) {
        return;
    }
    let Some(providers) = providers.upgrade() else { return };
    let mut current = providers.write().unwrap_or_else(PoisonError::into_inner);
    let Some(snapshot) = current.as_ref() else { return };
    let mut next = Arc::clone(snapshot).as_ref().clone();
    next.remove_tool_prefix(&format!("{id}."));
    for tool in tools {
        next = next.with_tool(McpTool::new(
            format!("{id}.{}", tool.name),
            tool.clone(),
            Arc::clone(client),
        ));
    }
    *current = Some(Arc::new(next));
}

struct McpWatcher {
    transport: McpTransport,
    cancel: tokio::sync::oneshot::Sender<()>,
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
            linked_services: Arc::new(MemoryStore::new()),
            link_token_cipher: None,
            wake_event_keys: Arc::new(Mutex::new(BTreeMap::new())),
            providers: Arc::new(RwLock::new(None)),
            provider_snapshot_update: Arc::new(tokio::sync::Mutex::new(())),
            provider_reachability: Arc::new(RwLock::new(BTreeMap::new())),
            metrics: Arc::new(Metrics::new()),
            status: RuntimeStatus::new(),
            turns,
            access: Arc::new(Access::anonymous()),
            turn_idle_timeout: Some(DEFAULT_IDLE_TIMEOUT),
            factories: Arc::new(Factories::builtin()),
            mcp_watchers: Arc::new(Mutex::new(BTreeMap::new())),
            esphome: None,
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

    /// Keeps Vox link records in `store` rather than in memory.
    #[must_use]
    pub fn with_linked_service_store(mut self, store: Arc<dyn LinkedServiceStore>) -> Self {
        self.linked_services = store;
        self
    }

    /// Configures authenticated encryption for peer bearers used by outbound
    /// side channels.
    pub fn with_peer_token_encryption_key(mut self, key: &[u8]) -> Result<Self> {
        self.link_token_cipher = Some(Arc::new(
            crate::link_token::LinkTokenCipher::new(key)
                .map_err(conduit_core::Error::Config)?,
        ));
        Ok(self)
    }

    pub(crate) fn encrypt_peer_token(
        &self,
        peer_id: &str,
        token: &str,
    ) -> Result<Option<String>> {
        self.link_token_cipher
            .as_ref()
            .map(|cipher| cipher.encrypt(peer_id, token).map_err(conduit_core::Error::Config))
            .transpose()
    }

    pub(crate) fn decrypt_peer_token(&self, peer_id: &str, ciphertext: &str) -> Result<String> {
        self.link_token_cipher
            .as_ref()
            .ok_or_else(|| {
                conduit_core::Error::Config(
                    "CONDUIT_LINK_TOKEN_ENCRYPTION_KEY is required to use a linked peer token"
                        .to_owned(),
                )
            })?
            .decrypt(peer_id, ciphertext)
            .map_err(conduit_core::Error::Config)
    }

    /// Peer ids of every linked Vox instance, in order.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is unavailable.
    pub async fn linked_service_ids(&self) -> Result<Vec<String>> {
        self.linked_services.list().await
    }

    /// Fetches one Vox link.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is unavailable or the entry cannot be read.
    pub async fn linked_service(&self, peer_id: &str) -> Result<Option<LinkedService>> {
        self.linked_services.get(peer_id).await
    }

    /// Records an idempotency key, retaining the latest 1024 per peer.
    /// Returns `true` when the key was already present.
    pub(crate) fn remember_wake_event(&self, peer_id: &str, key: &str) -> bool {
        let mut keys = self.wake_event_keys.lock().unwrap_or_else(PoisonError::into_inner);
        let peer_keys = keys.entry(peer_id.to_owned()).or_default();
        if peer_keys.iter().any(|existing| existing == key) {
            return true;
        }
        peer_keys.push_back(key.to_owned());
        if peer_keys.len() > 1024 {
            peer_keys.pop_front();
        }
        false
    }

    /// Stores a Vox link, returning `true` if it replaced one.
    ///
    /// # Errors
    ///
    /// Returns an error if the id is unusable or the write fails.
    pub async fn put_linked_service(&self, link: LinkedService) -> Result<bool> {
        let peer_id = link.peer_id.clone();
        self.linked_services.put(&peer_id, link).await
    }

    /// Removes a Vox link, returning `true` if it existed.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is unavailable.
    pub async fn remove_linked_service(&self, peer_id: &str) -> Result<bool> {
        self.linked_services.remove(peer_id).await
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

    /// Hands rendered fragments to `dashboard`.
    #[must_use]
    pub fn with_esphome(mut self, dashboard: EsphomeDashboard) -> Self {
        self.esphome = Some(Arc::new(dashboard));
        self
    }

    /// The ESPHome dashboard fragments are handed to, if one is configured.
    ///
    /// `None` is the ordinary state rather than a misconfiguration: the console
    /// still renders and downloads fragments, which per ADR-0019 is the path
    /// this one degrades to anyway.
    #[must_use]
    pub fn esphome(&self) -> Option<Arc<EsphomeDashboard>> {
        self.esphome.clone()
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

    /// Moves the definition stored under `from` to `to`, pointing every
    /// pipeline that named it at the new id. Answers with the pipelines that
    /// changed, in the order they were listed.
    ///
    /// A rename rather than a save-and-delete because a provider id is not
    /// private to its definition: pipelines name it. Saving under a new id and
    /// deleting the old one leaves every graph in between referencing a
    /// provider nothing answers to — and the delete would be refused for
    /// exactly that reason. So the definition and the references move together,
    /// and the runtime snapshot is rebuilt once at the end rather than once per
    /// write.
    ///
    /// Returns an empty list without writing anything if `from` is not stored.
    ///
    /// # Errors
    ///
    /// Returns an error if either id is unusable or any write fails. The order
    /// is chosen so that a failure part-way leaves references pointing at a
    /// definition that exists: the new definition is written first and the old
    /// one removed last.
    pub async fn rename_provider_definition(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>> {
        // Renaming to the id it already has is a no-op rather than a write
        // followed by a delete of what was just written.
        if from == to {
            return Ok(Vec::new());
        }
        let Some(definition) = self.provider_definition(from).await? else {
            return Ok(Vec::new());
        };
        self.provider_definitions
            .put(to, ProviderDefinition { id: to.to_owned(), ..definition })
            .await?;

        let mut renamed = Vec::new();
        for name in self.pipeline_names().await? {
            // A pipeline that will not parse is stepped over rather than
            // failing the rename, for the same reason the reference scan steps
            // over it: it cannot be read, so it cannot be rewritten, and
            // refusing the rename would leave an operator unable to fix either.
            let graph = match self.pipeline(&name).await {
                Ok(Some(graph)) => graph,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(
                        pipeline = %name,
                        %error,
                        "skipping unreadable pipeline while renaming a provider"
                    );
                    continue;
                }
            };
            let mut graph = graph;
            if graph.rename_provider(from, to) {
                self.pipelines.put(&name, graph).await?;
                renamed.push(name);
            }
        }

        self.provider_definitions.remove(from).await?;
        self.clear_provider_reachability(from);
        self.rebuild_provider_snapshot().await?;
        Ok(renamed)
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
        let _update = self.provider_snapshot_update.lock().await;
        let mut snapshot = Providers::new();
        let mut mcp_definitions = BTreeMap::new();
        for id in self.provider_definition_ids().await? {
            let Some(definition) = self.provider_definition(&id).await? else {
                continue;
            };
            if let ProviderDefinitionVariant::Tool { variant: ToolVariant::Mcp { transport } } =
                &definition.variant
            {
                mcp_definitions.insert(definition.id.clone(), transport.clone());
            }
            if let ProviderDefinitionVariant::Transform {
                variant: TransformVariant::Dicta { peer_id },
            } = &definition.variant
            {
                snapshot =
                    self.register_dicta_transform(snapshot, &definition, peer_id).await?;
            } else {
                snapshot = self.factories.register(snapshot, &definition).await?;
            }
            // Checked here rather than at the store because the schema lives on
            // the provider that was just built: a definition's default settings
            // must be ones the provider it configures said it accepts, or the
            // write that stored them — and the startup that loaded them — fails
            // loudly instead of a mistyped setting reaching a request.
            validate_definition_settings(&snapshot, &definition)?;
        }
        *self.provider_lock() = Some(Arc::new(snapshot));
        self.sync_mcp_watchers(mcp_definitions);
        self.spawn_reachability_probe();
        Ok(())
    }

    async fn register_dicta_transform(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
        peer_id: &str,
    ) -> Result<Providers> {
        let link = self.linked_service(peer_id).await?.ok_or_else(|| {
            conduit_core::Error::Config(format!("linked Dicta peer `{peer_id}` does not exist"))
        })?;
        if link.service_kind != conduit_link::LinkedServiceKind::Dicta
            || !link.capabilities.iter().any(|capability| capability == "dicta.transform")
        {
            return Err(conduit_core::Error::Config(format!(
                "linked peer `{peer_id}` does not advertise dicta.transform"
            )));
        }
        let ciphertext = link.peer_token_ciphertext.as_deref()
            .ok_or_else(|| conduit_core::Error::Config(format!(
                "linked Dicta peer `{peer_id}` has no encrypted peer token; re-link it with token encryption configured"
            )))?;
        let token = self.decrypt_peer_token(peer_id, ciphertext)?;
        let base = reqwest::Url::parse(&link.peer_base_url).map_err(|error| {
            conduit_core::Error::Config(format!(
                "linked Dicta peer `{peer_id}` has invalid base URL: {error}"
            ))
        })?;
        let endpoint = link
            .capability_endpoints
            .get("dicta.transform")
            .and_then(|metadata| metadata.get("url"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("/transform");
        let url = base.join(endpoint).map_err(|error| {
            conduit_core::Error::Config(format!(
                "linked Dicta peer `{peer_id}` has invalid transform endpoint: {error}"
            ))
        })?;
        if url.scheme() != base.scheme()
            || url.host_str() != base.host_str()
            || url.port() != base.port()
        {
            return Err(conduit_core::Error::Config(format!(
                "linked Dicta peer `{peer_id}` transform endpoint must stay on its advertised origin"
            )));
        }
        let transform = DictaTransform::new(
            &definition.id,
            &definition.label,
            peer_id,
            url.to_string(),
            token,
        )?;
        Ok(providers.with_transform(transform))
    }

    fn sync_mcp_watchers(&self, definitions: BTreeMap<String, McpTransport>) {
        let mut watchers = self.mcp_watchers.lock().unwrap_or_else(PoisonError::into_inner);
        let stale: Vec<_> = watchers
            .iter()
            .filter(|(id, watcher)| definitions.get(*id) != Some(&watcher.transport))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            if let Some(watcher) = watchers.remove(&id) {
                let _ = watcher.cancel.send(());
            }
        }

        for (id, transport) in definitions {
            if watchers.contains_key(&id) {
                continue;
            }
            let (cancel, cancel_rx) = tokio::sync::oneshot::channel();
            tokio::spawn(watch_mcp_tools(
                Arc::downgrade(&self.providers),
                Arc::clone(&self.provider_snapshot_update),
                Arc::clone(&self.provider_definitions),
                id.clone(),
                transport.clone(),
                cancel_rx,
            ));
            watchers.insert(id, McpWatcher { transport, cancel });
        }
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
        ProviderCapability::Vad => {
            if let Some(provider) = providers.vad().get(id) {
                provider.descriptor().validate_settings(&values)?;
            }
        }
        ProviderCapability::Memory => {
            if let Some(provider) = providers.memory().get(id) {
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

#[cfg(test)]
mod tests {
    use super::*;
    const CHANGING_MCP_SERVER: &str = r#"
import json, sys
lists = 0
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if method == "notifications/initialized":
        print(json.dumps({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}), flush=True)
    elif "id" in message:
        if method == "initialize":
            result = {"protocolVersion":"2025-03-26","capabilities":{"tools":{"listChanged":True}}}
        elif method == "tools/list":
            lists += 1
            name = "old_tool" if lists == 1 else "new_tool"
            result = {"tools":[{"name":name,"inputSchema":{"type":"object"}}]}
        else:
            result = {}
        print(json.dumps({"jsonrpc":"2.0","id":message["id"],"result":result}), flush=True)
"#;

    #[tokio::test]
    async fn a_dicta_provider_resolves_only_from_a_linked_capable_peer() {
        let state = AppState::new(EventBus::default())
            .with_peer_token_encryption_key(&[7; 32])
            .expect("key is valid");
        let ciphertext =
            state.encrypt_peer_token("dicta-office", "dicta-peer-token").unwrap().unwrap();
        state
            .put_linked_service(LinkedService {
                service_kind: conduit_link::LinkedServiceKind::Dicta,
                peer_id: "dicta-office".into(),
                peer_name: "Office Dicta".into(),
                peer_base_url: "http://dicta:8080".into(),
                sync_token_hash: "unused".into(),
                peer_token_hash: Some("peer-token-hash".into()),
                peer_token_ciphertext: Some(ciphertext),
                capabilities: vec!["dicta.transform".into()],
                capability_endpoints: Default::default(),
                provider_definition_id: String::new(),
                panel: None,
                granted_by: "operator".into(),
                granted_at: chrono::Utc::now(),
                last_seen: None,
                proxy_auth_bearer: None,
                reachability: conduit_link::Reachability::Unknown,
                last_probed_at: None,
            })
            .await
            .expect("linked peer stored");
        state
            .put_provider_definition(
                "dicta-transform",
                ProviderDefinition {
                    id: "dicta-transform".into(),
                    label: "Dicta transform".into(),
                    variant: ProviderDefinitionVariant::Transform {
                        variant: TransformVariant::Dicta { peer_id: "dicta-office".into() },
                    },
                    settings: Default::default(),
                },
            )
            .await
            .expect("provider resolves from the advertised peer");
        assert!(state.providers().unwrap().transform().get("dicta-transform").is_some());
    }

    #[tokio::test]
    async fn advertised_tool_list_changes_replace_only_that_servers_snapshot_tools() {
        let state = AppState::new(EventBus::default());
        state
            .put_provider_definition(
                "dynamic",
                ProviderDefinition {
                    id: "dynamic".to_owned(),
                    label: "Dynamic MCP".to_owned(),
                    variant: ProviderDefinitionVariant::Tool {
                        variant: ToolVariant::Mcp {
                            transport: McpTransport::Stdio {
                                command: "python3".to_owned(),
                                args: vec!["-c".to_owned(), CHANGING_MCP_SERVER.to_owned()],
                            },
                        },
                    },
                    settings: Default::default(),
                },
            )
            .await
            .expect("store MCP provider");

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let providers = state.providers().expect("provider snapshot");
                if providers.tools().get("dynamic.new_tool").is_some() {
                    assert!(providers.tools().get("dynamic.old_tool").is_none());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("server notification refreshes the registered tools");

        state.remove_provider_definition("dynamic").await.expect("remove provider");
    }
}
