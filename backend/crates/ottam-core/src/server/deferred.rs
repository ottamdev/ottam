use crate::server::outbound::{ConnectionId, OutboundSink};
use serde_json::Value;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;

/// How many deferred handler tails may run at once.
///
/// These are subprocess-bound, not CPU-bound, so the cap exists to stop a
/// client that spams one method from spawning unbounded `git` fan-out, not to
/// ration cores. Work past the cap is rejected immediately so deferred tasks
/// cannot accumulate an unbounded waiting queue.
const DEFERRED_SLOTS: usize = 4;

/// A bounded, non-queuing admission lane for deferred work.
///
/// Owners keep separate lanes when their capacities or scheduling purposes
/// differ. Acquisition is deliberately nonblocking: callers map saturation to
/// their existing protocol-specific error response rather than waiting in an
/// unbounded queue.
#[derive(Clone)]
pub(super) struct DeferredLane {
    slots: Arc<Semaphore>,
}

impl DeferredLane {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(capacity)),
        }
    }

    pub(super) fn try_acquire(&self) -> Result<OwnedSemaphorePermit, ()> {
        Arc::clone(&self.slots).try_acquire_owned().map_err(|_| ())
    }
}

/// Owns deferred response tasks and routes each completed response back to the
/// connection that submitted it.
pub(super) struct DeferredResponseTasks {
    outbound: OutboundSink,
    tasks: JoinSet<()>,
    failure_context: &'static str,
}

impl DeferredResponseTasks {
    pub(super) fn new(outbound: OutboundSink, failure_context: &'static str) -> Self {
        Self {
            outbound,
            tasks: JoinSet::new(),
            failure_context,
        }
    }

    /// Reap completed handles so a long-lived Core does not retain one task
    /// allocation per deferred response.
    pub(super) fn reap_finished(&mut self) {
        while let Some(result) = self.tasks.try_join_next() {
            if let Err(error) = result {
                if !error.is_cancelled() {
                    tracing::warn!(
                        error = %error,
                        context = self.failure_context,
                        "deferred response task failed"
                    );
                }
            }
        }
    }

    /// Spawn work that has already acquired a permit from its owner's lane.
    pub(super) fn spawn_admitted<F>(
        &mut self,
        origin: ConnectionId,
        permit: OwnedSemaphorePermit,
        future: F,
    ) where
        F: Future<Output = Value> + Send + 'static,
    {
        let outbound = self.outbound.clone();
        self.tasks.spawn(async move {
            let _permit = permit;
            let _ = outbound.respond(origin, future.await);
        });
    }

    /// Abandon in-flight response work after the owner's dispatch loop has
    /// stopped. Owners that synchronize access must release their guard before
    /// awaiting this consuming operation.
    pub(super) async fn shutdown(mut self) {
        self.tasks.abort_all();
        while let Some(result) = self.tasks.join_next().await {
            if let Err(error) = result {
                if !error.is_cancelled() {
                    tracing::warn!(
                        error = %error,
                        context = self.failure_context,
                        "deferred response task failed during shutdown"
                    );
                }
            }
        }
    }
}

/// Runs the slow tail of a handler off the serial dispatch loop.
///
/// The dispatcher owns `AgentServer` and awaits each handler inline, so a
/// handler that waits on a subprocess holds up *every* request behind it — a
/// `git.diffSnapshot` of a repository with a few hundred untracked files spawns
/// one `git` per file and parks the loop for seconds while trivial calls queue
/// behind it. `tokio::task::spawn_blocking` alone does not help: it moves the
/// work off a runtime worker, but the dispatcher still awaits the result.
///
/// A handler does its `&mut self` work inline and hands the rest here. The
/// response is then written by the spawned task, so it can arrive out of
/// arrival order — which the client already tolerates, because it correlates
/// replies by request id, and because `agent.*` and `engine.list` have always
/// answered this way.
pub(super) struct DeferredResponses {
    lane: DeferredLane,
    tasks: DeferredResponseTasks,
}

impl DeferredResponses {
    pub(super) fn new(outbound: OutboundSink) -> Self {
        Self {
            lane: DeferredLane::new(DEFERRED_SLOTS),
            tasks: DeferredResponseTasks::new(outbound, "deferred handler response"),
        }
    }

    pub(super) fn spawn<F>(&mut self, origin: ConnectionId, future: F) -> Result<(), ()>
    where
        F: Future<Output = Value> + Send + 'static,
    {
        self.tasks.reap_finished();
        let permit = self.lane.try_acquire()?;
        self.tasks.spawn_admitted(origin, permit, future);
        Ok(())
    }

