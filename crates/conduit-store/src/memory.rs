//! Pipelines kept in memory.
//!
//! Everything is lost when the process ends, which is exactly right for tests
//! and for a server nobody has configured storage for yet.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use conduit_core::graph::PipelineGraph;
use conduit_core::Result;
use conduit_provider::storage::{validate_name, PipelineStore};

use crate::is_listable;

/// An in-memory pipeline store.
#[derive(Debug, Default)]
pub struct MemoryStore {
    pipelines: Mutex<BTreeMap<String, PipelineGraph>>,
}

impl MemoryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the map, recovering from a poisoned lock.
    ///
    /// Every mutation is a single insert or remove, so a panic elsewhere
    /// leaves the map structurally sound; refusing to serve afterwards would
    /// be worse than continuing.
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, PipelineGraph>> {
        self.pipelines.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

// A `BTreeMap` would happily hold `../../etc/passwd`, so nothing here needs
// validation to work. It is validated anyway, on every method: a name this
// backend accepts and another refuses is a bug that only shows up after a
// deployment switches its storage, which is the worst time to find it.
#[async_trait::async_trait]
impl PipelineStore for MemoryStore {
    async fn list(&self) -> Result<Vec<String>> {
        // `put` is the only way in, so every key here is already usable; the
        // filter states that rather than trusting it.
        Ok(self.lock().keys().filter(|name| is_listable(name)).cloned().collect())
    }

    async fn get(&self, name: &str) -> Result<Option<PipelineGraph>> {
        validate_name(name)?;
        Ok(self.lock().get(name).cloned())
    }

    async fn put(&self, name: &str, graph: PipelineGraph) -> Result<bool> {
        validate_name(name)?;
        Ok(self.lock().insert(name.to_owned(), graph).is_some())
    }

    async fn remove(&self, name: &str) -> Result<bool> {
        validate_name(name)?;
        Ok(self.lock().remove(name).is_some())
    }
}
