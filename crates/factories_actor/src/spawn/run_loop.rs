use crate::actor::event::EventDriver;
use crate::actor::handle::WeakActorHandle;
use crate::actor::state::SharedActorState;
use crate::actor::{Actor, ActorInit, ActorRunLoop};

/// A run loop that can be constructed and driven as part of generic actor assembly.
///
/// The init arrives as an [`ActorInit`] value: the *initializer* is what
/// crosses onto the actor task, [`ActorInit::init`] runs inside it, and the
/// actor is constructed where it will live. `I::Fut: Send` is *this*
/// contract's demand (the loop future is handed to a work-stealing spawner) -
/// a single-threaded assembly contract can omit it.
///
/// Implementations must:
/// - hold a [`SharedActorState::dead_on_drop`] guard for the entire future, and
/// - call [`SharedActorState::transition_running`] after successful init, or
///   [`SharedActorState::record_failure`] (before returning) on init failure,
///
/// so the lifecycle is reliable for `spawn_ready` and death observers.
pub trait SpawnableRunLoop<A>: ActorRunLoop<A>
where
    A: Actor<RunLoop = Self> + ?Sized,
    A::Error: Send + Sync + 'static,
{
    /// Configuration consumed when the loop starts.
    type Config: Send + 'static;

    /// Run the loop, constructing the actor with the given initializer.
    ///
    /// `self_ref` is the actor's own weak handle (its identity exists before the
    /// loop is spawned), threaded into the dispatch context to power
    /// [`ActorContext::actor_ref`](crate::actor::ActorContext::actor_ref).
    fn run_with<I, MB>(
        config: Self::Config,
        init: I,
        shared: SharedActorState<A>,
        mailbox: MB,
        self_ref: WeakActorHandle<A>,
    ) -> impl Future<Output = ()> + Send + 'static
    where
        I: ActorInit<A> + Send + 'static,
        I::Fut: Send,
        MB: Send + 'static,
        A::EventDriver: EventDriver<A, MB> + Send,
        WeakActorHandle<A>: Send + Sync,
        A: Sized;
}
