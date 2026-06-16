//! Tokio-`mpsc`-backed actor channel - the default mailbox.
//!
//! This is the standard single-lane actor channel. It is backed by
//! [`tokio::sync::mpsc`], whose `recv` future is cancellation-safe: it can be
//! held pending, raced in a `select`, and dropped without losing a message.
//! That property is mandatory for the [`ConcurrentRunLoop`] - which races the
//! mailbox `recv` against its in-flight work set - and is why this, rather than
//! a non-cancellation-safe channel, is the default.

use crate::actor::channel::{
    ActorChannel, ActorChannelSendError, ActorChannelSendResult, ActorChannelSendable,
};
use crate::actor::dispatch::DispatchedActorMessage;
use crate::spawn::{ActorMailbox, CreatableChannel};
use tokio::sync::mpsc;

/// The standard single-lane actor channel, backed by `tokio::sync::mpsc`.
///
/// Sufficient for the overwhelming majority of actors. Bounded by default
/// (depth 64), applying backpressure to senders when the mailbox fills; an
/// unbounded variant is available via [`TokioMpscChannelCapacity`].
#[derive(Debug, Clone)]
pub struct TokioMpscActorChannel {
    sender: TokioMpscSender,
}

#[derive(Debug, Clone)]
enum TokioMpscSender {
    Bounded(mpsc::Sender<DispatchedActorMessage>),
    Unbounded(mpsc::UnboundedSender<DispatchedActorMessage>),
}

impl TokioMpscActorChannel {
    /// Create a bounded channel of the given capacity together with its mailbox.
    pub fn new_bounded(capacity: usize) -> (Self, TokioMpscActorMailbox) {
        let (sender, receiver) = mpsc::channel(capacity);
        (
            Self {
                sender: TokioMpscSender::Bounded(sender),
            },
            TokioMpscActorMailbox {
                receiver: TokioMpscReceiver::Bounded(receiver),
            },
        )
    }

    /// Create an unbounded channel together with its mailbox.
    pub fn new_unbounded() -> (Self, TokioMpscActorMailbox) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                sender: TokioMpscSender::Unbounded(sender),
            },
            TokioMpscActorMailbox {
                receiver: TokioMpscReceiver::Unbounded(receiver),
            },
        )
    }

    /// Prepare sending a message through this channel.
    pub fn prepare_send(&self, message: DispatchedActorMessage) -> TokioMpscChannelSendable<'_> {
        TokioMpscChannelSendable {
            sender: &self.sender,
            message,
        }
    }
}

impl ActorChannel for TokioMpscActorChannel {
    fn prepare_send(&self, message: DispatchedActorMessage) -> impl ActorChannelSendable<'_> {
        self.prepare_send(message)
    }
}

impl CreatableChannel for TokioMpscActorChannel {
    type CreationOptions = TokioMpscChannelOptions;
    type Mailbox = TokioMpscActorMailbox;

    fn create(options: Self::CreationOptions) -> (Self, Self::Mailbox) {
        match options.capacity {
            TokioMpscChannelCapacity::Bounded(capacity) => Self::new_bounded(capacity),
            TokioMpscChannelCapacity::Unbounded => Self::new_unbounded(),
        }
    }
}

/// Options for building a [`TokioMpscActorChannel`].
#[derive(Debug, Clone, Default)]
pub struct TokioMpscChannelOptions {
    /// The mailbox capacity (bounded or unbounded).
    pub capacity: TokioMpscChannelCapacity,
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

/// A prepared, not-yet-sent message for a [`TokioMpscActorChannel`].
#[derive(Debug)]
pub struct TokioMpscChannelSendable<'a> {
    sender: &'a TokioMpscSender,
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
            TokioMpscSender::Bounded(sender) => sender
                .send(self.message)
                .await
                .map_err(|_| ActorChannelSendError::ActorDead),
            TokioMpscSender::Unbounded(sender) => sender
                .send(self.message)
                .map_err(|_| ActorChannelSendError::ActorDead),
        }
    }

    fn blocking_send(self) -> ActorChannelSendResult {
        if !self.message.envelope().is_sendable() {
            return Err(ActorChannelSendError::NotSendable);
        }

        match self.sender {
            TokioMpscSender::Bounded(sender) => sender
                .blocking_send(self.message)
                .map_err(|_| ActorChannelSendError::ActorDead),
            TokioMpscSender::Unbounded(sender) => sender
                .send(self.message)
                .map_err(|_| ActorChannelSendError::ActorDead),
        }
    }

    fn try_send(self) -> ActorChannelSendResult {
        if !self.message.envelope().is_sendable() {
            return Err(ActorChannelSendError::NotSendable);
        }

        match self.sender {
            TokioMpscSender::Bounded(sender) => {
                sender.try_send(self.message).map_err(|err| match err {
                    mpsc::error::TrySendError::Full(_) => ActorChannelSendError::MailboxFull,
                    mpsc::error::TrySendError::Closed(_) => ActorChannelSendError::ActorDead,
                })
            }
            TokioMpscSender::Unbounded(sender) => sender
                .send(self.message)
                .map_err(|_| ActorChannelSendError::ActorDead),
        }
    }
}

/// The mailbox half of a [`TokioMpscActorChannel`].
#[derive(Debug)]
pub struct TokioMpscActorMailbox {
    receiver: TokioMpscReceiver,
}

#[derive(Debug)]
enum TokioMpscReceiver {
    Bounded(mpsc::Receiver<DispatchedActorMessage>),
    Unbounded(mpsc::UnboundedReceiver<DispatchedActorMessage>),
}

impl ActorMailbox for TokioMpscActorMailbox {
    async fn receive(&mut self) -> Option<DispatchedActorMessage> {
        match &mut self.receiver {
            TokioMpscReceiver::Bounded(receiver) => receiver.recv().await,
            TokioMpscReceiver::Unbounded(receiver) => receiver.recv().await,
        }
    }
}
