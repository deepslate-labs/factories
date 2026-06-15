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
pub mod lifecycle;
pub mod protocol;
pub mod rtti;
pub mod state;
pub mod supervision;
pub mod task;
pub mod work;

/// Derive macro generating an [`Actor`] implementation together with its RTTI
/// declaration. Configured via `#[actor(...)]`; omitted components fall back
/// to [`runtime::defaults`](crate::runtime::defaults).
#[cfg(feature = "derive")]
pub use factories_actor_macro::Actor;

/// The heart of the actor system. Defines a struct as being an actor.
///
/// # Safety
/// An actor implementation must ensure that the associated RTTI
/// const is of the actors self type.
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

    /// Run once after the actor is constructed, before it processes any message
    /// and before the lifecycle leaves
    /// [`Starting`](state::LifecycleState::Starting).
    ///
    /// The actor is not yet behind its lock, so access is plain `&mut self`. The
    /// hook returns handler-style work - the run loop's
    /// [`WorkConverter`](ActorRunLoop::WorkConverter) governs its `Send`-ness, so
    /// no hardcoded `Send` bound is needed - and the default does nothing.
    ///
    /// Fail the actor through `cx` to abort startup: the loop then never runs and
    /// [`on_stop`](Self::on_stop) is skipped. The `#[messages]` `die_on_err` sugar
    /// wires this up from a `Result`-returning hook body.
    ///
    /// Implement this directly on a hand-written actor; the derive routes a
    /// `#[on_start]` method here for you.
    fn on_start<'a>(
        &'a mut self,
        cx: ActorContext<'a, Self>,
    ) -> impl IntoRunLoopWork<<Self::RunLoop as ActorRunLoop<Self>>::WorkConverter> + 'a {
        let _ = cx;
        work::NoWork
    }

    /// Run once after the run loop has quiesced, before the actor is dropped.
    ///
    /// The actor arrives **by value** - reclaimed from its lock via
    /// [`ReclaimableLockStrategy`] once the loop is its sole owner - so the hook
    /// can decompose it and move fields into owned teardown work. `reason` says
    /// whether the loop drained cleanly or a failure stopped it. The default drops
    /// the actor and yields no work.
    ///
    /// As with [`on_start`](Self::on_start), implement it directly on a manual
    /// actor; the derive routes a `#[on_stop]` method here.
    fn on_stop<'a>(
        self,
        reason: lifecycle::StopReason<'a, Self>,
        cx: ActorContext<'a, Self>,
    ) -> impl IntoRunLoopWork<<Self::RunLoop as ActorRunLoop<Self>>::WorkConverter> + 'a
    where
        Self: Sized,
    {
        let _ = (reason, cx);
        work::NoWork
    }
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
pub trait LockStrategy<A: Actor + ?Sized> {
    /// Reclaim the actor state by value.
    ///
    /// Used by the spawn scaffolding to hand the actor to
    /// [`Actor::on_stop`] once the loop has quiesced and the lock is the sole
    /// owner. Every real lock owns the actor and can give it back; the bound is
    /// always available (`A::LockStrategy: LockStrategy<A>` holds for every
    /// actor), so no separate capability is needed.
    fn into_inner(self) -> A
    where
        Self: Sized,
        A: Sized;
}

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
    self_ref: &'a handle::WeakActorHandle<A>,
    envelope: MessageEnvelope,
    _data: PhantomData<(M, fn() -> A, E)>,
}

impl<'a, M: Message, A: Actor + ?Sized, E: AccessMode<A>> MessageHandlerContext<'a, M, A, E> {
    /// Create a new message handler context.
    ///
    /// Creation only succeeds if the envelope actually carries the correct message
    /// type. `self_ref` is the actor's own weak handle, supplied by the run loop's
    /// dispatch context (see [`ActorRunLoopDispatchContext::self_ref`]); pass
    /// `None` from a loop that does not provide one.
    pub fn new(
        actor_access: E::Guard<'a>,
        actor_state: &'a SharedActorState<A>,
        self_ref: &'a handle::WeakActorHandle<A>,
        envelope: MessageEnvelope,
    ) -> Result<Self, MessageEnvelope> {
        if envelope.rtti() != M::RTTI {
            // Type mismatch
            return Err(envelope);
        }

        // SAFETY: We checked that M is indeed the payload type of the envelope
        Ok(unsafe { Self::new_unchecked(actor_access, actor_state, self_ref, envelope) })
    }

