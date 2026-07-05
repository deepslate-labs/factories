#[cfg(feature = "tokio-answer")]
use crate::actor::channel::ActorChannelSendResult;
use crate::actor::channel::{
    ActorChannel, ActorChannelSendError, ActorChannelSendable, DynActorChannelSendable,
};
use crate::actor::dispatch::{DispatchedActorMessage, DispatchedActorMessageContext};
use crate::actor::identity::{ActorIdentity, AnyActorIdentity};
use crate::actor::rtti::ActorRtti;
use crate::actor::state::SharedActorState;
use crate::actor::supervision::{Subscription, Terminated, ActorId};
use crate::actor::task::ActorTaskHandle;
use crate::actor::{Actor, MessageHandler};
use crate::message::Message;
use crate::message::channel::{AnswerReceiver, AnswerSender};
use crate::message::envelope::MessageEnvelope;
use crate::message::rtti::MessageRtti;
use alloc::boxed::Box;
use alloc::sync::{Arc, Weak};
#[cfg(feature = "tokio-answer")]
use core::future::{Future, IntoFuture};
use core::marker::PhantomData;
use thiserror::Error;

pub struct TypedActorHandle<A: Actor + ?Sized>(Arc<ActorIdentity<A>>);

// Manual impls so the handle stays clonable / debuggable even when `A: !Clone`,
// and works for unsized `A` (the handle only holds `A`'s associated types).
impl<A: Actor + ?Sized> Clone for TypedActorHandle<A> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<A: Actor + ?Sized> core::fmt::Debug for TypedActorHandle<A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("TypedActorHandle").field(&self.0).finish()
    }
}

impl<A: Actor + ?Sized> TypedActorHandle<A> {
    /// Assemble an actor handle from its parts.
    ///
    /// This is the layer-0 entry point used by the actor builder internally and
    /// by hand-crafted assembly. The caller is responsible for having spawned a
    /// run loop that consumes the mailbox side of `channel` and reports into
    /// `shared`.
    pub fn assemble(
        channel: A::Channel,
        binder: A::RuntimeBinder,
        shared: SharedActorState<A>,
    ) -> Self {
        Self(Arc::new(ActorIdentity::new(channel, binder, shared)))
    }

    /// Access the shared state of this actor (lifecycle, failure error, task).
    pub fn state(&self) -> &SharedActorState<A> {
        &self.0.shared
    }

    /// Downgrade to a non-owning [`WeakActorHandle`].
    ///
    /// The weak handle observes the actor and can be upgraded back to a strong
    /// handle while one still exists, but never keeps the actor alive on its own.
    pub fn downgrade(&self) -> WeakActorHandle<A> {
        WeakActorHandle(Arc::downgrade(&self.0))
    }

    /// Watch `watched`: when it terminates, a [`Terminated`] signal is pushed
    /// into *this* actor's mailbox (handled by its
    /// [`MessageHandler<Terminated>`]), carrying `tag` as the correlation key.
    ///
    /// Unidirectional and non-owning: the watch keeps neither actor alive (this
    /// actor is held weakly on the watched side).
    pub fn watch(&self, watched: &impl ActorHandle, tag: u64)
    where
        A: MessageHandler<Terminated> + 'static,
        A::Channel: Send + Sync,
        A::Error: Send + Sync,
    {
        register_watch::<A>(self.downgrade().erase(), self.state().id(), watched, tag);
    }

    /// Stop watching `watched`: remove every subscription this actor registered
    /// on it. Idempotent; a no-op if not watching (or already fired).
    pub fn unwatch(&self, watched: &impl ActorHandle) {
        unregister_watch(self.state().id(), watched);
    }

    /// Type erase the actor handle into an untyped local handle.
    pub fn erase_type_local(self) -> AnyLocalActorHandle
    where
        A: 'static,
    {
        let this = self.0 as Arc<dyn AnyActorIdentity>;
        AnyLocalActorHandle(this)
    }

