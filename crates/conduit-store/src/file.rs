//! A directory of pipeline definitions.
//!
//! One JSON file per pipeline: readable, diffable, and editable with the tools
//! anyone already has. For a local-first assistant that is usually the right
//! amount of database.

use std::path::{Path, PathBuf};

use conduit_core::graph::PipelineGraph;
use conduit_core::{Error, Result};
use conduit_provider::storage::{validate_name, PipelineStore};

use crate::is_listable;

/// The extension every stored definition carries.
const EXTENSION: &str = "json";

/// Pipelines stored as files in a directory.
#[derive(Debug, Clone)]
pub struct FileStore {
    directory: PathBuf,
}

impl FileStore {
    /// Opens `directory`, creating it if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created or is not a
    /// directory.
    pub async fn open(directory: impl Into<PathBuf>) -> Result<Self> {
        let directory = directory.into();
        tokio::fs::create_dir_all(&directory).await.map_err(|error| {
            Error::Config(format!(
                "cannot use `{}` for pipelines: {error}",
                directory.display()
            ))
        })?;
        Ok(Self { directory })
    }

    /// The path a pipeline is stored at.
    ///
    /// # Errors
    ///
    /// Returns an error if `name` is not usable as a file name.
    fn path(&self, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        Ok(self.directory.join(format!("{name}.{EXTENSION}")))
    }

    /// Wraps an I/O failure, naming the file it happened on.
    fn failure(path: &Path, error: &std::io::Error) -> Error {
        Error::provider(
            "file-store",
            std::io::Error::new(error.kind(), format!("{}: {error}", path.display())),
        )
    }
}

#[async_trait::async_trait]
impl PipelineStore for FileStore {
    async fn list(&self) -> Result<Vec<String>> {
        let mut entries = tokio::fs::read_dir(&self.directory)
            .await
            .map_err(|error| Self::failure(&self.directory, &error))?;

        let mut names = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| Self::failure(&self.directory, &error))?
        {
            let path = entry.path();
            // A directory may hold anything — notes, subdirectories, a
            // `.json.tmp` left by a crash mid-write. Only our own finished
            // files are pipelines.
            if path.extension().is_some_and(|extension| extension == EXTENSION) {
                if let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) {
                    if is_listable(name) {
                        names.push(name.to_owned());
                    }
                }
            }
        }

        names.sort();
        Ok(names)
    }

    async fn get(&self, name: &str) -> Result<Option<PipelineGraph>> {
        let path = self.path(name)?;
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Self::failure(&path, &error)),
        };

        serde_json::from_slice(&bytes)
            .map(Some)
            // A file that is present but unreadable is a different problem
            // from one that is absent, and saying so is the difference
            // between "create it" and "fix it".
            .map_err(|error| {
                Error::Config(format!("`{}` is not a valid pipeline: {error}", path.display()))
            })
    }

    async fn put(&self, name: &str, graph: PipelineGraph) -> Result<bool> {
        let path = self.path(name)?;
        let existed = tokio::fs::try_exists(&path).await.unwrap_or(false);

        let json = serde_json::to_vec_pretty(&graph)
            .map_err(|error| Error::Config(format!("cannot encode the pipeline: {error}")))?;

        // Write beside the target and rename, so a crash mid-write leaves the
        // previous definition intact rather than a truncated file.
        let temporary = path.with_extension("json.tmp");
        tokio::fs::write(&temporary, &json)
            .await
            .map_err(|error| Self::failure(&temporary, &error))?;
        tokio::fs::rename(&temporary, &path)
            .await
            .map_err(|error| Self::failure(&path, &error))?;

        Ok(existed)
    }

    async fn remove(&self, name: &str) -> Result<bool> {
        let path = self.path(name)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(Self::failure(&path, &error)),
        }
    }
}
