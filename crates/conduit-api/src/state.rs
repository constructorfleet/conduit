//! Shared application state.

use std::sync::Arc;
use std::time::Duration;

use conduit_core::bus::EventBus;
use conduit_core::graph::PipelineGraph;
use conduit_core::Result;
use conduit_metrics::Metrics;
use conduit_provider::storage::PipelineStore;
use conduit_runtime::{Providers, DEFAULT_IDLE_TIMEOUT};
use conduit_store::MemoryStore;

use crate::auth::Access;

/// State shared by every request handler. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    /// The process-wide event bus.
    pub bus: EventBus,
    pipelines: Arc<dyn PipelineStore>,
    /// Providers available to pipelines, if any have been configured. A
    /// server without them still serves everything except conversations.
    providers: Option<Arc<Providers>>,
    /// Metrics derived from the bus, rendered by the scrape endpoint.
    metrics: Arc<Metrics>,
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
        Self {
            bus,
            pipelines,
            providers: None,
            metrics: Arc::new(Metrics::new()),
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

    /// Makes `providers` available to conversations.
    #[must_use]
    pub fn with_providers(mut self, providers: Providers) -> Self {
        self.providers = Some(Arc::new(providers));
        self
    }

    /// The configured providers, if any.
    #[must_use]
    pub fn providers(&self) -> Option<Arc<Providers>> {
        self.providers.clone()
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
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").field("providers", &self.providers).finish_non_exhaustive()
    }
}