    /// Type erase the actor handle into an untyped handle.
    pub fn erase_type(self) -> AnyActorHandle
    where
        A: 'static,
        <A as Actor>::Channel: Send + Sync,
        <A as Actor>::Error: Send + Sync,
    {
        let this = self.0 as Arc<dyn AnyActorIdentity + Send + Sync>;
        AnyActorHandle(this)
    }

    /// Retrieve the channel of the actor.
    pub fn channel(&self) -> &A::Channel {
        &self.0.channel
    }

    /// Prepare sending a statically dispatched message to this actor.
    pub fn prepare_send<M: Message>(
        &self,
        message: M,
        answer_sender: Option<AnswerSender<M>>,
    ) -> impl ActorChannelSendable<'_>
    where
        A: MessageHandler<M>,
    {
        let dispatcher = <A as MessageHandler<M>>::DISPATCHER.into_dispatcher();

        // SAFETY: The dispatcher is `A`'s declaration-checked static dispatcher
        //         for `M`, and the envelope is constructed from an `M` right here.
        let message = unsafe {
            DispatchedActorMessage::new(
                dispatcher,
                DispatchedActorMessageContext::of(MessageEnvelope::new(message, answer_sender)),
            )
        };

        ActorChannel::prepare_send(self.channel(), message)
    }

    /// Send a message to the actor without expecting a reply.
    pub fn tell<M: Message>(&self, message: M) -> impl ActorChannelSendable<'_>
    where
        A: MessageHandler<M>,
    {
        self.prepare_send(message, None)
    }

    /// Send a message to the actor expecting a reply.
    #[cfg(feature = "tokio-answer")]
    pub fn ask<M: Message>(&self, message: M) -> AskSendable<'_, M, impl ActorChannelSendable<'_>>
    where
        A: MessageHandler<M>,
    {
        let (answer_sender, answer_receiver) = crate::message::channel::answer_channel();

        let sendable = self.prepare_send(message, Some(answer_sender));

        AskSendable {
            sendable,
            receive: answer_receiver,
            _data: PhantomData,
        }
    }

    /// Prepare a typed call to this actor.
    ///
    /// The returned [`MessageCall`] must be used. See its documentation
    /// for how to choose the form of communication.
    #[cfg(feature = "tokio-answer")]
    pub fn call<M: Message>(
        &self,
        message: M,
    ) -> MessageCall<impl Calling<Output = Result<M::Answer, AskError>> + use<'_, A, M>>
    where
        A: MessageHandler<M>,
    {
        let handle = self;
        MessageCall(PreparedCall {
            handle,
            message,
            ask_gen: move |message: M| handle.ask(message).exchange(),
        })
    }
}

/// A non-owning handle to an actor.
///
/// Holds a [`Weak`] to the actor's identity: it can be [`upgrade`](Self::upgrade)d
/// to a strong [`TypedActorHandle`] while one still exists, but never keeps the
/// actor alive by itself. This is what an actor hands out to be watched, and what
/// backs [`ActorContext::weak_ref`](crate::actor::ActorContext)-style access.
pub struct WeakActorHandle<A: Actor + ?Sized>(Weak<ActorIdentity<A>>);

impl<A: Actor + ?Sized> WeakActorHandle<A> {
    /// Create a dangling weak handle.
    ///
    /// This handle can never be upgraded.
    pub const fn dangling() -> Self {
        Self(Weak::new())
    }

    /// Upgrade to a strong handle, if the actor's identity is still alive.
    ///
    /// Returns `None` once the last strong handle is gone (e.g. the actor has
    /// died, or is in its final drain after the last handle dropped).
    pub fn upgrade(&self) -> Option<TypedActorHandle<A>> {
        self.0.upgrade().map(TypedActorHandle)
    }

    /// Erase to a weak, `Send + Sync` identity reference.
    ///
    /// Used to register this actor as a watcher. The coercion is a plain
    /// unsizing of the inner `Weak`, so no upgrade (and no live strong handle)
    /// is required.
    pub(crate) fn erase(self) -> Weak<dyn AnyActorIdentity + Send + Sync>
    where
        A: 'static,
        A::Channel: Send + Sync,
        A::Error: Send + Sync,
    {
        self.0
    }
}

