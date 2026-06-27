//! The reference [`CaptureSink`]: per-thread segment buffers flushed
//! synchronously to a shared writer.

use std::cell::RefCell;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use factories_actor::capture::{CaptureEvent, CaptureSink};
use factories_capture_codec::segment::SegmentAnchor;

use crate::buffer::SegmentBuffer;
use crate::clock::Clock;
use crate::encode::encode_event;
use crate::writer::{SegmentWriter, SinkWriter};

/// Flush a segment once its records reach this many bytes.
const DEFAULT_SIZE_THRESHOLD: usize = 64 * 1024;

static NEXT_SINK_ID: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Each thread's open segment, one per sink it has recorded to, keyed by
    /// sink id and linear-scanned (the common case is a single capture).
    static SLOTS: RefCell<Vec<ThreadSlot>> = const { RefCell::new(Vec::new()) };
}

struct ThreadSlot {
    sink_id: usize,
    buffer: SegmentBuffer,
    writer: Arc<dyn SegmentWriter>,
    clock: Clock,
}

impl Drop for ThreadSlot {
    fn drop(&mut self) {
        // Flush this thread's final open segment on thread exit (e.g. a tokio
        // worker stopping at runtime shutdown).
        if self.buffer.is_open() {
            let (_, close_mono, close_tick) = self.clock.anchors();
            let bytes = self.buffer.take(close_mono, close_tick);
            self.writer.write_segment(&bytes);
        }
    }
}

/// A [`CaptureSink`] that buffers each thread's events into codec segments and
/// flushes them synchronously to a shared writer.
///
/// The per-event hot path is lock-free - a thread-local buffer, no shared state.
/// The writer's mutex is taken only when a full segment flushes (usually just a
/// memcpy into a `BufWriter`). There's no background thread, so flushing is
/// inherently complete and no guard is needed; a thread's final partial segment
/// flushes when the thread exits.
pub struct BufferedCaptureSink {
    id: usize,
    clock: Clock,
    writer: Arc<dyn SegmentWriter>,
    size_threshold: usize,
    max_age_ticks: u64,
}

impl BufferedCaptureSink {
    /// Capture to `writer` with default flush thresholds.
    pub fn new<W: Write + Send + 'static>(writer: W) -> Self {
        // Age-based flush is off by default: a tick budget is clock-rate
        // dependent (and we no longer track a rate), so a meaningful wall-clock
        // age needs rate-awareness - deferred. Size + thread-exit cover the MVP.
        Self::with_thresholds(writer, DEFAULT_SIZE_THRESHOLD, u64::MAX)
    }

    /// Capture to `writer`, flushing a segment once it reaches `size_threshold`
    /// record bytes or spans `max_age_ticks` clock ticks (in this sink's clock
    /// units). `u64::MAX` disables the age trigger.
    pub fn with_thresholds<W: Write + Send + 'static>(
        writer: W,
        size_threshold: usize,
        max_age_ticks: u64,
    ) -> Self {
        Self {
            id: NEXT_SINK_ID.fetch_add(1, Ordering::Relaxed),
            clock: Clock::detect(),
            writer: Arc::new(SinkWriter::new(writer)),
            size_threshold,
            max_age_ticks,
        }
    }

    /// Flush this thread's open segment (if any), then push buffered bytes
    /// through to the underlying writer. Other threads' buffers are untouched.
    pub fn flush(&self) {
        SLOTS.with_borrow_mut(|slots| {
            if let Some(slot) = slots.iter_mut().find(|slot| slot.sink_id == self.id)
                && slot.buffer.is_open()
            {
                let (_, close_mono, close_tick) = self.clock.anchors();
                let bytes = slot.buffer.take(close_mono, close_tick);
                self.writer.write_segment(&bytes);
            }
        });
        self.writer.flush();
    }

    fn open_anchor(&self) -> SegmentAnchor {
        let (unix_micros, mono_micros, tick) = self.clock.anchors();
        SegmentAnchor {
            mode: self.clock.mode(),
            unix_micros,
            mono_micros,
            tick,
        }
    }
}

impl CaptureSink for BufferedCaptureSink {
    fn record(&self, event: CaptureEvent) {
        let tick = self.clock.now();
        SLOTS.with_borrow_mut(|slots| {
            let slot = match slots.iter_mut().find(|slot| slot.sink_id == self.id) {
                Some(slot) => slot,
                None => slots.push_mut(ThreadSlot {
                    sink_id: self.id,
                    buffer: SegmentBuffer::new(self.size_threshold, self.max_age_ticks),
                    writer: Arc::clone(&self.writer),
                    clock: self.clock,
                }),
            };

            if !slot.buffer.is_open() {
                slot.buffer.open(self.open_anchor());
            }
            encode_event(slot.buffer.encoder_mut(), tick, event);
            if slot.buffer.should_flush(tick) {
                let (_, close_mono, close_tick) = self.clock.anchors();
                let bytes = slot.buffer.take(close_mono, close_tick);
                self.writer.write_segment(&bytes);
            }
        });
    }
}
