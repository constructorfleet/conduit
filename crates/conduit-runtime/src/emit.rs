//! Publishing events on behalf of one turn.

use conduit_core::bus::EventBus;
use conduit_core::event::{Envelope, Event};
use conduit_core::id::{ConversationId, TraceId};

/// Publishes events stamped with one turn's correlation ids.
///
/// Cloning yields a handle to the same turn, so work that runs concurrently —
/// tools, synthesis — still reports under one trace.
#[derive(Debug, Clone)]
pub struct Emitter {
    bus: EventBus,
    trace: TraceId,
    conversation: ConversationId,
}

impl Emitter {
    /// Creates an emitter for a fresh turn.
    pub fn new(bus: EventBus) -> Self {
        Self { bus, trace: TraceId::new(), conversation: ConversationId::new() }
    }

    /// Publishes `event` for this turn.
    pub fn emit(&self, event: Event) {
        self.bus.publish(Envelope::new(self.trace, event).with_conversation(self.conversation));
    }

    /// The conversation every event in this turn belongs to.
    pub const fn conversation(&self) -> ConversationId {
        self.conversation
    }
}
