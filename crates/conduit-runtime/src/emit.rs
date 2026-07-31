//! Publishing events on behalf of one turn.

use conduit_core::bus::EventBus;
use conduit_core::event::{Envelope, Event};
use conduit_core::id::{ConversationId, DeviceId, TraceId};

/// Publishes events stamped with one turn's correlation ids.
///
/// Cloning yields a handle to the same turn, so work that runs concurrently —
/// tools, synthesis — still reports under one trace.
#[derive(Debug, Clone)]
pub struct Emitter {
    bus: EventBus,
    trace: TraceId,
    conversation: ConversationId,
    /// Which satellite this turn belongs to, when the caller knew.
    device: Option<DeviceId>,
}

impl Emitter {
    /// Creates an emitter for a fresh turn.
    pub fn new(bus: EventBus) -> Self {
        Self { bus, trace: TraceId::new(), conversation: ConversationId::new(), device: None }
    }

    /// Tags every event from this turn with the device it came from.
    ///
    /// This is what makes `/v1/events?device=` select anything. The identity has
    /// to come from an authenticated device token, not from a hostname or a
    /// client-supplied field, or the filter would select whatever a caller
    /// claimed.
    #[must_use]
    pub const fn with_device(mut self, device: DeviceId) -> Self {
        self.device = Some(device);
        self
    }

    /// Publishes `event` for this turn.
    pub fn emit(&self, event: Event) {
        let mut envelope =
            Envelope::new(self.trace, event).with_conversation(self.conversation);
        if let Some(device) = self.device {
            envelope = envelope.with_device(device);
        }
        self.bus.publish(envelope);
    }

    /// The conversation every event in this turn belongs to.
    pub const fn conversation(&self) -> ConversationId {
        self.conversation
    }
}
