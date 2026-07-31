//! Deriving metrics from the event bus.
//!
//! Every stage already publishes what it did, so the collector is an ordinary
//! subscriber rather than instrumentation threaded through the pipeline. Add
//! an event and it is counted; nothing in the audio path has to know that
//! metrics exist.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use conduit_core::bus::{EventBus, Subscription};
use conduit_core::event::{CancelReason, Event};
use conduit_core::id::ConversationId;

use crate::registry::{labels, Counter, Gauge, Histogram, Labels, Registry};

/// Latency buckets, in seconds.
///
/// Weighted towards the low end: for a voice assistant the interesting
/// question is whether a reply began within a few hundred milliseconds, not
/// how it is distributed above ten seconds.
const LATENCY_BUCKETS: [f64; 12] =
    [0.05, 0.1, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 5.0, 10.0, 30.0];

/// How many in-flight conversations to track before dropping the oldest.
///
/// A conversation whose end is never published would otherwise be remembered
/// forever, turning the collector into a slow leak.
pub const MAX_TRACKED: usize = 4096;

/// Which subscription a dropped-event count belongs to.
///
/// The bus counts losses per subscription, and the collector can only speak for
/// its own, so the series says whose drops these are.
const SUBSCRIBER: &str = "metrics";

/// What is known about a conversation while it runs.
struct InFlight {
    started: Instant,
    spoke_at: Option<Instant>,
}

/// The metrics Conduit publishes.
#[derive(Debug)]
pub struct Metrics {
    /// The registry to render for a scrape.
    pub registry: Registry,
    events: Arc<Counter>,
    conversations: Arc<Counter>,
    active: Arc<Gauge>,
    turn_duration: Arc<Histogram>,
    time_to_speech: Arc<Histogram>,
    tool_calls: Arc<Counter>,
    tool_requests: Arc<Counter>,
    tool_duration: Arc<Histogram>,
    stage_failures: Arc<Counter>,
    tokens: Arc<Counter>,
    forgotten: Arc<Counter>,
    dropped: Arc<Counter>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    /// Registers every metric.
    #[must_use]
    pub fn new() -> Self {
        let registry = Registry::new();
        let forgotten = registry.counter(
            "conduit_conversations_forgotten_total",
            "Conversations dropped from tracking before they ended.",
        );
        let dropped = registry.counter(
            "conduit_events_dropped_total",
            "Events a subscription lost to lag, by subscriber.",
        );
        // Both are health signals whose interesting value is zero, so declare
        // the series rather than letting a healthy process expose no sample.
        forgotten.init(Vec::new());
        dropped.init(labels(&[("subscriber", SUBSCRIBER)]));

        let active =
            registry.gauge("conduit_conversations_active", "Conversations in progress.");
        // An idle process should read zero, not expose nothing.
        active.set(Vec::new(), 0);

        Self {
            events: registry
                .counter("conduit_events_total", "Events published, by pipeline stage."),
            conversations: registry
                .counter("conduit_conversations_total", "Conversations, by outcome."),
            active,
            turn_duration: registry.histogram(
                "conduit_turn_duration_seconds",
                "Time from the start of a conversation to its end.",
                &LATENCY_BUCKETS,
            ),
            time_to_speech: registry.histogram(
                "conduit_time_to_first_audio_seconds",
                "Time from the start of a conversation until the first audio is sent.",
                &LATENCY_BUCKETS,
            ),
            tool_calls: registry.counter("conduit_tool_calls_total", "Tool calls, by outcome."),
            tool_requests: registry.counter(
                "conduit_tool_calls_requested_total",
                "Tool calls the model asked for.",
            ),
            tool_duration: registry.histogram(
                "conduit_tool_duration_seconds",
                "Time a tool took to run.",
                &LATENCY_BUCKETS,
            ),
            stage_failures: registry
                .counter("conduit_stage_failures_total", "Stage failures, by node."),
            tokens: registry.counter("conduit_llm_tokens_total", "Model tokens, by direction."),
            forgotten,
            dropped,
            registry,
        }
    }

