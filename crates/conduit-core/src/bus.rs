//! The in-process event bus.
//!
//! Publishers never block on subscribers. A slow subscriber falls behind and
//! loses the oldest events rather than applying backpressure to the audio
//! path — a dropped log line is always preferable to a stalled conversation.
//! Losses are counted per subscription so they remain observable.

use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::broadcast;

use crate::event::{Envelope, Stage};
use crate::id::{ConversationId, DeviceId, TraceId};

/// Default number of events retained for lagging subscribers.
pub const DEFAULT_CAPACITY: usize = 1024;

/// A multi-producer, multi-subscriber broadcast channel for [`Envelope`]s.
///
/// Cloning is cheap and yields a handle to the same bus.
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Arc<Envelope>>,
}

impl EventBus {
    /// Creates a bus retaining `capacity` events for lagging subscribers.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "event bus capacity must be non-zero");
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publishes an event, returning the number of subscriptions it reached.
    ///
    /// Publishing with no subscribers is not an error; the event is dropped.
    pub fn publish(&self, envelope: Envelope) -> usize {
        let stage = envelope.event.stage();
        let envelope = Arc::new(envelope);
        match self.tx.send(envelope) {
            Ok(received) => received,
            Err(_) => {
                tracing::trace!(?stage, "event published with no subscribers");
                0
            }
        }
    }

    /// Subscribes to every event published from now on.
    #[must_use]
    pub fn subscribe(&self) -> Subscription {
        self.subscribe_filtered(Filter::default())
    }

    /// Subscribes to the events matching `filter`.
    #[must_use]
    pub fn subscribe_filtered(&self, filter: Filter) -> Subscription {
        Subscription { rx: self.tx.subscribe(), filter, dropped: 0 }
    }

    /// Number of live subscriptions.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

/// Selects which events a [`Subscription`] receives.
///
/// An empty filter matches everything. Each populated field narrows the
/// selection further, and all populated fields must match.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    stages: Option<HashSet<Stage>>,
    conversation: Option<ConversationId>,
    device: Option<DeviceId>,
    trace: Option<TraceId>,
}

impl Filter {
    /// A filter that matches every event.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// Restricts the subscription to the given stages.
    #[must_use]
    pub fn stages(mut self, stages: impl IntoIterator<Item = Stage>) -> Self {
        self.stages = Some(stages.into_iter().collect());
        self
    }

    /// Restricts the subscription to one conversation.
    #[must_use]
    pub fn conversation(mut self, conversation: ConversationId) -> Self {
        self.conversation = Some(conversation);
        self
    }

    /// Restricts the subscription to one device.
    #[must_use]
    pub fn device(mut self, device: DeviceId) -> Self {
        self.device = Some(device);
        self
    }

    /// Restricts the subscription to one trace.
    #[must_use]
    pub fn trace(mut self, trace: TraceId) -> Self {
        self.trace = Some(trace);
        self
    }

    /// Whether `envelope` passes this filter.
    #[must_use]
    pub fn matches(&self, envelope: &Envelope) -> bool {
        if let Some(stages) = &self.stages {
            if !stages.contains(&envelope.event.stage()) {
                return false;
            }
        }
        if let Some(conversation) = self.conversation {
            if envelope.conversation != Some(conversation) {
                return false;
            }
        }
        if let Some(device) = self.device {
            if envelope.device != Some(device) {
                return false;
            }
        }
        if let Some(trace) = self.trace {
            if envelope.trace != trace {
                return false;
            }
        }
        true
    }
}

/// A handle for consuming events from an [`EventBus`].
#[derive(Debug)]
pub struct Subscription {
    rx: broadcast::Receiver<Arc<Envelope>>,
    filter: Filter,
    dropped: u64,
}

impl Subscription {
    /// Waits for the next matching event.
    ///
    /// Returns `None` once the bus and all its clones have been dropped.
    /// Events lost to lag are counted in [`Subscription::dropped`] rather
    /// than surfaced as errors.
    pub async fn recv(&mut self) -> Option<Arc<Envelope>> {
        loop {
            match self.rx.recv().await {
                Ok(envelope) => {
                    if self.filter.matches(&envelope) {
                        return Some(envelope);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    self.dropped = self.dropped.saturating_add(count);
                    tracing::warn!(
                        lost = count,
                        total = self.dropped,
                        "subscription lagged; events dropped"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Total events this subscription has lost to lag.
    ///
    /// Monotonic, so a consumer can export the difference since it last looked.
    /// Whoever owns a subscription is the only one who can report its losses —
    /// the bus deliberately knows nothing about metrics — so a subscriber that
    /// wants its drops on `/metrics` reads this as it consumes.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;

    fn envelope(event: Event) -> Envelope {
        Envelope::new(TraceId::new(), event)
    }

    #[tokio::test]
    async fn delivers_to_every_subscriber() {
        let bus = EventBus::default();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();

        assert_eq!(bus.publish(envelope(Event::ConversationStarted)), 2);

        for sub in [&mut a, &mut b] {
            let received = sub.recv().await.expect("event");
            assert_eq!(received.event, Event::ConversationStarted);
        }
    }

    #[tokio::test]
    async fn filter_selects_by_stage() {
        let bus = EventBus::default();
        let mut sub = bus.subscribe_filtered(Filter::all().stages([Stage::Reasoning]));

        bus.publish(envelope(Event::ConversationStarted));
        bus.publish(envelope(Event::LlmToken { delta: "hi".into() }));

        let received = sub.recv().await.expect("event");
        assert_eq!(received.event, Event::LlmToken { delta: "hi".into() });
    }

    #[tokio::test]
    async fn filter_selects_by_conversation() {
        let bus = EventBus::default();
        let mine = ConversationId::new();
        let mut sub = bus.subscribe_filtered(Filter::all().conversation(mine));

        bus.publish(
            envelope(Event::ConversationStarted).with_conversation(ConversationId::new()),
        );
        bus.publish(envelope(Event::ConversationCompleted).with_conversation(mine));

        let received = sub.recv().await.expect("event");
        assert_eq!(received.conversation, Some(mine));
    }

    #[tokio::test]
    async fn slow_subscriber_loses_events_instead_of_blocking() {
        let bus = EventBus::new(2);
        let mut sub = bus.subscribe();

        for _ in 0..5 {
            // Never blocks, even though the subscriber has not read anything.
            bus.publish(envelope(Event::ConversationStarted));
        }

        assert!(sub.recv().await.is_some());
        assert_eq!(sub.dropped(), 3);
    }

    #[tokio::test]
    async fn recv_ends_when_bus_is_dropped() {
        let bus = EventBus::default();
        let mut sub = bus.subscribe();
        drop(bus);
        assert!(sub.recv().await.is_none());
    }
}
