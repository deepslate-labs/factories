//! Shared building blocks for the standard run loops.
//!
//! A run loop is mostly the same boilerplate: an identical dispatch context, an
//! identical "ask the driver for the next handler future" turn, and identical
//! spawn scaffolding. The only thing a loop really owns is *how it schedules*
//! the handler futures - serialized, concurrent, bounded, prioritised. These
//! bricks factor out the rest, so the built-in loops (and any user loop) shrink
//! to their scheduling structure.

use crate::actor::dispatch::{AssertSend, BoxedHandlerFuture};
use crate::actor::event::{DemandSendDriver, EventContext, EventDriver};
use crate::actor::state::SharedActorState;
use crate::actor::{Actor, ActorInit, ActorRunLoop, ActorRunLoopDispatchContext};
use core::fmt::{Debug, Formatter};
use core::future::Future;

/// The dispatch context shared by the standard run loops: the actor's lock
/// strategy plus its shared state.
///
/// A run loop owns one of these and exposes it as its
/// [`ActorRunLoop::DispatchContext`]. Dispatching a message goes through
/// [`next_dispatch`](Self::next_dispatch).
pub struct StandardDispatchContext<A: Actor + ?Sized> {
    lock_strategy: A::LockStrategy,
    shared: SharedActorState<A>,
}

impl<A: Actor + ?Sized> StandardDispatchContext<A> {
    /// Build the context from the actor's lock strategy and shared state.
    pub fn new(lock_strategy: A::LockStrategy, shared: SharedActorState<A>) -> Self {
        Self {
            lock_strategy,
            shared,
        }
    }

    /// The actor's shared state (lifecycle, failure).
    pub fn shared(&self) -> &SharedActorState<A> {
        &self.shared
    }

    /// One dispatch turn: ask the `driver` for the next message - it owns the
    /// mailbox-polling decision - and resolve its handler future, ready to run.
    /// `None` means the driver stopped the loop.
    ///
    /// This is the brick a run loop dispatches through: it bundles the
    /// event-driver turn, the lock acquisition, and the `Send` reclaim, so a loop
    /// only decides *how to schedule* the returned handler future.
    pub fn next_dispatch<'ctx, 'turn, D, M>(
        &'ctx self,
        driver: &'turn mut DemandSendDriver<D>,
        mailbox: &'turn mut M,
    ) -> impl Future<Output = Option<AssertSend<BoxedHandlerFuture<'ctx>>>> + use<'ctx, 'turn, A, D, M>
    where
        // The handler borrows the context (`'ctx`); the driver/mailbox borrow
        // (`'turn`) is released when this future completes - so a concurrent loop
        // can hold the resolved handler in a work set without pinning the driver.
        'ctx: 'turn,
        D: EventDriver<A, M>,
        A::RunLoop: ActorRunLoop<A, DispatchContext = Self>,
    {
        async move {
            let cx = EventContext::new(&self.lock_strategy, &self.shared);

            // SAFETY: demand obligation - the driver's `next` future satisfies
            //         the loop's demand (the `EventDriver` impl / `#[event_source]`
            //         derive demand-checks it; the driver value is reclaimed by
            //         `DemandSendDriver`).
            let message = unsafe { AssertSend::new(driver.0.next(cx, mailbox)) }.await?;

            // SAFETY: the message was produced by our own driver while we are in
            //         the actor loop, so it can be dispatched onto our loop, and
            //         `self` is exactly this loop's `DispatchContext`.
            let acquire = unsafe { message.dispatch_onto_loop::<A>(self) };

            // SAFETY: demand obligation - the dispatcher was demand-checked
            //         against the loop's `ThreadSafe` demand at its declaration
            //         site, so its acquire/handler futures are `Send`.
            let handler = unsafe { AssertSend::new(acquire) }.await;
            Some(unsafe { AssertSend::new(handler) })
        }
    }
}

impl<A: Actor + ?Sized> ActorRunLoopDispatchContext<A> for StandardDispatchContext<A> {
    fn lock_strategy(&self) -> &A::LockStrategy {
        &self.lock_strategy
    }

    fn shared_state(&self) -> &SharedActorState<A> {
        &self.shared
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
    /// Assemble the loop from the actor's lock strategy and shared state.
    fn build(lock_strategy: A::LockStrategy, shared: SharedActorState<A>) -> Self;

    /// Drive the loop. This is the loop's whole identity - how it schedules the
    /// handler futures produced each turn by
    /// [`StandardDispatchContext::next_dispatch`].
    fn run<D, M>(self, mailbox: M, driver: DemandSendDriver<D>) -> impl Future<Output = ()> + Send
    where
        D: EventDriver<A, M>,
        M: Send + 'static,
        A::LockStrategy: Send + Sync,
        A::Error: Send + Sync;
}

/// The standard spawn scaffolding: drop-guard, init, driver selection, then hand
/// off to the loop's [`StandardLoop::run`]. A [`StandardLoop`]'s
/// [`SpawnableRunLoop`](crate::spawn::SpawnableRunLoop) `run_with` is a one-line
/// delegation to this.
pub fn standard_run_with<A, L, I, M>(
    init: I,
    shared: SharedActorState<A>,
    mailbox: M,
) -> impl Future<Output = ()> + Send + 'static
where
    A: Actor<RunLoop = L> + Into<A::LockStrategy>,
    L: StandardLoop<A>,
    I: ActorInit<A> + Send + 'static,
    I::Fut: Send,
    A::EventDriver: EventDriver<A, M>,
    A::LockStrategy: Send + Sync,
    A::Error: Send + Sync + 'static,
    M: Send + 'static
{
    async move {
        // Loop exit, handler panic and task abort all transition to `Dead`.
        let _guard = shared.dead_on_drop();

        // Scope the (possibly `!Send`) actor so it is provably gone - moved into
        // its lock - before the loop's first await. The driver it yields is the
        // named `A::EventDriver` (does not borrow it); its `Send` is reclaimed by
        // `DemandSendDriver`.
        let lock_strategy;
        let driver;
        {
            let Some(actor) = init_or_fail(init, &shared).await else {
                return;
            };

            // Build the driver from the actor (seedable via its `From<&Actor>`
            // impl), before the actor moves into its lock.
            // SAFETY: standard loops are `ThreadSafe`; the driver upholds that
            //         demand (the `#[event_source]` derive demand-checks the
            //         driver and its `next` future; a hand-written driver upholds
            //         it the same way).
            driver = unsafe { DemandSendDriver::new(A::EventDriver::from(&actor)) };
            lock_strategy = actor.into();
        }

        L::build(lock_strategy, shared).run(mailbox, driver).await;
    }
}

/// Run the actor initializer, transitioning to `Running` on success or recording
/// the error on failure.
///
/// Returns the constructed actor, or `None` if init failed (the error is already
/// recorded; the caller's dead-on-drop guard then marks the actor dead, so
/// observers of [`Dead`](crate::actor::state::LifecycleState::Dead) see it).
pub async fn init_or_fail<A, I>(init: I, shared: &SharedActorState<A>) -> Option<A>
where
    A: Actor,
    I: ActorInit<A>,
{
    match init.init().await {
        Ok(actor) => {
            shared.transition_running();
            Some(actor)
        }
        Err(err) => {
            let _ = shared.set_error(err);
            None
        }
    }
}
