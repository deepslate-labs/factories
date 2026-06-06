use crate::actor::channel::{
    ActorChannel, ActorChannelSendError, ActorChannelSendResult, ActorChannelSendable,
};
use crate::actor::dispatch::DispatchedActorMessage;
use crate::runtime::routing::{ActorMessagePriorityRouter, NoPriorityRouter};
use crate::spawn::{ActorMailbox, CreatableChannel};
use core::pin::Pin;
use core::task::{Context, Poll};

type DispatchedActorMessageSender = kanal::Sender<DispatchedActorMessage>;

/// The most simple standard actor channel.
///
/// Probably sufficient for 99% of use cases.
pub type SimpleKanalActorChannel = KanalActorChannel<1, NoPriorityRouter>;

/// `kanal` backend multi-priority actor channel implementation.
#[derive(Debug, Clone)]
pub struct KanalActorChannel<const SEND_LANES: usize, R: ActorMessagePriorityRouter> {
    senders: [DispatchedActorMessageSender; SEND_LANES],
    router: R,
}

impl KanalActorChannel<1, NoPriorityRouter> {
    /// Create the channel with a bounded capacity for exactly 1 lane.
    pub fn new_bounded(capacity: usize) -> (kanal::Receiver<DispatchedActorMessage>, Self) {
        Self::new_bounded_single_lane(NoPriorityRouter, capacity)
    }

    /// Create the channel with an unbounded capacity for exactly 1 lane.
    pub fn new_unbounded() -> (kanal::Receiver<DispatchedActorMessage>, Self) {
        Self::new_unbounded_single_lane(NoPriorityRouter)
    }
}

impl<R: ActorMessagePriorityRouter> KanalActorChannel<1, R> {
    /// Create the channel with a bounded capacity for exactly 1 lane.
    pub fn new_bounded_single_lane(
        router: R,
        capacity: usize,
    ) -> (kanal::Receiver<DispatchedActorMessage>, Self) {
        let (sender, receiver) = kanal::bounded(capacity);
        let channel = Self::new_single_lane_with(router, |_| sender);
        (receiver, channel)
    }

    /// Create the channel with an unbounded capacity for exactly 1 lane.
    pub fn new_unbounded_single_lane(router: R) -> (kanal::Receiver<DispatchedActorMessage>, Self) {
        let (sender, receiver) = kanal::unbounded();
        let channel = Self::new_single_lane_with(router, |_| sender);
        (receiver, channel)
    }

    /// Create the channel with the given sender factory for exactly 1 lane.
    pub fn new_single_lane_with<F: FnOnce(&R) -> DispatchedActorMessageSender>(
        router: R,
        sender_factory: F,
    ) -> Self {
        let mut sender_factory = Some(sender_factory);

        Self::new_with(router, |router, _| {
            // SAFETY: There is only 1 lane, so this closure is called exactly once
            (unsafe { sender_factory.take().unwrap_unchecked() })(router)
        })
    }
}

impl<const SEND_LANES: usize, R: ActorMessagePriorityRouter> KanalActorChannel<SEND_LANES, R> {
    /// Create the channel with the given sender factory.
    ///
    /// The sender factory will be called exactly `SEND_LANES` times, with the router and
    /// the lane index as arguments.
    pub fn new_with<F: FnMut(&R, usize) -> DispatchedActorMessageSender>(
        router: R,
        mut sender_factory: F,
    ) -> Self {
        let senders = core::array::from_fn(|i| sender_factory(&router, i));
        Self { senders, router }
    }

    /// Retrieve the sender at the given index.
    ///
    /// Compile time checks that the index is valid.
    pub const fn sender<const N: usize>(&self) -> KanalActorChannelSender<'_> {
        const { assert!(N < SEND_LANES, "Lane index out of bounds") };
        KanalActorChannelSender::new(&self.senders[N])
    }

    /// Retrieve the sender at the given index.
    pub fn maybe_sender(&self, index: usize) -> Option<KanalActorChannelSender<'_>> {
        self.senders.get(index).map(KanalActorChannelSender::new)
    }

    /// Prepare sending a message through this channel.
    pub fn prepare_send(&self, message: DispatchedActorMessage) -> KanalChannelSendable<'_> {
        let lane = self.router.priority(&message);
        KanalChannelSendable {
            message,
            sender: self.maybe_sender(lane),
        }
    }
}

impl<const SEND_LANES: usize, R: ActorMessagePriorityRouter> ActorChannel
    for KanalActorChannel<SEND_LANES, R>
{
    fn prepare_send(&self, message: DispatchedActorMessage) -> impl ActorChannelSendable<'_> {
        self.prepare_send(message)
    }
}

impl<const SEND_LANES: usize, R: ActorMessagePriorityRouter> CreatableChannel
    for KanalActorChannel<SEND_LANES, R>
{
    type CreationOptions = KanalActorChannelOptions<SEND_LANES, R>;
    type Mailbox = KanalActorMailbox<SEND_LANES>;

    fn create(options: Self::CreationOptions) -> (Self, Self::Mailbox) {
        let mut receivers = [const { None }; SEND_LANES];

        let channel = Self::new_with(options.router, |_, i| {
            let (sender, receiver) = match options.capacity[i] {
                KanalActorChannelCapacity::Unbounded => kanal::unbounded(),
                KanalActorChannelCapacity::Bounded(capacity) => kanal::bounded(capacity),
            };

            receivers[i] = Some(receiver);
            sender
        });

        (
            channel,
            KanalActorMailbox {
                receivers: receivers.map(Option::unwrap),
            },
        )
    }
}

