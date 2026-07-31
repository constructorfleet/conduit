//! Storage provider interface.
//!
//! Pipelines outlive the process that serves them, so where they live is a
//! deployment decision: a directory on a laptop, a shared database in a
//! cluster. The runtime only needs them to come back.

use conduit_core::graph::PipelineGraph;
use conduit_core::{Error, Result};

/// The longest a pipeline name may be.
const MAX_NAME: usize = 128;

/// Rejects names that are not safe to use as a storage key.
///
/// A name reaches this from a URL path, and a backend may turn it into a file
/// name — so `../../etc/passwd` must never get that far. Only characters that
/// are unambiguous in a path, a URL, and a SQL identifier are allowed.
///
/// # Errors
///
/// Returns [`Error::Config`] describing what is wrong with the name.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::Config("a pipeline name cannot be empty".to_owned()));
    }
    if name.len() > MAX_NAME {
        return Err(Error::Config(format!(
            "a pipeline name cannot be longer than {MAX_NAME} characters"
        )));
    }
    if let Some(bad) = name
        .chars()
        .find(|character| !matches!(character, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_'))
    {
        return Err(Error::Config(format!(
            "a pipeline name may only contain letters, digits, `-` and `_`; found `{bad}`"
        )));
    }
    Ok(())
}

/// Somewhere pipeline definitions are kept.
///
/// # Contract
///
/// Which backend a deployment uses is configuration, so it must not be
/// observable behaviour. Every implementation owes its callers all of this:
///
/// - **Names are validated on every method.** `list`, `get`, `put` and
///   `remove` all reject a name [`validate_name`] refuses. In particular
///   `get` and `remove` return [`Error::Config`] rather than `Ok(None)` /
///   `Ok(false)`: an unusable name is a malformed request, not a missing
///   pipeline, and reporting absence would tell the caller to go create one
///   under a name that can never be created. It must not depend on whether a
///   given backend happens to need the name to be safe — an in-memory map is
///   in no danger from `../../etc/passwd`, but it refuses it all the same, so
///   that a request accepted before a storage migration is accepted after it.
/// - **`list` only returns names `get` will answer for.** Storage outlives the
///   rule that governs it — a directory is editable by hand, a table is
///   writable by anything holding the credentials — so a name that predates
///   this contract may be sitting in it. Such entries are omitted rather than
///   returned as names that would then be rejected.
/// - **Absence is not failure.** `get` on a name that is merely not there is
///   `Ok(None)`, and `remove` on it is `Ok(false)`.
/// - **Unreadable is not absent.** A stored definition that will not decode is
///   an error, never `Ok(None)`: "it is not there" invites an editor to
///   overwrite something that is merely broken.
/// - **Round-tripping is lossless.** `get` after `put` returns an equal graph.
/// - **`put` reports replacement.** `true` when it overwrote an existing
///   pipeline, `false` when it created one.
///
/// The shared conformance suite in `conduit-store`
/// (`crates/conduit-store/tests/conformance/mod.rs`) is the executable form of
/// this list, and every backend is run through it.
///
/// [`Error::Config`]: conduit_core::Error::Config
#[async_trait::async_trait]
pub trait PipelineStore: Send + Sync + 'static {
    /// Names of every stored pipeline, sorted, excluding any that
    /// [`validate_name`] would refuse.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend is unavailable.
    async fn list(&self) -> Result<Vec<String>>;

    /// Fetches one pipeline, or `None` if there is no such name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if [`validate_name`] refuses `name`, or an
    /// error if the backend is unavailable or the stored definition cannot be
    /// read.
    async fn get(&self, name: &str) -> Result<Option<PipelineGraph>>;

    /// Stores a pipeline, returning `true` if it replaced an existing one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if [`validate_name`] refuses `name`, or an
    /// error if the write fails.
    async fn put(&self, name: &str, graph: PipelineGraph) -> Result<bool>;

    /// Removes a pipeline, returning `true` if it existed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if [`validate_name`] refuses `name`, or an
    /// error if the backend is unavailable.
    async fn remove(&self, name: &str) -> Result<bool>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_are_accepted() {
        for name in ["kitchen", "living-room", "desk_2", "A1"] {
            validate_name(name).unwrap_or_else(|error| panic!("{name} rejected: {error}"));
        }
    }

    #[test]
    fn path_traversal_is_rejected() {
        // This is the whole point: a name becomes a file name in some backends.
        for name in ["../etc/passwd", "..", "a/b", "a\\b", "a\0b"] {
            assert!(validate_name(name).is_err(), "{name} should be rejected");
        }
    }

    #[test]
    fn an_empty_name_is_rejected() {
        assert!(validate_name("").is_err());
    }

    #[test]
    fn an_overlong_name_is_rejected() {
        assert!(validate_name(&"a".repeat(MAX_NAME + 1)).is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME)).is_ok());
    }

    #[test]
    fn the_error_names_the_offending_character() {
        let error = validate_name("kitchen light").expect_err("spaces are not allowed");
        assert!(error.to_string().contains('`'), "{error}");
    }
}