/// Register `watcher` (already erased) to be notified when `watched` terminates,
/// binding the watcher's own `Terminated` dispatcher now. Shared by
/// [`TypedActorHandle::watch`] and `ActorContext::watch`.
pub(crate) fn register_watch<A>(
    watcher: Weak<dyn AnyActorIdentity + Send + Sync>,
    watcher_id: ActorId,
    watched: &impl ActorHandle,
    tag: u64,
) where
    A: MessageHandler<Terminated> + ?Sized,
{
    let dispatcher = <A as MessageHandler<Terminated>>::DISPATCHER.into_dispatcher();
    let subscription = Subscription::new(watcher, watcher_id, dispatcher, tag);
    // `identity()` (the sealed `ActorHandleBase`) is crate-internal, so the
    // `Subscription` type never appears in a public signature.
    watched.identity().add_subscription(subscription);
}

/// Remove every subscription registered by `watcher_id` on `watched`. Shared by
/// the `unwatch` surfaces.
pub(crate) fn unregister_watch(watcher_id: ActorId, watched: &impl ActorHandle) {
    watched.identity().remove_subscriptions(watcher_id);
}

impl<A: Actor + ?Sized> Clone for WeakActorHandle<A> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<A: Actor + ?Sized> core::fmt::Debug for WeakActorHandle<A>
where
    Weak<ActorIdentity<A>>: core::fmt::Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("WeakActorHandle").field(&self.0).finish()
    }
}

pub struct AskSendable<'a, M: Message, S: ActorChannelSendable<'a>> {
    sendable: S,
    receive: AnswerReceiver<M>,
    _data: PhantomData<&'a M>,
}

impl<'a, M: Message, S: ActorChannelSendable<'a>> AskSendable<'a, M, S> {
    /// Perform the message exchange asynchronously with the actor.
    pub async fn exchange(self) -> Result<M::Answer, AskError> {
        self.sendable.send().await?;
        self.receive.recv().await.ok_or(AskError::NoReply)
    }

    /// Perform the message exchange synchronously with the actor.
    pub fn blocking_exchange(self) -> Result<M::Answer, AskError> {
        self.sendable.blocking_send()?;
        self.receive.blocking_recv().ok_or(AskError::NoReply)
    }
}

unsafe impl<'a, M: Message, S: ActorChannelSendable<'a>> Send for AskSendable<'a, M, S> where M: Send
{}
unsafe impl<'a, M: Message, S: ActorChannelSendable<'a>> Sync for AskSendable<'a, M, S> where M: Sync
{}

#[derive(Debug, Error)]
pub enum AskError {
    #[error(transparent)]
    SendFailed(#[from] ActorChannelSendError),

    #[error("no reply received")]
    NoReply,
}

/// A prepared call to an actor that has not yet been dispatched.
///
/// Built by [`TypedActorHandle::call`] and by the generated typed-handle
/// methods.
///
/// The form of communication is encoded via this struct:
/// - `.await` performs an ask and yields the answer,
/// - [`tell`](Self::tell) sends without awaiting a reply,
/// - [`blocking_tell`](Self::blocking_tell) / [`blocking_ask`](Self::blocking_ask)
///   do the same off an async runtime.
#[cfg(feature = "tokio-answer")]
#[must_use = "a MessageCall does nothing until it is awaited, told, or blocked on"]
pub struct MessageCall<T>(T);

#[cfg(feature = "tokio-answer")]
pub(crate) mod sealed {
    pub trait Sealed {}
}

/// The prepared-call operations a [`MessageCall`] forwards to.
///
/// Sealed - implemented only by the framework's prepared-call type. The
/// [`IntoFuture`] supertrait is the trick that keeps the ask unboxed: the
/// channel's future is unnameable, so it travels as this type's
/// [`IntoFuture::IntoFuture`] and the surface [`MessageCall`] just relays it.
#[cfg(feature = "tokio-answer")]
#[allow(private_bounds)]
pub trait Calling: sealed::Sealed + IntoFuture {
    /// Sends the message and waits for the reply.
    fn ask(self) -> <Self as IntoFuture>::IntoFuture;

