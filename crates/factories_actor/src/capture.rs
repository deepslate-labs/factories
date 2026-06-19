//! Append-only capture log: typed observation events emitted to a sink.
//!
//! The actor framework emits a [`CaptureEvent`] for each observable mesh action
//! - an actor born, an actor dead, a message delivered - to a [`CaptureSink`]
//! the mesh is configured with. It is an *observation record*, not an
//! event-sourcing journal: it captures what the mesh did, not enough to re-run
//! an actor's internals.
//!
//! Causality is by explicit id links, never by timing: every event has an
//! [`EventId`] = `(ActorId, local-seq)`, and each carries the `caused_by` event
//! that triggered it. The host-side sink adds wall-clock time and stream order;
//! the framework never consults a clock.

use alloc::sync::Arc;
use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use crate::actor::lifecycle::TerminationKind;
use crate::actor::state::SharedActorState;
use crate::actor::supervision::ActorId;
use crate::actor::Actor;
use crate::message::envelope::EnvelopeType;

/// Identity of a captured event: the actor that emitted it plus that actor's
/// own monotonic sequence number.
///
/// Globally unique with no global counter - `ActorId` is process-unique and
/// `seq` is per-actor (bumped only on the emitting actor's loop). `Option<EventId>`
/// niche-packs via `ActorId`'s `NonZero`, so "no cause" / "external sender" costs
/// no extra space.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct EventId {
    /// The actor that emitted the event.
    pub actor: ActorId,
    /// That actor's per-actor sequence number for this event.
    pub seq: usize,
}

/// How a captured message was dispatched.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Dispatch {
    /// Fire-and-forget (`tell`).
    Tell,
    /// Request-response (`ask`).
    Ask,
}

/// One observation in the capture log.
///
/// Every event carries its own [`EventId`] and the `caused_by` event that
/// triggered it (`None` at a causal root - a message from outside the mesh, or a
/// spawn with no parent), forming a causal DAG independent of wall-clock time.
#[derive(Debug, Clone)]
pub enum CaptureEvent {
    /// An actor was born (reached `Running`).
    Spawned {
        id: EventId,
        actor_type: &'static str,
        /// The actor that spawned it, or `None` for a root spawn.
        parent: Option<ActorId>,
        caused_by: Option<EventId>,
    },
    /// An actor terminated.
    Died {
        id: EventId,
        actor_type: &'static str,
        reason: TerminationKind,
        caused_by: Option<EventId>,
    },
    /// A message was delivered to an actor - emitted by the receiver.
    Message {
        id: EventId,
        /// The sender, or `None` if sent from outside any actor.
        from: Option<ActorId>,
        /// The receiver (always equal to `id.actor`).
        to: ActorId,
        message_type: &'static str,
        dispatch: Dispatch,
        caused_by: Option<EventId>,
    },
}

/// The write end of the capture log: receives [`CaptureEvent`]s as the mesh runs.
///
/// Shared across every actor in a captured mesh (carried by an inheritable
/// extension), hence `Send + Sync`. Implementations own the byte format, framing,
/// and wall-clock stamping; the framework hands them only typed events and never
/// blocks the actor loop waiting on one.
pub trait CaptureSink: Send + Sync {
    /// Record one event. Called from actor loops - must not block.
    fn record(&self, event: CaptureEvent);
}

crate::declare_extension!(
    /// The capture sink for a mesh.
    ///
    /// Set once at the root spawn with
    /// [`ActorLauncher::with_extension`](crate::spawn::ActorLauncher::with_extension);
    /// it is inheritable, so it flows to every actor spawned under that root, and
    /// each actor's loop emits its [`CaptureEvent`]s to it.
    pub CAPTURE_SINK: Arc<dyn CaptureSink>, inheritable
);

/// Emit a [`CaptureEvent::Spawned`] for `shared`'s actor, if the mesh is captured.
///
/// Called from the run loop once the actor reaches `Running`.
pub(crate) fn record_spawned<A: Actor + ?Sized>(shared: &SharedActorState<A>) {
    if let Some(sink) = shared.capture_sink() {
        let id = shared.next_capture_event_id();
        let origin = shared.capture_birth();
        sink.record(CaptureEvent::Spawned {
            id,
            actor_type: A::RTTI.name(),
            parent: origin.map(|frame| frame.actor),
            caused_by: origin.and_then(|frame| frame.handling),
        });
    }
}

/// Emit a [`CaptureEvent::Died`] for `shared`'s actor, if the mesh is captured.
///
/// Called from the run loop's stop path (clean / failed) and the terminal drop
/// guard (abort); the reason is read from the recorded termination outcome.
pub(crate) fn record_died<A: Actor + ?Sized>(shared: &SharedActorState<A>) {
    if let Some(sink) = shared.capture_sink() {
        let id = shared.next_capture_event_id();
        let reason = shared
            .termination_reason()
            .map(|reason| reason.kind())
            .unwrap_or(TerminationKind::Aborted);
        sink.record(CaptureEvent::Died {
            id,
            actor_type: A::RTTI.name(),
            reason,
            caused_by: None, // future: link a failure to its handler event
        });
    }
}

