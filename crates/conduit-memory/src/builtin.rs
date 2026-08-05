//! Memory in this process, ranked with BM25.
//!
//! No service to reach, no schema to migrate, and no embedding model: a
//! deployment that wants the assistant to remember the last few exchanges gets
//! that by naming this and nothing else. What it gives up is semantic
//! retrieval — "the bins" will not find "recycling" — which is exactly the
//! trade a keyword store makes.
//!
//! # Persistence
//!
//! [`BuiltinBuilder::path`] is optional and defaults to absent. That default is
//! deliberate: a memory store holds what people said to the assistant, and one
//! that silently began writing transcripts to disk because disk was the easier
//! default would be a surprise of the worst kind. With no path this store is
//! genuinely ephemeral — the records live in this process and die with it.
//!
//! With a path the structure is identical and every write additionally dumps
//! the whole set beside the target and renames it, the same
//! write-temp-then-rename that [`conduit_store::FileStore`] uses, so a crash
//! mid-write leaves the previous file intact rather than a truncated one. The
//! file is read once at construction, and a file that will not parse is an
//! [`Error::Config`] from [`BuiltinBuilder::build`] rather than an empty store:
//! starting fresh would be silent data loss, and the operator who mistyped a
//! path deserves to hear about it at startup.
//!
//! [`conduit_store::FileStore`]: https://docs.rs/conduit-store

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use conduit_core::id::ConversationId;
use conduit_core::{Error, Result};
use conduit_provider::memory::{Match, Memory, Query, Record};
use conduit_provider::{Capability, Descriptor, Health, Provider};

use crate::bm25;

/// How many records this store keeps before dropping the oldest.
///
/// Nothing calls [`Memory::forget_conversation`] today, so conversation-scoped
/// records outlive their conversation and the set only grows. A thousand
/// records is enough to remember a long day of exchanges and small enough that
/// rescoring all of them per turn is not worth an index.
pub const DEFAULT_CAPACITY: usize = 1000;

/// One stored record and the tokens it was scored with.
///
/// Tokenised once at [`Memory::store`] rather than per search. That is not an
/// optimisation so much as the reason full rescoring is affordable: the per-turn
/// cost becomes a walk over pre-split tokens rather than a re-split of every
/// document.
#[derive(Debug, Clone)]
struct Entry {
    record: Record,
    tokens: Vec<String>,
}

/// What is written to disk, versioned so a later format can be recognised.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Persisted {
    /// Format version. Bumped only if the shape changes incompatibly.
    version: u32,
    /// Records oldest first, which is the order they are replayed in.
    records: Vec<Record>,
}

/// The format version this build writes and reads.
const FORMAT_VERSION: u32 = 1;

/// Builds a [`Builtin`] store.
#[derive(Debug, Clone)]
pub struct BuiltinBuilder {
    id: String,
    label: Option<String>,
    path: Option<PathBuf>,
    capacity: usize,
}

impl BuiltinBuilder {
    /// Sets the human-readable name operator screens show.
    ///
    /// Separate from the identity the store was built with: the identity is
    /// what a pipeline selects and what appears in metric labels and warnings,
    /// and this is only what a person reads.
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Keeps the records in `path` across restarts.
    ///
    /// Absent by default, and absent means nothing is written anywhere. See the
    /// module documentation for why that is the default and not the other way
    /// round.
    #[must_use]
    pub fn path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Caps how many records are kept, oldest dropped first.
    ///
    /// A capacity of zero is refused by [`BuiltinBuilder::build`]: it describes
    /// a store that accepts every write and remembers nothing, which is a
    /// configuration mistake that would otherwise present as a store that never
    /// recalls anything.
    #[must_use]
    pub const fn capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }

    /// Reads any persisted records and builds the store.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the capacity is zero, or if a configured
    /// file exists and cannot be read or parsed. An unparseable file is
    /// reported rather than replaced, because replacing it is data loss.
    pub async fn build(self) -> Result<Builtin> {
        if self.capacity == 0 {
            return Err(Error::Config(format!(
                "memory store `{}` was given a capacity of zero, so it would remember nothing",
                self.id
            )));
        }

        let records = match &self.path {
            // A directory that cannot be created is not a reason to refuse to
            // build: the in-process half of this store works regardless, and a
            // server that will not start because an optional file cannot be
            // written has turned a degraded capability into an outage.
            // `health` reports it, and every `store` returns the error.
            Some(path) if Builtin::prepare(path).await => Builtin::read(path).await?,
            Some(path) => {
                tracing::warn!(
                    store = %self.id,
                    path = %path.display(),
                    "cannot prepare the directory for remembered records; \
                     recall will work but nothing will persist"
                );
                Vec::new()
            }
            None => Vec::new(),
        };

        let mut entries = VecDeque::with_capacity(self.capacity.min(records.len().max(1)));
        for record in records {
            entries.push_back(Entry { tokens: bm25::tokens(&record.content), record });
        }
        // A file written by a build with a larger capacity than this one.
        while entries.len() > self.capacity {
            entries.pop_front();
        }

        let label = self.label.unwrap_or_else(|| self.id.clone());
        Ok(Builtin {
            descriptor: Descriptor::new(self.id, Capability::Memory)
                .with_label(label)
                .with_version(env!("CARGO_PKG_VERSION")),
            entries: Mutex::new(entries),
            path: self.path,
            capacity: self.capacity,
        })
    }
}