    /// Send the message without awaiting a reply.
    fn tell(self) -> impl Future<Output = ActorChannelSendResult>;

    /// Send the message without awaiting a reply, without blocking or awaiting.
    ///
    /// Enqueues if there is room and fails immediately otherwise (e.g.
    /// [`MailboxFull`](ActorChannelSendError::MailboxFull) /
    /// [`ActorDead`](ActorChannelSendError::ActorDead)).
    fn try_tell(self) -> ActorChannelSendResult;

    /// Send the message without awaiting a reply, blocking the thread.
    fn blocking_tell(self) -> ActorChannelSendResult;

    /// Perform the ask exchange, blocking the thread.
    fn blocking_ask(self) -> <Self as IntoFuture>::Output;
}

/// The inner prepared call: handle + message + a generator for the ask future.
///
/// The generator exists solely to give the otherwise-unnameable ask future a
/// name (the closure's return type), which is what lets [`MessageCall`] be
/// awaited without boxing. `tell`/`blocking_*` build their own differently
/// shaped sends from the retained handle and message.
#[cfg(feature = "tokio-answer")]
struct PreparedCall<'a, A: Actor + ?Sized, M: Message, C> {
    handle: &'a TypedActorHandle<A>,
    message: M,
    ask_gen: C,
}

#[cfg(feature = "tokio-answer")]
impl<'a, A, M, C, Fut> IntoFuture for PreparedCall<'a, A, M, C>
where
    A: Actor + MessageHandler<M> + ?Sized,
    M: Message,
    C: FnOnce(M) -> Fut,
    Fut: Future<Output = Result<M::Answer, AskError>> + 'a,
{
    type Output = Result<M::Answer, AskError>;
    type IntoFuture = Fut;

    fn into_future(self) -> Fut {
        (self.ask_gen)(self.message)
    }
}

#[cfg(feature = "tokio-answer")]
impl<A: Actor + ?Sized, M: Message, C> sealed::Sealed for PreparedCall<'_, A, M, C> {}

#[cfg(feature = "tokio-answer")]
impl<'a, A, M, C, Fut> Calling for PreparedCall<'a, A, M, C>
where
    A: Actor + MessageHandler<M> + ?Sized,
    M: Message,
    C: FnOnce(M) -> Fut,
    Fut: Future<Output = Result<M::Answer, AskError>> + 'a,
{
    fn ask(self) -> <Self as IntoFuture>::IntoFuture {
        self.into_future()
    }

    fn tell(self) -> impl Future<Output = ActorChannelSendResult> {
        self.handle.tell(self.message).send()
    }

    fn try_tell(self) -> ActorChannelSendResult {
        self.handle.tell(self.message).try_send()
    }

    fn blocking_tell(self) -> ActorChannelSendResult {
        self.handle.tell(self.message).blocking_send()
    }

    fn blocking_ask(self) -> Result<M::Answer, AskError> {
        self.handle.ask(self.message).blocking_exchange()
    }
}

#[cfg(feature = "tokio-answer")]
impl<T: IntoFuture> IntoFuture for MessageCall<T> {
    type Output = T::Output;
    type IntoFuture = T::IntoFuture;

    fn into_future(self) -> T::IntoFuture {
        self.0.into_future()
    }
}

#[cfg(feature = "tokio-answer")]
impl<T: Calling> MessageCall<T> {
    /// Wrap a prepared call. Hand-written typed handles build their inner call
    /// with [`TypedActorHandle::call`] and wrap it here.
    pub fn new(inner: T) -> Self {
        Self(inner)
    }

    /// Sends the message and awaits the reply.
    pub fn ask(self) -> impl Future<Output = <T as IntoFuture>::Output> {
        self.0.ask()
    }

    /// Send the message without awaiting a reply.
    pub fn tell(self) -> impl Future<Output = ActorChannelSendResult> {
        self.0.tell()
    }

