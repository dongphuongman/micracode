//! Live read-model projection kept in sync with the event log (PRD FR2).
//!
//! [`ProjectionHandle`] owns a [`Projection`] behind an `RwLock` and a
//! background task that applies every newly appended event. It is built by
//! folding the existing log, then stays current off the store's hot broadcast
//! stream — the same gap-free pattern the SSE handler uses: subscribe *before*
//! the initial rebuild so no event slips through the seam, and rely on
//! [`Projection::apply`] being idempotent over `seq` to drop the overlap.

use std::sync::Arc;

use core_orchestration::Engine;
use core_projection::{Projection, Thread, ThreadSummary};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct ProjectionHandle {
    inner: Arc<RwLock<Projection>>,
}

impl ProjectionHandle {
    /// Build the projection from the current log and keep it live.
    ///
    /// Must be called from within a Tokio runtime (it spawns the applier task).
    pub fn spawn(engine: Arc<Engine>) -> Self {
        // Subscribe before reading the backlog so an event appended during the
        // rebuild is still delivered to the applier (and then skipped by the
        // seq guard if the rebuild already covered it).
        let mut rx = engine.store().subscribe();
        let backlog = engine.store().read_from(0).unwrap_or_default();
        let inner = Arc::new(RwLock::new(Projection::rebuild_from(&backlog)));

        let task_inner = Arc::clone(&inner);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => task_inner.write().await.apply(&event),
                    Err(RecvError::Lagged(_)) => {
                        // Fell behind the ring buffer: re-read the gap from the
                        // durable log and apply it, then resume the live stream.
                        let cursor = task_inner.read().await.cursor();
                        let missed = engine.store().read_from(cursor).unwrap_or_default();
                        let mut guard = task_inner.write().await;
                        for event in &missed {
                            guard.apply(event);
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });

        ProjectionHandle { inner }
    }

    /// All threads as summaries, in first-seen order.
    pub async fn summaries(&self) -> Vec<ThreadSummary> {
        self.inner.read().await.summaries()
    }

    /// A full thread (turns + messages) by id.
    pub async fn thread(&self, id: &str) -> Option<Thread> {
        self.inner.read().await.thread(id).cloned()
    }
}
