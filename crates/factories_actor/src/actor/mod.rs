use crate::actor::dispatch::ActorMessageDispatcher;
use crate::actor::rtti::ActorRtti;
use crate::message::Message;
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
}

/// Implementation of a message handler for an actor.
pub trait MessageHandler<M: Message>: Actor {
    type AccessMode: AccessMode<Self> + 'static;

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
/// actor thread.
pub unsafe trait ActorRuntimeBinder: Send + Sync {
    /// Bind the handler for the given message.
    ///
    /// Note that this intentionally doesn't have access to the actor state,
    /// as the binding happens on the caller side.
    ///
    /// Note that this must also validate that the message can be dispatched
    /// to the actor thread. If the message is not `Send`, then this must
    /// return `None` if the dispatcher can't guarantee that the message
    /// is accessed on the thread this is called from.
    fn bind(&self, message: &MessageRtti) -> Option<ActorMessageDispatcher>;
}

pub trait ActorRunLoop<A: Actor + ?Sized> {
    /// The dispatch context type owned by the run loop.
    ///
    /// `'static` is required so the dispatcher's fn-pointer signature can stay
    /// late-bound in the call-site lifetime (HRTB-coercible). In practice the
    /// run loop owns its dispatch context outright, so this is rarely a real
    /// constraint.
    type DispatchContext: ActorRunLoopDispatchContext<A> + 'static;
}

/// The dispatch-side view of an actor's run loop.
///
/// A reference to this type is what the dispatcher casts the opaque
/// [`crate::actor::dispatch::DispatchContextPtr`] back into.
pub trait ActorRunLoopDispatchContext<A: Actor + ?Sized> {
    /// The lock strategy used to acquire access to the actor state.
    fn lock_strategy(&self) -> &A::LockStrategy;
}
