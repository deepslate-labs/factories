//! Shared building blocks for the standard run loops.
//!
//! A run loop is mostly the same boilerplate: an identical dispatch context, an
//! identical "ask the driver for the next handler future" turn, and identical
//! spawn scaffolding. The only thing a loop really owns is *how it schedules*
//! the handler futures - serialized, concurrent, bounded, prioritised. These
//! bricks factor out the rest, so the built-in loops (and any user loop) shrink
//! to their scheduling structure.

use crate::actor::event::{EventContext, EventDriver};
use crate::actor::handle::WeakActorHandle;
use crate::actor::lifecycle::StopReason;
use crate::actor::state::SharedActorState;
use crate::actor::work::{FutureWorkConverter, into_work};
use crate::actor::{
    Actor, ActorContext, ActorInit, ActorRunLoop, ActorRunLoopDispatchContext, LockStrategy,
};
use alloc::boxed::Box;
use core::fmt::{Debug, Formatter};
use core::future::Future;
use core::pin::Pin;

/// The unit of work the standard run loops drive: a `Send` future.
/// [`next_dispatch`](StandardDispatchContext::next_dispatch) gets the converter's
/// `Erased` straight from the dispatcher (written into a caller-owned slot) and
/// views it as this concrete future, so the loops never touch the GAT directly.
pub type StandardWork<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// The dispatch context shared by the standard run loops: the actor's lock
/// strategy plus its shared state.
///
/// A run loop owns one of these and exposes it as its
/// [`ActorRunLoop::DispatchContext`]. Dispatching a message goes through
/// [`next_dispatch`](Self::next_dispatch).
pub struct StandardDispatchContext<A: Actor + ?Sized> {
    lock_strategy: A::LockStrategy,
    shared: SharedActorState<A>,
    self_ref: WeakActorHandle<A>,
}

impl<A: Actor + ?Sized> StandardDispatchContext<A> {
    /// Build the context from the actor's lock strategy, shared state and weak
    /// self-reference (the actor's own handle, used for [`ActorContext::actor_ref`]).
    pub fn new(
        lock_strategy: A::LockStrategy,
        shared: SharedActorState<A>,
        self_ref: WeakActorHandle<A>,
    ) -> Self {
        Self {
            lock_strategy,
            shared,
            self_ref,
        }
    }

    /// The actor's shared state (lifecycle, failure).
    pub fn shared(&self) -> &SharedActorState<A> {
        &self.shared
    }

    /// One dispatch turn: ask the `driver` for the next message - it owns the
    /// mailbox-polling decision - and produce the [`StandardWork`] that acquires
    /// the lock and runs the handler, ready to drive. `None` means the driver
    /// stopped the loop.
    pub async fn next_dispatch<'ctx, 'turn, D, M>(
        &'ctx self,
        driver: &'turn mut D,
        mailbox: &'turn mut M,
    ) -> Option<StandardWork<'ctx>>
    where
        // The work borrows the context (`'ctx`); the driver/mailbox borrow
        // (`'turn`) is released when this future completes - so a concurrent loop
        // can hold the work in a set without pinning the driver.
        'ctx: 'turn,
        D: EventDriver<A, M> + Send,
        A::RunLoop: ActorRunLoop<A, DispatchContext = Self>,
        <A::RunLoop as ActorRunLoop<A>>::WorkConverter: FutureWorkConverter,
    {
        let cx = EventContext::new(&self.lock_strategy, &self.shared);

        // `Send`-readable by the `EventDriver::next` bound - no reclaim.
        let message = driver.next(cx, mailbox).await?;

        // SAFETY: the message was produced by our own driver while we are in
        //         the actor loop, so it can be dispatched onto our loop, and
        //         `self` is exactly this loop's `DispatchContext`.
        let erased = unsafe { message.dispatch_onto_loop::<A>(self) };

        // Unpacked to the converter's `Erased`; the standard loops drive it as
        // a `Send` future. The opaque cell is gone by here.
        Some(
            <<A::RunLoop as ActorRunLoop<A>>::WorkConverter as FutureWorkConverter>::into_future(
                erased,
            ),
        )
    }

    /// Run the actor's stop hook and consume the context.
    ///
    /// Called once the loop has quiesced (no more dispatches in flight), so the
    /// context is the actor's sole owner and can reclaim it by value from the
    /// lock. The stop reason is derived from whether an error was recorded.
    pub async fn run_stop_hook(self)
    where
        A: Sized + Send,
        <A::RunLoop as ActorRunLoop<A>>::WorkConverter: FutureWorkConverter,
    {
        let StandardDispatchContext {
            lock_strategy,
            shared,
            self_ref,
        } = self;

        // Observable transition; a no-op if the actor never reached `Running`.
        shared.transition_stopping();

        let reason = match shared.failed_error() {
            Some(error) => StopReason::Failed(error),
            None => {
                // Record the clean outcome before reclaiming the actor, so the
                // reason is observable as soon as the loop has quiesced.
                shared.mark_finished();
                StopReason::Finished
            }
        };

        // Sole owner now: reclaim the actor by value for the by-value stop hook,
        // erase its work through the loop's converter, and drive it.
        let actor = lock_strategy.into_inner();
        let cx = ActorContext::new(&shared, &self_ref);
        let stop = into_work::<<A::RunLoop as ActorRunLoop<A>>::WorkConverter, _>(
            actor.on_stop(reason, cx),
        );
        <<A::RunLoop as ActorRunLoop<A>>::WorkConverter as FutureWorkConverter>::into_future(stop)
            .await;

        // Push termination signals to watchers on the async path, awaiting
        // mailbox room so none are dropped. Drains the set, so the drop guard's
        // non-awaiting fallback is a no-op for this (clean / failed) path.
        shared.notify_subscribers().await;
    }
}