    /// Create a new message handler context.
    ///
    /// # Safety
    /// The caller is responsible for ensuring that the envelope actually carries a message
    /// of type `M`.
    pub unsafe fn new_unchecked(
        actor_access: E::Guard<'a>,
        actor_state: &'a SharedActorState<A>,
        self_ref: &'a handle::WeakActorHandle<A>,
        envelope: MessageEnvelope,
    ) -> Self {
        Self {
            actor_access,
            actor_state,
            self_ref,
            envelope,
            _data: PhantomData,
        }
    }

    /// The actor's own runtime services (failing the actor, lifecycle, self-ref).
    ///
    /// The returned value borrows the run loop's state, not this context: it
    /// can be grabbed first and stays usable after the context is decomposed.
    pub fn actor_context(&self) -> ActorContext<'a, A> {
        ActorContext::new(self.actor_state, self.self_ref)
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
    /// Usually written through
    /// [`implement_message_handler!`](crate::implement_message_handler) (which
    /// emits this whole impl) or, at a lower level,
    /// [`declare_static_async_dispatcher!`](crate::declare_static_async_dispatcher);
    /// a custom implementation is also possible.
    const DISPATCHER: StaticDispatcher<Self, M>;
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

    /// The actor's own weak self-reference.
    ///
    /// Every actor has an identity, so every dispatch context carries one; it
    /// powers [`ActorContext::weak_ref`] / [`ActorContext::actor_ref`]. A run
    /// loop obtains it before spawning (the identity exists first) and threads
    /// it into its dispatch context.
    fn self_ref(&self) -> &handle::WeakActorHandle<A>;
}

/// Handle to the actor's own runtime services, available to message handlers
/// through [`MessageHandlerContext::actor_context`].
///
/// This is a reduced surface to the shared actor state, as this is exposed
/// to message handlers that should not have full access to the internal
/// shared state.
pub struct ActorContext<'a, A: Actor + ?Sized> {
    state: &'a SharedActorState<A>,
    self_ref: &'a handle::WeakActorHandle<A>,
}

impl<'a, A: Actor + ?Sized> ActorContext<'a, A> {
    /// Create a context over the actor's shared state and weak self-reference.
    pub fn new(
        state: &'a SharedActorState<A>,
        self_ref: &'a handle::WeakActorHandle<A>,
    ) -> Self {
        Self { state, self_ref }
    }

    /// The actor's own weak handle.
    ///
    /// Always available - it is what an actor hands out to be watched, and never
    /// keeps the actor alive on its own.
    pub fn weak_ref(&self) -> handle::WeakActorHandle<A> {
        self.self_ref.clone()
    }

    /// A strong handle to this actor, if one still exists.
    ///
    /// Upgrades the weak self-reference. Returns `None` only when no strong
    /// handle survives - the last external handle dropped and the actor is in
    /// its final drain. In the normal case a handler runs while a strong handle
    /// exists, so this is `Some`.
    pub fn actor_ref(&self) -> Option<handle::TypedActorHandle<A>>
    where
        A: Sized,
    {
        self.self_ref.upgrade()
    }

    /// Watch `watched` from this actor: a [`Terminated`](supervision::Terminated)
    /// tagged `tag` is pushed into this actor's mailbox when `watched` stops.
    ///
    /// The in-handler counterpart of
    /// [`TypedActorHandle::watch`](handle::TypedActorHandle::watch): it uses this
    /// actor's own weak self-reference, so no handle to self is needed. Requires
    /// `Self: MessageHandler<Terminated>`.
    pub fn watch(&self, watched: &impl handle::ActorHandle, tag: u64)
    where
        A: MessageHandler<supervision::Terminated> + 'static,
        A::Channel: Send + Sync,
        A::Error: Send + Sync,
    {
        handle::register_watch::<A>(self.self_ref.clone().erase(), self.state.id(), watched, tag);
    }

    /// Stop watching `watched`: remove every subscription this actor registered
    /// on it. Idempotent.
    pub fn unwatch(&self, watched: &impl handle::ActorHandle) {
        handle::unregister_watch(self.state.id(), watched);
    }

    /// Fail the actor: record `error` and stop the run loop.
    ///
    /// The first error wins, later errors are dropped. The error is recorded
    /// immediately; the run loop notices at its next turn - typically when
    /// the failing handler completes - drops in-flight work and exits, which
    /// transitions the lifecycle to [`Dead`](state::LifecycleState::Dead).
    /// As on the init path, the error is observable before `Dead` is.
    pub fn fail(&self, error: A::Error) {
        self.state.record_failure(error);
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
    SharedActorState<A>: Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ActorContext")
            .field("state", &self.state)
            .finish()
    }
}
