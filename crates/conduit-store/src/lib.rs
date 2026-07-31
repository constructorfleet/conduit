//! Storage backends for Conduit pipeline definitions.
//!
//! All three implement [`PipelineStore`] and are held to one shared contract
//! (`tests/conformance/mod.rs`), so which one a deployment uses is
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

/// Whether a name found in storage is one [`list`] may return.
///
/// Storage outlives the rule that governs it: a pipeline directory is editable
/// by hand and a table can be written to by anything holding the credentials,
/// so either may hold a name [`put`] would now refuse. Returning such a name
/// would hand a caller something [`get`] then rejects, so it is dropped — but
/// never quietly, because a name that cannot be served is something an operator
/// needs to know about.
///
/// [`list`]: conduit_provider::storage::PipelineStore::list
/// [`put`]: conduit_provider::storage::PipelineStore::put
/// [`get`]: conduit_provider::storage::PipelineStore::get
pub(crate) fn is_listable(name: &str) -> bool {
    match conduit_provider::storage::validate_name(name) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                pipeline = name,
                %error,
                "ignoring stored pipeline: its name cannot be served",
            );
            false
        }
    }
}
