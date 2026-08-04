//! Core types for Conduit, the modular voice assistant framework.
//!
//! This crate holds the contracts every other part of the platform agrees on
//! and deliberately holds nothing else: no provider implementations, no
//! transport, no storage. Adding Whisper or ElevenLabs support must never
//! require a change here.
//!
//! - [`id`] — strongly typed identifiers
//! - [`event`] — the event vocabulary of the pipeline
//! - [`bus`] — publish/subscribe distribution of those events
//! - [`graph`] — the serializable pipeline definition
//! - [`memory`] — how long what a pipeline remembers should live
//! - [`audio`] — audio format description
//! - [`error`] — shared error types
//!
//! # Example
//!
//! ```
//! use conduit_core::bus::EventBus;
//! use conduit_core::event::{Envelope, Event};
//! use conduit_core::id::TraceId;
//!
//! # #[tokio::main]
//! # async fn main() {
//! let bus = EventBus::default();
//! let mut events = bus.subscribe();
//!
//! bus.publish(Envelope::new(TraceId::new(), Event::ConversationStarted));
//!
//! let received = events.recv().await.expect("bus is open");
//! assert_eq!(received.event, Event::ConversationStarted);
//! # }
//! ```

pub mod audio;
pub mod bus;
pub mod device;
pub mod error;
pub mod event;
pub mod graph;
pub mod id;
pub mod memory;
pub mod pcm;
pub mod resample;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod wav;

pub use error::{Error, GraphError, Result};
