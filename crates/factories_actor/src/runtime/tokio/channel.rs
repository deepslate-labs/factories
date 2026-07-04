//! Tokio-`mpsc`-backed actor channel - the default mailbox.

use crate::actor::channel::{
    ActorChannel, ActorChannelSendError, ActorChannelSendResult, ActorChannelSendable,
};
use crate::actor::dispatch::DispatchedActorMessage;
use crate::runtime::routing::{ActorMessagePriorityRouter, NoPriorityRouter};
use crate::spawn::{ActorMailbox, CreatableChannel};
use crate::util::future::select_biased;
use tokio::sync::mpsc;

pub type TokioMpscActorChannel = TokioMpscMultiLineActorChannel<1, NoPriorityRouter>;
pub type TokioMpscActorMailbox = TokioMpscMultilaneActorMailbox<1>;

/// The standard single-lane actor channel, backed by `tokio::sync::mpsc`.
///
/// Sufficient for the overwhelming majority of actors. Bounded by default
/// (depth 64), applying backpressure to senders when the mailbox fills; an
/// unbounded variant is available via [`TokioMpscChannelCapacity`].
#[derive(Debug, Clone)]
pub struct TokioMpscMultiLineActorChannel<const SEND_LANES: usize, R: ActorMessagePriorityRouter> {
    sender: [TokioMpscSender; SEND_LANES],
    router: R,
}

/// A tokio mspc channel send and receiver pair.
#[derive(Debug)]
pub enum TokioMpscChannel {
    Bounded(
        mpsc::Sender<DispatchedActorMessage>,
        mpsc::Receiver<DispatchedActorMessage>,
    ),
    Unbounded(
        mpsc::UnboundedSender<DispatchedActorMessage>,
        mpsc::UnboundedReceiver<DispatchedActorMessage>,
    ),
}

impl TokioMpscChannel {
    /// Create a new mpsc channel with the given capacity.
    pub fn new_bounded(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self::Bounded(sender, receiver)
    }