/// The options used for building a kanal actor channel.
#[derive(Debug, Clone)]
pub struct KanalActorChannelOptions<const SEND_LANES: usize, R: ActorMessagePriorityRouter> {
    pub router: R,
    pub capacity: [KanalActorChannelCapacity; SEND_LANES],
}

impl<const SEND_LANES: usize, R: ActorMessagePriorityRouter> Default
    for KanalActorChannelOptions<SEND_LANES, R>
where
    R: Default,
{
    fn default() -> Self {
        Self {
            router: Default::default(),
            capacity: [Default::default(); SEND_LANES],
        }
    }
}

/// Setting for configuring how large an actor channel is.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum KanalActorChannelCapacity {
    /// The channel has unbounded capacity
    Unbounded,

    /// The channel has the bounded specified capacity
    Bounded(usize),
}

impl Default for KanalActorChannelCapacity {
    fn default() -> Self {
        Self::Bounded(64)
    }
}

/// A sender for a `KanalActorChannel`.
#[derive(Debug, Copy, Clone)]
pub struct KanalActorChannelSender<'a> {
    sender: &'a DispatchedActorMessageSender,
}

impl<'a> KanalActorChannelSender<'a> {
    const fn new(sender: &'a DispatchedActorMessageSender) -> Self {
        Self { sender }
    }

    /// Prepare sending a message through this sender.
    pub fn prepare_send(self, message: DispatchedActorMessage) -> KanalChannelSendable<'a> {
        KanalChannelSendable {
            message,
            sender: Some(self),
        }
    }

    /// Send a message asynchronously through this sender.
    pub async fn send(&self, message: DispatchedActorMessage) -> ActorChannelSendResult {
        // This channel transports messages across threads, so per the
        // `ActorChannel` sendability contract the envelope must be sendable.
        if !message.envelope().is_sendable() {
            return Err(ActorChannelSendError::NotSendable);
        }

        self.sender
            .as_async()
            .send(message)
            .await
            .map_err(Self::handle_send_error)
    }

    /// Send a message blocking through this sender.
    pub fn blocking_send(&self, message: DispatchedActorMessage) -> ActorChannelSendResult {
        // See `send` for why this check exists.
        if !message.envelope().is_sendable() {
            return Err(ActorChannelSendError::NotSendable);
        }

        self.sender.send(message).map_err(Self::handle_send_error)
    }

    fn handle_send_error(err: kanal::SendError) -> ActorChannelSendError {
        match err {
            kanal::SendError::Closed => ActorChannelSendError::ActorDead,
            kanal::SendError::ReceiveClosed => ActorChannelSendError::ActorDead,
        }
    }
}

impl<'a> ActorChannel for KanalActorChannelSender<'a> {
    fn prepare_send(&self, message: DispatchedActorMessage) -> impl ActorChannelSendable<'_> {
        (*self).prepare_send(message)
    }
}

#[derive(Debug)]
pub struct KanalChannelSendable<'a> {
    message: DispatchedActorMessage,
    sender: Option<KanalActorChannelSender<'a>>,
}

impl<'a> ActorChannelSendable<'a> for KanalChannelSendable<'a> {
    async fn send(self) -> ActorChannelSendResult {
        let Some(sender) = self.sender else {
            return Err(ActorChannelSendError::Unroutable);
        };

        sender.send(self.message).await
    }

    fn blocking_send(self) -> ActorChannelSendResult {
        let Some(sender) = self.sender else {
            return Err(ActorChannelSendError::Unroutable);
        };

        sender.blocking_send(self.message)
    }
}

#[derive(Debug)]
pub struct KanalActorMailbox<const SEND_LANES: usize> {
    receivers: [kanal::Receiver<DispatchedActorMessage>; SEND_LANES],
}

impl<const SEND_LANES: usize> ActorMailbox for KanalActorMailbox<SEND_LANES> {
    async fn receive(&mut self) -> Option<DispatchedActorMessage> {
        // Reverse the selected array so that the highest priority lane gets polled first
        let biased_select = BiasedFutureSelect::<SEND_LANES, _>::new(core::array::from_fn(|i| {
            let receiver = &self.receivers[SEND_LANES - 1 - i];
            receiver.as_async().recv()
        }));

        biased_select.await.ok()
    }
}

#[pin_project::pin_project]
struct BiasedFutureSelect<const COUNT: usize, F: Future> {
    #[pin]
    futures: [F; COUNT],
}

impl<const COUNT: usize, F: Future> BiasedFutureSelect<COUNT, F> {
    fn new(futures: [F; COUNT]) -> Self {
        Self { futures }
    }
}

impl<const COUNT: usize, F: Future> Future for BiasedFutureSelect<COUNT, F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        for future in pin_iter(this.futures) {
            if let Poll::Ready(output) = future.poll(cx) {
                return Poll::Ready(output);
            }
        }

        Poll::Pending
    }
}

fn pin_iter<T>(slice: Pin<&mut [T]>) -> impl Iterator<Item = Pin<&mut T>> {
    // SAFETY: We structurally project into the iterator and re-pin its elements
    let unwrapped_slice = unsafe { slice.get_unchecked_mut() };

    unwrapped_slice.iter_mut().map(|item| {
        // SAFETY: We are re-pinning the same element, which is safe as long as we don't move it
        unsafe { Pin::new_unchecked(item) }
    })
}
