use crate::actor::task::ActorTaskHandle;

/// The executor seam: spawns actor run-loop futures onto a task executor.
///
/// Implementations live in `runtime` backends (e.g. the tokio spawner). The
/// returned [`ActorTaskHandle`] is the type-erased handle attached to the
/// actor's shared state.
pub trait ActorTaskSpawner {
    /// Spawn the future onto the executor.
    fn spawn<F>(&self, fut: F) -> ActorTaskHandle
    where
        F: Future<Output = ()> + Send + 'static;
}
