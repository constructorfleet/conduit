//! Memory store backends for Conduit.
//!
//! [`Builtin`] implements [`conduit_provider::memory::Memory`] with BM25 over
//! unigrams, entirely in process, needing no external service at all.
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
//! own deadline — [`Builtin`] has nothing to wait on — and an expired deadline
//! returns `Ok(Vec::new())` rather than an error: a store that is slow should
//! cost the turn a few seconds and no memory, not a few seconds *and* a warning.
//! Emptiness is likewise never an error — the first turn of every conversation
//! legitimately finds nothing.
//!
//! [`Memory::search`]: conduit_provider::memory::Memory::search

pub mod bm25;
pub mod builtin;

pub use builtin::{Builtin, BuiltinBuilder};

use std::time::Duration;

/// How long a search may take before the turn gives up on remembering.
///
/// A voice turn cannot wait on a store: the person who spoke is listening for
/// a reply, and the HTTP read timeouts elsewhere in the deployment are measured
/// in tens of seconds. Three seconds is long enough for an embedding round trip
/// and an indexed query on a healthy deployment, and short enough that an
/// unhealthy one is a pause rather than a hang.
pub const SEARCH_DEADLINE: Duration = Duration::from_secs(3);
