//! Where a vector store gets its vectors.
//!
//! One trait with one method, so that a vector store depends on "something that
//! turns text into numbers" rather than on any particular server. The
//! implementation a deployment actually uses is
//! [`conduit_openai::OpenAiEmbeddings`], behind this crate's `openai` feature;
//! the trait exists so that tests can supply a deterministic vector without a
//! network, and so that a local embedding library can be dropped in later
//! without touching the store.

use conduit_core::Result;

/// Turns text into a vector.
#[async_trait::async_trait]
pub trait Embedder: Send + Sync + 'static {
    /// Embeds one text.
    ///
    /// Every vector this returns must have the same number of dimensions:
    /// [`PgVector`] builds a fixed-width column from the first one it sees, and
    /// a later vector of a different width is refused by the database rather
    /// than silently stored.
    ///
    /// # Errors
    ///
    /// Returns an error if the embedding cannot be produced. The store treats
    /// that as a reason to fall back to keyword ranking, not as a reason to fail
    /// the turn.
    ///
    /// [`PgVector`]: crate::PgVector
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// How many dimensions this embedder produces.
    ///
    /// Needed before anything has been embedded, because it is what the
    /// `vector(n)` column is declared with. An embedding model's width is a
    /// property of the model, so the caller configuring one knows it.
    fn dimensions(&self) -> usize;
}

#[cfg(feature = "openai")]
#[async_trait::async_trait]
impl Embedder for OpenAiEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embeddings.embed(text).await
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

/// An [`Embedder`] served by an OpenAI-compatible `/embeddings` endpoint.
///
/// The dimension count is supplied rather than discovered, because it is needed
/// before the first embedding exists — the `vector(n)` column is declared with
/// it. `text-embedding-3-small` is 1536, `text-embedding-3-large` is 3072, and
/// `nomic-embed-text` is 768; a local model is whatever it says it is.
#[cfg(feature = "openai")]
#[derive(Debug, Clone)]
pub struct OpenAiEmbedder {
    embeddings: conduit_openai::OpenAiEmbeddings,
    dimensions: usize,
}

#[cfg(feature = "openai")]
impl OpenAiEmbedder {
    /// Wraps `embeddings`, which produces vectors of `dimensions` numbers.
    #[must_use]
    pub const fn new(embeddings: conduit_openai::OpenAiEmbeddings, dimensions: usize) -> Self {
        Self { embeddings, dimensions }
    }
}
