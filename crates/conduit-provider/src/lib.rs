//! Provider interfaces for Conduit.
//!
//! Every replaceable component of the voice pipeline — wake word, speech
//! recognition, speaker identification, reasoning, tools, memory, synthesis —
//! is expressed here as a trait, as is every stage that rewrites what passes
//! between them. Supporting a new vendor means implementing
//! one of these traits and registering it; it must never mean editing the
//! pipeline.
//!
//! All traits are object safe and all streaming methods return
//! [`ChunkStream`], so providers can be stored behind `Arc<dyn Trait>` in a
//! [`Registry`] and swapped at runtime.

pub mod descriptor;
pub mod llm;
pub mod memory;
pub mod registry;
pub mod speaker;
pub mod storage;
pub mod stt;
#[cfg(feature = "testing")]
pub mod testing;
pub mod tool;
pub mod transform;
pub mod tts;
pub mod vad;
pub mod wake;

use std::pin::Pin;

use conduit_core::Result;
use futures_core::Stream;
use serde::{Deserialize, Serialize};

pub use descriptor::{Descriptor, Metadata, Settings, SettingsSchema};
pub use registry::{Capability, Registry, RegistryHandle};

/// A boxed stream of fallible items, the shape every streaming provider
/// method returns.
pub type ChunkStream<T> = Pin<Box<dyn Stream<Item = Result<T>> + Send>>;

/// Behaviour common to every provider.
///
/// Implemented alongside the capability trait, e.g. a Whisper backend
/// implements both `Provider` and [`stt::SpeechToText`].
#[async_trait::async_trait]
pub trait Provider: Send + Sync + 'static {
    /// What this provider is, what it can do, and what it needs configured.
    ///
    /// One descriptor per provider, built when the provider is constructed.
    /// Everything a caller used to ask a provider one method at a time — its
    /// name, its version, the models it serves, the voices it speaks with, the
    /// encodings it handles, whether it can call tools — is read from here, so
    /// a status screen can describe a provider of any capability without
    /// knowing which capability it is.
    fn descriptor(&self) -> &Descriptor;

    /// Stable identity, e.g. `"whisper"` — the descriptor's
    /// [`id`](Descriptor::id).
    ///
    /// Used in metric labels and error messages, so it must not change between
    /// versions. It is *not* the registry key: a deployment chooses that, and
    /// may register the same implementation under two of them.
    fn name(&self) -> &str {
        &self.descriptor().id
    }

    /// Provider version, surfaced in diagnostics.
    fn version(&self) -> &str {
        &self.descriptor().version
    }

    /// Reports whether the provider can currently serve requests.
    ///
    /// Called by health endpoints and by the failover logic before routing
    /// to a provider. The default assumes an always-available local
    /// implementation.
    async fn health(&self) -> Health {
        Health::Healthy
    }
}

/// A provider's readiness to serve requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Health {
    /// Serving normally.
    Healthy,
    /// Serving, but impaired — e.g. a fallback model or elevated latency.
    Degraded {
        /// What is impaired.
        reason: String,
    },
    /// Not serving. Routing should fail over.
    Unhealthy {
        /// Why the provider is unavailable.
        reason: String,
    },
}

impl Health {
    /// Whether requests may be routed to this provider.
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        matches!(self, Self::Healthy | Self::Degraded { .. })
    }
}
