//! Prometheus metrics for Conduit, derived from the event bus.
//!
//! Nothing in the pipeline calls into this crate. Every stage already
//! publishes what it did, so metrics are an ordinary subscriber — which means
//! a new event is counted the day it is added, and the audio path never pays
//! for instrumentation it does not know about.
//!
//! The one thing a subscriber cannot learn from events alone is what it never
//! received. The bus counts each subscription's losses, so the collector reads
//! its own [`conduit_core::bus::Subscription::dropped`] after every event and
//! exports the difference as `conduit_events_dropped_total`. That keeps the
//! direction of the dependency intact — the collector asks the subscription it
//! owns, rather than the bus reaching into a registry — at the cost of
//! reporting only its own drops, which is why the series names the subscriber
//! it speaks for. Another subscriber that wants its losses on `/metrics` does
//! the same with its own handle.
//!
//! ```
//! use std::sync::Arc;
//! use conduit_core::bus::EventBus;
//! use conduit_metrics::{Collector, Metrics};
//!
//! # #[tokio::main]
//! # async fn main() {
//! let bus = EventBus::default();
//! let metrics = Arc::new(Metrics::new());
//! Collector::spawn(Arc::clone(&metrics), &bus);
//!
//! // Later, when something scrapes /metrics:
//! let _body = metrics.render();
//! # }
//! ```

pub mod collector;
pub mod registry;

pub use collector::{Collector, Metrics};
pub use registry::Registry;
