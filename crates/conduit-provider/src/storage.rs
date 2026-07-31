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
#[async_trait::async_trait]
pub trait PipelineStore: Send + Sync + 'static {
    /// Names of every stored pipeline, in a stable order.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend is unavailable.
    async fn list(&self) -> Result<Vec<String>>;

    /// Fetches one pipeline, or `None` if there is no such name.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend is unavailable or the stored
    /// definition cannot be read.
    async fn get(&self, name: &str) -> Result<Option<PipelineGraph>>;

    /// Stores a pipeline, returning `true` if it replaced an existing one.
    ///
    /// # Errors
    ///
    /// Returns an error if the name is unusable or the write fails.
    async fn put(&self, name: &str, graph: PipelineGraph) -> Result<bool>;

    /// Removes a pipeline, returning `true` if it existed.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend is unavailable.
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
