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
use alloc::boxed::Box;
use alloc::string::String;
use core::any::Any;
use core::fmt::{Debug, Display, Formatter};

/// A panic payload captured from an unwinding handler, hook or event driver,
/// in a form that is always `Send + Sync`.
///
/// The panic message is extracted eagerly at capture time (a copy), so
/// [`message`](Self::message) and the [`Display`] impl keep working no matter
/// what happens to the payload. The original payload - exactly the box
/// `catch_unwind` produced - is retained untouched and can be reclaimed once
/// via [`take_raw`](Self::take_raw), e.g. to feed existing panic-reporting
/// infrastructure or `resume_unwind`.
pub struct CapturedPanic {
    /// Eagerly extracted for string payloads (`panic!` literals and formatted
    /// messages), so it survives [`take_raw`](Self::take_raw).
    message: Option<Box<str>>,

    /// The original unwind payload, take-once. The lock turns the `Send`-only
    /// box `Sync`; it is held only for the pointer-sized move in `take_raw`.
    payload: factories_sync::Mutex<Option<Box<dyn Any + Send>>>,
}

impl CapturedPanic {
    /// Capture the payload an unwind produced (what `catch_unwind` returned).
    pub fn from_unwind_payload(payload: Box<dyn Any + Send>) -> Self {
        let message = if let Some(message) = payload.downcast_ref::<&'static str>() {
            Some(Box::from(*message))
        } else {
            payload
                .downcast_ref::<String>()
                .map(|message| Box::from(message.as_str()))
        };

        Self {
            message,
            payload: factories_sync::Mutex::new(Some(payload)),
        }
    }

    /// The panic message, if the payload was a string (every `panic!`,
    /// `assert!` and `unwrap` - the overwhelmingly common case). Unaffected by
    /// [`take_raw`](Self::take_raw).
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Take the original unwind payload - byte-for-byte what `catch_unwind`
    /// produced, uniformly for every payload type. `None` after the first
    /// call.
    pub fn take_raw(&self) -> Option<Box<dyn Any + Send>> {
        self.payload.with_lock(Option::take)
    }
}

impl Display for CapturedPanic {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self.message() {
            Some(message) => f.write_str(message),
            None => f.write_str("<non-string panic payload>"),
        }
    }
}

impl Debug for CapturedPanic {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CapturedPanic")
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

/// Why an actor's run loop is stopping, handed to
/// [`on_stop`](crate::actor::Actor::on_stop).
pub enum StopReason<'a, A: Actor + ?Sized> {
    /// The event source signalled completion - the mailbox closed because the
    /// last handle dropped or the actor [asked to stop](crate::actor::ActorContext::stop)
    /// - and the loop drained without failing.
    Finished,

    /// A handler - or the start hook - failed the actor. Carries the error that
    /// was recorded with [`ActorContext::fail`](crate::actor::ActorContext::fail).
    Failed(&'a A::Error),

    /// A handler or the event driver panicked and the loop captured the
    /// unwind.
    ///
    /// When dealing with a panic, keep in mind that message handler work may not
    /// have completed and inconsistent state is possibly observable on the actor.
    Panicked(&'a CapturedPanic),
}

/// The recorded outcome of an actor, observable through
/// [`SharedActorState::termination_reason`](crate::actor::state::SharedActorState::termination_reason).
pub enum TerminationReason<A: Actor + ?Sized> {
    /// The run loop drained and exited cleanly: the last handle dropped, or the
    /// actor [asked to stop](crate::actor::ActorContext::stop).
    Finished,

    /// A handler - or the start hook / init - failed the actor. Carries the
    /// error recorded with [`ActorContext::fail`](crate::actor::ActorContext::fail).
    Failed(A::Error),

    /// A handler, hook or event driver panicked and the loop captured the
    /// unwind.
    ///
    /// If a hook panics/errors, the error is discarded and the already existing
    /// error is kept.
    ///
    /// If factories was not able to catch a panic (for example, `std` is not
    /// available), then [`Aborted`] might be signalled instead.
    Panicked(CapturedPanic),

    /// The actor reached [`Dead`](crate::actor::state::LifecycleState::Dead) with
    /// no outcome recorded: the task was aborted.
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
            Self::Panicked(_) => TerminationKind::Panicked,
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
            Self::Panicked(panic) => f.debug_tuple("Panicked").field(panic).finish(),
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
    /// The run loop drained and exited cleanly: the last handle dropped, or the
    /// actor [asked to stop](crate::actor::ActorContext::stop).
    Finished,

    /// A handler, the start hook, or init failed the actor.
    Failed,

    /// A handler, hook or event driver panicked and the loop captured the
    /// unwind.
    Panicked,

    /// The actor reached [`Dead`](crate::actor::state::LifecycleState::Dead)
    /// with no recorded outcome: a task abort, or a panic that escaped capture.
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
            Self::Panicked(panic) => f.debug_tuple("Panicked").field(panic).finish(),
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
pub type ErasedStartHook<A> = for<'a> fn(&'a mut A, ActorContext<'a, A>) -> ErasedHookWork<'a, A>;

/// A stop hook, erased to a function pointer (see [`ErasedStartHook`]).
#[doc(hidden)]
pub type ErasedStopHook<A> =
    for<'a> fn(A, StopReason<'a, A>, ActorContext<'a, A>) -> ErasedHookWork<'a, A>;

/// Implemented by `#[messages]` for an actor with an `#[on_start]` method; routed
/// to by the derive's [`Actor::on_start`](crate::actor::Actor::on_start).
#[doc(hidden)]
pub trait OnStartHook: Actor {
    fn __erased_on_start<'a>(&'a mut self, cx: ActorContext<'a, Self>) -> ErasedHookWork<'a, Self>;
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
