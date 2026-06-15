use crate::actor::event::EventDriver;
use crate::actor::handle::WeakActorHandle;
use crate::actor::state::SharedActorState;
use crate::actor::work::SendFutureConverter;
use crate::actor::{Actor, ActorRunLoop, SerializedDispatch};
use crate::runtime::loop_support::{self, StandardDispatchContext, StandardLoop};
use crate::spawn::SpawnableRunLoop;
use core::fmt::{Debug, Formatter};

/// Run loop that runs message handlers strictly sequentially.
///
/// Each dispatch - lock acquisition and handler - is driven to completion
/// before the next is pulled. This makes the loop a [`SerializedDispatch`]
/// provider, enabling lock-eliding strategies such as
/// [`UnguardedLock`](crate::runtime::lock::UnguardedLock).
///
/// The loop ships the [`SendFutureConverter`]: handler work is `Send` (its task
/// may migrate between executor threads). Dispatches still never overlap.
pub struct SequentialRunLoop<A: Actor + ?Sized> {
    dispatch_context: StandardDispatchContext<A>,
}

impl<A: Actor<RunLoop = Self> + ?Sized> StandardLoop<A> for SequentialRunLoop<A> {
    fn build(
        lock_strategy: A::LockStrategy,
        shared: SharedActorState<A>,
        self_ref: WeakActorHandle<A>,
    ) -> Self {
        Self {
            dispatch_context: StandardDispatchContext::new(lock_strategy, shared, self_ref),
        }
    }

    /// Schedule by awaiting each handler to completion before pulling the next
    /// dispatch - serialized, so the actor state needs no real lock.
    async fn run<D, M>(self, mut mailbox: M, mut driver: D)
    where
        A: Sized + Send,
        D: EventDriver<A, M> + Send,
        M: Send + 'static,
        A::LockStrategy: Send + Sync,
        A::Error: Send + Sync,
        WeakActorHandle<A>: Send + Sync,
    {
        loop {
            // A handler failed the actor: stop pulling (the stop hook still runs).
            if self.dispatch_context.shared().failed_error().is_some() {
                break;
            }

            let Some(handler) = self
                .dispatch_context
                .next_dispatch(&mut driver, &mut mailbox)
                .await
            else {
                break;
            };

            handler.await;
        }

        self.dispatch_context.run_stop_hook().await;
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
    type WorkConverter = SendFutureConverter;
}

// SAFETY: `run` drives each dispatch - the acquire future and then the resolved
//         handler future - to completion before pulling the next, so no two
//         dispatches of the actor instance are ever in flight at once. Aborting
//         the task drops the in-flight dispatch and no further dispatch follows.
unsafe impl<A: Actor<RunLoop = Self> + ?Sized> SerializedDispatch<A> for SequentialRunLoop<A> {}

impl<A> SpawnableRunLoop<A> for SequentialRunLoop<A>
where
    A: Actor<RunLoop = Self> + Into<A::LockStrategy> + Send,
    A::LockStrategy: Send + Sync,
    A::Error: Send + Sync + 'static,
{
    type Config = ();

    fn run_with<I, MB>(
        _config: (),
        init: I,
        shared: SharedActorState<A>,
        mailbox: MB,
        self_ref: WeakActorHandle<A>,
    ) -> impl Future<Output = ()> + Send + 'static
    where
        I: crate::actor::ActorInit<A> + Send + 'static,
        I::Fut: Send,
        MB: Send + 'static,
        A::EventDriver: EventDriver<A, MB> + Send,
        WeakActorHandle<A>: Send + Sync,
    {
        loop_support::standard_run_with::<A, Self, I, MB>(init, shared, mailbox, self_ref)
    }
}