impl<A: Actor + ?Sized> ActorRunLoopDispatchContext<A> for StandardDispatchContext<A> {
    fn lock_strategy(&self) -> &A::LockStrategy {
        &self.lock_strategy
    }

    fn shared_state(&self) -> &SharedActorState<A> {
        &self.shared
    }

    fn self_ref(&self) -> &WeakActorHandle<A> {
        &self.self_ref
    }
}

impl<A: Actor + ?Sized> Debug for StandardDispatchContext<A>
where
    A::LockStrategy: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StandardDispatchContext")
            .field("lock_strategy", &self.lock_strategy)
            .finish_non_exhaustive()
    }
}

/// A run loop built from the standard bricks: it only has to say how to *build*
/// itself and how to *schedule* the handler futures
/// [`next_dispatch`](StandardDispatchContext::next_dispatch) yields.
///
/// Implementing this gets the whole spawn scaffolding for free via
/// [`standard_run_with`] (which the loop's
/// [`SpawnableRunLoop`](crate::spawn::SpawnableRunLoop) delegates to).
pub trait StandardLoop<A: Actor + ?Sized>: ActorRunLoop<A> + Sized {
    /// Assemble the loop from the actor's lock strategy, shared state and weak
    /// self-reference.
    fn build(
        lock_strategy: A::LockStrategy,
        shared: SharedActorState<A>,
        self_ref: WeakActorHandle<A>,
    ) -> Self;

    /// Drive the loop. This is the loop's whole identity - how it schedules the
    /// handler futures produced each turn by
    /// [`StandardDispatchContext::next_dispatch`].
    ///
    /// Implementations must run the stop hook via
    /// [`StandardDispatchContext::run_stop_hook`] once the loop has quiesced,
    /// whether it drained cleanly or a handler failed the actor.
    fn run<D, M>(self, mailbox: M, driver: D) -> impl Future<Output = ()> + Send
    where
        A: Sized + Send,
        D: EventDriver<A, M> + Send,
        M: Send + 'static,
        A::LockStrategy: Send + Sync,
        A::Error: Send + Sync,
        WeakActorHandle<A>: Send + Sync,
        <A::RunLoop as ActorRunLoop<A>>::WorkConverter: FutureWorkConverter;
}

/// The standard spawn scaffolding: drop-guard, init, driver selection, then hand
/// off to the loop's [`StandardLoop::run`]. A [`StandardLoop`]'s
/// [`SpawnableRunLoop`](crate::spawn::SpawnableRunLoop) `run_with` is a one-line
/// delegation to this.
pub async fn standard_run_with<A, L, I, M>(
    init: I,
    shared: SharedActorState<A>,
    mailbox: M,
    self_ref: WeakActorHandle<A>,
) where
    A: Actor<RunLoop = L> + Into<A::LockStrategy> + Send,
    L: StandardLoop<A>,
    I: ActorInit<A> + Send + 'static,
    I::Fut: Send,
    A::EventDriver: EventDriver<A, M> + Send,
    A::LockStrategy: Send + Sync,
    A::Error: Send + Sync + 'static,
    WeakActorHandle<A>: Send + Sync,
    M: Send + 'static,
    <A::RunLoop as ActorRunLoop<A>>::WorkConverter: FutureWorkConverter,
{
    // Loop exit, handler panic and task abort all transition to `Dead`.
    let _guard = shared.dead_on_drop();

    // The actor is held across the start-hook await (its work borrows it), so
    // the task future needs `A: Send` - already implied by `A::LockStrategy:
    // Send` for the real locks, made explicit above. After the hook it moves
    // into its lock before the loop's first await; the driver it yields is the
    // named `A::EventDriver`, `Send` by the bound above.
    let lock_strategy;
    let driver;
    {
        let Some(mut actor) = init_or_fail(init, &shared).await else {
            return;
        };

        // Drive the start hook before announcing `Running`: the actor is not
        // yet behind its lock (plain `&mut`), and a waiter on `spawn_ready`
        // only observes `Running` once startup has completed.
        {
            let cx = ActorContext::new(&shared, &self_ref);
            let start =
                into_work::<<A::RunLoop as ActorRunLoop<A>>::WorkConverter, _>(actor.on_start(cx));
            <<A::RunLoop as ActorRunLoop<A>>::WorkConverter as FutureWorkConverter>::into_future(
                start,
            )
            .await;
        }

        // A failing start hook aborts startup before the loop runs - and the
        // stop hook does not run, since the actor never reached `Running`.
        if shared.failed_error().is_some() {
            return;
        }
        shared.transition_running();

        // Build the driver from the actor (seedable via its `From<&Actor>`
        // impl), before the actor moves into its lock.
        driver = A::EventDriver::from(&actor);
        lock_strategy = actor.into();
    }

    L::build(lock_strategy, shared, self_ref)
        .run(mailbox, driver)
        .await;
}

/// Run the actor initializer, returning the constructed actor or recording the
/// error on failure.
///
/// Does *not* transition the lifecycle to `Running`: the caller drives the start
/// hook first and announces `Running` only once it succeeds (see
/// [`standard_run_with`]). Returns `None` if init failed (the error is already
/// recorded; the caller's dead-on-drop guard then marks the actor dead, so
/// observers of [`Dead`](crate::actor::state::LifecycleState::Dead) see it).
pub async fn init_or_fail<A, I>(init: I, shared: &SharedActorState<A>) -> Option<A>
where
    A: Actor,
    I: ActorInit<A>,
{
    match init.init().await {
        Ok(actor) => Some(actor),
        Err(err) => {
            shared.record_failure(err);
            None
        }
    }
}
