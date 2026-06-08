use crate::actor::dispatch::AssertSend;
use crate::actor::event::{DemandSendDriver, EventContext, EventDriver};
use crate::actor::state::SharedActorState;
use crate::actor::{
    Actor, ActorRunLoop, ActorRunLoopDispatchContext, SerializedDispatch, ThreadSafe,
};
use crate::spawn::{ActorMailbox, SpawnableRunLoop};
use core::fmt::{Debug, Formatter};

/// Run loop that runs message handlers strictly sequentially.
///
/// Each dispatch - lock acquisition and handler - is driven to completion
/// before the next message is pulled from the mailbox. This makes the loop a
/// [`SerializedDispatch`] provider, enabling lock-eliding strategies such as
/// [`UnguardedLock`](crate::runtime::lock::UnguardedLock).
///
/// The loop demands [`ThreadSafe`] handler futures: its task may migrate
/// between executor threads. Dispatches still never overlap.
pub struct SequentialRunLoop<A: Actor + ?Sized> {
    dispatch_context: SequentialRunLoopDispatchContext<A>,
}

impl<A: Actor<RunLoop = Self> + ?Sized> SequentialRunLoop<A> {
    pub fn new(lock_strategy: A::LockStrategy, shared: SharedActorState<A>) -> Self {
        Self {
            dispatch_context: SequentialRunLoopDispatchContext {
                lock_strategy,
                shared,
            },
        }
    }

    /// Drive the loop until the driver stops it or a handler fails the actor.
    ///
    /// Each turn the driver produces the next message (deciding for itself how to
    /// poll the mailbox); the loop dispatches it. [`DefaultDriver`] makes this a
    /// plain mailbox pull.
    pub async fn run<D>(self, mut mailbox: impl ActorMailbox, mut driver: DemandSendDriver<D>)
    where
        D: EventDriver<A>,
    {
        loop {
            // A handler failed the actor: stop pulling messages.
            if self.dispatch_context.shared.get_error().is_some() {
                return;
            }

            let cx = EventContext::new(
                self.dispatch_context.lock_strategy(),
                self.dispatch_context.shared_state(),
            );

            // SAFETY: the driver's `next` future satisfies the loop's
            //         `ThreadSafe` demand - the same anchor as the handler
            //         futures below: the `EventDriver` impl demand-checks it
            //         (the `#[event_source]` derive does so; hand-written drivers
            //         uphold it), and the driver value is reclaimed by
            //         `DemandSendDriver`.
            let next = unsafe { AssertSend::new(driver.0.next(cx, &mut mailbox)) };
            let Some(message) = next.await else {
                return;
            };

            // SAFETY: The message was sent to our mailbox (or produced by our own
            //         event driver), and we are in the actor loop, thus this
            //         message can be dispatched onto our loop.
            let acquire = unsafe { message.dispatch_onto_loop::<A>(&self.dispatch_context) };

            // SAFETY: Every dispatcher reaching this mailbox was checked against
            //         `A::RunLoop = Self`'s `ThreadSafe` demand: statically declared
            //         dispatchers at their declaration site, dynamically bound ones
            //         by the `ActorRuntimeBinder` contract, and
            //         `DispatchedActorMessage::new` is unsafe with the same
            //         obligation, so safe code cannot forge unchecked deliveries.
            let work = unsafe { AssertSend::new(acquire) }.await;

            // SAFETY: Same anchor as above.
            unsafe { AssertSend::new(work) }.await;
        }
    }
}

impl<A: Actor<RunLoop = Self> + ?Sized> Debug for SequentialRunLoop<A>
where
    A::LockStrategy: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SequentialRunLoop")
            .field("dispatch_context", &self.dispatch_context)
            .finish()
    }
}

impl<A: Actor<RunLoop = Self> + ?Sized> ActorRunLoop<A> for SequentialRunLoop<A> {
    type DispatchContext = SequentialRunLoopDispatchContext<A>;
    type Demand = ThreadSafe;
}

// SAFETY: `run` drives each dispatch - the acquire future and then the resolved
//         handler future - to completion before pulling the next message from
//         the mailbox, so no two dispatches of the actor instance are ever in
//         flight at once. Aborting the task drops the in-flight dispatch and no
//         further dispatch follows.
unsafe impl<A: Actor<RunLoop = Self> + ?Sized> SerializedDispatch<A> for SequentialRunLoop<A> {}

impl<A> SpawnableRunLoop<A> for SequentialRunLoop<A>
where
    A: Actor<RunLoop = Self> + Into<A::LockStrategy>,
    A::LockStrategy: Send + Sync,
    A::Error: Send + Sync + 'static,
{
    type Config = ();

    fn run_with<I>(
        _config: (),
        init: I,
        shared: SharedActorState<A>,
        mailbox: impl ActorMailbox + Send + 'static,
    ) -> impl Future<Output = ()> + Send + 'static
    where
        I: crate::actor::ActorInit<A> + Send + 'static,
        I::Fut: Send,
    {
        async move {
            // Loop exit, handler panic and task abort all transition to `Dead`.
            let _guard = shared.dead_on_drop();

            // Scope the (possibly `!Send`) actor so it is provably gone - moved
            // into its lock - before the loop's first await. The driver it yields
            // does not borrow it (`use<Self>`), and its `Send` is reclaimed by
            // `DemandSendDriver` for transport on the loop task.
            let lock_strategy;
            let driver;
            {
                // The initializer crossed onto this task; the actor is
                // constructed where it will live.
                let actor = match init.init().await {
                    Ok(actor) => actor,
                    Err(err) => {
                        // Error first, then the guard's drop transitions to dead
                        // - observers of `Dead` reliably see the error.
                        let _ = shared.set_error(err);
                        return;
                    }
                };

                shared.transition_running();

                // SAFETY: this is a `ThreadSafe` loop; the driver upholds that
                //         demand (the `#[event_source]` derive demand-checks the
                //         driver and its `next` future; a hand-written driver
                //         upholds it the same way).
                driver = unsafe { DemandSendDriver::new(actor.select_event_driver()) };
                lock_strategy = actor.into();
            }

            let this = Self::new(lock_strategy, shared);
            this.run(mailbox, driver).await;
        }
    }
}

pub struct SequentialRunLoopDispatchContext<A: Actor + ?Sized> {
    lock_strategy: A::LockStrategy,
    shared: SharedActorState<A>,
}

impl<A: Actor + ?Sized> Debug for SequentialRunLoopDispatchContext<A>
where
    A::LockStrategy: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SequentialRunLoopDispatchContext")
            .field("lock_strategy", &self.lock_strategy)
            .finish()
    }
}

impl<A: Actor + ?Sized> ActorRunLoopDispatchContext<A> for SequentialRunLoopDispatchContext<A> {
    fn lock_strategy(&self) -> &A::LockStrategy {
        &self.lock_strategy
    }

    fn shared_state(&self) -> &SharedActorState<A> {
        &self.shared
    }
}
