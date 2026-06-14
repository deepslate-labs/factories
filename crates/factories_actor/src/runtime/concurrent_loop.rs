use crate::actor::event::EventDriver;
use crate::actor::state::SharedActorState;
use crate::actor::work::SendFutureConverter;
use crate::actor::{Actor, ActorRunLoop};
use crate::runtime::loop_support::{self, StandardDispatchContext, StandardLoop};
use crate::spawn::SpawnableRunLoop;
use core::fmt::{Debug, Formatter};
use futures::StreamExt;
use futures::future::Either;
use futures::stream::FuturesUnordered;

/// Run loop that runs message handlers concurrently in a work set.
///
/// Handlers whose locks are acquired run concurrently on the actor task while
/// the loop keeps pulling. Dispatch (lock acquisition) is serialized through the
/// driver so acquisition order matches the order the driver yields messages.
///
/// The loop ships the [`SendFutureConverter`]: handler work is `Send`, since its
/// task may migrate between executor threads.
pub struct ConcurrentRunLoop<A: Actor + ?Sized> {
    dispatch_context: StandardDispatchContext<A>,
}

impl<A: Actor<RunLoop = Self> + ?Sized> StandardLoop<A> for ConcurrentRunLoop<A> {
    fn build(lock_strategy: A::LockStrategy, shared: SharedActorState<A>) -> Self {
        Self {
            dispatch_context: StandardDispatchContext::new(lock_strategy, shared),
        }
    }

    /// Schedule by pushing resolved handlers into a work set that runs
    /// concurrently while the next dispatch is pulled; drain on a clean stop,
    /// drop the set on failure.
    async fn run<D, M>(self, mut mailbox: M, mut driver: D)
    where
        A: Sized + Send,
        D: EventDriver<A, M> + Send,
        M: Send + 'static,
        A::LockStrategy: Send + Sync,
        A::Error: Send + Sync,
    {
        let mut work_set = FuturesUnordered::new();

        loop {
            // A handler failed the actor: stop pulling and skip the drain - the
            // actor state is compromised. The stop hook still runs.
            if self.dispatch_context.shared().get_error().is_some() {
                break;
            }

            let pull = self
                .dispatch_context
                .next_dispatch(&mut driver, &mut mailbox);
            futures::pin_mut!(pull);

            // An empty work set resolves `next()` with `None` immediately;
            // selecting on it would busy-spin. Only race it when it has work.
            let handler = if work_set.is_empty() {
                match pull.await {
                    Some(handler) => handler,
                    None => break,
                }
            } else {
                match futures::future::select(work_set.next(), pull).await {
                    // A handler finished: take another lap to pull again.
                    Either::Left(_) => continue,
                    Either::Right((Some(handler), _)) => handler,
                    Either::Right((None, _)) => break,
                }
            };

            work_set.push(handler);
        }

        // Drive pending work to completion before shutting down, unless a failure
        // already compromised the state (then the in-flight work is dropped).
        if self.dispatch_context.shared().get_error().is_none() {
            while work_set.next().await.is_some() {
                if self.dispatch_context.shared().get_error().is_some() {
                    break;
                }
            }
        }

        // Drop the work set before reclaiming the actor: its futures borrow the
        // dispatch context (and hold guards), so they must be gone before
        // `run_stop_hook` takes the lock by value.
        drop(work_set);
        self.dispatch_context.run_stop_hook().await;
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
    type DispatchContext = StandardDispatchContext<A>;
    type WorkConverter = SendFutureConverter;
}

impl<A> SpawnableRunLoop<A> for ConcurrentRunLoop<A>
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
    ) -> impl Future<Output = ()> + Send + 'static
    where
        I: crate::actor::ActorInit<A> + Send + 'static,
        I::Fut: Send,
        MB: Send + 'static,
        A::EventDriver: EventDriver<A, MB> + Send,
    {
        loop_support::standard_run_with::<A, Self, I, MB>(init, shared, mailbox)
    }
}
