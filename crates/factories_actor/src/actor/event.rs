//! Additional event sources multiplexed onto an actor's run loop.
//!
//! An actor can drive its run loop from more than its mailbox - timers,
//! sockets, a long-running computation it owns - by providing an
//! [`EventDriver`]. Each turn the run loop hands the driver the mailbox and asks
//! it for the next message to dispatch; the driver decides *whether and how* to
//! poll the mailbox (race it against its own sources, prioritise one, or skip it
//! to drain a backlog). Whatever it yields is dispatched exactly like a mailbox
//! message, so the locking model is untouched.

use crate::actor::dispatch::{DispatchedActorMessage, DispatchedActorMessageContext};
use crate::actor::state::SharedActorState;
use crate::actor::{Actor, MessageHandler};
use crate::message::Message;
use crate::message::channel::AnswerSender;
use crate::message::envelope::MessageEnvelope;
use crate::runtime::lock::{ExclusiveLockStrategy, SharedLockStrategy};
use crate::spawn::ActorMailbox;
use core::future::Future;

/// An event source that drives an actor's run loop alongside (or instead of)
/// the mailbox of type `M`.
///
/// `M` is the run loop's message source; it is *not* required to be an
/// [`ActorMailbox`] - that is the standard interface, and a driver that polls it
/// asks for it in its own impl (`impl<A, M: ActorMailbox> EventDriver<A, M>`). A
/// pure source that never touches the mailbox can be `impl<A, M> EventDriver<A,
/// M>` and run under any loop, and a fully custom loop may use a completely
/// different `M`.
pub trait EventDriver<A: Actor + ?Sized, M> {
    /// Produce the next message to dispatch, or `None` to stop the loop.
    ///
    /// The driver owns the mailbox for the turn: it may receive from it, race
    /// that against its own futures, or ignore it entirely (e.g. when shared
    /// state flags that its backlog must drain first - the actor's lever against
    /// starvation). Touching the actor state is up to the driver, via `cx`; the
    /// framework imposes no locking policy.
    ///
    /// Cancel-safety is the driver's responsibility: any racing it does must
    /// keep in-flight progress in `self`, so a branch dropped when another wins
    /// can resume next turn.
    // The `Send` bound here mainly exists because RTN is not stable yet.
    fn next<'a>(
        &'a mut self,
        cx: EventContext<'a, A>,
        mailbox: &'a mut M,
    ) -> impl Future<Output = Option<DispatchedActorMessage>> + Send + 'a;
}

/// The default driver: pull straight from the mailbox.
#[derive(Debug, Default, Copy, Clone)]
pub struct DefaultMailboxDriver;

// Built from any actor, ignoring it - so `type EventDriver = DefaultMailboxDriver`
// satisfies the `From<&Self>` bound for every actor with no extra boilerplate.
impl<A: ?Sized> From<&A> for DefaultMailboxDriver {
    fn from(_actor: &A) -> Self {
        Self
    }
}

impl<A: Actor + ?Sized, M> EventDriver<A, M> for DefaultMailboxDriver
where
    M: ActorMailbox + Send,
{
    fn next<'a>(
        &'a mut self,
        _cx: EventContext<'a, A>,
        mailbox: &'a mut M,
    ) -> impl Future<Output = Option<DispatchedActorMessage>> + Send + 'a {
        mailbox.receive()
    }
}

/// Capabilities handed to an [`EventDriver`] for one turn.
///
/// Borrows the run loop's dispatch state, so it is valid only for the duration
/// of a single [`EventDriver::next`] call.
pub struct EventContext<'a, A: Actor + ?Sized> {
    lock_strategy: &'a A::LockStrategy,
    shared: &'a SharedActorState<A>,
}

impl<'a, A: Actor + ?Sized> EventContext<'a, A> {
    /// Build a context over the run loop's lock strategy and shared state.
    pub fn new(lock_strategy: &'a A::LockStrategy, shared: &'a SharedActorState<A>) -> Self {
        Self {
            lock_strategy,
            shared,
        }
    }

