//! Shared application state.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use conduit_core::bus::EventBus;
use conduit_core::graph::PipelineGraph;
use conduit_metrics::Metrics;
use conduit_runtime::Providers;

/// State shared by every request handler. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    /// The process-wide event bus.
    pub bus: EventBus,
    pipelines: Arc<RwLock<BTreeMap<String, PipelineGraph>>>,
    /// Providers available to pipelines, if any have been configured. A
    /// server without them still serves everything except conversations.
    providers: Option<Arc<Providers>>,
    /// Metrics derived from the bus, rendered by the scrape endpoint.
    metrics: Arc<Metrics>,
}

impl AppState {
    /// Creates state backed by `bus` and an empty pipeline store.
    ///
    /// The store is in-memory; persistence arrives with the storage backends
    /// and will replace this type's internals, not its API.
    #[must_use]
    pub fn new(bus: EventBus) -> Self {
        Self {
            bus,
            pipelines: Arc::new(RwLock::new(BTreeMap::new())),
            providers: None,
            metrics: Arc::new(Metrics::new()),
        }
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
    #[must_use]
    pub fn pipeline_names(&self) -> Vec<String> {
        self.read().keys().cloned().collect()
    }

    /// Fetches a pipeline by name.
    #[must_use]
    pub fn pipeline(&self, name: &str) -> Option<PipelineGraph> {
        self.read().get(name).cloned()
    }

    /// Stores a pipeline, returning `true` if it replaced an existing one.
    pub fn put_pipeline(&self, name: impl Into<String>, graph: PipelineGraph) -> bool {
        self.write().insert(name.into(), graph).is_some()
    }

    /// Removes a pipeline, returning `true` if it existed.
    pub fn remove_pipeline(&self, name: &str) -> bool {
        self.write().remove(name).is_some()
    }

    /// Reads the store, recovering from a poisoned lock.
    ///
    /// A panic while holding the lock leaves the map structurally sound —
    /// every mutation is a single insert or remove — so refusing to serve
    /// afterwards would be worse than continuing.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, PipelineGraph>> {
        self.pipelines.read().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Writes to the store, recovering from a poisoned lock.
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, BTreeMap<String, PipelineGraph>> {
        self.pipelines.write().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
