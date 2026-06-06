//! Reusable actor component bundles.
//!
//! Many actors in a system tend to share the same component selections (the
//! same channel, lock strategy, run loop, ...). An [`ActorTemplate`] names
//! such a bundle once so `#[derive(Actor)]` can pull from it via
//! `#[actor(template = MyTemplate)]`; explicitly configured keys still
//! override individual members.

use crate::actor::Actor;

/// A reusable bundle of actor component selections.
///
/// This is a pure type-level lookup table: members carry no usage bounds, the
/// trait only *names* types. Whether a member actually satisfies the
/// corresponding [`Actor`] associated-type bound is checked where the actor
/// implementation is generated, so errors point at the actor (with the
/// actor's context), not at the template.
///
/// The per-actor members are generic over the actor so they can name
/// actor-parameterized components:
///
/// ```ignore
/// struct SequentialSet;
///
/// impl ActorTemplate for SequentialSet {
///     type Channel = SimpleKanalActorChannel;
///     type Error = core::convert::Infallible;
///     type RuntimeBinder<A: Actor> = RegistryBinder<A>;
///     type LockStrategy<A: Actor> = UnguardedLock<A>;
///     type RunLoop<A: Actor> = SequentialRunLoop<A>;
/// }
/// ```
pub trait ActorTemplate {
    /// The [`Actor::Channel`] to use.
    type Channel;

    /// The [`Actor::Error`] to use.
    type Error;

    /// The [`Actor::RuntimeBinder`] to use.
    type RuntimeBinder<A: Actor>;

    /// The [`Actor::LockStrategy`] to use.
    type LockStrategy<A: Actor>;

    /// The [`Actor::RunLoop`] to use.
    type RunLoop<A: Actor>;
}
