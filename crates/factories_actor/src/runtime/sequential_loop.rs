use crate::actor::dispatch::AssertSend;
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

    /// Drive the loop until the mailbox closes or a handler fails the actor.
    pub async fn run(self, mut mailbox: impl ActorMailbox) {
        loop {
            // A handler failed the actor: stop pulling messages.
            if self.dispatch_context.shared.get_error().is_some() {
                return;
            }

            let Some(message) = mailbox.receive().await else {
                return;
            };

            // SAFETY: The message was sent to our mailbox, and we are in the actor
            //         loop, thus this message can be dispatched onto our loop.
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

    fn run_with<F>(
        _config: (),
        init: F,
        shared: SharedActorState<A>,
        mailbox: impl ActorMailbox + Send + 'static,
    ) -> impl Future<Output = ()> + Send + 'static
    where
        F: Future<Output = Result<A, A::Error>> + Send + 'static,
    {
        async move {
            // Loop exit, handler panic and task abort all transition to `Dead`.
            let _guard = shared.dead_on_drop();

            let actor = match init.await {
                Ok(actor) => actor,
                Err(err) => {
                    // Error first, then the guard's drop transitions to dead -
                    // observers of `Dead` reliably see the error.
                    let _ = shared.set_error(err);
                    return;
                }
            };

            shared.transition_running();

            let this = Self::new(actor.into(), shared);
            this.run(mailbox).await;
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