    /// Renders the current values for a scrape.
    #[must_use]
    pub fn render(&self) -> String {
        self.registry.render()
    }
}

/// Subscribes to a bus and keeps [`Metrics`] up to date.
pub struct Collector {
    metrics: Arc<Metrics>,
    in_flight: HashMap<ConversationId, InFlight>,
    /// Insertion order, so the oldest can be dropped when tracking is full.
    order: Vec<ConversationId>,
    /// Drops already reported, so only the difference is added to the counter.
    reported_drops: u64,
}

impl Collector {
    /// Creates a collector feeding `metrics`.
    #[must_use]
    pub fn new(metrics: Arc<Metrics>) -> Self {
        Self { metrics, in_flight: HashMap::new(), order: Vec::new(), reported_drops: 0 }
    }

    /// Consumes `subscription` until the bus closes.
    pub async fn run(mut self, mut subscription: Subscription) {
        while let Some(envelope) = subscription.recv().await {
            self.record(envelope.conversation, &envelope.event);
            self.report_drops(&subscription);
        }
        // A lag while draining the last events would otherwise go unreported.
        self.report_drops(&subscription);
        tracing::debug!("event bus closed; metrics collector stopping");
    }

    /// Publishes what this collector's own subscription has lost to lag.
    ///
    /// The bus counts losses per subscription and the collector owns its own,
    /// so a drop reaches the registry without anything in the pipeline calling
    /// into this crate. No other subscriber's drops are visible from here,
    /// which is why the series names the subscriber it speaks for.
    fn report_drops(&mut self, subscription: &Subscription) {
        let total = subscription.dropped();
        let unreported = total.saturating_sub(self.reported_drops);
        if unreported > 0 {
            self.metrics.dropped.add(labels(&[("subscriber", SUBSCRIBER)]), unreported);
            self.reported_drops = total;
        }
    }

    /// Subscribes to `bus` and runs in the background.
    pub fn spawn(metrics: Arc<Metrics>, bus: &EventBus) -> tokio::task::JoinHandle<()> {
        let subscription = bus.subscribe();
        tokio::spawn(Self::new(metrics).run(subscription))
    }

    /// Updates the metrics for one event.
    pub fn record(&mut self, conversation: Option<ConversationId>, event: &Event) {
        let stage = stage_name(event);
        self.metrics.events.increment(labels(&[("stage", stage)]));

        match event {
            Event::ConversationStarted => {
                if let Some(id) = conversation {
                    self.begin(id);
                } else {
                    // Nothing to track, so nothing that could ever end it.
                    tracing::debug!(
                        "conversation started without an id; not tracked as active"
                    );
                }
            }
            Event::AudioStreaming { .. } => {
                // Only the first chunk answers "how long until it spoke?".
                if let Some(entry) = conversation.and_then(|id| self.in_flight.get_mut(&id)) {
                    if entry.spoke_at.is_none() {
                        entry.spoke_at = Some(Instant::now());
                        let elapsed = entry.started.elapsed().as_secs_f64();
                        self.metrics.time_to_speech.observe(Vec::new(), elapsed);
                    }
                }
            }
            Event::ConversationCompleted => self.finish(conversation, "completed"),
            Event::ConversationCancelled { reason } => {
                self.finish(conversation, cancel_name(*reason));
            }
            Event::ToolRequested { .. } => {
                // Its own metric rather than another `outcome` on
                // `conduit_tool_calls_total`: that counter means "calls that
                // resolved", and a `requested` label would double count every
                // call, silently changing what every existing panel reads.
                // Requests minus outcomes is then the number still in flight.
                self.metrics.tool_requests.increment(Vec::new());
            }
            Event::ToolCompleted { duration_ms, .. } => {
                self.metrics.tool_calls.increment(labels(&[("outcome", "completed")]));
                #[allow(clippy::cast_precision_loss)]
                self.metrics.tool_duration.observe(Vec::new(), *duration_ms as f64 / 1000.0);
            }
            Event::ToolFailed { .. } => {
                self.metrics.tool_calls.increment(labels(&[("outcome", "failed")]));
            }
            Event::ToolConfirmationRequested { .. } => {
                // The runtime answers the model and stops here, so this is where
                // the call ends unless something resumes it.
                self.metrics
                    .tool_calls
                    .increment(labels(&[("outcome", "awaiting_confirmation")]));
            }
            Event::StageFailed { node, recovered, .. } => {
                self.metrics.stage_failures.increment(labels(&[
                    ("node", node.as_str()),
                    ("recovered", if *recovered { "true" } else { "false" }),
                ]));
            }
            Event::LlmFinished { prompt_tokens, completion_tokens, .. } => {
                if let Some(count) = prompt_tokens {
                    self.metrics
                        .tokens
                        .add(labels(&[("direction", "prompt")]), u64::from(*count));
                }
                if let Some(count) = completion_tokens {
                    self.metrics
                        .tokens
                        .add(labels(&[("direction", "completion")]), u64::from(*count));
                }
            }
            _ => {}
        }
    }

