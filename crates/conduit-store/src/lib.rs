//! Storage backends for Conduit pipeline definitions.
//!
//! Both implement [`PipelineStore`], so which one a deployment uses is
//! configuration rather than code.
//!
//! [`PipelineStore`]: conduit_provider::storage::PipelineStore

pub mod file;
pub mod memory;
#[cfg(feature = "postgres")]
pub mod postgres;

pub use file::FileStore;
pub use memory::MemoryStore;
#[cfg(feature = "postgres")]
pub use postgres::PostgresStore;
