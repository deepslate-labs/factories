//! Actor protocols: type-erased handles that carry a *proof* a fixed set of
//! messages dispatch.
//!
//! A protocol sits between a [`TypedActorHandle`](crate::actor::handle::TypedActorHandle)
//! (knows the exact actor type, hence every message it handles) and an
//! [`AnyActorHandle`](crate::actor::handle::AnyActorHandle) (knows nothing about
//! the messages - every send may fail to bind). A protocol handle has erased the
//! actor type but retained the guarantee that its declared messages bind, so a
//! send through it never fails to bind.
//!
//! # The proof
//!
//! That guarantee is concrete: a protocol handle holds one
//! [`ActorMessageDispatcher`] per protocol message - the fn-pointer that builds
//! and runs the handler. There are two ways to obtain that table, and they
//! produce the *same* table (hence the same handle type):
//!
//! - **From a typed handle** (`A: MessageHandler<M>` for each message): read
//!   `<A as MessageHandler<M>>::DISPATCHER`. Compile-time, no registry.
//! - **From an erased handle**: ask the actor's runtime binder per message RTTI
//!   ([`ActorHandle::bind_dispatcher`]). If every message binds the table is
//!   complete; if any fails, construction fails.
//!
//! # Dispatch
//!
//! Every protocol send - whether from the static generic-bound surface (a typed
//! handle) or from the erased handle - routes through the object-safe
//! [`ErasedDispatch`] trait: one [`ErasedCall`] borrows `&dyn ErasedDispatch`
//! plus the cached dispatcher, so the generated methods all return the same
//! [`MessageCall<ErasedCall<…>>`](crate::actor::handle::MessageCall). The
//! `#[protocol]` attribute generates the handle, the methods, and the
//! construction `impl`s on top of these primitives.

use crate::actor::channel::DynActorChannelSendable;
use crate::actor::dispatch::DispatchedActorMessage;
use crate::actor::handle::ActorHandle;
use alloc::boxed::Box;

#[cfg(feature = "tokio-answer")]
use crate::actor::channel::{ActorChannelSendResult, PinnedActorChannelSendFuture};
#[cfg(feature = "tokio-answer")]
use crate::actor::dispatch::{ActorMessageDispatcher, DispatchedActorMessageContext};
#[cfg(feature = "tokio-answer")]
use crate::actor::handle::{AskError, Calling};
#[cfg(feature = "tokio-answer")]
use crate::message::Message;
#[cfg(feature = "tokio-answer")]
use crate::message::channel::{AnswerReceiver, AnswerSender, answer_channel};
#[cfg(feature = "tokio-answer")]
use crate::message::envelope::MessageEnvelope;
#[cfg(feature = "tokio-answer")]
use core::future::{Future, IntoFuture};
#[cfg(feature = "tokio-answer")]
use core::pin::Pin;
#[cfg(feature = "tokio-answer")]
use core::task::{Context, Poll};

/// Object-safe dynamic-dispatched send, satisfied by every actor handle.
///
/// This is the one capability a protocol call needs: turn a dispatcher-tagged
/// message into a sendable. It exists because [`ActorHandle`] is not object-safe
/// (its `downcast` is generic), yet a protocol call must hold its handle behind a
/// `&dyn`. The blanket impl forwards to
/// [`ActorHandle::prepare_send_dynamic_dispatched`], so `TypedActorHandle`,
/// `AnyActorHandle` and `AnyLocalActorHandle` all qualify and coerce to
/// `&dyn ErasedDispatch`.
pub trait ErasedDispatch {
    /// Prepare a sendable from an already-dispatched message.
    ///
    /// # Safety
    /// Same contract as [`ActorHandle::prepare_send_dynamic_dispatched`]: the
    /// message's dispatcher must dispatch to this handle's actor and be handleable
    /// on its thread.
    unsafe fn prepare_dispatched(
        &self,
        message: DispatchedActorMessage,
    ) -> Box<dyn DynActorChannelSendable<'_> + '_>;
}

impl<H: ActorHandle> ErasedDispatch for H {
    unsafe fn prepare_dispatched(
        &self,
        message: DispatchedActorMessage,
    ) -> Box<dyn DynActorChannelSendable<'_> + '_> {
        // SAFETY: forwarded verbatim - the caller upholds the dispatcher contract.
        unsafe { self.prepare_send_dynamic_dispatched(message) }
    }
}

/// Seal a message with its pre-bound dispatcher into a dispatched message.
///
/// # Safety
/// `dispatcher` must dispatch envelopes carrying `M` to the target actor.
#[cfg(feature = "tokio-answer")]
unsafe fn dispatched<M: Message>(
    dispatcher: ActorMessageDispatcher,
    message: M,
    answer: Option<AnswerSender<M>>,
) -> DispatchedActorMessage {
    let envelope = MessageEnvelope::new(message, answer);
    // SAFETY: per contract, `dispatcher` dispatches an `M` envelope to the actor.
    unsafe { DispatchedActorMessage::new(dispatcher, DispatchedActorMessageContext::of(envelope)) }
}

