use crate::actor::channel::ActorMailbox;
use crate::actor::state::SharedActorState;
use crate::actor::{Actor, ActorInit, ActorRunLoop, ActorRunLoopDispatchContext};
use core::fmt::{Debug, Formatter};
use futures::StreamExt;
use futures::future::Either;
use futures::stream::FuturesUnordered;

/// Standard run loop for actors.
pub struct StandardActorRunLoopImpl<A: Actor<RunLoop = Self> + ?Sized> {
    dispatch_context: StandardActorRunLoopDispatchContext<A>,
    shared: SharedActorState<A>,
}

impl<A: Actor<RunLoop = Self> + ?Sized> StandardActorRunLoopImpl<A> {
    pub fn new(shared: SharedActorState<A>, lock_strategy: A::LockStrategy) -> Self {
        Self {
            dispatch_context: StandardActorRunLoopDispatchContext { lock_strategy },
            shared,
        }
    }
}

impl<A: Actor<RunLoop = Self> + ?Sized> StandardActorRunLoopImpl<A> {
    pub async fn run(self, mut mailbox: impl ActorMailbox) {
        let mut work_set = FuturesUnordered::new();

        loop {
            let mailbox_recv = async {
                let msg = mailbox.receive().await?;

                // SAFETY: The message was sent to our mailbox, and we are in the actor
                //         loop, thus this message can be dispatched onto our loop.
                Some(unsafe { msg.dispatch_onto_loop::<A>(&self.dispatch_context) }.await)
            };

            futures::pin_mut!(mailbox_recv);

            let work = match futures::future::select(work_set.next(), mailbox_recv).await {
                Either::Left(_) => continue,
                Either::Right((Some(work), _)) => work,
                Either::Right((None, _)) => break,
            };

            work_set.push(work);
        }

        // Drive pending work to completion before completely shutting down the actor
        while let Some(_) = work_set.next().await {}
    }
}

impl<A: Actor<RunLoop = Self> + ?Sized> Debug for StandardActorRunLoopImpl<A>
where
    A: Debug,
    A::LockStrategy: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StandardActorRunLoop")
            .field("dispatch_context", &self.dispatch_context)
            .finish()
    }
}

impl<A: Actor<RunLoop = Self> + ?Sized> ActorRunLoop<A> for StandardActorRunLoopImpl<A> {
    type DispatchContext = StandardActorRunLoopDispatchContext<A>;
}

#[derive(Debug)]
pub struct StandardActorRunLoopDispatchContext<A: Actor + ?Sized> {
    lock_strategy: A::LockStrategy,
}

impl<A: Actor + ?Sized> ActorRunLoopDispatchContext<A> for StandardActorRunLoopDispatchContext<A> {
    fn lock_strategy(&self) -> &A::LockStrategy {
        &self.lock_strategy
    }
}

/// Standard implementation of an actor run loop.
pub trait StandardActorRunLoop<A: Actor<RunLoop = Self> + ?Sized>: ActorRunLoop<A> {
    /// Run the loop by constructing the actor with the given arguments.
    fn run_with<I>(
        init: I,
        shared: SharedActorState<A>,
        mailbox: impl ActorMailbox,
    ) -> impl Future<Output = ()>
    where
        I: ActorInit<A>,
        A: Sized;

    /// Run the loop by using the already constructed actor.
    fn run(
        actor: A,
        shared: SharedActorState<A>,
        mailbox: impl ActorMailbox,
    ) -> impl Future<Output = ()>;
}

impl<A: Actor<RunLoop = Self>> StandardActorRunLoop<A> for StandardActorRunLoopImpl<A>
where
    A: Into<A::LockStrategy>,
{
    async fn run_with<I>(init: I, shared: SharedActorState<A>, mailbox: impl ActorMailbox)
    where
        I: ActorInit<A>,
        A: Sized,
    {
        let actor = match init.init().await {
            Ok(actor) => actor,
            Err(err) => {
                let _ = shared.set_error(err);
                return;
            }
        };

        <Self as StandardActorRunLoop<A>>::run(actor, shared, mailbox).await
    }

    async fn run(actor: A, shared: SharedActorState<A>, mailbox: impl ActorMailbox) {
        let this = Self::new(shared, actor.into());
        this.run(mailbox).await
    }
}