    /// Create a new unbounded mpsc channel.
    pub fn new_unbounded() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self::Unbounded(sender, receiver)
    }

    /// Split the channel into a sender and receiver.
    pub fn into_parts(self) -> (TokioMpscSender, TokioMpscReceiver) {
        match self {
            Self::Bounded(s, r) => (TokioMpscSender::Bounded(s), TokioMpscReceiver::Bounded(r)),
            Self::Unbounded(s, r) => (
                TokioMpscSender::Unbounded(s),
                TokioMpscReceiver::Unbounded(r),
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub enum TokioMpscSender {
    Bounded(mpsc::Sender<DispatchedActorMessage>),
    Unbounded(mpsc::UnboundedSender<DispatchedActorMessage>),
}

impl From<mpsc::Sender<DispatchedActorMessage>> for TokioMpscSender {
    fn from(sender: mpsc::Sender<DispatchedActorMessage>) -> Self {
        Self::Bounded(sender)
    }
}

impl From<mpsc::UnboundedSender<DispatchedActorMessage>> for TokioMpscSender {
    fn from(sender: mpsc::UnboundedSender<DispatchedActorMessage>) -> Self {
        Self::Unbounded(sender)
    }
}

impl<R: ActorMessagePriorityRouter> TokioMpscMultiLineActorChannel<1, R>
where
    R: Default,
{
    /// Create a new bounded mpsc actor channel with the given capacity.
    pub fn new_bounded(capacity: usize) -> (Self, TokioMpscMultilaneActorMailbox<1>) {
        let (sender, receiver) = mpsc::channel(capacity);

        let mailbox = TokioMpscMultilaneActorMailbox {
            receiver: [receiver.into()],
        };

        (Self::new([sender.into()], R::default()), mailbox)
    }

    /// Create a new unbounded mpsc actor channel.
    pub fn new_unbounded() -> (Self, TokioMpscMultilaneActorMailbox<1>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mailbox = TokioMpscMultilaneActorMailbox {
            receiver: [receiver.into()],
        };
        (Self::new([sender.into()], R::default()), mailbox)
    }
}

impl<const SEND_LANES: usize, R: ActorMessagePriorityRouter>
    TokioMpscMultiLineActorChannel<SEND_LANES, R>
{
    /// Create a new mpsc multilane actor channel from its components.
    pub const fn new(sender: [TokioMpscSender; SEND_LANES], router: R) -> Self {
        Self { sender, router }
    }

    /// Build a new mpsc multilane actor channel from a function that builds a
    /// channel for each lane.
    pub fn build_with(
        mut lane_builder: impl FnMut(&R, usize) -> TokioMpscChannel,
        router: R,
    ) -> (Self, TokioMpscMultilaneActorMailbox<SEND_LANES>) {
        let (senders, receivers) = crate::util::array::generate_pair_arrays(|lane| {
            lane_builder(&router, lane).into_parts()
        });
        (
            Self {
                sender: senders,
                router,
            },
            TokioMpscMultilaneActorMailbox {
                receiver: receivers,
            },
        )
    }

    /// Create a new mpsc multilane actor channel with the given capacity for each lane.
    pub fn new_all_bounded(
        capacity: usize,
        router: R,
    ) -> (Self, TokioMpscMultilaneActorMailbox<SEND_LANES>) {
        Self::build_with(|_, _| TokioMpscChannel::new_bounded(capacity), router)
    }

    /// Create a new unbounded mpsc multilane actor channel.
    pub fn new_all_unbounded(router: R) -> (Self, TokioMpscMultilaneActorMailbox<SEND_LANES>) {
        Self::build_with(|_, _| TokioMpscChannel::new_unbounded(), router)
    }

    /// Prepare sending a message through this channel.
    pub fn prepare_send(&self, message: DispatchedActorMessage) -> TokioMpscChannelSendable<'_> {
        let lane = self.router.priority(&message);

        TokioMpscChannelSendable {
            sender: self.sender.get(lane),
            message,
        }
    }
}

impl<const SEND_LANES: usize, R: ActorMessagePriorityRouter> ActorChannel
    for TokioMpscMultiLineActorChannel<SEND_LANES, R>
{
    fn prepare_send(&self, message: DispatchedActorMessage) -> impl ActorChannelSendable<'_> {
        self.prepare_send(message)
    }
}

impl<const SEND_LANES: usize, R: ActorMessagePriorityRouter> CreatableChannel
    for TokioMpscMultiLineActorChannel<SEND_LANES, R>
{
    type CreationOptions = TokioMpscChannelOptions<R>;
    type Mailbox = TokioMpscMultilaneActorMailbox<SEND_LANES>;

    fn create(options: Self::CreationOptions) -> (Self, Self::Mailbox) {
        match options.capacity {
            TokioMpscChannelCapacity::Bounded(capacity) => {
                Self::new_all_bounded(capacity, options.router)
            }
            TokioMpscChannelCapacity::Unbounded => Self::new_all_unbounded(options.router),
        }
    }
}

/// Options for building a [`TokioMpscMultiLineActorChannel`].
#[derive(Debug, Clone, Default)]
pub struct TokioMpscChannelOptions<R: ActorMessagePriorityRouter = NoPriorityRouter> {
    /// The mailbox capacity (bounded or unbounded).
    pub capacity: TokioMpscChannelCapacity,

    /// The priority router to use.
    pub router: R,
}

/// How large the actor mailbox is.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum TokioMpscChannelCapacity {
    /// The channel has unbounded capacity (senders never block).
    Unbounded,

    /// The channel is bounded to the given capacity (senders apply backpressure).
    Bounded(usize),
}

impl Default for TokioMpscChannelCapacity {
    fn default() -> Self {
        Self::Bounded(64)
    }
}

/// A prepared, not-yet-sent message for a [`TokioMpscMultiLineActorChannel`].
#[derive(Debug)]
pub struct TokioMpscChannelSendable<'a> {
    sender: Option<&'a TokioMpscSender>,
    message: DispatchedActorMessage,
}

