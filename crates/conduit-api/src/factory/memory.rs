//! Memory stores, in this process and in PostgreSQL.
//!
//! Two factories rather than one, because the two stores share no
//! configuration: the built-in one names a file and a bound, and the pgvector
//! one names a database and an embedding endpoint. A single factory over both
//! would be a `match` with nothing above it, which is the shape
//! [`ProviderFactory`] exists to remove.

use conduit_core::Result;
use conduit_memory::Builtin;
use conduit_provider::storage::{MemoryVariant, ProviderDefinition, ProviderDefinitionVariant};
use conduit_runtime::Providers;

use super::{unclaimed, ProviderFactory};

/// Remembering in this process, ranked by keyword.
///
/// Reaches nothing: the whole configuration is where to keep the records and
/// how many of them to keep.
pub struct BuiltinMemory;

#[async_trait::async_trait]
impl ProviderFactory for BuiltinMemory {
    fn name(&self) -> &'static str {
        "builtin-memory"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::Memory { variant: MemoryVariant::Builtin { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let ProviderDefinitionVariant::Memory {
            variant: MemoryVariant::Builtin { path, capacity },
        } = &definition.variant
        else {
            return Err(unclaimed(self.name(), definition));
        };

        let mut builder =
            Builtin::builder(&definition.id).label(&definition.label).capacity(*capacity);
        // Absent means nothing is written anywhere, which is the builder's own
        // default — so an absent path is a call not made rather than a path of
        // `""`, which would be a file in the working directory.
        if let Some(path) = path {
            builder = builder.path(path);
        }
        Ok(providers.with_memory(builder.build().await?))
    }
}

/// Remembering in PostgreSQL, ranked by embedding distance where it can be.
///
/// Claims its definitions in every build, including one compiled without the
/// backend. A definition an operator stored describes a store they configured,
/// and a build that declined to claim it would report "no factory builds this"
/// — true, and no help at all. Refusing it by name says which feature is
/// missing instead.
pub struct PgVectorMemory;

#[async_trait::async_trait]
impl ProviderFactory for PgVectorMemory {
    fn name(&self) -> &'static str {
        "pgvector"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::Memory { variant: MemoryVariant::PgVector { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let ProviderDefinitionVariant::Memory {
            variant: variant @ MemoryVariant::PgVector { .. },
        } = &definition.variant
        else {
            return Err(unclaimed(self.name(), definition));
        };
        build_pgvector(providers, definition, variant).await
    }
}

/// Builds the store, when this build has a store to build.
#[cfg(feature = "postgres")]
async fn build_pgvector(
    providers: Providers,
    definition: &ProviderDefinition,
    variant: &MemoryVariant,
) -> Result<Providers> {
    use std::sync::Arc;

    use conduit_core::Error;
    use conduit_memory::PgVector;
    use conduit_openai::{OpenAiConfig, OpenAiEmbeddings};

    let MemoryVariant::PgVector {
        url,
        embedding_base_url,
        api_key,
        embedding_model,
        dimensions,
    } = variant
    else {
        return Err(unclaimed("pgvector", definition));
    };

    if *dimensions == 0 {
        return Err(Error::Config(format!(
            "memory store `{}` names an embedding width of zero, which is not a vector",
            definition.id
        )));
    }

    let embeddings = OpenAiEmbeddings::new(
        &OpenAiConfig {
            base_url: embedding_base_url.clone(),
            api_key: super::secret_value(api_key),
            name: definition.id.clone(),
            label: Some(definition.label.clone()),
            ..OpenAiConfig::default()
        },
        embedding_model,
    )?;
    let embedder =
        Arc::new(conduit_memory::embed::OpenAiEmbedder::new(embeddings, *dimensions));
    let store = PgVector::builder(&definition.id, embedder)
        .label(&definition.label)
        .connect(url)
        .await?;
    Ok(providers.with_memory(store))
}

/// Why this build cannot supply the store the definition describes.
///
/// The Bedrock factory's shape, for the same reason: a lean build says what is
/// missing rather than registering a store that cannot answer.
#[cfg(not(feature = "postgres"))]
async fn build_pgvector(
    _providers: Providers,
    definition: &ProviderDefinition,
    _variant: &MemoryVariant,
) -> Result<Providers> {
    Err(conduit_core::Error::Config(format!(
        "memory store `{}` needs PostgreSQL support, which this build lacks; rebuild with \
         --features postgres",
        definition.id
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::memory::Scope;
    use conduit_provider::memory::Record;
    use conduit_provider::storage::TransformVariant;

    fn definition(variant: MemoryVariant) -> ProviderDefinition {
        ProviderDefinition {
            id: "recall".to_owned(),
            label: "Household memory".to_owned(),
            variant: ProviderDefinitionVariant::Memory { variant },
            settings: Default::default(),
        }
    }

    #[tokio::test]
    async fn a_stored_definition_becomes_a_store_under_its_own_id() {
        let providers = BuiltinMemory
            .register(
                Providers::new(),
                &definition(MemoryVariant::Builtin { path: None, capacity: 50 }),
            )
            .await
            .expect("builds");

        assert_eq!(providers.memory().names().collect::<Vec<_>>(), ["recall"]);
        let store = providers.memory().get("recall").expect("registered");
        assert_eq!(store.descriptor().label, "Household memory");
    }

    #[tokio::test]
    async fn a_stored_path_is_the_file_the_records_are_written_to() {
        // The path has to travel from the definition to the builder. Asserted by
        // storing a record and looking for the file, because a path that was
        // dropped somewhere in between gives a store that works perfectly until
        // the server restarts.
        let path = std::env::temp_dir()
            .join(format!("conduit-memory-factory-{}.json", uuid::Uuid::new_v4()));
        let providers = BuiltinMemory
            .register(
                Providers::new(),
                &definition(MemoryVariant::Builtin {
                    path: Some(path.to_string_lossy().into_owned()),
                    capacity: 50,
                }),
            )
            .await
            .expect("builds");

        providers
            .memory()
            .get("recall")
            .expect("registered")
            .store(Record {
                content: "the recycling goes out on tuesday".to_owned(),
                scope: Scope::Global,
                conversation: None,
                speaker: None,
                metadata: serde_json::Value::Null,
            })
            .await
            .expect("stores");

        assert!(path.exists(), "{} was written", path.display());
        tokio::fs::remove_file(&path).await.expect("cleans up");
    }

    #[tokio::test]
    async fn a_capacity_of_zero_is_refused_rather_than_remembering_nothing() {
        // The store's own builder refuses it. Asserted here because this is
        // where a definition stored by an older build reaches that builder.
        let error = BuiltinMemory
            .register(
                Providers::new(),
                &definition(MemoryVariant::Builtin { path: None, capacity: 0 }),
            )
            .await
            .expect_err("a store that keeps nothing is not a store");

        assert!(error.to_string().contains("recall"), "{error}");
    }

    #[tokio::test]
    async fn a_definition_the_builtin_factory_does_not_build_is_refused_rather_than_ignored() {
        // `handles` and `register` have to agree. If they ever disagree, the
        // definition is reported rather than silently producing no provider.
        let mut wrong = definition(MemoryVariant::Builtin { path: None, capacity: 50 });
        wrong.variant = ProviderDefinitionVariant::Transform {
            variant: TransformVariant::Builtin { rules: Vec::new() },
        };

        let error =
            BuiltinMemory.register(Providers::new(), &wrong).await.expect_err("not ours");
        assert!(error.to_string().contains("recall"), "{error}");
    }

    #[tokio::test]
    async fn the_pgvector_factory_refuses_a_definition_it_does_not_build() {
        let mut wrong = definition(MemoryVariant::Builtin { path: None, capacity: 50 });
        wrong.variant = ProviderDefinitionVariant::Transform {
            variant: TransformVariant::Builtin { rules: Vec::new() },
        };

        let error =
            PgVectorMemory.register(Providers::new(), &wrong).await.expect_err("not ours");
        assert!(error.to_string().contains("recall"), "{error}");
    }

    /// A width of zero would declare a `vector(0)` column.
    ///
    /// Reached before the database is, so this holds without one: the check is
    /// on the definition, and a store that cannot be described cannot be built
    /// whether or not PostgreSQL is up.
    #[cfg(feature = "postgres")]
    #[tokio::test]
    async fn an_embedding_width_of_zero_is_refused_before_anything_is_reached() {
        let error = PgVectorMemory
            .register(
                Providers::new(),
                &definition(MemoryVariant::PgVector {
                    url: "postgres://localhost/conduit".to_owned(),
                    embedding_base_url: "https://api.openai.com/v1".to_owned(),
                    api_key: None,
                    embedding_model: "text-embedding-3-small".to_owned(),
                    dimensions: 0,
                }),
            )
            .await
            .expect_err("zero numbers is not a vector");

        assert!(error.to_string().contains("recall"), "{error}");
    }

    /// A build without the backend says which feature is missing.
    #[cfg(not(feature = "postgres"))]
    #[tokio::test]
    async fn a_build_without_postgres_names_the_feature_rather_than_the_definition() {
        let error = PgVectorMemory
            .register(
                Providers::new(),
                &definition(MemoryVariant::PgVector {
                    url: "postgres://localhost/conduit".to_owned(),
                    embedding_base_url: "https://api.openai.com/v1".to_owned(),
                    api_key: None,
                    embedding_model: "text-embedding-3-small".to_owned(),
                    dimensions: 1536,
                }),
            )
            .await
            .expect_err("this build has no store to build");

        assert!(error.to_string().contains("--features postgres"), "{error}");
    }
}
