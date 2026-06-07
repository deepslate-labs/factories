use crate::actor::dispatch::AssertSend;
use crate::actor::state::SharedActorState;
use crate::actor::{Actor, ActorRunLoop, ActorRunLoopDispatchContext, ThreadSafe};
use crate::spawn::{ActorMailbox, SpawnableRunLoop};
use core::fmt::{Debug, Formatter};
use futures::StreamExt;
use futures::future::Either;
use futures::stream::FuturesUnordered;

/// Run loop that runs message handlers concurrently in a work set.
///
/// Handlers whose locks are acquired run concurrently on the actor task while
/// the loop keeps pulling from the mailbox. Lock acquisition is serialized
/// through the mailbox reader so acquisition order matches mailbox order.
///
/// The loop demands [`ThreadSafe`] handler futures: its task may migrate
/// between executor threads.
pub struct ConcurrentRunLoop<A: Actor + ?Sized> {
    dispatch_context: ConcurrentRunLoopDispatchContext<A>,
}

impl<A: Actor<RunLoop = Self> + ?Sized> ConcurrentRunLoop<A> {
    pub fn new(lock_strategy: A::LockStrategy, shared: SharedActorState<A>) -> Self {
        Self {
            dispatch_context: ConcurrentRunLoopDispatchContext {
                lock_strategy,
                shared,
            },
        }
    }

    /// Drive the loop until the mailbox closes (then drain pending work) or a
    /// handler fails the actor (then drop pending work).
    pub async fn run(self, mut mailbox: impl ActorMailbox) {
        let mut work_set = FuturesUnordered::new();

        loop {
            // A handler failed the actor: drop the in-flight work set and
            // exit without draining - the actor state is compromised.
            if self.dispatch_context.shared.get_error().is_some() {
                return;
            }

            let mailbox_recv = async {
                let msg = mailbox.receive().await?;

                // SAFETY: The message was sent to our mailbox, and we are in the actor
                //         loop, thus this message can be dispatched onto our loop.
                let acquire = unsafe { msg.dispatch_onto_loop::<A>(&self.dispatch_context) };

                // SAFETY: Every dispatcher reaching this mailbox was checked against
                //         `A::RunLoop = Self`'s `ThreadSafe` demand: statically declared
                //         dispatchers at their declaration site, dynamically bound ones
                //         by the `ActorRuntimeBinder` contract, and
                //         `DispatchedActorMessage::new` is unsafe with the same
                //         obligation, so safe code cannot forge unchecked deliveries.
                Some(unsafe { AssertSend::new(acquire) }.await)
            };

            futures::pin_mut!(mailbox_recv);

            // An empty work set resolves `next()` with `None` immediately; selecting
            // on it would busy-spin. Only race the work set when it has work.
            let work = if work_set.is_empty() {
                match mailbox_recv.await {
                    Some(work) => work,
                    None => break,
                }
            } else {
                match futures::future::select(work_set.next(), mailbox_recv).await {
                    Either::Left(_) => continue,
                    Either::Right((Some(work), _)) => work,
                    Either::Right((None, _)) => break,
                }
            };

            // SAFETY: Same anchor as above - the handler future was demand-checked
            //         `ThreadSafe`.
            work_set.push(unsafe { AssertSend::new(work) });
        }

        // Drive pending work to completion before completely shutting down the actor
        while work_set.next().await.is_some() {
            // Failures also cut the drain short.
            if self.dispatch_context.shared.get_error().is_some() {
                return;
            }
        }
    }
}

impl<A: Actor<RunLoop = Self> + ?Sized> Debug for ConcurrentRunLoop<A>
where
    A::LockStrategy: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ConcurrentRunLoop")
            .field("dispatch_context", &self.dispatch_context)
            .finish()
    }
}

impl<A: Actor<RunLoop = Self> + ?Sized> ActorRunLoop<A> for ConcurrentRunLoop<A> {
    type DispatchContext = ConcurrentRunLoopDispatchContext<A>;
    type Demand = ThreadSafe;
}

impl<A> SpawnableRunLoop<A> for ConcurrentRunLoop<A>
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

            // The initializer crossed onto this task; the actor is constructed
            // where it will live.
            let actor = match init.init().await {
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

pub struct ConcurrentRunLoopDispatchContext<A: Actor + ?Sized> {
    lock_strategy: A::LockStrategy,
    shared: SharedActorState<A>,
}

impl<A: Actor + ?Sized> Debug for ConcurrentRunLoopDispatchContext<A>
where
    A::LockStrategy: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ConcurrentRunLoopDispatchContext")
            .field("lock_strategy", &self.lock_strategy)
            .finish()
    }
}

impl<A: Actor + ?Sized> ActorRunLoopDispatchContext<A> for ConcurrentRunLoopDispatchContext<A> {
    fn lock_strategy(&self) -> &A::LockStrategy {
        &self.lock_strategy
    }

    fn shared_state(&self) -> &SharedActorState<A> {
        &self.shared
    }
}