/// A prepared protocol call: the erased handle, the message's cached dispatcher,
/// and the message itself.
///
/// The single call type behind every protocol method (shared and `local` alike).
/// It implements [`Calling`], so the generated methods wrap it in a
/// [`MessageCall`](crate::actor::handle::MessageCall) and the surface is
/// identical to a typed handle's call: `.await`, `.tell()`, `.ask()`, and the
/// blocking variants. `Send`-ness of its futures falls out of the reply type, so
/// one type serves both shared and thread-local protocols.
#[cfg(feature = "tokio-answer")]
#[must_use = "a protocol call does nothing until it is awaited, told, or blocked on"]
pub struct ErasedCall<'a, M: Message> {
    inner: &'a dyn ErasedDispatch,
    dispatcher: ActorMessageDispatcher,
    message: M,
}

#[cfg(feature = "tokio-answer")]
impl<'a, M: Message> ErasedCall<'a, M> {
    /// Assemble a call from an erased handle and one of its cached dispatchers.
    ///
    /// # Safety
    /// `dispatcher` must be the dispatcher `inner`'s actor declares for `M` (the
    /// protocol handle guarantees this by construction).
    pub const unsafe fn new(
        inner: &'a dyn ErasedDispatch,
        dispatcher: ActorMessageDispatcher,
        message: M,
    ) -> Self {
        Self {
            inner,
            dispatcher,
            message,
        }
    }

    /// Build the sendable for this call, optionally carrying an answer sender.
    fn sendable(self, answer: Option<AnswerSender<M>>) -> Box<dyn DynActorChannelSendable<'a> + 'a> {
        // SAFETY: `dispatcher` dispatches `M` to `inner`'s actor (asserted at `new`).
        let message = unsafe { dispatched(self.dispatcher, self.message, answer) };
        // SAFETY: same contract - the dispatcher is bound for `inner`'s actor.
        unsafe { self.inner.prepare_dispatched(message) }
    }
}

#[cfg(feature = "tokio-answer")]
impl<'a, M: Message> IntoFuture for ErasedCall<'a, M> {
    type Output = Result<M::Answer, AskError>;
    type IntoFuture = ErasedAsk<'a, M>;

    fn into_future(self) -> Self::IntoFuture {
        let (tx, rx) = answer_channel::<M>();
        let send = self.sendable(Some(tx)).send();
        ErasedAsk {
            state: AskState::Sending {
                send,
                receiver: Some(rx),
            },
        }
    }
}

#[cfg(feature = "tokio-answer")]
impl<M: Message> crate::actor::handle::sealed::Sealed for ErasedCall<'_, M> {}

#[cfg(feature = "tokio-answer")]
impl<'a, M: Message> Calling for ErasedCall<'a, M> {
    fn ask(self) -> <Self as IntoFuture>::IntoFuture {
        self.into_future()
    }

    fn tell(self) -> impl Future<Output = ActorChannelSendResult> {
        self.sendable(None).send()
    }

    fn blocking_tell(self) -> ActorChannelSendResult {
        self.sendable(None).blocking_send()
    }

    fn blocking_ask(self) -> Result<M::Answer, AskError> {
        let (tx, rx) = answer_channel::<M>();
        self.sendable(Some(tx)).blocking_send()?;
        rx.blocking_recv().ok_or(AskError::NoReply)
    }
}

/// The ask future of an [`ErasedCall`]: drive the send, then await the reply.
///
/// A hand-written state machine rather than a boxed `async` block, so it carries
/// no extra allocation and stays `Send` exactly when the reply type is - which is
/// what lets a single [`ErasedCall`] type back both shared and `local` protocols.
/// Both fields are `Unpin`, so the future is too.
#[cfg(feature = "tokio-answer")]
#[must_use = "futures do nothing unless polled"]
pub struct ErasedAsk<'a, M: Message> {
    state: AskState<'a, M>,
}

#[cfg(feature = "tokio-answer")]
enum AskState<'a, M: Message> {
    Sending {
        send: PinnedActorChannelSendFuture<'a>,
        receiver: Option<AnswerReceiver<M>>,
    },
    Receiving(AnswerReceiver<M>),
    Done,
}

#[cfg(feature = "tokio-answer")]
impl<M: Message> Future for ErasedAsk<'_, M> {
    type Output = Result<M::Answer, AskError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Both `PinnedActorChannelSendFuture` (a `Pin<Box<_>>`) and
        // `AnswerReceiver` are `Unpin`, so `ErasedAsk` is too.
        let this = self.get_mut();
        loop {
            match &mut this.state {
                AskState::Sending { send, receiver } => match send.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(error)) => {
                        this.state = AskState::Done;
                        return Poll::Ready(Err(error.into()));
                    }
                    Poll::Ready(Ok(())) => {
                        let receiver = receiver.take().expect("send phase holds the receiver");
                        this.state = AskState::Receiving(receiver);
                    }
                },
                AskState::Receiving(receiver) => {
                    let reply = core::task::ready!(Pin::new(receiver).poll(cx));
                    this.state = AskState::Done;
                    return Poll::Ready(reply.ok_or(AskError::NoReply));
                }
                AskState::Done => panic!("ErasedAsk polled after completion"),
            }
        }
    }
}
