//! Asking whether a tool may run, and hearing back.
//!
//! Some tools change something in the world. Whether a given call should go
//! ahead is not a question the model can settle and not one the graph can
//! settle in advance, so it has to be asked while the turn is in progress —
//! which means a channel back into a turn that is already running.
//!
//! Deliberately shaped like [`crate::stop::Stop`]: latching, cheap to clone,
//! and answerable before the turn asks. A decision that arrives early is still
//! there when the turn looks for it, so an answer cannot be lost to timing.

use std::collections::HashMap;
use std::sync::Arc;

use conduit_core::id::ToolCallId;
use tokio::sync::watch;

/// A handle for answering a turn's confirmation requests.
///
/// Every clone refers to the same turn. Holding one is what makes a deployment
/// *able* to answer: a turn with no listener refuses a call that needs
/// confirming rather than waiting for a decision nothing will send.
#[derive(Clone, Debug)]
pub struct Confirmations(Arc<watch::Sender<HashMap<ToolCallId, bool>>>);

impl Confirmations {
    /// A handle for a turn nobody is listening to yet.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(watch::Sender::new(HashMap::new())))
    }

    /// Starts listening for confirmation requests.
    ///
    /// The returned guard is what [`Confirmations::answerable`] counts. Drop it
    /// and the turn goes back to refusing calls that need confirming, which is
    /// the right answer for a client that has gone away mid-turn.
    #[must_use]
    pub fn listen(&self) -> ConfirmationListener {
        ConfirmationListener(self.0.subscribe())
    }

    /// Whether anything is listening and could therefore answer.
    ///
    /// A turn asks this before waiting. Waiting for a decision no one can send
    /// would leave the person who spoke listening to silence until the idle
    /// deadline gave up on the turn.
    #[must_use]
    pub fn answerable(&self) -> bool {
        self.0.receiver_count() > 0
    }

    /// Allows or refuses one requested call.
    ///
    /// Answering a call twice keeps the first answer: a decision that has
    /// already been acted on cannot be taken back, and pretending otherwise
    /// would make the second answer look effective.
    pub fn answer(&self, call: ToolCallId, allowed: bool) {
        self.0.send_if_modified(|answers| {
            if answers.contains_key(&call) {
                return false;
            }
            answers.insert(call, allowed);
            true
        });
    }

    /// Resolves once `call` has been answered, with the decision.
    ///
    /// Resolves immediately if it already has been. Unbounded on purpose: the
    /// turn's idle deadline is what stops a question nobody answers from
    /// running forever, and duplicating that bound here would mean two
    /// timeouts disagreeing about when a turn is stuck.
    pub async fn wait(&self, call: &ToolCallId) -> bool {
        let mut receiver = self.0.subscribe();
        loop {
            if let Some(allowed) = receiver.borrow_and_update().get(call) {
                return *allowed;
            }
            if receiver.changed().await.is_err() {
                // The sender lives in this `Arc`, so it cannot have dropped
                // while `self` is alive. Nothing can arrive now regardless.
                std::future::pending::<()>().await;
            }
        }
    }
}

impl Default for Confirmations {
    fn default() -> Self {
        Self::new()
    }
}

/// Proof that something is listening for confirmation requests.
///
/// Held by a client for as long as it can answer them.
#[derive(Debug)]
pub struct ConfirmationListener(
    // Never read, and load-bearing anyway: holding a receiver is what makes
    // `Confirmations::answerable` true, and dropping this is how a client says
    // it can no longer answer.
    #[allow(dead_code)] watch::Receiver<HashMap<ToolCallId, bool>>,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_answer_given_before_the_turn_asks_is_still_there() {
        // The race this type exists to lose safely: a client that answers
        // quickly must not have its decision dropped for arriving early.
        let confirmations = Confirmations::new();
        let _listener = confirmations.listen();
        let call = ToolCallId::new("call_one");
        confirmations.answer(call.clone(), true);

        assert!(confirmations.wait(&call).await);
    }

    #[tokio::test]
    async fn a_turn_waits_until_the_call_it_asked_about_is_answered() {
        // Answers name a call, so a decision about one tool does not release
        // another that happened to be waiting.
        let confirmations = Confirmations::new();
        let _listener = confirmations.listen();
        let waited = confirmations.clone();
        let wanted = ToolCallId::new("wanted");
        let task = tokio::spawn(async move { waited.wait(&wanted).await });

        confirmations.answer(ToolCallId::new("other"), true);
        assert!(!task.is_finished(), "another call's answer is not this one's");

        confirmations.answer(ToolCallId::new("wanted"), false);
        assert!(!task.await.expect("joins"), "and the refusal is the answer");
    }

    #[test]
    fn nothing_is_answerable_until_something_listens() {
        let confirmations = Confirmations::new();
        assert!(!confirmations.answerable());

        let listener = confirmations.listen();
        assert!(confirmations.answerable());

        drop(listener);
        assert!(
            !confirmations.answerable(),
            "a client that goes away mid-turn can no longer answer"
        );
    }

    #[tokio::test]
    async fn an_answer_is_not_taken_back() {
        let confirmations = Confirmations::new();
        let _listener = confirmations.listen();
        let call = ToolCallId::new("call_one");

        confirmations.answer(call.clone(), true);
        confirmations.answer(call.clone(), false);

        assert!(confirmations.wait(&call).await, "the first decision stands");
    }
}
