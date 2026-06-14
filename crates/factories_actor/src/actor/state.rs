use crate::actor::Actor;
use crate::actor::task::ActorTaskHandle;
use alloc::sync::Arc;
use core::fmt::{Debug, Formatter};
use core::sync::atomic::{AtomicU8, Ordering};
use core::task::Poll;
use futures::task::AtomicWaker;
use once_cell::sync::OnceCell;

/// The lifecycle of an actor, from spawn to death.
#[repr(u8)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum LifecycleState {
    /// The actor task exists but initialization has not completed yet.
    Starting = 0,

    /// Initialization succeeded and the run loop is processing messages.
    Running = 1,

    /// The run loop has stopped pulling new work and is running the actor's
    /// stop hook before the actor is dropped. Reached from [`Running`](Self::Running).
    Stopping = 2,

    /// The actor is dead: init failed, the run loop exited or the task was
    /// aborted. This state is terminal.
    Dead = 3,
}

impl LifecycleState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Starting,
            1 => Self::Running,
            2 => Self::Stopping,
            3 => Self::Dead,
            _ => unreachable!("invalid lifecycle state"),
        }
    }
}

/// Non-generic lifecycle tracking cell.
///
/// Holds the lifecycle state of an actor plus the waker of a single waiter
/// awaiting the departure from [`LifecycleState::Starting`].
#[derive(Debug, Default)]
pub struct LifecycleCell {
    state: AtomicU8,
    waker: AtomicWaker,
}

impl LifecycleCell {
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(LifecycleState::Starting as u8),
            waker: AtomicWaker::new(),
        }
    }

    /// The current lifecycle state.
    pub fn current(&self) -> LifecycleState {
        LifecycleState::from_raw(self.state.load(Ordering::Acquire))
    }

    /// Transition from `Starting` to `Running`.
    ///
    /// No-op if the state is not `Starting` (the actor may already be dead).
    pub fn transition_running(&self) {
        let _ = self.state.compare_exchange(
            LifecycleState::Starting as u8,
            LifecycleState::Running as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.waker.wake();
    }

    /// Transition from `Running` to `Stopping`.
    ///
    /// No-op if the state is not `Running` (the actor may already be dead, or
    /// never left `Starting` because init/start failed).
    pub fn transition_stopping(&self) {
        let _ = self.state.compare_exchange(
            LifecycleState::Running as u8,
            LifecycleState::Stopping as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// Transition to `Dead`. Terminal and idempotent.
    pub fn transition_dead(&self) {
        self.state
            .store(LifecycleState::Dead as u8, Ordering::Release);
        self.waker.wake();
    }

    /// Wait until the state leaves `Starting` and return the observed state.
    ///
    /// Only a single waiter is supported: a later registration replaces an
    /// earlier one. The spawn path has exactly one waiter (`spawn_ready`).
    pub fn wait_leave_starting(&self) -> impl Future<Output = LifecycleState> + '_ {
        core::future::poll_fn(move |cx| {
            let state = self.current();
            if state != LifecycleState::Starting {
                return Poll::Ready(state);
            }

            self.waker.register(cx.waker());

            // Re-check after registering to close the race against a transition
            // between the first load and the registration.
            let state = self.current();
            if state != LifecycleState::Starting {
                Poll::Ready(state)
            } else {
                Poll::Pending
            }
        })
    }
}

struct InnerSharedActorState<A: Actor + ?Sized> {
    error: OnceCell<A::Error>,
    lifecycle: LifecycleCell,

    // Late-bound: the run loop future captures the shared state *before* the
    // spawner produces the task handle, so the handle is attached post-spawn.
    task: OnceCell<ActorTaskHandle>,
    extension: A::SharedStateExtension,
}

impl<A: Actor + ?Sized> InnerSharedActorState<A> {
    fn new() -> Self {
        Self {
            error: OnceCell::new(),
            lifecycle: LifecycleCell::new(),
            task: OnceCell::new(),
            extension: A::SharedStateExtension::default(),
        }
    }
}

impl<A: Actor + ?Sized> Debug for InnerSharedActorState<A>
where
    A::Error: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InnerSharedActorState")
            .field("error", &self.error)
            .field("lifecycle", &self.lifecycle)
            .field("task", &self.task)
            .finish()
    }
}

/// State shared between the identity and run loop.
pub struct SharedActorState<A: Actor + ?Sized> {
    inner: Arc<InnerSharedActorState<A>>,
}

