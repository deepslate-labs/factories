use crate::message::Message;
use core::fmt::Formatter;

/// The reply sender used to reply to messages.
///
/// This is an enum so that if multiple sender
/// types are enabled, they can be dynamically selected at runtime.
///
/// However, when this is a single value enum, the Rust compiler will collapse
/// this into a concrete type.
#[derive(Debug)]
pub enum AnswerSender<T: Message> {
    #[cfg(feature = "tokio-answer")]
    Tokio(tokio::sync::oneshot::Sender<T::Answer>),

    #[cfg(not(any(feature = "tokio-answer")))]
    #[doc(hidden)]
    Never(#[allow(private_interfaces)] Empty<T>),
}

/// The reply receiver used to receive replies.
#[derive(Debug)]
pub enum AnswerReceiver<T: Message> {
    #[cfg(feature = "tokio-answer")]
    Tokio(tokio::sync::oneshot::Receiver<T::Answer>),

    #[cfg(not(any(feature = "tokio-answer")))]
    #[doc(hidden)]
    Never(#[allow(private_interfaces)] Empty<T>),
}

impl<T: Message> AnswerSender<T> {
    /// Send the answer to the asking party.
    ///
    /// Returns the answer back if the receiving side is no longer listening.
    pub fn send(self, answer: T::Answer) -> Result<(), T::Answer> {
        match self {
            #[cfg(feature = "tokio-answer")]
            AnswerSender::Tokio(tokio) => tokio.send(answer),
            #[cfg(not(any(feature = "tokio-answer")))]
            AnswerSender::Never(_) => {
                let _ = answer;
                unreachable!()
            }
        }
    }
}

impl<T: Message> AnswerReceiver<T> {
    /// Receive the answer.
    pub async fn recv(self) -> Option<T::Answer> {
        match self {
            #[cfg(feature = "tokio-answer")]
            AnswerReceiver::Tokio(tokio) => tokio.await.ok(),
            #[cfg(not(any(feature = "tokio-answer")))]
            AnswerReceiver::Never(_) => unreachable!(),
        }
    }

    /// Receive the answer blocking.
    pub fn blocking_recv(self) -> Option<T::Answer> {
        match self {
            #[cfg(feature = "tokio-answer")]
            AnswerReceiver::Tokio(tokio) => tokio.blocking_recv().ok(),
            #[cfg(not(any(feature = "tokio-answer")))]
            AnswerReceiver::Never(_) => unreachable!(),
        }
    }
}

/// Create an answer channel.
#[cfg(feature = "tokio-answer")]
pub fn answer_channel<T: Message>() -> (AnswerSender<T>, AnswerReceiver<T>) {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    (AnswerSender::Tokio(sender), AnswerReceiver::Tokio(receiver))
}

// This is here so that the compiler allows us to have a variant in the enum
// when no other variant is available. This variant should always be Send,
// but also needs to carry phantom data so that T gets used.
//
// The `uninhabited` field uses T, but is never inhabited, so it doesn't complain
// about unreachable code when the enum is used, because the only possible inhabitant
// is unit, which still leaves this as a ZST.
#[doc(hidden)]
#[allow(dead_code)]
union Empty<T: Message> {
    uninhabited: (
        core::convert::Infallible,
        core::marker::PhantomData<T::Answer>,
    ),
    unit: (),
}

impl<T: Message> core::fmt::Debug for Empty<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("Emtpy").finish()
    }
}

#[cfg(all(test, feature = "tokio-answer"))]
mod tests {
    use crate::declare_message;

    #[derive(Debug)]
    struct Ping;
    declare_message!(Ping, u32);

    #[test]
    fn answer_send_recv_roundtrip() {
        let (tx, rx) = super::answer_channel::<Ping>();
        tx.send(42).expect("receiver alive");
        assert_eq!(rx.blocking_recv(), Some(42));
    }

    #[test]
    fn answer_send_after_receiver_dropped_returns_answer() {
        let (tx, rx) = super::answer_channel::<Ping>();
        drop(rx);
        assert_eq!(tx.send(7), Err(7));
    }
}
