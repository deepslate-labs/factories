use crate::actor::event::{DemandSendDriver, EventDriver};
use crate::actor::state::SharedActorState;
use crate::actor::{Actor, ActorRunLoop, SerializedDispatch, ThreadSafe};
use crate::runtime::loop_support::{self, StandardDispatchContext, StandardLoop};
use crate::spawn::{ActorMailbox, SpawnableRunLoop};
use core::fmt::{Debug, Formatter};

/// Run loop that runs message handlers strictly sequentially.
///
/// Each dispatch - lock acquisition and handler - is driven to completion
/// before the next is pulled. This makes the loop a [`SerializedDispatch`]
/// provider, enabling lock-eliding strategies such as
/// [`UnguardedLock`](crate::runtime::lock::UnguardedLock).
///
/// The loop demands [`ThreadSafe`] handler futures: its task may migrate
/// between executor threads. Dispatches still never overlap.
pub struct SequentialRunLoop<A: Actor + ?Sized> {
    dispatch_context: StandardDispatchContext<A>,
}

impl<A: Actor<RunLoop = Self> + ?Sized> StandardLoop<A> for SequentialRunLoop<A> {
    fn build(lock_strategy: A::LockStrategy, shared: SharedActorState<A>) -> Self {
        Self {
            dispatch_context: StandardDispatchContext::new(lock_strategy, shared),
        }
    }

    /// Schedule by awaiting each handler to completion before pulling the next
    /// dispatch - serialized, so the actor state needs no real lock.
    async fn run<D, M>(self, mut mailbox: M, mut driver: DemandSendDriver<D>)
    where
        D: EventDriver<A>,
        M: ActorMailbox + Send,
        A::LockStrategy: Send + Sync,
        A::Error: Send + Sync,
    {
        loop {
            // A handler failed the actor: stop pulling.
            if self.dispatch_context.shared().get_error().is_some() {
                return;
            }

            let Some(handler) = self
                .dispatch_context
                .next_dispatch(&mut driver, &mut mailbox)
                .await
            else {
                return;
            };

            handler.await;
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
    type DispatchContext = StandardDispatchContext<A>;
    type Demand = ThreadSafe;
}

// SAFETY: `run` drives each dispatch - the acquire future and then the resolved
//         handler future - to completion before pulling the next, so no two
//         dispatches of the actor instance are ever in flight at once. Aborting
//         the task drops the in-flight dispatch and no further dispatch follows.
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
        loop_support::standard_run_with::<A, Self, I, _>(init, shared, mailbox)
    }
}
