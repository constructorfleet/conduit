//! Memory store backends for Conduit.
//!
//! Two of them, for two very different deployments:
//!
//! | Backend | Ranking | Needs |
//! | --- | --- | --- |
//! | [`Builtin`] | BM25 over unigrams | nothing at all |
//! | [`PgVector`] | cosine distance over an embedding, keyword when degraded | PostgreSQL, ideally with `pgvector` |
//!
//! Both implement [`conduit_provider::memory::Memory`], so which one a
//! deployment runs is configuration rather than behaviour — with one honest
//! exception: a keyword store and a vector store retrieve genuinely different
//! records for the same question, and no amount of shared contract hides that.
//!
//! ```no_run
//! # use conduit_memory::Builtin;
//! # use conduit_provider::memory::{Memory, Query, Record};
//! # use conduit_core::memory::Scope;
//! # async fn example() -> conduit_core::Result<()> {
//! // No path, so nothing is written anywhere.
//! let memory = Builtin::builder("recall").build().await?;
//! memory
//!     .store(Record {
//!         content: "the recycling goes out on tuesday".to_owned(),
//!         scope: Scope::Global,
//!         conversation: None,
//!         speaker: None,
//!         metadata: serde_json::Value::Null,
//!     })
//!     .await?;
//!
//! let found = memory.search(Query::new("when is the recycling collected", 5)).await?;
//! assert_eq!(found.len(), 1);
//! # Ok(())
//! # }
//! ```
//!
//! # The critical path
//!
//! [`Memory::search`] is awaited once per turn, before the person who spoke
//! hears anything. Any backend that can wait on something therefore imposes its
//! own deadline — [`PgVector`] does, [`Builtin`] has nothing to wait on — and an
//! expired deadline returns `Ok(Vec::new())` rather than an error: a store that
//! is slow should cost the turn a few seconds and no memory, not a few seconds
//! *and* a warning. Emptiness is likewise never an error — the first turn of
//! every conversation legitimately finds nothing.
//!
//! [`Memory::search`]: conduit_provider::memory::Memory::search

pub mod bm25;
pub mod builtin;
#[cfg(feature = "postgres")]
pub mod embed;
#[cfg(feature = "postgres")]
pub mod pgvector;

pub use builtin::{Builtin, BuiltinBuilder};
#[cfg(feature = "postgres")]
pub use embed::Embedder;
#[cfg(feature = "postgres")]
pub use pgvector::{PgVector, PgVectorBuilder};

use std::time::Duration;

/// How long a search may take before the turn gives up on remembering.
///
/// A voice turn cannot wait on a store: the person who spoke is listening for
/// a reply, and the HTTP read timeouts elsewhere in the deployment are measured
/// in tens of seconds. Three seconds is long enough for an embedding round trip
/// and an indexed query on a healthy deployment, and short enough that an
/// unhealthy one is a pause rather than a hang.
pub const SEARCH_DEADLINE: Duration = Duration::from_secs(3);
