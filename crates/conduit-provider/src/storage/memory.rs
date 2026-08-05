//! Memory store provider variants.

use serde::{Deserialize, Serialize};

use super::{redact_secret, ProviderSecret};

/// Memory store provider variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryVariant {
    /// A store that runs in the Conduit process, ranking by keyword.
    ///
    /// Named `builtin` for the same reason a transform's rules are: it needs
    /// nothing installed and nothing reached, so the whole of its configuration
    /// is how much to keep and whether to keep it at all.
    Builtin {
        /// Where records are kept across restarts.
        ///
        /// Absent means nothing is written anywhere. Ephemeral by default and
        /// deliberately so: a memory store that silently began recording every
        /// conversation to disk is not a default anyone should get by omission.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// How many records to keep, oldest dropped first.
        ///
        /// A bound rather than an option because nothing deletes a record: the
        /// runtime never forgets a conversation, so an unbounded in-process
        /// store grows for as long as the process runs.
        #[serde(default = "default_memory_capacity")]
        capacity: usize,
    },
    /// A store in PostgreSQL, ranking by embedding distance where it can.
    ///
    /// Separate from [`Builtin`](Self::Builtin) because it is a different
    /// *retrieval*, not a different place to put the same records: a question
    /// phrased in words the stored record never used is found by one and missed
    /// by the other. What it needs is a database, and ideally the `pgvector`
    /// extension in it; without the extension the store still works and ranks
    /// by keyword, which is why the extension is not part of this definition.
    ///
    /// Written as the one word the extension is named by rather than as the
    /// `snake_case` this enum would otherwise produce: an operator reading a
    /// stored definition should see `pgvector`, which is what they installed.
    #[serde(rename = "pgvector")]
    PgVector {
        /// PostgreSQL connection URL.
        url: String,
        /// Base URL of the OpenAI-compatible `/embeddings` endpoint.
        embedding_base_url: String,
        /// Credential for the embedding endpoint. Local servers usually need
        /// none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<ProviderSecret>,
        /// Which embedding model to ask for.
        embedding_model: String,
        /// How many numbers that model's vectors have.
        ///
        /// Supplied rather than discovered because it is needed before the first
        /// embedding exists: it is what the `vector(n)` column is declared with.
        /// A model's width is a property of the model, so whoever chose the
        /// model knows it.
        dimensions: usize,
    },
}

impl MemoryVariant {
    /// Returns a copy with inline secrets redacted.
    pub(super) fn redacted(&self) -> Self {
        match self {
            // A path is not a secret, and neither is a capacity.
            Self::Builtin { path, capacity } => {
                Self::Builtin { path: path.clone(), capacity: *capacity }
            }
            Self::PgVector {
                url,
                embedding_base_url,
                api_key,
                embedding_model,
                dimensions,
            } => Self::PgVector {
                // The URL is left whole, as every other variant's endpoint is.
                // A PostgreSQL URL can carry a password in its userinfo, which
                // is why the API refuses one rather than redacting it: a
                // credential the store cannot show back is a credential an
                // operator cannot correct.
                url: url.clone(),
                embedding_base_url: embedding_base_url.clone(),
                api_key: redact_secret(api_key),
                embedding_model: embedding_model.clone(),
                dimensions: *dimensions,
            },
        }
    }
}

/// How many records a built-in store keeps when the definition does not say.
///
/// Nothing forgets a record — the runtime never calls `forget_conversation` —
/// so this is the only thing bounding an in-process store. A thousand records
/// is on the order of a few weeks of household conversation and a few megabytes
/// of memory, and dropping the oldest is the right eviction for a store whose
/// point is recalling what was said recently.
const fn default_memory_capacity() -> usize {
    1_000
}

#[cfg(test)]
mod tests {
    use super::super::ProviderDefinitionVariant;
    use super::*;

    #[test]
    fn a_builtin_store_that_names_no_capacity_gets_one_anyway() {
        // Nothing deletes a record, so an absent bound would be an unbounded
        // store growing for as long as the process runs.
        let decoded: MemoryVariant =
            serde_json::from_str(r#"{"type":"builtin"}"#).expect("deserializes");
        assert_eq!(
            decoded,
            MemoryVariant::Builtin { path: None, capacity: default_memory_capacity() }
        );
    }

    #[test]
    fn a_builtin_store_that_names_no_path_writes_nowhere() {
        // Ephemeral by omission rather than persistent by omission: a store
        // that silently began recording every conversation to disk is not
        // something an operator should get by leaving a field blank.
        let decoded: MemoryVariant =
            serde_json::from_str(r#"{"type":"builtin","capacity":10}"#).expect("deserializes");
        assert_eq!(decoded, MemoryVariant::Builtin { path: None, capacity: 10 });
        let encoded = serde_json::to_string(&decoded).expect("serializes");
        assert_eq!(
            encoded, r#"{"type":"builtin","capacity":10}"#,
            "an absent path is absent rather than null"
        );
    }

    #[test]
    fn a_pgvector_variant_round_trips_through_json() {
        let variant = MemoryVariant::PgVector {
            url: "postgres://localhost/conduit".to_owned(),
            embedding_base_url: "http://localhost:11434/v1".to_owned(),
            api_key: None,
            embedding_model: "nomic-embed-text".to_owned(),
            dimensions: 768,
        };
        let encoded = serde_json::to_string(&variant).expect("serializes");
        let decoded: MemoryVariant = serde_json::from_str(&encoded).expect("deserializes");
        assert_eq!(decoded, variant);
    }

    #[test]
    fn a_pgvector_definitions_key_is_redacted_and_survives_an_update_that_omits_it() {
        // The same secret semantics every keyed definition has: a read never
        // shows the key, and saving what a read returned must not erase it.
        let stored = ProviderDefinitionVariant::Memory {
            variant: MemoryVariant::PgVector {
                url: "postgres://localhost/conduit".to_owned(),
                embedding_base_url: "https://api.openai.com/v1".to_owned(),
                api_key: Some(ProviderSecret::Inline { value: "sk-live".to_owned() }),
                embedding_model: "text-embedding-3-small".to_owned(),
                dimensions: 1536,
            },
        };

        let read = stored.redacted();
        assert_eq!(
            read,
            ProviderDefinitionVariant::Memory {
                variant: MemoryVariant::PgVector {
                    url: "postgres://localhost/conduit".to_owned(),
                    embedding_base_url: "https://api.openai.com/v1".to_owned(),
                    api_key: Some(ProviderSecret::Redacted),
                    embedding_model: "text-embedding-3-small".to_owned(),
                    dimensions: 1536,
                }
            }
        );

        let saved = read.with_secret_updates_from(Some(&stored));
        assert_eq!(saved, stored, "saving a redacted key keeps the stored one");
    }

    #[test]
    fn a_builtin_store_has_nothing_to_redact_and_survives_redaction_whole() {
        // A path is where records are, not a secret about them, and blanking it
        // would move the store on the next save.
        let variant = MemoryVariant::Builtin {
            path: Some("/var/lib/conduit/memory.json".to_owned()),
            capacity: 500,
        };
        assert_eq!(variant.redacted(), variant);
    }
}