    /// Send the message without awaiting a reply, without blocking or awaiting.
    ///
    /// Enqueues if there is room and fails immediately otherwise (e.g.
    /// [`MailboxFull`](ActorChannelSendError::MailboxFull)). This is the
    /// sanctioned synchronous fire-and-forget: callable from non-async
    /// contexts (a sync handler, a callback) without a runtime handle and
    /// without blocking a thread.
    pub fn try_tell(self) -> ActorChannelSendResult {
        self.0.try_tell()
    }

    /// Send the message without awaiting a reply, blocking the thread.
    pub fn blocking_tell(self) -> ActorChannelSendResult {
        self.0.blocking_tell()
    }

    /// Perform the ask exchange, blocking the thread.
    pub fn blocking_ask(self) -> <T as IntoFuture>::Output {
        self.0.blocking_ask()
    }
}

#[derive(Debug, Clone)]
pub struct AnyLocalActorHandle(Arc<dyn AnyActorIdentity>);

impl AnyLocalActorHandle {
    /// Try to convert this handle into a shared handle.
    ///
    /// This fails if the channel or error type is not Send + Sync.
    pub fn into_shared(self) -> Result<AnyActorHandle, Self> {
        let rtti = self.0.rtti();
        let channel_rtti = rtti.channel();
        let error_rtti = rtti.error();

        if channel_rtti.is_send()
            && channel_rtti.is_sync()
            && error_rtti.is_send()
            && error_rtti.is_sync()
        {
            // SAFETY: We just checked that the channel and error type are Send and
            //         Sync, which is everything `erase_type` requires statically.
            let this = unsafe {
                core::mem::transmute::<_, Arc<dyn AnyActorIdentity + Send + Sync>>(self.0)
            };
            Ok(AnyActorHandle(this))
        } else {
            Err(self)
        }
    }
}

impl<A: Actor + ?Sized> From<TypedActorHandle<A>> for AnyLocalActorHandle
where
    A: 'static,
{
    fn from(value: TypedActorHandle<A>) -> Self {
        value.erase_type_local()
    }
}

impl TryFrom<AnyLocalActorHandle> for AnyActorHandle {
    type Error = AnyLocalActorHandle;

    fn try_from(value: AnyLocalActorHandle) -> Result<Self, Self::Error> {
        value.into_shared()
    }
}

