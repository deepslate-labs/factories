//! A per-thread, per-sink segment under construction, plus its flush policy.
//!
//! This sits at the codec level: it owns a [`SegmentEncoder`] and decides *when*
//! a segment is done, but it never sees a `CaptureEvent` - the sink encodes
//! events into [`encoder_mut`](SegmentBuffer::encoder_mut) from the outside. A
//! segment flushes once its accumulated records exceed `size_threshold` bytes,
//! or its tick span reaches `max_age_ticks` (computed from the per-event tick
//! the sink already read, so no extra clock reads), whichever comes first.

use factories_capture_codec::segment::{SegmentAnchor, SegmentEncoder};

/// A segment buffer with size- and age-based flush thresholds.
pub struct SegmentBuffer {
    encoder: Option<SegmentEncoder>,
    open_tick: u64,
    size_threshold: usize,
    max_age_ticks: u64,
}

impl SegmentBuffer {
    /// A closed buffer that flushes at `size_threshold` record bytes or a tick
    /// span of `max_age_ticks`.
    pub fn new(size_threshold: usize, max_age_ticks: u64) -> Self {
        Self {
            encoder: None,
            open_tick: 0,
            size_threshold,
            max_age_ticks,
        }
    }

    /// Whether a segment is currently open (events have been recorded since the
    /// last flush).
    pub fn is_open(&self) -> bool {
        self.encoder.is_some()
    }

    /// Open a fresh segment with the clock readings taken now. Call only when
    /// `!is_open()`.
    pub fn open(&mut self, anchor: SegmentAnchor) {
        self.open_tick = anchor.tick;
        self.encoder = Some(SegmentEncoder::new(anchor));
    }

    /// The open segment's encoder, for the sink to encode an event into. Call
    /// only when `is_open()`.
    pub fn encoder_mut(&mut self) -> &mut SegmentEncoder {
        self.encoder.as_mut().expect("segment buffer is open")
    }

    /// Whether the open segment has hit a flush threshold. Always `false` when
    /// closed.
    pub fn should_flush(&self, tick: u64) -> bool {
        match &self.encoder {
            None => false,
            Some(encoder) => {
                encoder.encoded_len() >= self.size_threshold
                    || tick.saturating_sub(self.open_tick) >= self.max_age_ticks
            }
        }
    }

    /// Finish the open segment with its close readings, returning the bytes and
    /// resetting to closed. Call only when `is_open()`.
    pub fn take(&mut self, close_mono_micros: u64, close_tick: u64) -> Vec<u8> {
        self.encoder
            .take()
            .expect("segment buffer is open")
            .finish(close_mono_micros, close_tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::num::NonZeroU64;
    use factories_capture_codec::segment::{ClockMode, decode_segment};

    fn nz(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("non-zero")
    }

    fn anchor(tick: u64) -> SegmentAnchor {
        SegmentAnchor {
            mode: ClockMode::Monotonic,
            unix_micros: 1_750_000_000_000_000,
            mono_micros: 0,
            tick,
        }
    }

    #[test]
    fn closed_buffer_never_flushes() {
        let buffer = SegmentBuffer::new(0, 0);
        assert!(!buffer.is_open());
        assert!(!buffer.should_flush(99_999));
    }

    #[test]
    fn flushes_when_size_threshold_exceeded() {
        let mut buffer = SegmentBuffer::new(16, u64::MAX); // age never triggers
        buffer.open(anchor(100));
        assert!(!buffer.should_flush(100), "freshly opened is not full");
        for seq in 1..=4 {
            buffer.encoder_mut().spawned(100, seq, nz(1), "A", None, None);
        }
        assert!(buffer.should_flush(100), "records now exceed the size threshold");
    }

    #[test]
    fn flushes_when_age_threshold_reached() {
        let mut buffer = SegmentBuffer::new(usize::MAX, 1000); // size never triggers
        buffer.open(anchor(100));
        buffer.encoder_mut().spawned(100, 1, nz(1), "A", None, None);
        assert!(!buffer.should_flush(900), "900 - 100 < 1000");
        assert!(buffer.should_flush(1100), "1100 - 100 >= 1000");
    }

    #[test]
    fn take_resets_and_yields_a_decodable_segment() {
        let mut buffer = SegmentBuffer::new(16, u64::MAX);
        buffer.open(anchor(100));
        buffer.encoder_mut().spawned(100, 1, nz(7), "Widget", None, None);

        let bytes = buffer.take(5, 200);
        assert!(!buffer.is_open(), "closed after take");

        let (segment, _) = decode_segment(&bytes).expect("valid segment");
        assert_eq!(segment.records.len(), 1);
        assert_eq!(segment.close_tick, 200);
    }
}