impl<A: Actor + ?Sized> SharedActorState<A> {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(InnerSharedActorState::new()),
        }
    }

    /// Set the error this actor has failed with.
    ///
    /// When reporting an init failure, set the error *before* transitioning to
    /// dead so that observers of [`LifecycleState::Dead`] reliably see it.
    pub fn set_error(&self, error: A::Error) -> Result<(), A::Error> {
        self.inner.error.set(error)
    }

    /// Get the error this actor has failed with, if any.
    pub fn get_error(&self) -> Option<&A::Error> {
        self.inner.error.get()
    }

    /// Attach the task handle of the spawned actor task.
    ///
    /// This is late-bound because the run loop future captures the shared state
    /// before the task spawner can produce a handle. Fails if a task handle was
    /// already attached.
    pub fn attach_task(&self, task: ActorTaskHandle) -> Result<(), ActorTaskHandle> {
        self.inner.task.set(task)
    }

    /// The task handle of the actor task, if one was attached.
    pub fn task(&self) -> Option<&ActorTaskHandle> {
        self.inner.task.get()
    }

    /// The actor's user-defined shared state extension
    /// ([`Actor::SharedStateExtension`]).
    pub fn extension(&self) -> &A::SharedStateExtension {
        &self.inner.extension
    }

    /// The current lifecycle state of the actor.
    pub fn lifecycle(&self) -> LifecycleState {
        self.inner.lifecycle.current()
    }

    /// Transition from `Starting` to `Running`. No-op outside `Starting`.
    pub fn transition_running(&self) {
        self.inner.lifecycle.transition_running();
    }

    /// Transition from `Running` to `Stopping`. No-op outside `Running`.
    pub fn transition_stopping(&self) {
        self.inner.lifecycle.transition_stopping();
    }

    /// Transition to `Dead`. Terminal and idempotent.
    pub fn transition_dead(&self) {
        self.inner.lifecycle.transition_dead();
    }

    /// Wait until the lifecycle leaves `Starting` and return the observed state.
    pub fn wait_leave_starting(&self) -> impl Future<Output = LifecycleState> + '_ {
        self.inner.lifecycle.wait_leave_starting()
    }

    /// Create a guard that transitions the lifecycle to `Dead` on drop.
    ///
    /// Run loops hold this for their whole lifetime so that loop exit, handler
    /// panics and task aborts all reliably mark the actor dead.
    pub fn dead_on_drop(&self) -> DeadOnDropGuard<A> {
        DeadOnDropGuard {
            state: self.clone(),
        }
    }
}

impl<A: Actor + ?Sized> Clone for SharedActorState<A> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<A: Actor + ?Sized> Debug for SharedActorState<A>
where
    Arc<InnerSharedActorState<A>>: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SharedActorState")
            .field("inner", &self.inner)
            .finish()
    }
}

/// Guard that transitions an actor's lifecycle to `Dead` when dropped.
pub struct DeadOnDropGuard<A: Actor + ?Sized> {
    state: SharedActorState<A>,
}

impl<A: Actor + ?Sized> Drop for DeadOnDropGuard<A> {
    fn drop(&mut self) {
        self.state.transition_dead();
    }
}

impl<A: Actor + ?Sized> Debug for DeadOnDropGuard<A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeadOnDropGuard").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::pin::Pin;
    use core::task::Context;

    fn poll_once<F: Future + Unpin>(fut: &mut F) -> Poll<F::Output> {
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        Pin::new(fut).poll(&mut cx)
    }

    #[test]
    fn starts_in_starting() {
        let cell = LifecycleCell::new();
        assert_eq!(cell.current(), LifecycleState::Starting);
    }

    #[test]
    fn transition_running_from_starting() {
        let cell = LifecycleCell::new();
        cell.transition_running();
        assert_eq!(cell.current(), LifecycleState::Running);
    }

    #[test]
    fn transition_dead_is_terminal() {
        let cell = LifecycleCell::new();
        cell.transition_dead();
        cell.transition_running();
        assert_eq!(cell.current(), LifecycleState::Dead, "dead is terminal");
    }

    #[test]
    fn wait_observes_running() {
        let cell = LifecycleCell::new();
        let mut fut = cell.wait_leave_starting();
        assert!(poll_once(&mut fut).is_pending());
        cell.transition_running();
        assert_eq!(poll_once(&mut fut), Poll::Ready(LifecycleState::Running));
    }

    #[test]
    fn wait_observes_dead() {
        let cell = LifecycleCell::new();
        let mut fut = cell.wait_leave_starting();
        assert!(poll_once(&mut fut).is_pending());
        cell.transition_dead();
        assert_eq!(poll_once(&mut fut), Poll::Ready(LifecycleState::Dead));
    }

    #[test]
    fn wait_ready_immediately_when_not_starting() {
        let cell = LifecycleCell::new();
        cell.transition_running();
        let mut fut = cell.wait_leave_starting();
        assert_eq!(poll_once(&mut fut), Poll::Ready(LifecycleState::Running));
    }
}
