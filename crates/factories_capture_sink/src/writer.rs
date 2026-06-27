//! The shared, synchronous segment writer behind a [`BufferedCaptureSink`].
//!
//! [`BufferedCaptureSink`]: crate::sink::BufferedCaptureSink
//!
//! Per-thread buffers flush their finished segments through a [`SegmentWriter`]
//! without knowing the concrete writer type, so the thread-local slots can be
//! type-erased. [`SinkWriter`] is the reference implementation: it writes
//! synchronously to `W` under a mutex, buffered by a [`BufWriter`]. The lock is
//! taken only at segment boundaries (and the per-segment write is usually just a
//! memcpy into the buffer), so the actor loops' lock-free hot path is untouched
//! and there's no background thread to join - flushing is inherently complete.

use std::io::{BufWriter, Write};
use std::sync::Mutex;

use factories_capture_codec::stream::write_stream_header;

/// Type-erased sink of finished segment bytes.
pub trait SegmentWriter: Send + Sync {
    /// Append one finished segment's bytes to the stream.
    fn write_segment(&self, bytes: &[u8]);
    /// Flush buffered bytes through to the underlying writer.
    fn flush(&self);
}

/// A [`SegmentWriter`] that writes synchronously to `W` under a mutex.
pub struct SinkWriter<W: Write> {
    writer: Mutex<BufWriter<W>>,
}

impl<W: Write> SinkWriter<W> {
    /// Wrap `writer`, emitting the stream header immediately.
    pub fn new(writer: W) -> Self {
        let mut buffered = BufWriter::new(writer);
        let mut header = Vec::new();
        write_stream_header(&mut header);
        // Best-effort: a capture writer must never panic the actor loop.
        let _ = buffered.write_all(&header);
        Self {
            writer: Mutex::new(buffered),
        }
    }
}

impl<W: Write + Send> SegmentWriter for SinkWriter<W> {
    fn write_segment(&self, bytes: &[u8]) {
        // Recover a poisoned lock: a panic in another writer mustn't silence
        // capture, and swallow IO errors so a bad disk never panics a loop.
        let mut writer = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        let _ = writer.write_all(bytes);
    }

    fn flush(&self) {
        let mut writer = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        let _ = writer.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::num::NonZeroU64;
    use factories_capture_codec::segment::{ClockMode, SegmentAnchor, SegmentEncoder};
    use factories_capture_codec::stream::{read_stream_header, segments};
    use std::sync::Arc;

    /// A `Write` whose bytes we can inspect after the fact.
    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn nz(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("non-zero")
    }

    fn segment(tick: u64, actor: u64) -> Vec<u8> {
        let mut encoder = SegmentEncoder::new(SegmentAnchor {
            mode: ClockMode::Monotonic,
            unix_micros: 1_750_000_000_000_000,
            mono_micros: 0,
            tick,
        });
        encoder.spawned(tick, 1, nz(actor), "A", None, None);
        encoder.finish(1, tick + 100)
    }

    #[test]
    fn writes_header_then_segments_and_round_trips() {
        let sink_bytes = Arc::new(Mutex::new(Vec::new()));
        let writer = SinkWriter::new(SharedWriter(sink_bytes.clone()));

        writer.write_segment(&segment(100, 5));
        writer.write_segment(&segment(300, 6));
        writer.flush();

        let bytes = sink_bytes.lock().unwrap().clone();
        let body = read_stream_header(&bytes).expect("valid stream header");
        let decoded: Vec<_> = segments(body)
            .collect::<Result<_, _>>()
            .expect("segments decode");

        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].anchor.tick, 100);
        assert_eq!(decoded[1].anchor.tick, 300);
    }
}
