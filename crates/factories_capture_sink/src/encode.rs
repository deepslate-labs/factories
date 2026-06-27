//! Mapping from framework [`CaptureEvent`]s onto codec encoder calls.
//!
//! This is the one place that knows both the framework's event types and the
//! codec's wire model. The `*_tag` helpers own the wire-stable numbering of the
//! framework enums - the codec stores those bytes opaquely, and the log reader
//! interprets them - so they're pinned by tests.

use core::num::NonZeroU64;
use factories_actor::actor::lifecycle::TerminationKind;
use factories_actor::actor::supervision::ActorId;
use factories_actor::capture::{CaptureEvent, Dispatch, EventId};
use factories_capture_codec::segment::{EventRef, SegmentEncoder};
use std::num::NonZeroUsize;

/// Convert an [`ActorId`] (a non-zero `usize`) to the codec's `NonZeroU64`.
const fn actor_nz(id: ActorId) -> NonZeroU64 {
    let id = id.as_non_zero_usize();

    // At the moment this check is theoretically unnecessary,
    // but nothing dictates that usize is smaller or equal sized
    // to a u64.
    if NonZeroUsize::BITS > NonZeroU64::BITS {
        assert!(
            id.get() <= NonZeroU64::MAX.get() as usize,
            "ActorId is too large to fit in NonZeroU64"
        );
    }

    // SAFETY: A fitting, non-zero usize must always have at least one bit
    //         set in the bit range for a NonZeroU64.
    unsafe { NonZeroU64::new_unchecked(id.get() as _) }
}

/// Convert an [`EventId`] to the codec's [`EventRef`].
fn event_ref(id: EventId) -> EventRef {
    EventRef {
        actor: actor_nz(id.actor),
        seq: id.seq as u64,
    }
}

/// The wire tag for a message's dispatch kind.
const fn dispatch_tag(dispatch: Dispatch) -> u8 {
    match dispatch {
        Dispatch::Tell => 0,
        Dispatch::Ask => 1,
    }
}

/// The wire tag for an actor's termination kind.
const fn termination_tag(kind: TerminationKind) -> u8 {
    match kind {
        TerminationKind::Finished => 0,
        TerminationKind::Failed => 1,
        TerminationKind::Aborted => 2,
    }
}

/// Encode one [`CaptureEvent`] into `encoder`, stamped with `tick`. Metadata
/// only - the message payload slot stays empty.
pub fn encode_event(encoder: &mut SegmentEncoder, tick: u64, event: CaptureEvent) {
    match event {
        CaptureEvent::Spawned {
            id,
            actor_type,
            parent,
            caused_by,
        } => encoder.spawned(
            tick,
            id.seq as u64,
            actor_nz(id.actor),
            actor_type,
            parent.map(actor_nz),
            caused_by.map(event_ref),
        ),
        CaptureEvent::Died {
            id,
            actor_type,
            reason,
            caused_by,
        } => encoder.died(
            tick,
            id.seq as u64,
            actor_nz(id.actor),
            actor_type,
            termination_tag(reason),
            caused_by.map(event_ref),
        ),
        CaptureEvent::Message {
            id,
            from,
            to: _, // always equal to `id.actor`, so the codec stores it once
            message_type,
            dispatch,
            caused_by,
        } => encoder.message(
            tick,
            id.seq as u64,
            actor_nz(id.actor),
            from.map(actor_nz),
            message_type,
            dispatch_tag(dispatch),
            caused_by.map(event_ref),
            &[],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_tags_are_stable() {
        assert_eq!(dispatch_tag(Dispatch::Tell), 0);
        assert_eq!(dispatch_tag(Dispatch::Ask), 1);
    }

    #[test]
    fn termination_tags_are_stable() {
        assert_eq!(termination_tag(TerminationKind::Finished), 0);
        assert_eq!(termination_tag(TerminationKind::Failed), 1);
        assert_eq!(termination_tag(TerminationKind::Aborted), 2);
    }
}