/// Memory kept in this process, ranked with BM25.
#[derive(Debug)]
pub struct Builtin {
    descriptor: Descriptor,
    /// Records oldest first, so dropping the oldest is a pop from the front and
    /// searching most-recent-first is a reversed walk.
    entries: Mutex<VecDeque<Entry>>,
    path: Option<PathBuf>,
    capacity: usize,
}

impl Builtin {
    /// A builder for a store identified as `id`.
    ///
    /// `id` is what a pipeline names and what appears in metric labels and
    /// warnings, so it must be stable across releases.
    #[must_use]
    pub fn builder(id: impl Into<String>) -> BuiltinBuilder {
        BuiltinBuilder { id: id.into(), label: None, path: None, capacity: DEFAULT_CAPACITY }
    }

    /// Where records are kept, if anywhere.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// How many records this store keeps.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Locks the record set, recovering from a poisoned lock.
    ///
    /// Every mutation is a push and at most one pop, so a panic elsewhere
    /// leaves the deque structurally sound. Refusing to remember anything for
    /// the rest of the process would be a worse outcome than continuing.
    fn lock(&self) -> MutexGuard<'_, VecDeque<Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ensures the directory holding `path` exists, reporting whether it does.
    ///
    /// Returns `false` rather than an error: the caller treats an unusable
    /// directory as "this store does not persist", not as a reason to refuse to
    /// build. What is *not* tolerated is a file that exists and will not parse,
    /// which is why that check is separate and does fail.
    async fn prepare(path: &Path) -> bool {
        match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                tokio::fs::create_dir_all(parent).await.is_ok()
            }
            // A bare file name, so the working directory is the directory.
            _ => true,
        }
    }

    /// Reads the persisted records, or none if the file is not there.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the file is present and cannot be read or
    /// parsed. "It is not there" invites creating it; "it is unreadable"
    /// invites fixing it, and only one of those is true here.
    async fn read(path: &Path) -> Result<Vec<Record>> {
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(Error::Config(format!(
                    "cannot read remembered records from `{}`: {error}",
                    path.display()
                )))
            }
        };

        let persisted: Persisted = serde_json::from_slice(&bytes).map_err(|error| {
            Error::Config(format!(
                "`{}` is not a valid memory store file: {error}",
                path.display()
            ))
        })?;
        if persisted.version != FORMAT_VERSION {
            return Err(Error::Config(format!(
                "`{}` is a version {} memory store file; this build reads version {FORMAT_VERSION}",
                path.display(),
                persisted.version
            )));
        }
        Ok(persisted.records)
    }

    /// Writes `records` to `path` via a temporary file and a rename.
    ///
    /// The whole set every time. A thousand short records is a file measured in
    /// hundreds of kilobytes, and an append log would need compaction, a reader
    /// that tolerates a torn last line, and its own tests — none of which buys
    /// anything at this size.
    async fn write(path: &Path, records: &[Record]) -> Result<()> {
        let persisted = Persisted { version: FORMAT_VERSION, records: records.to_vec() };
        let json = serde_json::to_vec(&persisted)
            .map_err(|error| Error::provider("builtin-memory", std::io::Error::other(error)))?;

        // Beside the target and renamed, so a crash mid-write leaves the
        // previous file intact rather than a truncated one. The same pattern
        // the pipeline file store uses, for the same reason. The directory was
        // created at construction, so nothing creates it here — a directory
        // that has since gone is a failure worth reporting, not one to paper
        // over by recreating it under a store that already read from it.
        let temporary = path.with_extension("json.tmp");
        tokio::fs::write(&temporary, &json)
            .await
            .map_err(|error| Self::failure(&temporary, "write", &error))?;
        tokio::fs::rename(&temporary, path)
            .await
            .map_err(|error| Self::failure(path, "replace", &error))
    }

    /// Wraps an I/O failure, naming the file and what was being attempted.
    fn failure(path: &Path, what: &str, error: &std::io::Error) -> Error {
        Error::provider(
            "builtin-memory",
            std::io::Error::new(
                error.kind(),
                format!("cannot {what} `{}`: {error}", path.display()),
            ),
        )
    }

    /// Persists the current set, if this store persists at all.
    ///
    /// Called with the lock released: the records are cloned out first so the
    /// I/O does not hold a mutex across an await.
    async fn flush(&self, records: Vec<Record>) -> Result<()> {
        match &self.path {
            Some(path) => Self::write(path, &records).await,
            None => Ok(()),
        }
    }
}

