//! Initializer building blocks.

use crate::actor::{Actor, ActorInit};
use core::fmt::{Debug, Formatter};

/// Initializer built from a construction closure.
///
/// Covers async and fallible construction without hand-implementing
/// [`ActorInit`]: arguments are the closure's captures, so their sendability
/// is the closure's sendability. The future type is a generic parameter
/// inferred at the call site, where its auto traits are known - a
/// `Send`-captures-only `InitFn` satisfies the `I::Fut: Send` bound of
/// work-stealing assembly even though [`ActorInit`] itself never demands it.
///
/// Rarely constructed by hand: the spawn entry points accept bare closures
/// through [`IntoActorInit`](crate::spawn::IntoActorInit) and wrap them in
/// this type.
pub struct InitFn<F> {
    init: F,
}

impl<F> InitFn<F> {
    /// Create the initializer from a construction closure.
    pub const fn new(init: F) -> Self {
        Self { init }
    }
}

impl<A, F, Fut> ActorInit<A> for InitFn<F>
where
    A: Actor,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<A, A::Error>>,
{
    type Fut = Fut;

    fn init(self) -> Fut {
        (self.init)()
    }
}

impl<F> Debug for InitFn<F> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InitFn").finish()
    }
}
