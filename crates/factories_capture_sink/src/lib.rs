//! Reference capture sink for the factories actor framework.
//!
//! The framework emits [`CaptureEvent`](factories_actor::capture::CaptureEvent)s
//! to a [`CaptureSink`](factories_actor::capture::CaptureSink) on the actor loop
//! thread. This crate provides the batteries-included implementation: each loop
//! thread fills its own [`factories_capture_codec`] segment buffer (no shared
//! state, no contention), timestamping events with the cheapest clock the
//! machine offers, and hands full or aged segments to a background writer
//! thread that drives the bytes to an [`std::io::Write`].
//!
//! It is one implementation of the sink contract, not the only one - the format
//! lives in [`factories_capture_codec`] and the trait in `factories_actor`, so a
//! different sink (a network streamer, a ring buffer) can reuse both.

pub mod clock;