/// Whether `record` is a candidate for `query`.
///
/// Applied before any scoring: the filters are cheap and exact, and scoring a
/// record the caller may not see is wasted work — but more importantly, a
/// record excluded here never contributes to the document frequencies the
/// ranking is computed from, which is what makes a per-conversation search
/// score like a search of that conversation rather than of everything.
///
/// # A speaker-scoped record with no speaker
///
/// It is shared: it matches every speaker's query, and a query naming no
/// speaker too. The alternative — refusing the write — would mean a pipeline
/// bound to `scope: speaker` with nothing identifying the speaker silently
/// discarded every single turn, with no signal but a store that never
/// remembers anything. Sharing is visible; a store that quietly does nothing is
/// not. It is nonetheless a privacy-shaped default: if speaker identification
/// is not wired up, `scope: speaker` records are readable by everyone the
/// deployment serves.
fn matches(record: &Record, query: &Query) -> bool {
    if let Some(scope) = query.scope {
        if record.scope != scope {
            return false;
        }
    }
    if let Some(conversation) = query.conversation {
        // A record belonging to no conversation is not conversation-specific,
        // so it is in scope for any of them.
        if record.conversation.is_some_and(|stored| stored != conversation) {
            return false;
        }
    }
    if let Some(speaker) = query.speaker {
        // The shared case: a record with no speaker matches every speaker.
        if record.speaker.is_some_and(|stored| stored != speaker) {
            return false;
        }
    }
    true
}

/// Whether a conversation-scoped record is one that outlives its conversation.
fn belongs_to(record: &Record, conversation: ConversationId) -> bool {
    record.conversation == Some(conversation)
}

/// Whether `record` names `speaker`, treating an absent speaker as shared.
///
/// Public because it is the readable form of the privacy-shaped default
/// documented on [`matches`], and a caller auditing what a speaker can see
/// should be able to ask this directly rather than reconstruct it from a query.
#[must_use]
pub fn readable_by(record: &Record, speaker: conduit_core::id::SpeakerId) -> bool {
    record.speaker.is_none_or(|stored| stored == speaker)
}

#[async_trait::async_trait]
impl Provider for Builtin {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// Whether this store can serve.
    ///
    /// With no path there is nothing to reach and nothing that can be wrong,
    /// so it is [`Health::Healthy`]. With a path whose directory cannot be
    /// written it is [`Health::Degraded`] rather than [`Health::Unhealthy`]:
    /// the in-process half still stores and still recalls for as long as the
    /// process lives, which is most of what this store does. Reporting it
    /// unhealthy would take a store that is doing most of its job out of
    /// service entirely.
    async fn health(&self) -> Health {
        let Some(path) = &self.path else { return Health::Healthy };

        // The write path itself, minus the payload: a directory that rejects
        // this rejects the real write for the same reason.
        let probe = path.with_extension("json.probe");
        match tokio::fs::write(&probe, b"").await {
            Ok(()) => {
                let _ = tokio::fs::remove_file(&probe).await;
                Health::Healthy
            }
            Err(error) => Health::Degraded {
                reason: format!(
                    "cannot persist to `{}` ({error}); recall works until this process ends",
                    path.display()
                ),
            },
        }
    }
}

