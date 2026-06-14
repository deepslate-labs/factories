use crate::actor::dispatch::{ActorMessageDispatcher, StaticDispatcher};
use crate::actor::rtti::ActorRtti;
use crate::actor::work::IntoRunLoopWork;
use crate::message::Message;
use crate::message::channel::AnswerSender;
use crate::message::envelope::MessageEnvelope;
use crate::message::rtti::MessageRtti;
use channel::ActorChannel;
use core::fmt::Debug;
use core::marker::PhantomData;
use state::SharedActorState;

pub mod channel;
pub mod dispatch;
pub mod event;
pub mod handle;
pub mod identity;
pub mod rtti;
pub mod state;
pub mod task;
pub mod work;

/// Derive macro generating an [`Actor`] implementation together with its RTTI
/// declaration. Configured via `#[actor(...)]`; omitted components fall back
/// to [`runtime::defaults`](crate::runtime::defaults).
#[cfg(feature = "derive")]
pub use factories_actor_macro::Actor;

/// The heart of the actor system. Defines a struct as being an actor.
pub unsafe trait Actor: 'static {
    const RTTI: &'static ActorRtti;

    type Channel: ActorChannel;

    /// Error type produced by the actor.
    ///
    /// This is not necessarily the same as the per-message error type, but
    /// rather an error type that occurs when the actor itself fails.
    type Error;

    /// The binder used to bind message handlers for dynamically dispatched messages.
    ///
    /// If the message handler target is known at compile time, the binder might not be
    /// invoked.
    type RuntimeBinder: ActorRuntimeBinder;

    /// The lock strategy used to gain access to the actor state.
    type LockStrategy: LockStrategy<Self> + 'static;

    /// The run loop that is used to drive this actor.
    type RunLoop: ActorRunLoop<Self>;

    /// The typed handle returned when this actor is spawned.
    type TypedHandle: From<handle::TypedActorHandle<Self>>;

    /// User-defined data woven into the actor's shared state.
    type SharedStateExtension: Default + Send + Sync;

    /// The event-source driver multiplexed onto this actor's run loop.
    type EventDriver: for<'a> From<&'a Self>;
}

/// Actor initialization protocol: the `Send` boundary of actor construction.
///
/// The *initializer* is what crosses onto the actor task; [`init`](Self::init)
/// runs over there, so the actor is constructed where it will live. The purpose
/// is to allow `Send`ing arguments to a run loop, even if the actor itself is
/// not send.
pub trait ActorInit<A: Actor>: Sized {
    // This is just here because RTN is not stable yet.
    /// The future performing the initialization.
    type Fut: Future<Output = Result<A, A::Error>>;

    /// Consume the initializer and construct the actor.
    fn init(self) -> Self::Fut;
}

// Every actor value is its own initializer: immediate and infallible.
//
// In this case the actor itself is what crosses onto its task - available
// whenever that is fine (the actor is `Send`), which the work-stealing loops
// require anyway. Actors that cannot cross use a dedicated initializer (or
// [`InitFn`](crate::runtime::init::InitFn)) instead.
impl<A: Actor> ActorInit<A> for A {
    type Fut = core::future::Ready<Result<A, A::Error>>;

    fn init(self) -> Self::Fut {
        core::future::ready(Ok(self))
    }
}

/// Lock strategy that encapsulates the locking of the actor state.
pub trait LockStrategy<A: Actor + ?Sized> {}

/// Access mode that defines how a given lock strategy is used to obtain an actor lock.
///
/// Acquisition is async so the dispatch path can serialize lock acquisition
/// through the mailbox reader: while a contended acquire is pending the run
/// loop can still drive in-flight handler futures, but cannot pull a fresh
/// message from the mailbox and race ahead of the pending acquire.
pub trait AccessMode<A: Actor + ?Sized> {
    /// The resulting lock type.
    type Guard<'a>
    where
        Self: 'a;

    fn acquire<'a>(lock_strategy: &'a A::LockStrategy) -> impl Future<Output = Self::Guard<'a>>
    where
        Self: 'a;
}

/// Context passed to a message handler of an actor.
pub struct MessageHandlerContext<'a, M: Message, A: Actor + ?Sized, E: AccessMode<A> + 'a> {
    actor_access: E::Guard<'a>,
    actor_state: &'a SharedActorState<A>,
    envelope: MessageEnvelope,
    _data: PhantomData<(M, fn() -> A, E)>,
}

impl<'a, M: Message, A: Actor + ?Sized, E: AccessMode<A>> MessageHandlerContext<'a, M, A, E> {
    /// Create a new message handler context.
    ///
    /// Creation only succeeds if the envelope actually carries the correct message
    /// type.
    pub fn new(
        actor_access: E::Guard<'a>,
        actor_state: &'a SharedActorState<A>,
        envelope: MessageEnvelope,
    ) -> Result<Self, MessageEnvelope> {
        if envelope.rtti() != M::RTTI {
            // Type mismatch
            return Err(envelope);
        }

        // SAFETY: We checked that M is indeed the payload type of the envelope
        Ok(unsafe { Self::new_unchecked(actor_access, actor_state, envelope) })
    }