/// The loop-scoped capture context of the actor whose handler is currently
/// running on this thread: its id and the event it is handling (for `caused_by`).
///
/// `handling` is itself optional: a frame can exist without a causing event (work
/// done outside message dispatch).
#[derive(Copy, Clone)]
pub(crate) struct CaptureFrame {
    actor: ActorId,
    handling: Option<EventId>,
}

std::thread_local! {
    /// The running actor's capture frame, set per-poll around its handler (see
    /// [`WithFrame`]); `None` when no captured actor is running on this thread -
    /// e.g. a send from outside the mesh.
    static CURRENT_FRAME: Cell<Option<CaptureFrame>> = const { Cell::new(None) };
}

impl From<EnvelopeType> for Dispatch {
    fn from(ty: EnvelopeType) -> Self {
        match ty {
            EnvelopeType::Tell => Dispatch::Tell,
            EnvelopeType::Ask => Dispatch::Ask,
        }
    }
}

/// What a send stamps onto its dispatch context from the current frame: the
/// sender and the event whose handling produced the send (both `None` for a send
/// from outside any captured actor).
#[derive(Debug, Copy, Clone, Default)]
pub struct CaptureStamp {
    /// The sending actor, or `None` if sent from outside any actor.
    pub from: Option<ActorId>,
    /// The event the sender was handling when it sent, for the receiver's `caused_by`.
    pub caused_by: Option<EventId>,
}

/// Read the current frame as a [`CaptureStamp`] - called at every send site.
pub(crate) fn current_stamp() -> CaptureStamp {
    CURRENT_FRAME.with(|frame| match frame.get() {
        Some(frame) => CaptureStamp {
            from: Some(frame.actor),
            caused_by: frame.handling,
        },
        None => CaptureStamp::default(),
    })
}

/// The capture frame of the actor currently running on this thread, if any. Read
/// at spawn to record the new actor's `parent` (the frame's actor) and the event
/// that caused the spawn (the frame's `handling`).
pub(crate) fn current_frame() -> Option<CaptureFrame> {
    CURRENT_FRAME.with(|frame| frame.get())
}

/// Record the message being dispatched (if the mesh is captured) and wrap the
/// handler future so that, while it runs, *this* actor is the current frame -
/// so any send or spawn it makes is attributed to it and to this message.
///
/// Called by the dispatcher for each delivered message - the `#[messages]`-generated
/// dispatcher reaches it through `capture_instrument_if_enabled!`; a hand-written
/// dispatcher calls it directly.
pub fn instrument_handler<A: Actor + ?Sized, F: Future<Output = ()>>(
    shared: &SharedActorState<A>,
    stamp: CaptureStamp,
    message_type: &'static str,
    dispatch: Dispatch,
    handler: F,
) -> WithFrame<F> {
    let frame = shared.capture_sink().map(|sink| {
        let id = shared.next_capture_event_id();
        sink.record(CaptureEvent::Message {
            id,
            from: stamp.from,
            to: id.actor,
            message_type,
            dispatch,
            caused_by: stamp.caused_by,
        });
        CaptureFrame {
            actor: id.actor,
            handling: Some(id),
        }
    });

    WithFrame {
        frame,
        inner: handler,
    }
}

/// Future that makes `frame` the current capture frame on this thread for each
/// poll of `inner`, restoring the previous frame afterward - the same per-poll
/// enter/exit pattern as `tracing`'s `Instrument`.
#[pin_project::pin_project]
pub struct WithFrame<F> {
    frame: Option<CaptureFrame>,
    #[pin]
    inner: F,
}

impl<F: Future> Future for WithFrame<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();

        // Restore the previous frame on scope exit - *including* a panic unwind out
        // of the inner poll - so a panicking handler never leaves its frame set for
        // the next actor polled on this worker thread (which would misattribute that
        // actor's sends). This is the analogue of `tracing`'s span-guard drop.
        struct Restore(Option<CaptureFrame>);
        impl Drop for Restore {
            fn drop(&mut self) {
                CURRENT_FRAME.with(|current| current.set(self.0));
            }
        }

        let _restore = Restore(CURRENT_FRAME.with(|current| current.replace(*this.frame)));
        this.inner.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::supervision::ActorId;
    use alloc::vec::Vec;
    use std::sync::Mutex;

    #[derive(Default)]
    struct CollectingSink(Mutex<Vec<CaptureEvent>>);

    impl CaptureSink for CollectingSink {
        fn record(&self, event: CaptureEvent) {
            self.0.lock().expect("sink mutex").push(event);
        }
    }

    #[test]
    fn sink_receives_recorded_events() {
        let sink = CollectingSink::default();
        let id = EventId {
            actor: ActorId::new(),
            seq: 1,
        };

        sink.record(CaptureEvent::Spawned {
            id,
            actor_type: "Widget",
            parent: None,
            caused_by: None,
        });

        let events = sink.0.lock().expect("sink mutex");
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            CaptureEvent::Spawned {
                actor_type: "Widget",
                parent: None,
                ..
            }
        ));
    }
}
