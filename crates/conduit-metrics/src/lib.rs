//! Prometheus metrics for Conduit, derived from the event bus.
//!
//! Nothing in the pipeline calls into this crate. Every stage already
//! publishes what it did, so metrics are an ordinary subscriber — which means
//! a new event is counted the day it is added, and the audio path never pays
//! for instrumentation it does not know about.
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