    /// Create a new message handler context.
    ///
    /// # Safety
    /// The caller is responsible for ensuring that the envelope actually carries a message
    /// of type `M`.
    pub unsafe fn new_unchecked(
        actor_access: E::Guard<'a>,
        actor_state: &'a SharedActorState<A>,
        envelope: MessageEnvelope,
    ) -> Self {
        Self {
            actor_access,
            actor_state,
            envelope,
            _data: PhantomData,
        }
    }

    /// The actor's own runtime services (failing the actor, lifecycle).
    ///
    /// The returned value borrows the run loop's state, not this context: it
    /// can be grabbed first and stays usable after the context is decomposed.
    pub fn actor_context(&self) -> ActorContext<'a, A> {
        ActorContext::new(self.actor_state)
    }

    /// Access the actor guard.
    pub fn guard(&self) -> &E::Guard<'a> {
        &self.actor_access
    }

    /// Mutably access the actor guard.
    pub fn guard_mut(&mut self) -> &mut E::Guard<'a> {
        &mut self.actor_access
    }

    /// Access the message payload.
    pub fn message(&self) -> &M {
        // SAFETY: Construction of the context guarantees the envelope carries an M.
        unsafe { self.envelope.payload_unchecked::<M>() }
    }

    /// Decompose the context into the actor guard, the message and the answer sender.
    pub fn into_parts(self) -> (E::Guard<'a>, M, Option<AnswerSender<M>>) {
        // SAFETY: Construction of the context guarantees the envelope carries an M.
        let (message, answer_sender) = unsafe { self.envelope.unwrap_unchecked::<M>() };
        (self.actor_access, message, answer_sender)
    }

    /// Decompose the context into the actor guard and the message envelope.
    pub fn into_parts_with_envelope(self) -> (E::Guard<'a>, MessageEnvelope) {
        (self.actor_access, self.envelope)
    }
}

// SAFETY: The guard's and shared state reference's sendability are
//         reflected by the where-clause. The envelope's sendability is guaranteed
//         by the boundary contracts: channels that transport deliveries across
//         threads verify `MessageEnvelope::is_sendable` before doing so, and the
//         runtime binder validates dynamic dispatch. This mirrors the
//         `unsafe impl Send for DispatchedActorMessage` justification.
unsafe impl<'a, M: Message, A: Actor + ?Sized, E: AccessMode<A>> Send
    for MessageHandlerContext<'a, M, A, E>
where
    E::Guard<'a>: Send,
    SharedActorState<A>: Sync,
{
}

/// Implementation of a message handler for an actor.
pub trait MessageHandler<M: Message>: Actor {
    type AccessMode: AccessMode<Self> + 'static;

    /// The statically bound dispatcher for this actor/message pair.
    ///
    /// Declared via [`declare_static_dispatcher!`](crate::declare_static_dispatcher),
    /// which erases the dispatch into the run loop's
    /// [`ErasedWork`](crate::actor::work::ErasedWork) via its
    /// [`WorkConverter`](ActorRunLoop::WorkConverter) where the concrete handler
    /// types are known (so the converter's `Send` requirement is checked there).
    /// Typed handles use this constant to skip the runtime binder at statically
    /// known dispatch sites.
    const DISPATCHER: StaticDispatcher<Self, M>;

    /// Handle the message, producing this loop's work.
    fn handle<'a>(
        ctx: MessageHandlerContext<'a, M, Self, Self::AccessMode>,
    ) -> impl IntoRunLoopWork<<Self::RunLoop as ActorRunLoop<Self>>::WorkConverter> + 'a;
}

/// Implementation that binds handlers to messages.
///
/// Runtime binders are required to be Send + Sync as they are expected to only
/// look up function pointers and ensure that thread safe dispatch is possible.
///
/// In other words, a runtime binder must be the instance that itself performs
/// the thread safety checks and as such must be thread safe itself.
///
/// # Safety
/// The implementation must ensure that the bound handler will be able to handle
/// envelopes of the given message type and that the handler is invokable on the
/// actor thread. The bound dispatcher must satisfy the demand of the target
/// actor's run loop.
pub unsafe trait ActorRuntimeBinder: Send + Sync {
    /// Bind the handler for the given message.
    ///
    /// Note that this intentionally doesn't have access to the actor state,
    /// as the binding happens on the caller side.
    ///
    /// Envelope sendability is NOT the binder's concern: channels that
    /// transport deliveries across threads verify
    /// [`MessageEnvelope::is_sendable`](crate::message::envelope::MessageEnvelope::is_sendable)
    /// at the boundary (see [`ActorChannel`]).
    fn bind(&self, message: &MessageRtti) -> Option<ActorMessageDispatcher>;
}

