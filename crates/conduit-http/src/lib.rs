//! Shared plumbing for Conduit's HTTP-backed providers.
//!
//! Every provider that talks to a jsonl-over-HTTP vendor needs the same three
//! things, and none of them are that vendor's API: a client that authenticates
//! and turns a non-2xx status into an error a caller can classify
//! ([`Http`]), that classification itself ([`Failure`]), and server-sent event
//! reassembly for the streaming responses ([`sse::Decoder`]).
//!
//! They live here rather than in one vendor's crate because they are shared by
//! several. What differs between vendors is the request and response shape, and
//! that stays with the vendor.
//!
//! ```no_run
//! # use conduit_http::{Credential, Http, HttpConfig};
//! # use std::time::Duration;
//! let http = Http::new(HttpConfig {
//!     base_url: "https://api.anthropic.com/v1".to_owned(),
//!     name: "anthropic".to_owned(),
//!     // The credential mechanism is the vendor's, not this crate's.
//!     credential: Credential::header("x-api-key", std::env::var("ANTHROPIC_API_KEY").ok()),
//!     headers: vec![("anthropic-version".to_owned(), "2023-06-01".to_owned())],
//!     connect_timeout: Duration::from_secs(30),
//!     read_timeout: Some(Duration::from_secs(60)),
//! })?;
//! # Ok::<(), conduit_core::Error>(())
//! ```

pub mod client;
pub mod failure;
pub mod sse;

pub use client::{BearerSource, Credential, Http, HttpConfig};
pub use failure::{Failure, FailureKind};
