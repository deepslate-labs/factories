use crate::actor::task::{ActorTaskHandle, ActorTaskHandleVTable, WaitForTerminationFut};
use crate::spawn::ActorTaskSpawner;
use alloc::boxed::Box;

/// Task spawner backed by a tokio runtime.
#[derive(Debug, Clone)]
pub struct TokioTaskSpawner {
    handle: tokio::runtime::Handle,
}

impl TokioTaskSpawner {
    /// Create a spawner for the given runtime handle.
    pub fn new(handle: tokio::runtime::Handle) -> Self {
        Self { handle }
    }

    /// Create a spawner for the current runtime.
    ///
    /// # Panics
    /// Panics when called outside a tokio runtime context.
    pub fn current() -> Self {
        Self::new(tokio::runtime::Handle::current())
    }
}

impl ActorTaskSpawner for TokioTaskSpawner {
    fn spawn<F>(&self, fut: F) -> ActorTaskHandle
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // The watch channel lets `wait_for_termination` be observed from `&self`;
        // a `JoinHandle` alone cannot (awaiting it consumes it).
        let (done_tx, done_rx) = tokio::sync::watch::channel(false);

        let join = self.handle.spawn(async move {
            fut.await;
            let _ = done_tx.send(true);
        });

        ActorTaskHandle::from_vtable(TokioTaskHandle {
            join,
            done: done_rx,
        })
    }
}

struct TokioTaskHandle {
    join: tokio::task::JoinHandle<()>,
    done: tokio::sync::watch::Receiver<bool>,
}

impl ActorTaskHandleVTable for TokioTaskHandle {
    fn abort(&self) {
        self.join.abort();
    }

    fn wait_for_termination(&self) -> WaitForTerminationFut {
        let mut done = self.done.clone();

        Box::pin(async move {
            // `wait_for` errors when the sender is dropped, which happens both on
            // normal completion (after sending true) and on abort - either way the
            // task has terminated.
            let _ = done.wait_for(|finished| *finished).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn spawn_runs_future_and_wait_observes_completion() {
        let spawner = TokioTaskSpawner::current();
        let ran = Arc::new(AtomicBool::new(false));
        let ran_clone = ran.clone();

        let task = spawner.spawn(async move {
            ran_clone.store(true, Ordering::SeqCst);
        });

        task.wait_for_termination().await;
        assert!(ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn abort_terminates_pending_task() {
        let spawner = TokioTaskSpawner::current();

        let task = spawner.spawn(async {
            // Never completes on its own
            futures::future::pending::<()>().await;
        });

        task.abort();
        // Must complete (and not hang) because the task was aborted.
        task.wait_for_termination().await;
    }
}