    /// Raw access to the actor's lock strategy.
    ///
    /// Always available, whatever the strategy is - a custom lock strategy need
    /// not implement any standard capability. The
    /// [`acquire_exclusive`](Self::acquire_exclusive) /
    /// [`acquire_shared`](Self::acquire_shared) helpers are conveniences for the
    /// strategies that do.
    pub fn lock_strategy(&self) -> &'a A::LockStrategy {
        self.lock_strategy
    }

    /// Acquire exclusive access to the actor state (convenience over
    /// [`lock_strategy`](Self::lock_strategy)).
    ///
    /// Cancel-safe: if the driver loses its turn's race, the pending acquire is
    /// dropped without taking the lock.
    pub fn acquire_exclusive(
        &self,
    ) -> impl Future<Output = <A::LockStrategy as ExclusiveLockStrategy<A>>::ExclusiveGuard<'a>>
    where
        A::LockStrategy: ExclusiveLockStrategy<A>,
    {
        self.lock_strategy.acquire_exclusive()
    }

    /// Acquire shared access to the actor state (convenience over
    /// [`lock_strategy`](Self::lock_strategy)).
    pub fn acquire_shared(
        &self,
    ) -> impl Future<Output = <A::LockStrategy as SharedLockStrategy<A>>::SharedGuard<'a>>
    where
        A::LockStrategy: SharedLockStrategy<A>,
    {
        self.lock_strategy.acquire_shared()
    }

    /// The actor's shared state (lifecycle, failure).
    pub fn state(&self) -> &'a SharedActorState<A> {
        self.shared
    }

    /// The actor's lock-free shared state extension
    /// ([`Actor::SharedStateExtension`]) -
    /// the coordination channel shared with message handlers.
    pub fn extension(&self) -> &'a A::SharedStateExtension {
        self.shared.extension()
    }

    /// Build a fire-and-forget self-dispatch for `message`.
    ///
    /// Return it from [`EventDriver::next`] to have the run loop dispatch it.
    pub fn message<M: Message>(&self, message: M) -> DispatchedActorMessage
    where
        A: MessageHandler<M>,
    {
        self.message_with(message, None)
    }

    /// Build a self-dispatch carrying an explicit answer sender.
    pub fn message_with<M: Message>(
        &self,
        message: M,
        answer_sender: Option<AnswerSender<M>>,
    ) -> DispatchedActorMessage
    where
        A: MessageHandler<M>,
    {
        let dispatcher = <A as MessageHandler<M>>::DISPATCHER.into_dispatcher();

        // SAFETY: the dispatcher is `A`'s declaration-checked static dispatcher
        //         for `M`, and the envelope is built from an `M` right here -
        //         the same anchor as `TypedActorHandle::prepare_send`.
        unsafe {
            DispatchedActorMessage::new(
                dispatcher,
                DispatchedActorMessageContext::of(MessageEnvelope::new(message, answer_sender)),
            )
        }
    }
}

/// An actor's extra event source: the logic half of an [`EventDriver`], with no
/// driver of its own to carry state.
pub trait ActorEventSource: Actor {
    /// Produce the next message to dispatch, or `None` to stop the loop.
    ///
    /// Owns the turn's mailbox exactly like [`EventDriver::next`]: receive from
    /// it, race it against its own futures, or skip it to drain a backlog. The
    /// same cancel-safety contract applies - keep in-flight progress reachable
    /// across `cx` / shared state so a dropped branch can resume next turn.
    // The `Send` bound here mainly exists because RTN is not stable yet.
    fn next_event(
        cx: EventContext<'_, Self>,
        mailbox: &mut (impl ActorMailbox + Send),
    ) -> impl Future<Output = Option<DispatchedActorMessage>> + Send;
}
