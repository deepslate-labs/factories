use crate::actor::Actor;
use crate::actor::extension::ExtensionSet;
use crate::actor::lifecycle::{TerminationKind, TerminationReason};
use crate::actor::supervision::{Subscription, ActorId};
use crate::actor::task::ActorTaskHandle;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt::{Debug, Formatter};
use core::sync::atomic::{AtomicU8, Ordering};
#[cfg(feature = "capture")]
use core::sync::atomic::AtomicUsize;
use core::task::{Poll, Waker};
use critical_section::Mutex;
use once_cell::sync::OnceCell;

/// A set of wakers awaiting a lifecycle transition.
///
/// Unlike [`futures::task::AtomicWaker`], this supports any number of
/// concurrent waiters: every registered waker is woken on the next
/// transition. Registration is idempotent per task (a waker that
/// [`Waker::will_wake`] an already-registered one is dropped), so a future
/// re-registering on each poll does not grow the set without bound.
struct WakerSet {
    wakers: Mutex<RefCell<Vec<Waker>>>,
}

impl Default for WakerSet {
    fn default() -> Self {
        Self::new()
    }
}

impl WakerSet {
    const fn new() -> Self {
        Self {
            wakers: Mutex::new(RefCell::new(Vec::new())),
        }
    }

    /// Register `waker` to be woken on the next transition, unless an
    /// equivalent waker is already registered.
    fn register(&self, waker: &Waker) {
        critical_section::with(|cs| {
            let mut wakers = self.wakers.borrow_ref_mut(cs);
            if wakers.iter().any(|existing| existing.will_wake(waker)) {
                return;
            }
            wakers.push(waker.clone());
        });
    }

    /// Wake and clear every registered waker. Still-pending futures
    /// re-register on their next poll.
    fn wake_all(&self) {
        let wakers =
            critical_section::with(|cs| core::mem::take(&mut *self.wakers.borrow_ref_mut(cs)));
        for waker in wakers {
            waker.wake();
        }
    }
}

impl Debug for WakerSet {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let len = critical_section::with(|cs| self.wakers.borrow_ref(cs).len());
        f.debug_struct("WakerSet").field("waiters", &len).finish()
    }
}

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
    waiters: WakerSet,
}

