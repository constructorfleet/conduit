//! Pipelines in PostgreSQL.
//!
//! What a directory of files cannot do is serve several API replicas at once.
//! This backend exists for that: shared state, one writer wins, and every
//! replica sees the same pipelines.

use std::time::Duration;

use conduit_core::graph::PipelineGraph;
use conduit_core::{Error, Result};
use conduit_provider::storage::{validate_name, PipelineStore};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;

use crate::is_listable;

/// Migrations embedded at compile time, so a deployment needs no side-car.
static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// How long to wait for a connection before giving up.
///
/// The pool retries until this elapses, so the default of thirty seconds turns
/// a wrong URL into a server that hangs at startup instead of one that says
/// what is wrong. Ten seconds is long enough to ride out a database restart
/// and short enough to be a clear failure.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// How many connections one replica keeps.
///
/// Pipelines are read at connect time and written by hand; the traffic is
/// nothing like the audio path, so a small pool is plenty and leaves headroom
/// for the replicas sharing the database.
const MAX_CONNECTIONS: u32 = 5;

/// Pipelines stored in PostgreSQL.
#[derive(Debug, Clone)]
pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    /// Connects to `url` and applies any outstanding migrations.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the database cannot be reached or the
    /// migrations cannot be applied.
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(MAX_CONNECTIONS)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .connect(url)
            .await
            .map_err(|error| Error::Config(format!("cannot reach the database: {error}")))?;
        Self::from_pool(pool).await
    }

    /// Uses an existing pool, applying any outstanding migrations.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the migrations cannot be applied.
    pub async fn from_pool(pool: PgPool) -> Result<Self> {
        MIGRATIONS.run(&pool).await.map_err(|error| {
            Error::Config(format!("cannot apply database migrations: {error}"))
        })?;
        Ok(Self { pool })
    }

    /// The underlying pool, for callers that need their own queries.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Wraps a query failure.
    fn failure(error: sqlx::Error) -> Error {
        Error::provider("postgres", error)
    }
}

#[async_trait::async_trait]
impl PipelineStore for PostgresStore {
    async fn list(&self) -> Result<Vec<String>> {
        // Ordered in the database rather than in Rust: the index is already
        // sorted, and a replica should not depend on collation luck.
        let rows = sqlx::query("SELECT name FROM pipelines ORDER BY name")
            .fetch_all(&self.pool)
            .await
            .map_err(Self::failure)?;
        // The table is shared and writable by anything with the credentials,
        // so a row may carry a name `put` would refuse — and a caller turns
        // every listed name straight back into a `get`.
        Ok(rows
            .iter()
            .map(|row| row.get::<String, _>("name"))
            .filter(|name| is_listable(name))
            .collect())
    }

    async fn get(&self, name: &str) -> Result<Option<PipelineGraph>> {
        validate_name(name)?;
        let row = sqlx::query("SELECT graph FROM pipelines WHERE name = $1")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(Self::failure)?;

        let Some(row) = row else { return Ok(None) };
        let graph: serde_json::Value = row.try_get("graph").map_err(Self::failure)?;
        // A row that will not decode is broken, not absent — saying so is the
        // difference between "create it" and "fix it".
        serde_json::from_value(graph).map(Some).map_err(|error| {
            Error::Config(format!("the stored pipeline `{name}` is not valid: {error}"))
        })
    }

    async fn put(&self, name: &str, graph: PipelineGraph) -> Result<bool> {
        validate_name(name)?;
        let json = serde_json::to_value(&graph)
            .map_err(|error| Error::Config(format!("cannot encode the pipeline: {error}")))?;

        // One statement, so two replicas writing at once cannot interleave a
        // read and a write into a lost update. `xmax <> 0` is how PostgreSQL
        // reveals that the row already existed.
        let row = sqlx::query(
            "INSERT INTO pipelines (name, graph) VALUES ($1, $2)
             ON CONFLICT (name) DO UPDATE SET graph = EXCLUDED.graph, updated_at = now()
             RETURNING (xmax <> 0) AS replaced",
        )
        .bind(name)
        .bind(json)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::failure)?;

        row.try_get("replaced").map_err(Self::failure)
    }

    async fn remove(&self, name: &str) -> Result<bool> {
        validate_name(name)?;
        let result = sqlx::query("DELETE FROM pipelines WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(Self::failure)?;
        Ok(result.rows_affected() > 0)
    }
}
