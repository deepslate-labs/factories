use crate::actor::dispatch::{ActorMessageDispatcher, StaticDispatcher};
use crate::actor::rtti::ActorRtti;
use crate::message::Message;
use crate::message::channel::AnswerSender;
use crate::message::envelope::MessageEnvelope;
use crate::message::rtti::MessageRtti;
use channel::ActorChannel;
use core::marker::PhantomData;

pub mod channel;
pub mod dispatch;
pub mod handle;
pub mod identity;
pub mod rtti;
pub mod state;
pub mod task;

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
}

/// Actor initialization protocol.
///
/// The idea here is mainly that an actor `A` can be constructed with some
/// argument type `Args`.
pub trait ActorInit<A: Actor> {
    type Args;

    /// Prepare the initializer using the given arguments.
    fn prepare(args: Self::Args) -> Self;

    /// Consume the initializer and actually initialize the actor.
    fn init(self) -> impl Future<Output = Result<A, A::Error>>;
}

/// Identity-based actor initialization protocol.
#[derive(Debug)]
pub struct IdentityActorInit<A: Actor> {
    actor: A,
}

impl<A: Actor> IdentityActorInit<A> {
    pub const fn new(actor: A) -> Self {
        Self { actor }
    }
}

impl<A: Actor> ActorInit<A> for IdentityActorInit<A> {
    type Args = A;

    fn prepare(args: Self::Args) -> Self {
        Self::new(args)
    }

    fn init(self) -> impl Future<Output = Result<A, A::Error>> {
        core::future::ready(Ok(self.actor))
    }
}

impl<A: Actor> From<A> for IdentityActorInit<A> {
    fn from(value: A) -> Self {
        Self::new(value)
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

/// A demand a run loop places on the message handling machinery it drives.
///
/// Demands are enforced at dispatcher *declaration* sites (see
/// [`declare_static_dispatcher!`](crate::declare_static_dispatcher)), where the
/// concrete handler future types are known and their auto traits leak. They never
/// appear in caller-facing signatures - whether a handler future is `Send` is the
/// run loop's problem, not the sender's.
pub trait DispatchDemand {}

/// Demand of run loops that drive handler futures on tasks which may migrate
/// between threads (e.g. work-stealing executors).
#[derive(Debug, Default, Copy, Clone)]
pub struct ThreadSafe;

impl DispatchDemand for ThreadSafe {}

/// Demand of run loops that drive handler futures on a single thread.
#[derive(Debug, Default, Copy, Clone)]
pub struct ThreadLocal;

impl DispatchDemand for ThreadLocal {}

/// A future satisfying the given dispatch demand.
#[diagnostic::on_unimplemented(
    message = "this handler future does not satisfy the `{D}` demand of the actor's run loop",
    note = "run loops with a `ThreadSafe` demand require handler futures to be `Send`"
)]
pub trait DemandedFuture<D: DispatchDemand>: Future {}

impl<F: Future + Send> DemandedFuture<ThreadSafe> for F {}
impl<F: Future> DemandedFuture<ThreadLocal> for F {}

/// Identity helper that enforces a dispatch demand on a future at compile time.
///
/// Used by dispatcher declaration macros; the returned future is the input,
/// unchanged - only the bound matters.
pub const fn demand_check<D: DispatchDemand, F: DemandedFuture<D>>(fut: F) -> F {
    fut
}

/// Context passed to a message handler of an actor.
pub struct MessageHandlerContext<'a, M: Message, A: Actor + ?Sized, E: AccessMode<A> + 'a> {
    actor_access: E::Guard<'a>,
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
        envelope: MessageEnvelope,
    ) -> Result<Self, MessageEnvelope> {
        if envelope.rtti() != M::RTTI {
            // Type mismatch
            return Err(envelope);
        }

        // SAFETY: We checked that M is indeed the payload type of the envelope
        Ok(unsafe { Self::new_unchecked(actor_access, envelope) })
    }

    /// Create a new message handler context.
    ///
    /// # Safety
    /// The caller is responsible for ensuring that the envelope actually carries a message
    /// of type `M`.
    pub unsafe fn new_unchecked(actor_access: E::Guard<'a>, envelope: MessageEnvelope) -> Self {
        Self {
            actor_access,
            envelope,
            _data: PhantomData,
        }
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

// SAFETY: The guard's sendability is honestly reflected by the where-clause. The
//         envelope's sendability is guaranteed by the boundary contracts: channels
//         that transport deliveries across threads verify `MessageEnvelope::is_sendable`
//         before doing so, and the runtime binder validates dynamic dispatch. This
//         mirrors the `unsafe impl Send for DispatchedActorMessage` justification.
unsafe impl<'a, M: Message, A: Actor + ?Sized, E: AccessMode<A>> Send
    for MessageHandlerContext<'a, M, A, E>
where
    E::Guard<'a>: Send,
{
}

/// Implementation of a message handler for an actor.
pub trait MessageHandler<M: Message>: Actor {
    type AccessMode: AccessMode<Self> + 'static;

    /// The statically bound dispatcher for this actor/message pair.
    ///
    /// Declared via [`declare_static_dispatcher!`](crate::declare_static_dispatcher),
    /// which enforces the run loop's [`DispatchDemand`] where the concrete future
    /// types are known. Typed handles use this constant to skip the runtime binder
    /// at statically known dispatch sites.
    const DISPATCHER: StaticDispatcher<Self, M>;

    fn handle<'a>(
        ctx: MessageHandlerContext<'a, M, Self, Self::AccessMode>,
    ) -> impl Future<Output = ()> + 'a;
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
/// actor's run loop (see [`DispatchDemand`]).
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

    /// The demand this run loop places on handler futures.
    ///
    /// Enforced at dispatcher declaration sites; see [`DispatchDemand`].
    type Demand: DispatchDemand;
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
}