    /// Starts tracking a conversation, evicting the oldest if full.
    fn begin(&mut self, id: ConversationId) {
        if self.in_flight.len() >= MAX_TRACKED {
            if let Some(oldest) = self.order.first().copied() {
                self.order.remove(0);
                self.in_flight.remove(&oldest);
                self.metrics.forgotten.increment(Vec::new());
                tracing::warn!(%oldest, "dropped an unfinished conversation from tracking");
            }
        }
        self.in_flight.insert(id, InFlight { started: Instant::now(), spoke_at: None });
        self.order.push(id);
        self.publish_active();
    }

    /// Records the end of a conversation.
    fn finish(&mut self, conversation: Option<ConversationId>, outcome: &'static str) {
        self.metrics.conversations.increment(labels(&[("outcome", outcome)]));

        let Some(id) = conversation else { return };
        if let Some(entry) = self.in_flight.remove(&id) {
            self.order.retain(|tracked| *tracked != id);
            self.metrics.turn_duration.observe(
                vec![("outcome", outcome.to_owned())],
                entry.started.elapsed().as_secs_f64(),
            );
            self.publish_active();
        } else {
            // Never tracked: begun before this collector subscribed, or evicted
            // by `begin`. Either way its slot was already released, so counting
            // this end again would leave the gauge permanently short.
            tracing::debug!(%id, outcome, "conversation ended without being tracked");
        }
    }

    /// Republishes the active gauge from what is actually being tracked.
    ///
    /// Set rather than incremented: the tracking map is the truth, and a gauge
    /// derived from it cannot drift the way paired increments and decrements do
    /// when an end arrives without its start.
    fn publish_active(&self) {
        let tracked = i64::try_from(self.in_flight.len()).unwrap_or(i64::MAX);
        self.metrics.active.set(Vec::new(), tracked);
    }
}

/// The stage label for an event.
fn stage_name(event: &Event) -> &'static str {
    use conduit_core::event::Stage;
    match event.stage() {
        Stage::WakeWord => "wake_word",
        Stage::Capture => "capture",
        Stage::Transcription => "transcription",
        Stage::Identity => "identity",
        Stage::Conversation => "conversation",
        Stage::Reasoning => "reasoning",
        Stage::Tools => "tools",
        Stage::Synthesis => "synthesis",
        Stage::Diagnostics => "diagnostics",
        // `Stage` is non-exhaustive; an unknown stage is still worth counting.
        _ => "other",
    }
}

/// The outcome label for a cancellation.
fn cancel_name(reason: CancelReason) -> &'static str {
    match reason {
        CancelReason::BargeIn => "barge_in",
        CancelReason::IdleTimeout => "idle_timeout",
        CancelReason::UserRequested => "user_requested",
        CancelReason::Disconnected => "disconnected",
        CancelReason::Error => "error",
        CancelReason::Shutdown => "shutdown",
        _ => "cancelled",
    }
}

/// Convenience for building a single-label set in tests and callers.
#[must_use]
pub fn label(name: &'static str, value: &str) -> Labels {
    labels(&[(name, value)])
}
