//! Actor lifecycle: the [`StopReason`] handed to
//! [`Actor::on_stop`](crate::actor::Actor::on_stop).
//!
//! The hooks themselves - [`on_start`](crate::actor::Actor::on_start) and
//! [`on_stop`](crate::actor::Actor::on_stop) - are provided methods on
//! [`Actor`](crate::actor::Actor): implement them directly on a hand-written
//! actor, or write `#[on_start]` / `#[on_stop]` methods in a `#[messages]` block
//! and let the derive route them. Both return a handler-style
//! [`IntoRunLoopWork`](crate::actor::work::IntoRunLoopWork), so the loop's
//! [`WorkConverter`](crate::actor::ActorRunLoop::WorkConverter) governs their
//! `Send`-ness exactly as it does message-handler work; the default is a no-op
//! ([`NoWork`](crate::actor::work::NoWork)).

use crate::actor::work::WorkConverter;
use crate::actor::{Actor, ActorContext, ActorRunLoop};
use core::fmt::{Debug, Formatter};

/// Why an actor's run loop is stopping, handed to
/// [`on_stop`](crate::actor::Actor::on_stop).
pub enum StopReason<'a, A: Actor + ?Sized> {
    /// The event source signalled completion (e.g. the mailbox closed) and the
    /// loop drained without failing.
    Finished,

    /// A handler - or the start hook - failed the actor. Carries the error that
    /// was recorded with [`ActorContext::fail`](crate::actor::ActorContext::fail).
    Failed(&'a A::Error),
}

/// The recorded outcome of an actor, observable through
/// [`SharedActorState::termination_reason`](crate::actor::state::SharedActorState::termination_reason).
pub enum TerminationReason<A: Actor + ?Sized> {
    /// The run loop drained and exited cleanly.
    Finished,

    /// A handler - or the start hook / init - failed the actor. Carries the
    /// error recorded with [`ActorContext::fail`](crate::actor::ActorContext::fail).
    Failed(A::Error),

    /// The actor reached [`Dead`](crate::actor::state::LifecycleState::Dead) with
    /// no outcome recorded: a panic unwound the loop, or the task was aborted.
    Aborted,
}

impl<A: Actor + ?Sized> TerminationReason<A> {
    /// The error-free discriminant of this reason.
    ///
    /// This is what watchers observe: a heterogeneous supervisor cannot name a
    /// child's concrete `A::Error`, so the pushed [`Terminated`](crate::actor::supervision::Terminated)
    /// signal carries the [`TerminationKind`] only.
    pub fn kind(&self) -> TerminationKind {
        match self {
            Self::Finished => TerminationKind::Finished,
            Self::Failed(_) => TerminationKind::Failed,
            Self::Aborted => TerminationKind::Aborted,
        }
    }
}

impl<A: Actor + ?Sized> Debug for TerminationReason<A>
where
    A::Error: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Finished => f.write_str("Finished"),
            Self::Failed(error) => f.debug_tuple("Failed").field(error).finish(),
            Self::Aborted => f.write_str("Aborted"),
        }
    }
}

/// The error-free outcome of an actor, as observed by a watcher.
///
/// The non-generic projection of [`TerminationReason`] (it drops the typed
/// `A::Error`), carried by the [`Terminated`](crate::actor::supervision::Terminated)
/// signal pushed to watchers.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum TerminationKind {
    /// The run loop drained and exited cleanly.
    Finished,

    /// A handler, the start hook, or init failed the actor.
    Failed,

    /// The actor reached [`Dead`](crate::actor::state::LifecycleState::Dead)
    /// with no recorded outcome: a panic, or a task abort.
    Aborted,
}

// `&A::Error` is `Copy` regardless of `A::Error`, so the reason is freely
// copyable; the manual impls avoid the spurious `A: Copy`/`Clone` bounds a
// derive would add.
impl<A: Actor + ?Sized> Copy for StopReason<'_, A> {}

impl<A: Actor + ?Sized> Clone for StopReason<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A: Actor + ?Sized> Debug for StopReason<'_, A>
where
    A::Error: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Finished => f.write_str("Finished"),
            Self::Failed(error) => f.debug_tuple("Failed").field(error).finish(),
        }
    }
}

/// The erased work a lifecycle hook produces: the actor's run loop's converter's
/// [`Erased`](WorkConverter::Erased).
#[doc(hidden)]
pub type ErasedHookWork<'a, A> =
    <<<A as Actor>::RunLoop as ActorRunLoop<A>>::WorkConverter as WorkConverter>::Erased<'a>;

/// A start hook, erased and reduced to a plain function pointer so the derive can
/// carry it out of a `match_specialize!` arm (where the `OnStartHook` bound is
/// discharged) and apply it where `cx` is concrete.
#[doc(hidden)]
pub type ErasedStartHook<A> =
    for<'a> fn(&'a mut A, ActorContext<'a, A>) -> ErasedHookWork<'a, A>;

/// A stop hook, erased to a function pointer (see [`ErasedStartHook`]).
#[doc(hidden)]
pub type ErasedStopHook<A> =
    for<'a> fn(A, StopReason<'a, A>, ActorContext<'a, A>) -> ErasedHookWork<'a, A>;

/// Implemented by `#[messages]` for an actor with an `#[on_start]` method; routed
/// to by the derive's [`Actor::on_start`](crate::actor::Actor::on_start).
#[doc(hidden)]
pub trait OnStartHook: Actor {
    fn __erased_on_start<'a>(
        &'a mut self,
        cx: ActorContext<'a, Self>,
    ) -> ErasedHookWork<'a, Self>;
}

/// Implemented by `#[messages]` for an actor with an `#[on_stop]` method; routed
/// to by the derive's [`Actor::on_stop`](crate::actor::Actor::on_stop).
#[doc(hidden)]
pub trait OnStopHook: Actor {
    fn __erased_on_stop<'a>(
        self,
        reason: StopReason<'a, Self>,
        cx: ActorContext<'a, Self>,
    ) -> ErasedHookWork<'a, Self>
    where
        Self: Sized;
}