impl LifecycleCell {
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(LifecycleState::Starting as u8),
            waiters: WakerSet::new(),
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
        self.waiters.wake_all();
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
        self.waiters.wake_all();
    }

    /// Wait until the state reaches the terminal `Dead`.
    pub fn wait_for_terminal(&self) -> impl Future<Output = ()> + '_ {
        core::future::poll_fn(move |cx| {
            if self.current() == LifecycleState::Dead {
                return Poll::Ready(());
            }

            self.waiters.register(cx.waker());

            // Re-check after registering to close the race against the
            // transition between the first load and the registration.
            if self.current() == LifecycleState::Dead {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
    }

    /// Wait until the state leaves `Starting` and return the observed state.
    pub fn wait_leave_starting(&self) -> impl Future<Output = LifecycleState> + '_ {
        core::future::poll_fn(move |cx| {
            let state = self.current();
            if state != LifecycleState::Starting {
                return Poll::Ready(state);
            }

            self.waiters.register(cx.waker());

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
    id: ActorId,
    reason: OnceCell<TerminationReason<A>>,
    lifecycle: LifecycleCell,

    // Termination watchers registered via `watch`. Each holds a weak reference
    // to its watcher, so this never keeps a watcher alive; non-generic
    // (`Subscription` is erased), so it never couples this state to
    // `A::Channel: Send`. Drained and delivered on the terminal transition.
    subscriptions: Mutex<RefCell<Vec<Subscription>>>,

    // Late-bound: the run loop future captures the shared state *before* the
    // spawner produces the task handle, so the handle is attached post-spawn.
    task: OnceCell<ActorTaskHandle>,
    shared_data: A::SharedData,

    // Type-erased, pointer-keyed context injected at spawn (see
    // [`crate::actor::extension`]); frozen here, read-only for the actor's life.
    extensions: ExtensionSet,

    // Per-actor capture sequence: mints `(id, seq)` event ids on this actor's
    // own cache line, so no global counter is needed. Bumped `Relaxed` only when
    // the mesh is captured.
    #[cfg(feature = "capture")]
    capture_seq: AtomicUsize,

    // The spawn-site link recorded when this actor's `Spawned` event is emitted:
    // who spawned it and the event that caused the spawn.
    #[cfg(feature = "capture")]
    capture_birth: OnceCell<Option<crate::capture::CaptureFrame>>,
}

impl<A: Actor + ?Sized> InnerSharedActorState<A> {
    fn new(extensions: ExtensionSet) -> Self {
        Self {
            id: ActorId::new(),
            reason: OnceCell::new(),
            lifecycle: LifecycleCell::new(),
            subscriptions: Mutex::new(RefCell::new(Vec::new())),
            task: OnceCell::new(),
            shared_data: A::SharedData::default(),
            extensions,
            #[cfg(feature = "capture")]
            capture_seq: AtomicUsize::new(1),
            #[cfg(feature = "capture")]
            capture_birth: OnceCell::new(),
        }
    }
}

impl<A: Actor + ?Sized> Debug for InnerSharedActorState<A>
where
    A::Error: Debug,
    A::SharedData: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InnerSharedActorState")
            .field("id", &self.id)
            .field("reason", &self.reason)
            .field("lifecycle", &self.lifecycle)
            .field("task", &self.task)
            .field("shared_data", &self.shared_data)
            .finish()
    }
}

/// State shared between the identity and run loop.
pub struct SharedActorState<A: Actor + ?Sized> {
    inner: Arc<InnerSharedActorState<A>>,
}

impl<A: Actor + ?Sized> SharedActorState<A> {
    /// Create the shared state, seeding it with the extensions injected at spawn
    /// (empty for an actor with none). The set is frozen here for the actor's life.
    pub fn new(extensions: ExtensionSet) -> Self {
        Self {
            inner: Arc::new(InnerSharedActorState::new(extensions)),
        }
    }

    /// Record that the actor failed with `error`.
    ///
    /// Write-once: the first recorded outcome wins, so this is a no-op once any
    /// reason ([`Finished`](TerminationReason::Finished) /
    /// [`Failed`](TerminationReason::Failed) /
    /// [`Aborted`](TerminationReason::Aborted)) is set. The reason is recorded
    /// *before* the [`Dead`](LifecycleState::Dead) transition so observers of
    /// the terminal state reliably see it.
    pub fn record_failure(&self, error: A::Error) {
        let _ = self.inner.reason.set(TerminationReason::Failed(error));
    }

    /// Record that the actor finished cleanly. No-op if an outcome is already
    /// recorded (e.g. a handler failed it first).
    pub fn mark_finished(&self) {
        let _ = self.inner.reason.set(TerminationReason::Finished);
    }

    /// Record that the actor was aborted (panic / task abort). No-op if an
    /// outcome is already recorded; this is the
    /// [`DeadOnDropGuard`]'s fallback for the paths that record nothing else.
    pub fn mark_aborted(&self) {
        let _ = self.inner.reason.set(TerminationReason::Aborted);
    }

    /// The recorded termination reason, if any.
    pub fn termination_reason(&self) -> Option<&TerminationReason<A>> {
        self.inner.reason.get()
    }

    /// The error the actor failed with, if its recorded reason is
    /// [`Failed`](TerminationReason::Failed).
    pub fn failed_error(&self) -> Option<&A::Error> {
        match self.inner.reason.get() {
            Some(TerminationReason::Failed(error)) => Some(error),
            _ => None,
        }
    }

    /// This actor's process-unique, never-reused identity.
    pub fn id(&self) -> ActorId {
        self.inner.id
    }

    /// Register a termination subscription (a watcher to notify on death).
    ///
    /// Appended to this (the watched) actor's set; delivered on the terminal
    /// transition. Registering after the actor is already dead simply never
    /// fires (the set was drained at termination).
    pub(crate) fn add_subscription(&self, subscription: Subscription) {
        critical_section::with(|cs| {
            self.inner.subscriptions.borrow_ref_mut(cs).push(subscription);
        });
    }

    /// Remove every subscription registered by the given watcher (its `unwatch`).
    pub(crate) fn remove_subscriptions(&self, watcher: ActorId) {
        critical_section::with(|cs| {
            self.inner
                .subscriptions
                .borrow_ref_mut(cs)
                .retain(|subscription| subscription.watcher_id() != watcher);
        });
    }

    /// Drain the recorded subscriptions, returning them with the termination
    /// kind to deliver. `None` if no reason is recorded yet.
    fn take_subscriptions(&self) -> Option<(TerminationKind, Vec<Subscription>)> {
        let kind = self.inner.reason.get().map(TerminationReason::kind)?;
        let subscriptions = critical_section::with(|cs| {
            core::mem::take(&mut *self.inner.subscriptions.borrow_ref_mut(cs))
        });
        Some((kind, subscriptions))
    }

    /// Drain the subscriptions and deliver a termination signal to each watcher,
    /// awaiting mailbox room so signals are not dropped under back-pressure.
    ///
    /// Used on the run loop's async terminal path. Delivers before `Dead` is
    /// announced, so a `wait_for_terminal` waiter that then queries a watcher
    /// sees the signal already delivered.
    pub(crate) async fn notify_subscribers(&self) {
        let Some((kind, subscriptions)) = self.take_subscriptions() else {
            return;
        };

        let id = self.id();
        for subscription in subscriptions {
            subscription.deliver(id, A::RTTI, kind).await;
        }
    }

    /// Non-awaiting subscriber notification for the terminal `Drop` path (panic
    /// / task abort), where the async [`notify_subscribers`](Self::notify_subscribers)
    /// could not run. Best-effort per subscriber.
    pub(crate) fn notify_subscribers_now(&self) {
        let Some((kind, subscriptions)) = self.take_subscriptions() else {
            return;
        };

        let id = self.id();
        for subscription in subscriptions {
            subscription.deliver_now(id, A::RTTI, kind);
        }
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

    /// The actor's user-defined shared data
    /// ([`Actor::SharedData`]).
    pub fn shared_data(&self) -> &A::SharedData {
        &self.inner.shared_data
    }

    /// The set of type-erased extensions injected at spawn.
    pub fn extensions(&self) -> &ExtensionSet {
        &self.inner.extensions
    }

    /// The capture sink configured for this actor's mesh, if any.
    #[cfg(feature = "capture")]
    pub(crate) fn capture_sink(&self) -> Option<&Arc<dyn crate::capture::CaptureSink>> {
        self.inner.extensions.get(crate::capture::CAPTURE_SINK)
    }

    /// Mint this actor's next capture event id `(id, next per-actor seq)`. The
    /// seq lives on this actor's own cache line, bumped `Relaxed` - no global
    /// counter.
    #[cfg(feature = "capture")]
    pub(crate) fn next_capture_event_id(&self) -> crate::capture::EventId {
        let seq = self.inner.capture_seq.fetch_add(1, Ordering::Relaxed);
        crate::capture::EventId {
            actor: self.inner.id,
            seq,
        }
    }

    /// Record the frame of the actor that spawned this one.
    #[cfg(feature = "capture")]
    pub(crate) fn set_capture_birth(&self, frame: Option<crate::capture::CaptureFrame>) {
        let _ = self.inner.capture_birth.set(frame);
    }

    /// The frame of the actor that spawned this one, if any (`None` for a root
    /// spawn or if none was recorded).
    #[cfg(feature = "capture")]
    pub(crate) fn capture_birth(&self) -> Option<crate::capture::CaptureFrame> {
        self.inner.capture_birth.get().copied().flatten()
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

    /// Wait until the lifecycle reaches the terminal `Dead`.
    ///
    /// Any number of observers may await this concurrently; all are woken when
    /// the actor dies. Resolves immediately if the actor is already dead.
    pub fn wait_for_terminal(&self) -> impl Future<Output = ()> + '_ {
        self.inner.lifecycle.wait_for_terminal()
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
        // Record the fallback outcome before the terminal transition, so
        // observers of `Dead` always see *some* reason. A clean drain or a
        // handler failure has already recorded one, making this a no-op; only
        // a panic / task abort lands here with an empty reason cell.
        self.state.mark_aborted();
        // Only the abort path lands here with `Aborted` actually recorded (the
        // clean / failed paths recorded their reason earlier, so `mark_aborted`
        // was a no-op above): emit the abnormal-termination event for it.
        if matches!(
            self.state.termination_reason(),
            Some(TerminationReason::Aborted)
        ) {
            crate::obs::actor_aborted(A::RTTI.name(), self.state.id());
            #[cfg(feature = "capture")]
            crate::capture::record_died(&self.state);
        }
        // Fallback notification for the paths that skip the async terminal
        // (panic / task abort): can't await here, so best-effort. The clean and
        // failed paths already delivered (and drained) via the async
        // `notify_subscribers`, making this a no-op for them.
        self.state.notify_subscribers_now();
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
    use alloc::boxed::Box;
    use core::pin::Pin;
    use core::sync::atomic::AtomicUsize;
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

    struct CountingWaker(AtomicUsize);

    impl futures::task::ArcWake for CountingWaker {
        fn wake_by_ref(arc_self: &Arc<Self>) {
            arc_self
                .0
                .fetch_add(1, Ordering::SeqCst);
        }
    }

    impl CountingWaker {
        fn new() -> Arc<Self> {
            Arc::new(Self(AtomicUsize::new(0)))
        }

        fn count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn wait_for_terminal_wakes_all_waiters() {
        let cell = LifecycleCell::new();

        let w1 = CountingWaker::new();
        let w2 = CountingWaker::new();
        let waker1 = futures::task::waker(w1.clone());
        let waker2 = futures::task::waker(w2.clone());

        let mut f1 = Box::pin(cell.wait_for_terminal());
        let mut f2 = Box::pin(cell.wait_for_terminal());

        assert!(
            f1.as_mut()
                .poll(&mut Context::from_waker(&waker1))
                .is_pending()
        );
        assert!(
            f2.as_mut()
                .poll(&mut Context::from_waker(&waker2))
                .is_pending()
        );

        cell.transition_dead();

        assert_eq!(w1.count(), 1, "first terminal waiter must be woken");
        assert_eq!(w2.count(), 1, "second terminal waiter must be woken");
    }

    #[test]
    fn wait_for_terminal_pending_until_dead() {
        let cell = LifecycleCell::new();
        let mut fut = cell.wait_for_terminal();

        // Still starting: pending.
        assert!(poll_once(&mut fut).is_pending());

        // Leaving Starting for Running is not terminal: still pending.
        cell.transition_running();
        assert!(poll_once(&mut fut).is_pending());

        // Reaching Dead resolves it.
        cell.transition_dead();
        assert_eq!(poll_once(&mut fut), Poll::Ready(()));
    }
}