#[derive(Debug, Clone)]
pub struct AnyActorHandle(Arc<dyn AnyActorIdentity + Send + Sync + 'static>);

impl AnyActorHandle {
    /// Convert this handle into a local handle.
    pub fn into_local(self) -> AnyLocalActorHandle {
        AnyLocalActorHandle(self.0)
    }
}

impl<A: Actor + ?Sized> From<TypedActorHandle<A>> for AnyActorHandle
where
    A: 'static,
    A::Channel: Send + Sync,
    A::Error: Send + Sync,
{
    fn from(value: TypedActorHandle<A>) -> Self {
        value.erase_type()
    }
}

impl From<AnyActorHandle> for AnyLocalActorHandle {
    fn from(value: AnyActorHandle) -> Self {
        value.into_local()
    }
}

/// Private trait that allows actor handles to automatically implement all actor handle
/// methods.
trait ActorHandleBase {
    type ActorIdentity: AnyActorIdentity + ?Sized;

    /// Retrieve the identity of the actor handle.
    fn identity(&self) -> &Arc<Self::ActorIdentity>;
}

impl<A: Actor + ?Sized> ActorHandleBase for TypedActorHandle<A> {
    type ActorIdentity = ActorIdentity<A>;

    fn identity(&self) -> &Arc<Self::ActorIdentity> {
        &self.0
    }
}

impl ActorHandleBase for AnyActorHandle {
    type ActorIdentity = dyn AnyActorIdentity + Send + Sync;

    fn identity(&self) -> &Arc<Self::ActorIdentity> {
        &self.0
    }
}

impl ActorHandleBase for AnyLocalActorHandle {
    type ActorIdentity = dyn AnyActorIdentity;

    fn identity(&self) -> &Arc<Self::ActorIdentity> {
        &self.0
    }
}

/// A handle to an actor.
///
/// This defines the operations that can generally be done on an actor handle.
#[allow(private_bounds)] // intentionally sealed via ActorHandleBase
pub trait ActorHandle: ActorHandleBase {
    /// Retrieve the RTTI of the actor this handle wraps.
    fn rtti(&self) -> &ActorRtti {
        self.identity().rtti()
    }

    /// Attempt to downcast this handle into a typed handle.
    ///
    /// This only succeeds if this handle actually points to an actor of
    /// type A.
    fn downcast<A: Actor>(&self) -> Option<TypedActorHandle<A>> {
        if self.rtti() != A::RTTI {
            return None;
        }

        let identity = self.identity().clone();

        // SAFETY: We just checked that the RTTI matches
        let identity = unsafe { Arc::from_raw(Arc::into_raw(identity).cast::<ActorIdentity<A>>()) };
        Some(TypedActorHandle(identity))
    }

    /// Bind the dispatcher for the given message type.
    fn bind_dispatcher(
        &self,
        message: &MessageRtti,
    ) -> Option<crate::actor::dispatch::ActorMessageDispatcher> {
        self.identity().bind(message)
    }

    /// Prepare sending an actor message with dynamic dispatch.
    ///
    /// This is a low level method and generally not recommended for general use.
    ///
    /// # Safety
    /// The caller must ensure that the actor message is dispatchable
    /// to the actor on this handle and that the message can be handled
    /// on the actor thread.
    unsafe fn prepare_send_dynamic_dispatched(
        &self,
        message: DispatchedActorMessage,
    ) -> Box<dyn DynActorChannelSendable<'_> + '_> {
        self.identity().dyn_channel().prepare_send(message)
    }

    /// Prepare sending a dynamic message to the actor,
    fn prepare_send_dynamic(
        &self,
        message: MessageEnvelope,
    ) -> Option<Box<dyn DynActorChannelSendable<'_> + '_>> {
        let dispatcher = self.bind_dispatcher(message.rtti())?;

        // SAFETY: The dispatcher was just bound by the actor's runtime binder,
        //         whose contract covers type coherence and the run loop's demand.
        let dispatched_message = unsafe {
            DispatchedActorMessage::new(dispatcher, DispatchedActorMessageContext::of(message))
        };

        // SAFETY: We just bound the dispatcher, so the message can be dispatched.
        Some(unsafe { self.prepare_send_dynamic_dispatched(dispatched_message) })
    }

    /// Retrieve the task handle of the actor task, if one was attached.
    fn task(&self) -> Option<&ActorTaskHandle> {
        self.identity().task()
    }
}

impl<T: ActorHandleBase> ActorHandle for T {}

// A smart pointer to a handle is itself a handle: this lets the derive's
// generated `…Handle` newtype (which `Deref`s to the `TypedActorHandle` it wraps)
// satisfy `&impl ActorHandle` at call sites like `watch`, without an explicit
// `&*`. No overlap with the inherent impls above - none of them implement
// `Deref`, and downstream crates cannot add such an impl for them.
impl<H> ActorHandleBase for H
where
    H: core::ops::Deref,
    H::Target: ActorHandleBase,
{
    type ActorIdentity = <H::Target as ActorHandleBase>::ActorIdentity;

    fn identity(&self) -> &Arc<Self::ActorIdentity> {
        (**self).identity()
    }
}

/// A handle that resolves to a concrete actor's [`TypedActorHandle`].
///
/// Implemented by `TypedActorHandle<A>` itself (the identity) and by the
/// derive's generated `…Handle` newtype. A protocol's erased handle is built
/// `From` any `DerivedHandle`, so `From`/`.into()` accepts either a bare typed
/// handle or a generated one. Not public API.
#[doc(hidden)]
pub trait DerivedHandle {
    /// The actor this handle talks to.
    type Actor: Actor + ?Sized;

    /// Recover the underlying typed handle.
    fn into_typed_handle(self) -> TypedActorHandle<Self::Actor>;
}

impl<A: Actor + ?Sized> DerivedHandle for TypedActorHandle<A> {
    type Actor = A;

    fn into_typed_handle(self) -> TypedActorHandle<A> {
        self
    }
}