#[async_trait::async_trait]
impl Memory for Builtin {
    async fn store(&self, record: Record) -> Result<()> {
        let records = {
            let mut entries = self.lock();
            entries.push_back(Entry { tokens: bm25::tokens(&record.content), record });
            // Oldest first, so the bound is enforced by dropping from the
            // front. A capacity of zero is refused at build time, so this
            // cannot drop what was just written.
            while entries.len() > self.capacity {
                entries.pop_front();
            }
            // Cloned so the file write below does not hold the lock across an
            // await, which would serialise every turn behind the slowest disk.
            entries.iter().map(|entry| entry.record.clone()).collect()
        };
        self.flush(records).await
    }

    async fn search(&self, query: Query) -> Result<Vec<Match>> {
        // Everything here is in-process and lock-bounded, so no deadline is
        // imposed: there is nothing to wait on that a deadline could rescue,
        // and a timeout around a mutex would only hide a bug.
        let entries = self.lock();

        // Filter first. A record the caller may not see must not influence the
        // ranking of one it may: it would otherwise contribute to the document
        // frequencies every score is computed from.
        let candidates: Vec<&Entry> =
            entries.iter().rev().filter(|entry| matches(&entry.record, &query)).collect();

        let documents: Vec<&[String]> =
            candidates.iter().map(|entry| entry.tokens.as_slice()).collect();
        // Most recent first, and `rank`'s sort is stable, so equal scores come
        // back newest first.
        let ranked: Vec<Match> = bm25::rank(&documents, &query.text, query.limit)
            .into_iter()
            .map(|(index, score)| Match { record: candidates[index].record.clone(), score })
            .collect();
        drop(entries);
        Ok(ranked)
    }

    async fn forget_conversation(&self, conversation: ConversationId) -> Result<()> {
        let records = {
            let mut entries = self.lock();
            let before = entries.len();
            entries.retain(|entry| !belongs_to(&entry.record, conversation));
            if entries.len() == before {
                // Nothing matched, so nothing to rewrite. Documented as
                // succeeding even when nothing was stored, and rewriting the
                // file to prove it would be work for no change.
                return Ok(());
            }
            entries.iter().map(|entry| entry.record.clone()).collect()
        };
        self.flush(records).await
    }
}

#[cfg(test)]
mod tests {
    use conduit_core::id::SpeakerId;
    use conduit_core::memory::Scope;

    use super::*;

    fn record(content: &str) -> Record {
        Record {
            content: content.to_owned(),
            scope: Scope::Global,
            conversation: None,
            speaker: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn a_store_built_with_no_path_writes_nothing_anywhere() {
        let memory = Builtin::builder("recall").build().await.expect("builds");
        assert!(memory.path().is_none(), "no path was asked for, so none is used");
        memory.store(record("the recycling goes out on tuesday")).await.expect("stores");
    }

    #[tokio::test]
    async fn a_capacity_of_zero_is_refused_rather_than_remembering_nothing() {
        let error = Builtin::builder("recall")
            .capacity(0)
            .build()
            .await
            .expect_err("a store that remembers nothing is a mistake");
        assert!(error.to_string().contains("capacity"), "{error}");
    }

    #[tokio::test]
    async fn a_record_with_no_speaker_is_readable_by_every_speaker() {
        // The privacy-shaped default, asserted directly so it cannot be
        // changed without a test failing.
        let shared = Record { scope: Scope::Speaker, ..record("the bins are green") };
        assert!(readable_by(&shared, SpeakerId::new()));
        assert!(readable_by(&shared, SpeakerId::new()));
    }

    #[tokio::test]
    async fn a_record_naming_a_speaker_is_not_readable_by_another() {
        let mine = SpeakerId::new();
        let mine_only = Record {
            scope: Scope::Speaker,
            speaker: Some(mine),
            ..record("my dentist is on thursday")
        };
        assert!(readable_by(&mine_only, mine));
        assert!(!readable_by(&mine_only, SpeakerId::new()));
    }
}
