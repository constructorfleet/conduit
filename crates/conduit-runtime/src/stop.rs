//! Asking a turn in progress to stop talking.
//!
//! A turn is spawned and then only reachable through the audio it produces, so
//! interrupting it needs a channel of its own. Dropping the audio stream would
//! also end the turn, but it cannot say *why*: a turn that discovers its
//! listener through a failed write cannot tell someone who interrupted from
//! someone whose wifi dropped. This is how a client says it meant to.

use std::future::Future;
use std::sync::Arc;

use tokio::sync::watch;

/// A handle for asking one turn to stop, and for the turn to notice.
///
/// Cheap to clone, and every clone refers to the same turn. Latching rather
/// than edge-triggered: a request made before the turn next looks is still
/// there when it does, so a stop cannot be lost to timing.
#[derive(Clone, Debug)]
pub struct Stop(Arc<watch::Sender<bool>>);

impl Stop {
    /// A handle for a turn nobody has asked to stop.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(watch::Sender::new(false)))
    }

    /// Asks the turn to stop talking and end.
    ///
    /// Returns immediately; the turn stops at its next await. Calling this more
    /// than once is harmless.
    pub fn request(&self) {
        self.0.send_replace(true);
    }

    /// Whether a stop has been asked for, without waiting.
    #[must_use]
    pub fn requested(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once a stop has been asked for.
    ///
    /// Resolves immediately if one already has been. Takes `&self` so a turn
    /// can race this against work that borrows the turn too.
    pub async fn wait(&self) {
        let mut receiver = self.0.subscribe();
        // `subscribe` marks the current value as seen, so the value is read
        // directly rather than waited for — otherwise an already-requested stop
        // would wait for a second request that never comes.
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                // The sender lives in this `Arc`, so it cannot have dropped
                // while `self` is alive. Nothing can arrive now regardless.
                std::future::pending::<()>().await;
            }
        }
    }
}

impl Default for Stop {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs `work`, unless a stop is asked for first.
///
/// Returns `None` when the stop won, in which case `work` is dropped part-way
/// through. That is why providers are documented as safe to abandon.
pub(crate) async fn until_stopped<F: Future>(stop: &Stop, work: F) -> Option<F::Output> {
    tokio::select! {
        // Biased so an outstanding stop is honoured rather than left to a coin
        // toss against work that is already ready.
        biased;
        () = stop.wait() => None,
        output = work => Some(output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_stop_asked_for_before_anyone_waits_is_still_seen() {
        // The turn only looks between awaits, so a request that arrived while
        // it was busy must not be missed.
        let stop = Stop::new();
        stop.request();

        assert!(stop.requested());
        tokio::time::timeout(std::time::Duration::from_secs(1), stop.wait())
            .await
            .expect("an already-requested stop resolves immediately");
    }

    #[tokio::test]
    async fn every_clone_stops_the_same_turn() {
        let stop = Stop::new();
        let clone = stop.clone();
        clone.request();

        assert!(stop.requested(), "a clone must not have its own signal");
    }

    #[tokio::test]
    async fn work_finishes_when_no_one_asks_to_stop() {
        let stop = Stop::new();
        assert_eq!(until_stopped(&stop, async { 7 }).await, Some(7));
    }

    #[tokio::test]
    async fn a_stop_abandons_work_that_would_never_finish() {
        let stop = Stop::new();
        stop.request();

        let abandoned = until_stopped(&stop, std::future::pending::<()>());
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), abandoned)
                .await
                .expect("the stop wins"),
            None
        );
    }
}