/// Runtime binder that never binds: the actor only supports statically
/// dispatched messages, dynamic dispatch always fails to bind.
#[derive(Debug, Default, Copy, Clone)]
pub struct StaticOnlyBinder;

// SAFETY: This binder never produces a dispatcher, so there is nothing to uphold.
unsafe impl ActorRuntimeBinder for StaticOnlyBinder {
    fn bind(&self, _message: &MessageRtti) -> Option<ActorMessageDispatcher> {
        None
    }
}

pub trait ActorRunLoop<A: Actor + ?Sized> {
    /// The dispatch context type owned by the run loop.
    ///
    /// `'static` is required so the dispatcher's fn-pointer signature can stay
    /// late-bound in the call-site lifetime (HRTB-coercible). In practice the
    /// run loop owns its dispatch context outright, so this is rarely a real
    /// constraint.
    type DispatchContext: ActorRunLoopDispatchContext<A> + 'static;

    /// Selects how a handler's return becomes this loop's work, and what that
    /// work *is* (its [`Erased`](crate::actor::work::WorkConverter::Erased)
    /// representation).
    ///
    /// `MessageHandler::handle` returns `impl IntoRunLoopWork<Self::WorkConverter>`;
    /// the converter the loop ships decides what shapes it accepts (a `Send`
    /// future for a work-stealing loop, a plain value for a single-thread loop,
    /// ...), whether the resulting work is `Send`, and how it is driven. This is
    /// where the loop owns its dispatch discipline.
    type WorkConverter: work::WorkConverter;
}

/// Marker contract for run loops that never overlap dispatches.
///
/// This is the counterpart of [`DispatchDemand`]: a demand is what the loop
/// *requires* of handler futures, this marker is what the loop *guarantees* to
/// the locking machinery. Lock strategies that elide synchronization (e.g.
/// [`UnguardedLock`](crate::runtime::lock::UnguardedLock)) bound on it so that
/// pairing them with an overlapping loop is a compile error.
///
/// # Safety
/// The implementor guarantees that dispatches of an actor instance driven by
/// this loop never overlap: from the first poll of the acquire future returned
/// by a dispatcher until the resolved handler future completes or is dropped,
/// no other acquire or handler future of the same actor instance is polled.
/// This must hold even across threads (the loop task may migrate, but
/// dispatches stay strictly one-after-another). Consumers may rely on this
/// guarantee for soundness.
pub unsafe trait SerializedDispatch<A: Actor + ?Sized>: ActorRunLoop<A> {}

/// The dispatch-side view of an actor's run loop.
///
/// A reference to this type is what the dispatcher casts the opaque
/// [`crate::actor::dispatch::DispatchContextPtr`] back into.
pub trait ActorRunLoopDispatchContext<A: Actor + ?Sized> {
    /// The lock strategy used to acquire access to the actor state.
    fn lock_strategy(&self) -> &A::LockStrategy;

    /// The state shared between the run loop and the actor's handles.
    fn shared_state(&self) -> &SharedActorState<A>;
}

/// Handle to the actor's own runtime services, available to message handlers
/// through [`MessageHandlerContext::actor_context`].
///
/// This is a reduced surface to the shared actor state, as this is exposed
/// to message handlers that should not have full access to the internal
/// shared state.
pub struct ActorContext<'a, A: Actor + ?Sized> {
    state: &'a SharedActorState<A>,
}

impl<'a, A: Actor + ?Sized> ActorContext<'a, A> {
    /// Create a context over the actor's shared state.
    pub fn new(state: &'a SharedActorState<A>) -> Self {
        Self { state }
    }

    /// Fail the actor: record `error` and stop the run loop.
    ///
    /// The first error wins, later errors are dropped. The error is recorded
    /// immediately; the run loop notices at its next turn - typically when
    /// the failing handler completes - drops in-flight work and exits, which
    /// transitions the lifecycle to [`Dead`](state::LifecycleState::Dead).
    /// As on the init path, the error is observable before `Dead` is.
    pub fn fail(&self, error: A::Error) {
        let _ = self.state.set_error(error);
    }

    /// The current lifecycle state of the actor.
    pub fn lifecycle(&self) -> state::LifecycleState {
        self.state.lifecycle()
    }

    /// The actor's lock-free shared state extension.
    pub fn extension(&self) -> &'a A::SharedStateExtension {
        self.state.extension()
    }
}

// Manual impls: the derives would spuriously require `A: Copy/Clone`.
impl<A: Actor + ?Sized> Copy for ActorContext<'_, A> {}

impl<A: Actor + ?Sized> Clone for ActorContext<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A: Actor + ?Sized> Debug for ActorContext<'_, A>
where
    A::Error: Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ActorContext")
            .field("state", &self.state)
            .finish()
    }
}
