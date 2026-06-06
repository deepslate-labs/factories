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
pub struct ConcurrentRunLoop<A: Actor<RunLoop = Self> + ?Sized> {
    dispatch_context: ConcurrentRunLoopDispatchContext<A>,
}

impl<A: Actor<RunLoop = Self> + ?Sized> ConcurrentRunLoop<A> {
    pub fn new(lock_strategy: A::LockStrategy) -> Self {
        Self {
            dispatch_context: ConcurrentRunLoopDispatchContext { lock_strategy },
        }
    }

    /// Drive the loop until the mailbox closes, then drain pending work.
    pub async fn run(self, mut mailbox: impl ActorMailbox) {
        let mut work_set = FuturesUnordered::new();

        loop {
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
        while let Some(_) = work_set.next().await {}
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

            let this = Self::new(actor.into());
            this.run(mailbox).await;
        }
    }
}

pub struct ConcurrentRunLoopDispatchContext<A: Actor + ?Sized> {
    lock_strategy: A::LockStrategy,
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
}
