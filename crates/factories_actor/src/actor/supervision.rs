//! Linkage: watching an actor's termination.
//!
//! `A.watch(B, tag)` registers an opt-in, unidirectional, non-owning
//! subscription on **B**: when B terminates, a [`Terminated`] signal is pushed
//! into A's mailbox as an ordinary message. Both sides are held weakly, so a
//! watch keeps neither actor alive.

use crate::actor::dispatch::{
    ActorMessageDispatcher, DispatchedActorMessage, DispatchedActorMessageContext,
};
use crate::actor::identity::AnyActorIdentity;
use crate::actor::lifecycle::TerminationKind;
use crate::actor::rtti::ActorRtti;
use crate::message::envelope::MessageEnvelope;
use alloc::sync::Weak;
use core::fmt::Display;
use core::num::NonZeroUsize;
use core::sync::atomic::AtomicUsize;

/// Source of process-unique [`ActorId`]s. Monotonic, so an id is never reused;
/// starts at 1 so 0 is free to serve as the `Option<ActorId>` niche.
static NEXT_ACTOR_ID: AtomicUsize = AtomicUsize::new(1);

/// A process-unique identifier for an actor.
///
/// Assigned once from a monotonic counter when the actor's shared state is
/// created, so it is never reused (unlike an address) and is stable for the life
/// of the actor's identity - including across a future restart, since a restart
/// replaces the instance but keeps the identity.
#[repr(transparent)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct ActorId(NonZeroUsize);

impl ActorId {
    pub(crate) fn new() -> Self {
        let raw = NEXT_ACTOR_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        // The counter starts at 1 and only increments, so `raw` is never 0
        // (barring a full `usize` wrap - 2^64 spawns, not reachable).
        Self(NonZeroUsize::new(raw).expect("ActorId counter starts at 1 and is monotonic"))
    }

    /// The raw counter value backing this id (never 0).
    pub const fn as_usize(self) -> usize {
        self.0.get()
    }

    /// The raw non-zero counter value backing this id.
    pub const fn as_non_zero_usize(self) -> NonZeroUsize {
        self.0
    }
}

impl From<ActorId> for usize {
    fn from(id: ActorId) -> Self {
        id.as_usize()
    }
}

impl From<ActorId> for NonZeroUsize {
    fn from(id: ActorId) -> Self {
        id.0
    }
}

impl Display for ActorId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Actor#{}", self.as_usize())
    }
}

/// The signal pushed into a watcher's mailbox when a watched actor terminates.
///
/// Delivered as an ordinary message; handle it with a
/// [`MessageHandler<Terminated>`](crate::actor::MessageHandler). It carries the
/// watcher-chosen `tag` (its correlation key) plus the watched actor's identity
/// and error-free [`TerminationKind`].
#[derive(Debug)]
pub struct Terminated {
    watched: ActorId,
    rtti: &'static ActorRtti,
    kind: TerminationKind,
    tag: u64,
}

crate::declare_message!(Terminated, ());

impl Terminated {
    pub(crate) fn new(
        watched: ActorId,
        rtti: &'static ActorRtti,
        kind: TerminationKind,
        tag: u64,
    ) -> Self {
        Self {
            watched,
            rtti,
            kind,
            tag,
        }
    }

    /// The watcher-assigned correlation key from the `watch` call.
    pub fn tag(&self) -> u64 {
        self.tag
    }

    /// How the watched actor terminated.
    pub fn kind(&self) -> TerminationKind {
        self.kind
    }

    /// The identity of the watched actor.
    pub fn watched(&self) -> ActorId {
        self.watched
    }

    /// The RTTI of the watched actor's type.
    pub fn rtti(&self) -> &'static ActorRtti {
        self.rtti
    }
}

/// A registered termination subscription, stored on the *watched* actor.
///
/// Holds a weak reference to the watcher (so it never keeps the watcher alive)
/// and the watcher's pre-bound [`Terminated`] dispatcher (so delivery needs no
/// registry lookup and `watch` fails to compile unless the watcher handles
/// `Terminated`).
pub(crate) struct Subscription {
    watcher: Weak<dyn AnyActorIdentity + Send + Sync>,
    watcher_id: ActorId,
    dispatcher: ActorMessageDispatcher,
    tag: u64,
}

impl Subscription {
    pub(crate) fn new(
        watcher: Weak<dyn AnyActorIdentity + Send + Sync>,
        watcher_id: ActorId,
        dispatcher: ActorMessageDispatcher,
        tag: u64,
    ) -> Self {
        Self {
            watcher,
            watcher_id,
            dispatcher,
            tag,
        }
    }

    /// The id of the watcher this subscription notifies (used by `unwatch`).
    pub(crate) fn watcher_id(&self) -> ActorId {
        self.watcher_id
    }

    /// Build the dispatched [`Terminated`] for this subscriber, or `None` if the
    /// watcher is already gone.
    fn prepare(
        &self,
        watched: ActorId,
        rtti: &'static ActorRtti,
        kind: TerminationKind,
    ) -> Option<(alloc::sync::Arc<dyn AnyActorIdentity + Send + Sync>, DispatchedActorMessage)> {
        let watcher = self.watcher.upgrade()?;

        let signal = Terminated::new(watched, rtti, kind, self.tag);
        let envelope = MessageEnvelope::new(signal, None);

        // SAFETY: `dispatcher` was bound at `watch` time as the watcher's own
        //         `Terminated` dispatcher, so it dispatches a `Terminated`
        //         envelope onto the watcher's run loop.
        let message = unsafe {
            DispatchedActorMessage::new(self.dispatcher, DispatchedActorMessageContext::of(envelope))
        };

        Some((watcher, message))
    }

    /// Deliver a [`Terminated`] to this subscriber, awaiting mailbox room so the
    /// signal is not dropped under back-pressure. Returns once delivered (or the
    /// watcher is gone).
    pub(crate) async fn deliver(
        &self,
        watched: ActorId,
        rtti: &'static ActorRtti,
        kind: TerminationKind,
    ) {
        let Some((watcher, message)) = self.prepare(watched, rtti, kind) else {
            return;
        };

        crate::obs::terminated_delivered(rtti.name(), watched, kind);

        let _ = crate::actor::channel::DynActorChannel::prepare_send(watcher.dyn_channel(), message)
            .send()
            .await;
    }

    /// Non-blocking delivery for the terminal `Drop` path (panic / task abort),
    /// where awaiting is impossible. Best-effort: a saturated mailbox drops the
    /// signal.
    pub(crate) fn deliver_now(
        &self,
        watched: ActorId,
        rtti: &'static ActorRtti,
        kind: TerminationKind,
    ) {
        let Some((watcher, message)) = self.prepare(watched, rtti, kind) else {
            return;
        };

        crate::obs::terminated_delivered(rtti.name(), watched, kind);

        let _ = crate::actor::channel::DynActorChannel::prepare_send(watcher.dyn_channel(), message)
            .try_send();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn actor_id_option_is_niche_packed() {
        assert_eq!(size_of::<ActorId>(), size_of::<usize>());
        assert_eq!(
            size_of::<Option<ActorId>>(),
            size_of::<ActorId>(),
            "Option<ActorId> must niche-pack so a send from outside any actor is a free `None`",
        );
    }

    #[test]
    fn ids_are_monotonic_and_nonzero() {
        let a = ActorId::new();
        let b = ActorId::new();
        assert!(b.as_usize() > a.as_usize(), "ids increase monotonically");
        assert_ne!(a.as_usize(), 0, "0 is reserved as the `Option<ActorId>` niche");
    }
}