impl<'a> ActorChannelSendable<'a> for TokioMpscChannelSendable<'a> {
    async fn send(self) -> ActorChannelSendResult {
        // This channel transports messages across threads, so per the
        // `ActorChannel` sendability contract the envelope must be sendable.
        if !self.message.envelope().is_sendable() {
            return Err(ActorChannelSendError::NotSendable);
        }

        match self.sender {
            Some(TokioMpscSender::Bounded(sender)) => sender
                .send(self.message)
                .await
                .map_err(|_| ActorChannelSendError::ActorDead),
            Some(TokioMpscSender::Unbounded(sender)) => sender
                .send(self.message)
                .map_err(|_| ActorChannelSendError::ActorDead),
            None => Err(ActorChannelSendError::Unroutable),
        }
    }

    fn blocking_send(self) -> ActorChannelSendResult {
        if !self.message.envelope().is_sendable() {
            return Err(ActorChannelSendError::NotSendable);
        }

        match self.sender {
            Some(TokioMpscSender::Bounded(sender)) => sender
                .blocking_send(self.message)
                .map_err(|_| ActorChannelSendError::ActorDead),
            Some(TokioMpscSender::Unbounded(sender)) => sender
                .send(self.message)
                .map_err(|_| ActorChannelSendError::ActorDead),
            None => Err(ActorChannelSendError::Unroutable),
        }
    }

    fn try_send(self) -> ActorChannelSendResult {
        if !self.message.envelope().is_sendable() {
            return Err(ActorChannelSendError::NotSendable);
        }

        match self.sender {
            Some(TokioMpscSender::Bounded(sender)) => {
                sender.try_send(self.message).map_err(|err| match err {
                    mpsc::error::TrySendError::Full(_) => ActorChannelSendError::MailboxFull,
                    mpsc::error::TrySendError::Closed(_) => ActorChannelSendError::ActorDead,
                })
            }
            Some(TokioMpscSender::Unbounded(sender)) => sender
                .send(self.message)
                .map_err(|_| ActorChannelSendError::ActorDead),
            None => Err(ActorChannelSendError::Unroutable),
        }
    }
}

/// The mailbox half of a [`TokioMpscMultiLineActorChannel`].
#[derive(Debug)]
pub struct TokioMpscMultilaneActorMailbox<const SEND_LANES: usize> {
    receiver: [TokioMpscReceiver; SEND_LANES],
}

#[derive(Debug)]
pub enum TokioMpscReceiver {
    Bounded(mpsc::Receiver<DispatchedActorMessage>),
    Unbounded(mpsc::UnboundedReceiver<DispatchedActorMessage>),
}

impl TokioMpscReceiver {
    async fn receive(&mut self) -> Option<DispatchedActorMessage> {
        match self {
            Self::Bounded(receiver) => receiver.recv().await,
            Self::Unbounded(receiver) => receiver.recv().await,
        }
    }
}

impl From<mpsc::Receiver<DispatchedActorMessage>> for TokioMpscReceiver {
    fn from(receiver: mpsc::Receiver<DispatchedActorMessage>) -> Self {
        Self::Bounded(receiver)
    }
}

impl From<mpsc::UnboundedReceiver<DispatchedActorMessage>> for TokioMpscReceiver {
    fn from(receiver: mpsc::UnboundedReceiver<DispatchedActorMessage>) -> Self {
        Self::Unbounded(receiver)
    }
}

