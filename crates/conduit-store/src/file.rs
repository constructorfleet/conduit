//! A directory of pipeline definitions.
//!
//! One JSON file per pipeline: readable, diffable, and editable with the tools
//! anyone already has. For a local-first assistant that is usually the right
//! amount of database.

use std::path::{Path, PathBuf};

use conduit_core::graph::PipelineGraph;
use conduit_core::{Error, Result};
use conduit_provider::storage::{
    validate_name, EnrolledSpeaker, PipelineStore, ProviderDefinition, ProviderDefinitionStore,
    SpeakerRosterStore, VoxLink, VoxLinkStore,
};

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

    /// The names of every finished definition file in the directory, in order.
    ///
    /// One directory holds one kind of thing — pipelines, providers, and the
    /// roster each get their own — so what a file is called is what it is
    /// named, whichever store is asking.
    async fn names(&self) -> Result<Vec<String>> {
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
            // files are definitions.
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

    /// Reads and decodes one file, or `None` if it is not there.
    ///
    /// `what` names the kind of thing for the error message: a file that is
    /// present but unreadable is a different problem from one that is absent,
    /// and saying so is the difference between "create it" and "fix it".
    async fn read<T: serde::de::DeserializeOwned>(
        &self,
        name: &str,
        what: &str,
    ) -> Result<Option<T>> {
        let path = self.path(name)?;
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Self::failure(&path, &error)),
        };

        serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            Error::Config(format!("`{}` is not a valid {what}: {error}", path.display()))
        })
    }

    /// Encodes and writes one file, returning whether it replaced one.
    async fn write<T: serde::Serialize + Sync>(
        &self,
        name: &str,
        value: &T,
        what: &str,
    ) -> Result<bool> {
        let path = self.path(name)?;
        let existed = tokio::fs::try_exists(&path).await.unwrap_or(false);
        let json = serde_json::to_vec_pretty(value)
            .map_err(|error| Error::Config(format!("cannot encode the {what}: {error}")))?;

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

    /// Deletes one file, returning whether it existed.
    async fn delete(&self, name: &str) -> Result<bool> {
        let path = self.path(name)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(Self::failure(&path, &error)),
        }
    }
}

#[async_trait::async_trait]
impl PipelineStore for FileStore {
    async fn list(&self) -> Result<Vec<String>> {
        self.names().await
    }

    async fn get(&self, name: &str) -> Result<Option<PipelineGraph>> {
        self.read(name, "pipeline").await
    }

    async fn put(&self, name: &str, graph: PipelineGraph) -> Result<bool> {
        self.write(name, &graph, "pipeline").await
    }

    async fn remove(&self, name: &str) -> Result<bool> {
        self.delete(name).await
    }
}

#[async_trait::async_trait]
impl ProviderDefinitionStore for FileStore {
    async fn list(&self) -> Result<Vec<String>> {
        self.names().await
    }

    async fn get(&self, id: &str) -> Result<Option<ProviderDefinition>> {
        self.read(id, "provider definition").await
    }

    async fn put(&self, id: &str, definition: ProviderDefinition) -> Result<bool> {
        validate_name(id)?;
        if definition.id != id {
            return Err(Error::Config(format!(
                "provider definition id `{}` does not match route id `{id}`",
                definition.id
            )));
        }
        self.write(id, &definition, "provider definition").await
    }

    async fn remove(&self, id: &str) -> Result<bool> {
        self.delete(id).await
    }
}

#[async_trait::async_trait]
impl VoxLinkStore for FileStore {
    async fn list(&self) -> Result<Vec<String>> {
        self.names().await
    }

    async fn get(&self, peer_id: &str) -> Result<Option<VoxLink>> {
        self.read(peer_id, "vox link").await
    }

    async fn put(&self, peer_id: &str, link: VoxLink) -> Result<bool> {
        validate_name(peer_id)?;
        if link.peer_id != peer_id {
            return Err(Error::Config(format!(
                "vox link peer id `{}` does not match route id `{peer_id}`",
                link.peer_id
            )));
        }
        self.write(peer_id, &link, "vox link").await
    }

    async fn remove(&self, peer_id: &str) -> Result<bool> {
        self.delete(peer_id).await
    }
}

#[async_trait::async_trait]
impl SpeakerRosterStore for FileStore {
    async fn list(&self) -> Result<Vec<String>> {
        self.names().await
    }

    async fn get(&self, id: &str) -> Result<Option<EnrolledSpeaker>> {
        self.read(id, "speaker").await
    }

    async fn put(&self, id: &str, speaker: EnrolledSpeaker) -> Result<bool> {
        validate_name(id)?;
        if speaker.id.to_string() != id {
            return Err(Error::Config(format!(
                "speaker id `{}` does not match route id `{id}`",
                speaker.id
            )));
        }
        self.write(id, &speaker, "speaker").await
    }

    async fn remove(&self, id: &str) -> Result<bool> {
        self.delete(id).await
    }
}
