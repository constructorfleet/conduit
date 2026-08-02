//! Error types shared across the platform.

use crate::graph::Modality;
use crate::id::ConversationId;

/// The result type used throughout Conduit.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong inside the core runtime.
///
/// Provider-specific failures are funnelled into [`Error::Provider`] so that
/// pipeline code can reason about failure *class* (retryable, fatal,
/// misconfigured) without knowing which vendor produced it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A pipeline graph failed validation.
    #[error("invalid pipeline graph: {0}")]
    InvalidGraph(#[from] GraphError),

    /// A provider failed while servicing a request.
    #[error("provider `{provider}` failed: {source}")]
    Provider {
        /// Registered name of the provider that failed.
        provider: String,
        /// The underlying failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// No provider is registered under the requested name.
    #[error("no provider registered as `{0}`")]
    UnknownProvider(String),

    /// A conversation was referenced after it ended or was never started.
    #[error("unknown conversation {0}")]
    UnknownConversation(ConversationId),

    /// Configuration was structurally valid but semantically wrong.
    #[error("configuration error: {0}")]
    Config(String),

    /// An operation exceeded its deadline.
    #[error("`{operation}` timed out after {}ms", .elapsed.as_millis())]
    Timeout {
        /// Human-readable name of the operation that timed out.
        operation: String,
        /// How long the operation ran before being abandoned.
        elapsed: std::time::Duration,
    },

    /// The operation was cancelled, typically by a barge-in or shutdown.
    #[error("operation cancelled")]
    Cancelled,
}

impl Error {
    /// Builds a [`Error::Provider`] from any standard error.
    pub fn provider<E>(provider: impl Into<String>, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Provider { provider: provider.into(), source: Box::new(source) }
    }

    /// Whether retrying the same operation could plausibly succeed.
    ///
    /// Callers use this to decide between a retry, a failover to the next
    /// provider in the chain, and giving up.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Provider { .. } | Self::Timeout { .. })
    }
}

/// Why a [`crate::graph::PipelineGraph`] was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum GraphError {
    /// Two nodes declared the same identifier.
    #[error("duplicate node id `{0}`")]
    DuplicateNode(String),

    /// An edge referenced a node that does not exist.
    #[error("edge references unknown node `{0}`")]
    UnknownNode(String),

    /// The graph contains a cycle; the listed nodes could not be ordered.
    #[error("graph contains a cycle involving: {}", .0.join(", "))]
    Cycle(Vec<String>),

    /// The graph has no node that can start a pipeline run.
    #[error("graph has no source node")]
    NoSource,

    /// The graph produced no output.
    #[error("graph has no sink node")]
    NoSink,

    /// The graph is not one pipeline: the listed nodes are not connected to
    /// the rest of it, so nothing would ever run them.
    ///
    /// A graph with no edges at all is the common case. Every node is then its
    /// own island, which validates as a sound *topology* while describing no
    /// pipeline whatsoever.
    #[error("nodes are not connected to the rest of the pipeline: {}", .0.join(", "))]
    Disconnected(Vec<String>),

    /// The graph reasons in more than one place.
    ///
    /// A pipeline has one reasoning core, so a graph with two describes two
    /// pipelines' worth of reasoning and says nothing about which of them
    /// answers. Counted across `core` and `llm` nodes alike: the refusal is
    /// about there being two models, not about how they are spelled.
    #[error("graph has more than one reasoning node: {}", .0.join(", "))]
    MultipleCores(Vec<String>),

    /// An edge delivers something the node at its far end cannot read.
    ///
    /// This is the structural form of a mistake that used to be found — when
    /// it was found at all — by the runtime asserting that recognition came
    /// before reasoning came before synthesis. A graph says what each stage
    /// consumes and produces, so a graph wired backwards is wrong on its own
    /// terms rather than wrong relative to one runtime's expectations.
    #[error(
        "edge `{from}` -> `{to}` carries {produced}, but `{to}` accepts {}",
        describe_modalities(.expected)
    )]
    ModalityMismatch {
        /// Position of the edge in the graph's edge list.
        ///
        /// `from` and `to` do not identify an edge on their own — two nodes
        /// may be joined more than once, on different ports — so an editor
        /// highlighting the offending connection needs this.
        edge: usize,
        /// Id of the upstream node.
        from: String,
        /// Id of the downstream node.
        to: String,
        /// What the upstream node emits.
        produced: Modality,
        /// What the downstream node can read. Empty when it reads nothing.
        expected: Vec<Modality>,
    },
}

/// Names the modalities a node accepts, for an operator reading the error.
fn describe_modalities(modalities: &[Modality]) -> String {
    if modalities.is_empty() {
        return "nothing".to_owned();
    }
    modalities.iter().map(|modality| modality.name()).collect::<Vec<_>>().join(" or ")
}
