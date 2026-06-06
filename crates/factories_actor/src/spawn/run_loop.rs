use crate::actor::state::SharedActorState;
use crate::actor::{Actor, ActorRunLoop};
use crate::spawn::ActorMailbox;

/// A run loop that can be constructed and driven as part of generic actor assembly.
///
/// Symmetric with [`crate::spawn::CreatableChannel`]: configuration in, running
/// part out. The returned future is the whole life of the actor - init, message
/// processing, death - and is what gets handed to an
/// [`crate::spawn::ActorTaskSpawner`].
///
/// The init arrives as a *future value* rather than an
/// [`crate::actor::ActorInit`] bound: callers write
/// `MyInit::prepare(args).init()` (or a bare `async` block), and auto-trait
/// leakage at the concrete call site provides the `Send` proof. Creating the
/// future runs no code - init still executes inside the spawned task.
///
/// Implementations must:
/// - hold a [`SharedActorState::dead_on_drop`] guard for the entire future, and
/// - call [`SharedActorState::transition_running`] after successful init, or
///   [`SharedActorState::set_error`] (before returning) on init failure,
///
/// so the lifecycle is reliable for `spawn_ready` and death observers.
pub trait SpawnableRunLoop<A>: ActorRunLoop<A>
where
    A: Actor<RunLoop = Self> + ?Sized,
    A::Error: Send + Sync + 'static,
{
    /// Configuration consumed when the loop starts.
    type Config: Send + 'static;

    /// Run the loop by constructing the actor with the given init future.
    fn run_with<F>(
        config: Self::Config,
        init: F,
        shared: SharedActorState<A>,
        mailbox: impl ActorMailbox + Send + 'static,
    ) -> impl Future<Output = ()> + Send + 'static
    where
        F: Future<Output = Result<A, A::Error>> + Send + 'static,
        A: Sized;
}