    /// Abandon in-flight work. Called once the dispatch loop has stopped, so
    /// nothing is left to receive these responses anyway.
    pub(super) async fn shutdown(self) {
        self.tasks.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::outbound::{test_sink, Destination};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn generic_pool_rejects_saturation_without_polling_rejected_work() {
        let (outbound, _outbound_rx) = test_sink();
        let mut responses = DeferredResponses::new(outbound);

        for _ in 0..DEFERRED_SLOTS {
            assert!(responses
                .spawn(ConnectionId::Primary, std::future::pending())
                .is_ok());
        }

        let rejected_polled = Arc::new(AtomicBool::new(false));
        let probe = Arc::clone(&rejected_polled);
        assert!(responses
            .spawn(ConnectionId::Primary, async move {
                probe.store(true, Ordering::SeqCst);
                json!({"unexpected": true})
            })
            .is_err());
        assert!(!rejected_polled.load(Ordering::SeqCst));

        responses.shutdown().await;
    }

    #[test]
    fn named_lanes_have_independent_capacity() {
        let regular = DeferredLane::new(1);
        let cancel = DeferredLane::new(1);
        let held_regular = regular.try_acquire().expect("regular lane admits work");

        assert!(regular.try_acquire().is_err());
        assert!(cancel.try_acquire().is_ok());

        drop(held_regular);
        assert!(regular.try_acquire().is_ok());
    }

    #[tokio::test]
    async fn completed_responses_preserve_primary_and_attached_origins() {
        let (outbound, mut outbound_rx) = test_sink();
        let mut tasks = DeferredResponseTasks::new(outbound, "test response");
        let lane = DeferredLane::new(2);

        tasks.spawn_admitted(
            ConnectionId::Primary,
            lane.try_acquire().expect("primary permit"),
            async { json!({"id": "primary"}) },
        );
        tasks.spawn_admitted(
            ConnectionId::Attached(7),
            lane.try_acquire().expect("attached permit"),
            async { json!({"id": "attached"}) },
        );

        let first = outbound_rx.recv().await.expect("first routed response");
        let second = outbound_rx.recv().await.expect("second routed response");
        let routed = [first, second];
        assert!(routed.iter().any(|frame| {
            frame.destination == Destination::Connection(ConnectionId::Primary)
                && frame.frame == json!({"id": "primary"})
        }));
        assert!(routed.iter().any(|frame| {
            frame.destination == Destination::Connection(ConnectionId::Attached(7))
                && frame.frame == json!({"id": "attached"})
        }));

        tasks.shutdown().await;
    }

    #[tokio::test]
    async fn permit_is_held_until_completion_and_finished_task_is_reaped() {
        let (outbound, mut outbound_rx) = test_sink();
        let mut tasks = DeferredResponseTasks::new(outbound, "test response");
        let lane = DeferredLane::new(1);
        let (release_tx, release_rx) = oneshot::channel();

        tasks.spawn_admitted(
            ConnectionId::Primary,
            lane.try_acquire().expect("response permit"),
            async move {
                release_rx.await.expect("release response");
                json!({"id": 1})
            },
        );
        assert!(lane.try_acquire().is_err(), "future must retain its permit");

        release_tx.send(()).expect("release response task");
        assert_eq!(
            outbound_rx.recv().await.expect("completed response").frame,
            json!({"id": 1})
        );
        for _ in 0..8 {
            tokio::task::yield_now().await;
            tasks.reap_finished();
            if tasks.tasks.is_empty() {
                break;
            }
        }
        assert!(
            tasks.tasks.is_empty(),
            "finished response handle must be reaped"
        );
        assert!(lane.try_acquire().is_ok(), "completion releases the permit");

        tasks.shutdown().await;
    }

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn shutdown_aborts_and_joins_in_flight_work() {
        let (outbound, _outbound_rx) = test_sink();
        let mut tasks = DeferredResponseTasks::new(outbound, "test response");
        let lane = DeferredLane::new(1);
        let dropped = Arc::new(AtomicBool::new(false));
        let drop_signal = Arc::clone(&dropped);
        let (started_tx, started_rx) = oneshot::channel();

        tasks.spawn_admitted(
            ConnectionId::Primary,
            lane.try_acquire().expect("response permit"),
            async move {
                let _drop_signal = DropSignal(drop_signal);
                started_tx.send(()).expect("mark task started");
                std::future::pending().await
            },
        );
        started_rx.await.expect("response task starts");
        assert!(lane.try_acquire().is_err());

        tasks.shutdown().await;

        assert!(dropped.load(Ordering::SeqCst));
        assert!(lane.try_acquire().is_ok(), "abort releases the permit");
    }
}