impl<const SEND_LANES: usize> ActorMailbox for TokioMpscMultilaneActorMailbox<SEND_LANES> {
    async fn receive(&mut self) -> Option<DispatchedActorMessage> {
        select_biased(self.receiver.each_mut().map(|receiver| receiver.receive())).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::dispatch::{
        ActorMessageDispatcher, DispatchContextPtr, DispatchedActorMessageContext,
    };
    use crate::declare_message;
    use crate::message::Message;
    use crate::message::envelope::MessageEnvelope;
    use core::future::poll_fn;
    use core::task::Poll;

    struct Control;
    declare_message!(Control, ());

    struct Data;
    declare_message!(Data, ());

    unsafe fn noop_dispatch(_: DispatchContextPtr<'_>, _: DispatchedActorMessageContext, _: *mut ()) {
        unreachable!("the lane channel never invokes the dispatcher");
    }

    fn dispatched<M: Message>(message: M) -> DispatchedActorMessage {
        // SAFETY: the dispatcher is never invoked by the channel under test; the
        //         envelope carries an `M` built right here.
        unsafe {
            DispatchedActorMessage::new(
                ActorMessageDispatcher::new(noop_dispatch),
                DispatchedActorMessageContext::of(MessageEnvelope::new(message, None)),
            )
        }
    }

    #[derive(Default, Clone, Copy, Debug)]
    struct ControlFirstRouter;

    impl ActorMessagePriorityRouter for ControlFirstRouter {
        fn priority(&self, dispatched: &DispatchedActorMessage) -> usize {
            if dispatched.envelope().rtti() == Control::RTTI {
                0
            } else {
                1
            }
        }
    }

    #[tokio::test]
    async fn priority_lane_drains_before_lower_lane() {
        let (channel, mut mailbox) =
            TokioMpscMultiLineActorChannel::<2, _>::new_all_bounded(8, ControlFirstRouter);

        // Data (lane 1) is enqueued first.
        channel.prepare_send(dispatched(Data)).send().await.unwrap();

        // Control (lane 0) is enqueued second, but must come out first.
        channel
            .prepare_send(dispatched(Control))
            .send()
            .await
            .unwrap();

        let first = mailbox.receive().await.expect("a message");
        assert_eq!(first.envelope().rtti(), Control::RTTI, "lane 0 must win");

        let second = mailbox.receive().await.expect("a message");
        assert_eq!(second.envelope().rtti(), Data::RTTI);
    }

    #[tokio::test]
    async fn single_lane_is_fifo() {
        let (channel, mut mailbox) = TokioMpscActorChannel::new_bounded(8);

        channel.prepare_send(dispatched(Data)).send().await.unwrap();
        channel
            .prepare_send(dispatched(Control))
            .send()
            .await
            .unwrap();

        assert_eq!(
            mailbox.receive().await.unwrap().envelope().rtti(),
            Data::RTTI
        );
        assert_eq!(
            mailbox.receive().await.unwrap().envelope().rtti(),
            Control::RTTI
        );
    }

    #[tokio::test]
    async fn per_lane_backpressure() {
        let (channel, _mailbox) =
            TokioMpscMultiLineActorChannel::<2, _>::new_all_bounded(1, ControlFirstRouter);

        channel.prepare_send(dispatched(Data)).try_send().unwrap();
        match channel.prepare_send(dispatched(Data)).try_send() {
            Err(ActorChannelSendError::MailboxFull) => {}
            other => panic!("expected MailboxFull, got {other:?}"),
        }

        channel.prepare_send(dispatched(Control)).try_send().unwrap();
    }

    #[tokio::test]
    async fn out_of_range_lane_is_unroutable() {
        #[derive(Default)]
        struct AlwaysLaneFive;
        impl ActorMessagePriorityRouter for AlwaysLaneFive {
            fn priority(&self, _: &DispatchedActorMessage) -> usize {
                5
            }
        }

        let (channel, _mailbox) =
            TokioMpscMultiLineActorChannel::<2, _>::new_all_bounded(1, AlwaysLaneFive);

        match channel.prepare_send(dispatched(Data)).try_send() {
            Err(ActorChannelSendError::Unroutable) => {}
            other => panic!("expected Unroutable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn receive_is_cancel_safe() {
        let (channel, mut mailbox) =
            TokioMpscMultiLineActorChannel::<2, _>::new_all_bounded(8, ControlFirstRouter);

        {
            let mut fut = core::pin::pin!(mailbox.receive());
            let polled = poll_fn(|cx| Poll::Ready(fut.as_mut().poll(cx))).await;
            assert!(matches!(polled, Poll::Pending));
            drop(fut);
        }

        channel
            .prepare_send(dispatched(Control))
            .send()
            .await
            .unwrap();
        let got = mailbox.receive().await.expect("a message");
        assert_eq!(got.envelope().rtti(), Control::RTTI);
    }
}
