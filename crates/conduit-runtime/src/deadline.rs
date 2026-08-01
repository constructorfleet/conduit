//! Bounding how long a turn may go without getting anywhere.
//!
//! A provider that accepts a request and never answers used to stall a turn for
//! as long as the client stayed connected: the bounded output channel bounds
//! memory, not time. Nothing else would notice, because a turn is reachable only
//! through the audio it produces, and a turn producing nothing looks exactly
//! like a turn thinking hard.
//!
//! Progress is defined here as *publishing an event*, which is the same thing an
//! operator watching `/v1/events` uses to decide a pipeline is alive. Defining
//! it that way means one deadline covers every stage — recognition, reasoning,
//! tools, synthesis — including a provider this runtime has never heard of,
//! rather than needing a bound written around each await separately. It also
//! bounds the right thing: a model streaming tokens for two minutes is working
//! while a model silent for one is not, and a deadline on a stage's *total* time
//! could not tell those apart.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use conduit_core::event::Stage;
use tokio::sync::Notify;

/// How long a turn may publish nothing before it is abandoned.
///
/// Deliberately generous. The failure this bounds is a turn wedged forever, not
/// a slow one, and a deadline tight enough to feel responsive would abandon a
/// working deployment whose local model takes a while to produce its first
/// token. A deployment that wants a different bound sets one; see
/// [`Runner::with_idle_timeout`](crate::Runner::with_idle_timeout).
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// A turn's "something is still happening" signal.
///
/// Cloning yields a handle to the same turn, so every stage reports through one
/// marker however concurrently they run.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    /// Woken every time the turn gets somewhere.
    made: Arc<Notify>,
    /// The stage that reported last, so a stalled turn can say where it was.
    stage: Arc<Mutex<Option<Stage>>>,
}

impl Progress {
    /// Records that the turn just reported something from `stage`.
    pub(crate) fn reached(&self, stage: Stage) {
        *self.stage.lock().unwrap_or_else(PoisonError::into_inner) = Some(stage);
        self.made.notify_one();
    }

    /// The stage that reported most recently, if anything has.
    #[must_use]
    pub fn stalled_at(&self) -> Option<Stage> {
        *self.stage.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Runs `work`, unless it stops reporting progress for `idle` first.
///
/// Returns `Err` with the stage that reported last when the deadline won, in
/// which case `work` is dropped part-way through — the same abandonment a client
/// asking to stop causes, which providers are documented as safe for.
///
/// `idle` of `None` removes the bound entirely: `work` then runs until it
/// finishes or something else ends it.
pub(crate) async fn until_idle<F: Future>(
    progress: &Progress,
    idle: Option<Duration>,
    work: F,
) -> Result<F::Output, Option<Stage>> {
    tokio::select! {
        // Not biased, unlike a stop: a deadline that fired at the same moment
        // the work finished describes a turn that did in fact finish, and
        // reporting it as abandoned would be a lie about a completed reply.
        output = work => Ok(output),
        stalled = when_idle(progress, idle) => Err(stalled),
    }
}

/// Resolves once `progress` has reported nothing for `idle`, naming where it
/// stopped. Never resolves when `idle` is `None`.
///
/// The clock restarts on every report rather than running against the turn as a
/// whole, so a long reply is never mistaken for a stalled one.
async fn when_idle(progress: &Progress, idle: Option<Duration>) -> Option<Stage> {
    let Some(idle) = idle else {
        return std::future::pending().await;
    };

    // A report that lands while nothing is waiting is remembered rather than
    // lost, which costs one immediate extra pass around this loop and buys the
    // guarantee that matters: the deadline can fire late, never early.
    while tokio::time::timeout(idle, progress.made.notified()).await.is_ok() {}

    progress.stalled_at()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deadline short enough to fire promptly in a test.
    const BRIEF: Option<Duration> = Some(Duration::from_millis(20));

    #[tokio::test]
    async fn work_that_reports_nothing_is_abandoned() {
        let progress = Progress::default();
        progress.reached(Stage::Reasoning);

        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            until_idle(&progress, BRIEF, std::future::pending::<()>()),
        )
        .await
        .expect("the deadline fires");

        assert_eq!(outcome, Err(Some(Stage::Reasoning)), "it must say where it got stuck");
    }

    #[tokio::test]
    async fn work_that_keeps_reporting_is_left_alone() {
        // The property that makes this usable at all: a long reply is not a
        // stalled one, so anything still reporting must never be abandoned.
        let progress = Progress::default();
        let reporting = progress.clone();

        let outcome = until_idle(&progress, Some(Duration::from_millis(40)), async move {
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(5)).await;
                reporting.reached(Stage::Synthesis);
            }
            "finished"
        })
        .await;

        assert_eq!(outcome, Ok("finished"), "reporting every 5ms must survive a 40ms deadline");
    }

    #[tokio::test]
    async fn work_that_finishes_is_not_reported_as_stalled() {
        let progress = Progress::default();
        assert_eq!(until_idle(&progress, BRIEF, async { 7 }).await, Ok(7));
    }

    #[tokio::test]
    async fn the_deadline_can_be_removed() {
        // A deployment may choose this. Nothing then abandons a turn for time.
        let progress = Progress::default();
        let never = tokio::time::timeout(
            Duration::from_millis(50),
            until_idle(&progress, None, std::future::pending::<()>()),
        );
        assert!(never.await.is_err(), "no deadline means no turn is abandoned for time");
    }

    #[tokio::test]
    async fn work_that_never_reported_anything_names_no_stage() {
        // Reachable when a deadline expires before a turn's opening events. It
        // must still end, rather than claim a stage the turn never reached.
        let progress = Progress::default();
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            until_idle(&progress, BRIEF, std::future::pending::<()>()),
        )
        .await
        .expect("the deadline fires");

        assert_eq!(outcome, Err(None));
    }

    #[test]
    fn every_clone_reports_to_the_same_turn() {
        let progress = Progress::default();
        progress.clone().reached(Stage::Tools);
        assert_eq!(progress.stalled_at(), Some(Stage::Tools));
    }
}
