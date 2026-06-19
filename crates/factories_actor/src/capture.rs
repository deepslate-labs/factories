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

use crate::actor::lifecycle::TerminationKind;
use crate::actor::state::SharedActorState;
use crate::actor::supervision::ActorId;
use crate::actor::Actor;

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
        sink.record(CaptureEvent::Spawned {
            id,
            actor_type: A::RTTI.name(),
            parent: None,    // stage 4: thread the spawning actor
            caused_by: None, // stage 4: thread the spawn's cause
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
            caused_by: None, // stage 4: link a failure to its handler event
        });
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
